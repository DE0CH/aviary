// egress as YYC's sandbox gateway (aviary platform spec 3.11, 17.2): the credential and liveness, the fixed names,
// self hosts (the package mirror, with real pip and npm), the internet rules, the upload cap, the model route's ZDR,
// a database outage, the drain, and the in-cluster API. The gateway contract itself is also checked from outside by
// aviary-conformance in chat.yml's conformance job.
import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { Agent, createServer, request, type IncomingMessage, type Server } from "node:http";
import { connect as netConnect, type AddressInfo, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { createServer as createTlsServer, connect as tlsConnect } from "node:tls";
import { randomUUID } from "node:crypto";
import { gzipSync } from "node:zlib";
import { createSocket } from "node:dgram";
import { HostPolicy, HostRules } from "../src/egress/allowlist.ts";
import { EgressApi, acceptLists } from "../src/egress/api.ts";
import { BudgetGate } from "../src/egress/budget.ts";
import { parseEgressConfig, type EgressConfig } from "../src/egress/config.ts";
import { EgressLog } from "../src/egress/egresslog.ts";
import { isRequestId, uuidv7 } from "../src/egress/ids.ts";
import { AddressGuard } from "../src/egress/netguard.ts";
import { CostSniffer, denyText, Egress, FIXED_BODY_MAX, parseTarget, routed, type EgressOptions } from "../src/egress/proxy.ts";
import { DnsRefusal, ThreatResolver } from "../src/egress/threatdns.ts";
import { credentialToken, hermitGetter, KeySet, SandboxTokens, signingKey } from "../src/egress/tokens.ts";
import { freshEgressDb } from "./egress-db.ts";
import { FakeHermit, FakeMagpie, masterKey } from "./fakes/hermit.ts";

const HERMIT_BEARER = "pair-egress-hermit";
const MAGPIE_BEARER = "pair-egress-magpie";
const TOOLS_BEARER = "pair-egress-tools";

let t: Awaited<ReturnType<typeof freshEgressDb>>;
let egress: Egress;
let tokens: SandboxTokens;
let hermit: FakeHermit;
let magpie: FakeMagpie;
let port: number;
let up: Server;
let upPort: number;
const upHits: Record<string, number> = {};
let upLastHeaders: IncomingMessage["headers"] = {};
let tls: ReturnType<typeof createTlsServer>;
let tlsPort: number;
let tmp: string;
let wheel: Buffer;
let sb: ReturnType<FakeHermit["token"]>;
let cfg: EgressConfig;

const RESOLVE: Record<string, string[]> = {
  "allowed.test": ["127.0.0.1"],
  "apt.test": ["127.0.0.1"],
  "www.ranked.test": ["127.0.0.1"],
  "paste.ranked.test": ["127.0.0.1"],
  "nope.ranked.test": ["127.0.0.1"],
  "unknown.test": ["127.0.0.1"],
  "evil.test": ["10.0.0.5", "fd00::1"],
};

function config(upstream: string, extra: Record<string, unknown> = {}): EgressConfig {
  return parseEgressConfig({
    product: { name: "YYC AI", locale: "en" },
    models: [{ id: "vendor/claude-x", label: "X", reasoning: "high" }],
    defaultModel: "vendor/claude-x",
    titleModel: "vendor/claude-x",
    contextTokens: 1048576,
    routes: [
      { name: "model", upstream: `${upstream}/api`, model: true, inject: { Authorization: "Bearer ${REAL_KEY}" }, allowPaths: ["/v1/messages"] },
      { name: "tools", upstream, inject: { Authorization: "Bearer ${EGRESS_TOKEN_TOOLS}" }, allowPaths: ["/mcp"], timeoutSeconds: 180 },
    ],
    selfHosts: ["vip.egress.test"],
    maxUploadBytes: 256 * 1024,
    ...extra,
  });
}

function options(o: Partial<EgressOptions> & { db: typeof t.db; cacheDir: string }): EgressOptions {
  const { db, ...rest } = o;
  return {
    config: cfg,
    log: new EgressLog(db),
    tokens,
    policy: new HostPolicy({ allow: ["allowed.test", "evil.test", ".apt.test", ".ranked.test", "malware.test"], deny: [".paste.ranked.test"], configDeny: ["nope.ranked.test"] }),
    pairTokens: { magpie: MAGPIE_BEARER, hermit: HERMIT_BEARER },
    cacheBytes: 50 * 1024 * 1024,
    guard: new AddressGuard([], ["127.0.0.1/32"]),
    allowedPorts: [80, 443, tlsPort, upPort],
    resolver: async (h) => {
      if (h === "malware.test") throw new DnsRefusal("deny:threat", "malware.test is blocked as a security threat (malware or phishing)");
      return RESOLVE[h] ?? [];
    },
    upstreams: { pypi: `http://127.0.0.1:${upPort}`, pypiFiles: `http://127.0.0.1:${upPort}/files`, npm: `http://127.0.0.1:${upPort}/npm` },
    aptHosts: ["apt.test"],
    env: { REAL_KEY: "sk-real", EGRESS_TOKEN_TOOLS: TOOLS_BEARER },
    internal: { magpie: `http://127.0.0.1:${magpie.port}`, hermit: `http://127.0.0.1:${hermit.port}` },
    ...rest,
  };
}

/** A minimal valid wheel of package foo 1.0, built with Python's zipfile. */
function buildWheel(dir: string): Buffer {
  const py = `
import zipfile, sys
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr("foo/__init__.py", "")
    z.writestr("foo-1.0.dist-info/METADATA", "Metadata-Version: 2.1\\nName: foo\\nVersion: 1.0\\n")
    z.writestr("foo-1.0.dist-info/WHEEL", "Wheel-Version: 1.0\\nGenerator: test\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n")
    z.writestr("foo-1.0.dist-info/RECORD", "")
`;
  const p = join(dir, "foo-1.0-py3-none-any.whl");
  execFileSync("python3", ["-c", py, p]);
  return readFileSync(p);
}

before(async () => {
  t = await freshEgressDb("egress");
  tmp = mkdtempSync(join(tmpdir(), "egress-"));
  wheel = buildWheel(tmp);
  const wheelSha = createHash("sha256").update(wheel).digest("hex");
  hermit = new FakeHermit(HERMIT_BEARER);
  const hermitUrl = await hermit.listen();
  magpie = new FakeMagpie();
  await magpie.listen();
  tokens = new SandboxTokens(hermitGetter(hermitUrl, HERMIT_BEARER));
  assert.equal(await tokens.start(), true);
  sb = hermit.token();

  up = createServer((req, res) => {
    const p = req.url || "/";
    upHits[p] = (upHits[p] ?? 0) + 1;
    upLastHeaders = req.headers;
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      // x-stall plays an upstream that goes quiet: "head" never answers, "mid" sends the head and one event then
      // nothing, "drip" sends an event every 200 ms for 1.5 s then ends, "end:<ms>" sends the head and ends after ms
      const stall = String(req.headers["x-stall"] ?? "");
      if (stall === "head") return;
      if (stall) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.write("event: ping\ndata: {}\n\n");
        if (stall === "drip") {
          const iv = setInterval(() => res.write("event: ping\ndata: {}\n\n"), 200);
          setTimeout(() => (clearInterval(iv), res.end()), 1500);
        }
        if (stall.startsWith("end:")) setTimeout(() => res.end(), Number(stall.slice(4)));
        return;
      }
      if (p === "/simple/foo/") {
        res.writeHead(200, { "content-type": "text/html" });
        return res.end(`<!DOCTYPE html><html><body><a href="http://127.0.0.1:${upPort}/files/packages/foo-1.0-py3-none-any.whl#sha256=${wheelSha}">foo-1.0-py3-none-any.whl</a></body></html>`);
      }
      if (p.endsWith(".metadata")) {
        // like files.pythonhosted.org: gzipped when the client allows it
        const plain = Buffer.from("Metadata-Version: 2.1\nName: foo\n".repeat(50));
        if (String(req.headers["accept-encoding"] || "").includes("gzip")) {
          const gz = gzipSync(plain);
          res.writeHead(200, { "content-type": "application/octet-stream", "content-encoding": "gzip", "content-length": String(gz.length) });
          return res.end(gz);
        }
        res.writeHead(200, { "content-type": "application/octet-stream", "content-length": String(plain.length) });
        return res.end(plain);
      }
      if (p === "/files/packages/foo-1.0-py3-none-any.whl") {
        res.writeHead(200, { "content-type": "application/octet-stream", "content-length": String(wheel.length) });
        return res.end(wheel);
      }
      if (p.startsWith("/npm/left-pad/-/")) {
        res.writeHead(200, { "content-type": "application/octet-stream" });
        return res.end("TARBALL");
      }
      if (p === "/npm/left-pad") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ name: "left-pad", "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { name: "left-pad", version: "1.0.0", dist: { tarball: `http://127.0.0.1:${upPort}/npm/left-pad/-/left-pad-1.0.0.tgz` } } } }));
      }
      if (p.startsWith("/pool/") || p.startsWith("/plain")) {
        res.writeHead(200, { "content-type": "application/octet-stream" });
        return res.end("DEB");
      }
      if (p === "/api/v1/messages" && req.headers.accept === "text/event-stream") {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.write('event: message_start\ndata: {"type":"message_start","message":{"usage":{"input_tokens":3}}}\n\n');
        setTimeout(() => res.end('event: message_delta\ndata: {"type":"message_delta","usage":{"output_tokens":2,"cost":0.0042}}\n\n'), 20);
        return;
      }
      if (p === "/api/v1/messages" || p === "/api/v1/messages/count_tokens") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ echo: JSON.parse(body || "{}"), usage: { cost: 0.0123 } }));
      }
      if (p.startsWith("/mcp")) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ headers: req.headers, url: p }));
      }
      res.writeHead(404).end();
    });
  });
  await new Promise<void>((r) => up.listen(0, "127.0.0.1", r));
  upPort = (up.address() as AddressInfo).port;
  cfg = config(`http://127.0.0.1:${upPort}`);

  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", join(tmp, "k.pem"), "-out", join(tmp, "c.pem"), "-days", "1", "-subj", "/CN=allowed.test"], { stdio: "ignore" });
  tls = createTlsServer({ key: readFileSync(join(tmp, "k.pem")), cert: readFileSync(join(tmp, "c.pem")) }, (s) => s.on("error", () => {}).on("data", (d) => s.write(`echo:${d}`)));
  tls.on("tlsClientError", () => {});
  await new Promise<void>((r) => tls.listen(0, "127.0.0.1", r));
  tlsPort = (tls.address() as AddressInfo).port;

  egress = new Egress(options({ db: t.db, cacheDir: join(tmp, "cache") }));
  port = await egress.listen(0, "127.0.0.1");
});

after(async () => {
  up.closeAllConnections();
  await egress.drain();
  await egress.log.close(1000);
  tokens.stop();
  up.close();
  tls.close();
  hermit.close();
  magpie.close();
  await t.drop();
  rmSync(tmp, { recursive: true, force: true });
});

const basic = (tok: string) => "Basic " + Buffer.from(`sbx:${tok}`).toString("base64");
const pa = () => ({ "proxy-authorization": basic(sb.token) });

function connectVia(target: string, auth?: string, p = port): Promise<{ status: number; head: string; sock: Socket }> {
  return new Promise((resolve, reject) => {
    const s = netConnect(p, "127.0.0.1");
    let buf = "";
    s.on("error", reject);
    s.write(`CONNECT ${target} HTTP/1.1\r\nHost: ${target}\r\n${auth ? `Proxy-Authorization: ${auth}\r\n` : ""}\r\n`);
    const onData = (d: Buffer) => {
      buf += d.toString("latin1");
      const i = buf.indexOf("\r\n\r\n");
      if (i < 0) return;
      s.off("data", onData);
      const status = Number(buf.split(" ")[1]);
      if (status !== 200) {
        s.on("data", (x) => (buf += x));
        s.on("end", () => resolve({ status, head: buf, sock: s }));
        return;
      }
      resolve({ status, head: buf, sock: s });
    };
    s.on("data", onData);
  });
}

type Res = { status: number; headers: IncomingMessage["headers"]; body: string; reused: boolean };
function http(path: string, o: { headers?: Record<string, string>; method?: string; body?: string | Buffer; port?: number; agent?: Agent } = {}): Promise<Res> {
  return new Promise((resolve, reject) => {
    const r = request({ host: "127.0.0.1", port: o.port ?? port, path, method: o.method ?? "GET", headers: o.headers, agent: o.agent }, (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("error", reject);
      res.on("aborted", () => reject(new Error("the answer was cut")));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body, reused: r.reusedSocket }));
    });
    r.on("error", reject);
    r.end(o.body);
  });
}

const rows = async (log = egress.log, db = t.db) => {
  await log.flush();
  return (await db.query("select * from egress_log order by id")).rows;
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));


// ---- configuration ---------------------------------------------------------------------------------------------

test("egress.json: only the fixed names model and tools are routes; smallModel defaults; the allowed models", () => {
  assert.equal(cfg.smallModel, "vendor/claude-x");
  assert.equal(cfg.product.locale, "en");
  const base = { product: { name: "P" }, models: [{ id: "a/m", label: "M" }], defaultModel: "a/m", contextTokens: 10, routes: cfg.routes };
  assert.throws(() => parseEgressConfig({ ...base, routes: [...cfg.routes, { name: "sent-files", upstream: "http://x", inject: {} }] }), /only the fixed names/);
  assert.throws(() => parseEgressConfig({ ...base, routes: [cfg.routes[1]] }), /no route model/);
  assert.throws(() => parseEgressConfig({ ...base, routes: [{ ...cfg.routes[0], model: false }, cfg.routes[1]] }), /model: true/);
  assert.throws(() => parseEgressConfig({ ...base, contextTokens: 0 }), /contextTokens/);
  assert.throws(() => parseEgressConfig({ ...base, budget: { limitUsd: -1, since: "2026-10-21T00:00:00Z" } }), /limitUsd/);
  const c = parseEgressConfig({ ...base, smallModel: "a/small", titleModel: "a/title", budget: { limitUsd: 30, since: "2026-10-21T10:00:00+08:00", ownerName: "Deyao" } });
  assert.deepEqual(c.budget, { limitUsd: 30, since: "2026-10-21T02:00:00.000Z", ownerName: "Deyao" });
  assert.equal(c.selfHosts.length, 0);
  assert.equal(c.maxUploadBytes, 8 * 1024 * 1024);
});

test("targets and fixed names: exact host, no port or port 80; dot and encoded paths are never routed", () => {
  assert.deepEqual(parseTarget("http://sbx:t@magpie/v1/sandbox/x?a=1"), { host: "magpie", authority: "magpie", port: 80, pathq: "/v1/sandbox/x?a=1" });
  assert.deepEqual(parseTarget("http://[fd00::1]:3128"), { host: "fd00::1", authority: "[fd00::1]:3128", port: 3128, pathq: "/" });
  assert.equal(parseTarget("/mirror/npm/x")!.host, null);
  assert.ok(routed("magpie", "PUT", "/v1/sandbox/blobs/ab?x=1"));
  assert.ok(!routed("magpie", "GET", "/v1/conversations"));
  assert.ok(!routed("hermit", "GET", "/v1/sandbox/../people"));
  assert.ok(!routed("hermit", "GET", "/v1/sandbox/%2e%2e/people"));
  assert.ok(!routed("hermit", "GET", "/v1/sandbox//self"));
  assert.ok(routed("model", "POST", "/v1/messages/count_tokens"));
  assert.ok(routed("model", "GET", "/v1/aviary/models"));
  assert.ok(!routed("model", "GET", "/v1/models"));
  assert.ok(routed("tools", "POST", "/mcp") && !routed("tools", "POST", "/mcp/x"));
  assert.ok(isRequestId(uuidv7()) && !isRequestId(uuidv7().toUpperCase()) && !isRequestId(randomUUID()));
});

// ---- the credential ---------------------------------------------------------------------------------------------

test("tokens: hermit's hst1 codec, the source address, expiry, kid and signature", async () => {
  assert.equal(credentialToken(undefined), "missing");
  assert.equal(credentialToken("Bearer x"), "missing");
  assert.equal(credentialToken(basic("abc")), "abc");
  assert.equal(credentialToken("Basic " + Buffer.from("root:abc").toString("base64")), "malformed");
  const key = signingKey(masterKey(7));
  const keys = KeySet.from({ keys: [{ kid: 7, alg: "Ed25519", publicKey: key.publicKey }, { kid: 9, alg: "RSA", publicKey: "x" }] });
  assert.equal(keys.size, 1);
  const h = new FakeHermit("b", masterKey(7));
  const now = Math.floor(Date.now() / 1000);
  const { token, claims } = h.token({ key, person: "staff@example.org", source: "192.0.2.9" });
  assert.deepEqual(keys.verify(token, now, "::ffff:192.0.2.9"), { claims });
  assert.deepEqual(keys.verify(token, now, "192.0.2.8"), { error: "wrong_source" });
  assert.deepEqual(keys.verify(h.token({ key, expiresAt: now }).token, now, "127.0.0.1"), { error: "expired" });
  assert.equal("claims" in keys.verify(h.token({ key, source: "0.0.0.0" }).token, now, "192.0.2.1"), true, "0.0.0.0: any source");
  const flipped = token.slice(0, -3) + (token.at(-3) === "A" ? "B" : "A") + token.slice(-2);
  assert.deepEqual(keys.verify(flipped, now, "192.0.2.9"), { error: "bad_signature" });
  assert.deepEqual(keys.verify(h.token({ key: signingKey(masterKey(8)) }).token, now, "127.0.0.1"), { error: "unknown_kid" });
  for (const bad of ["", "hst1", "hst1.07.AA.AA", "hst2.7.AA.AA", token + ".x", token.replace("hst1.7.", "hst1.7.!")]) assert.deepEqual(keys.verify(bad, now, "192.0.2.9"), { error: "malformed" }, bad);
});

test("liveness: revoked within 30 s, a 404 refuses, a hermit outage never refuses a valid token, a new kid is learnt, an empty key set is a failed fetch", async () => {
  const h = new FakeHermit("lb");
  const url = await h.listen();
  let clock = Date.now();
  const st = new SandboxTokens(hermitGetter(url, "lb"), { now: () => clock });
  try {
    assert.equal(await st.refreshKeys(false), true);
    const a = h.token();
    assert.ok("claims" in (await st.verify(a.token, "127.0.0.1")), "admitted before any liveness answer");
    await sleep(50);
    assert.equal(h.lookups.get(a.claims.sandboxId), 1);
    h.live.set(a.claims.sandboxId, false);
    assert.ok("claims" in (await st.verify(a.token, "127.0.0.1")), "the cached answer holds for 30 s");
    clock += 30_000;
    await st.verify(a.token, "127.0.0.1");
    await sleep(50);
    assert.deepEqual(await st.verify(a.token, "127.0.0.1"), { error: "not_live" });

    const u = h.token();
    h.live.set(u.claims.sandboxId, "unknown");
    await st.verify(u.token, "127.0.0.1");
    await sleep(50);
    assert.deepEqual(await st.verify(u.token, "127.0.0.1"), { error: "not_live" }, "hermit's 404 is definitive");

    const seen = h.token();
    await st.verify(seen.token, "127.0.0.1");
    await sleep(50);
    for (const o of ["reset", "hang", "unavailable"] as const) {
      h.outage = o;
      clock += 31_000;
      const never = h.token();
      for (let i = 0; i < 3; i++) {
        assert.ok("claims" in (await st.verify(seen.token, "127.0.0.1")), `${o}: a token seen before`);
        assert.ok("claims" in (await st.verify(never.token, "127.0.0.1")), `${o}: a token never seen`);
        await sleep(400);
      }
    }
    assert.deepEqual(await st.verify(a.token, "127.0.0.1"), { error: "not_live" }, "a refusal is kept while hermit fails");
    h.outage = "up";

    // a key published after the last fetch: the first token naming it refetches at once, even right after a routine
    // refresh (the unknown-kid clock is its own)
    assert.equal(await st.refreshKeys(false), true, "a routine refresh just now");
    const k2 = signingKey(masterKey(2));
    h.keys.push(k2);
    let fetches = h.keyFetches;
    let t0 = Date.now();
    assert.ok("claims" in (await st.verify(h.token({ key: k2 }).token, "127.0.0.1")));
    assert.equal(h.keyFetches, fetches + 1);
    const k3 = signingKey(masterKey(3));
    h.keys.push(k3);
    fetches = h.keyFetches;
    const tok3 = h.token({ key: k3 }).token;
    clock += 9_000;
    assert.deepEqual(await st.verify(tok3, "127.0.0.1"), { error: "unknown_kid" }, "within 10 s of the last unknown-kid refetch: 403 at once");
    assert.equal(h.keyFetches, fetches, "no refetch inside the window");
    clock += 1_000;
    assert.ok("claims" in (await st.verify(tok3, "127.0.0.1")), "the retry after the 10 s mark refetches and is admitted");
    assert.equal(h.keyFetches, fetches + 1);

    // a hermit key rotation: the refetch lands on a replica that does not publish the new kid yet: 403 after that one
    // refetch; a retry inside the window is refused at once; the retry after it refetches and is admitted
    const k4 = signingKey(masterKey(4));
    clock += 20_000;
    fetches = h.keyFetches;
    const tok4 = h.token({ key: k4 }).token;
    const orig = h.server.listeners("request")[0] as (...a: unknown[]) => void;
    let calls = 0;
    h.server.removeAllListeners("request");
    h.server.on("request", (req, res) => {
      if (req.url === "/v1/sandbox-token-keys" && ++calls === 2) h.keys.push(k4);
      orig(req, res);
    });
    assert.deepEqual(await st.verify(tok4, "127.0.0.1"), { error: "unknown_kid" }, "one refetch, without the kid: 403");
    assert.equal(h.keyFetches, fetches + 1);
    assert.deepEqual(await st.verify(tok4, "127.0.0.1"), { error: "unknown_kid" }, "a retry inside the window: 403 at once");
    assert.equal(h.keyFetches, fetches + 1);
    clock += 10_000;
    assert.ok("claims" in (await st.verify(tok4, "127.0.0.1")), "learnt by the retry's refetch");
    assert.equal(h.keyFetches, fetches + 2);

    // a kid hermit never publishes: concurrent requests share one refetch, then the definitive 403; nobody waits more
    clock += 20_000;
    fetches = h.keyFetches;
    const bogus = h.token({ key: signingKey(masterKey(99)) }).token;
    t0 = Date.now();
    const [x, y] = await Promise.all([st.verify(bogus, "127.0.0.1"), st.verify(bogus, "127.0.0.1")]);
    assert.deepEqual([x, y], [{ error: "unknown_kid" }, { error: "unknown_kid" }]);
    assert.equal(h.keyFetches, fetches + 1, "concurrent requests share the one refetch");
    assert.ok(Date.now() - t0 < 1000, "the wait is that one fetch");

    // hermit answers 200 with no usable key (an empty list, or only keys of another alg or length): a failed fetch,
    // the last set kept, never a 403 for a valid token
    const valid = h.token({ key: k2 }).token;
    const saved = [...h.keys];
    for (const body of ['{"keys":[]}', '{"keys":[{"kid":9,"alg":"RS256","publicKey":"AAAA"},{"kid":10,"alg":"Ed25519","publicKey":"AAAA"}]}']) {
      h.server.removeAllListeners("request");
      h.server.on("request", (req, res) => {
        if (req.url === "/v1/sandbox-token-keys") return void res.writeHead(200, { "content-type": "application/json" }).end(body);
        orig(req, res);
      });
      assert.equal(await st.refreshKeys(false), false, `${body}: a failed fetch`);
      assert.ok("claims" in (await st.verify(valid, "127.0.0.1")), `${body}: the last set kept`);
    }
    h.server.removeAllListeners("request");
    h.server.on("request", orig);
    h.keys.splice(0, h.keys.length, ...saved);
  } finally {
    st.stop();
    h.close();
  }
});

test("token keys: the last set hermit published is stored, and a start while hermit is away verifies with it", async () => {
  const { keyStore } = await import("../src/egress/db.ts");
  const h = new FakeHermit("ks");
  const url = await h.listen();
  const store = keyStore(t.db);
  try {
    await t.db.query("delete from token_keys");
    const a = new SandboxTokens(hermitGetter(url, "ks"), { store });
    assert.equal(await a.loadStored(), false, "nothing stored yet");
    assert.equal(await a.refreshKeys(false), true);
    for (let i = 0; i < 50 && !(await store.load()); i++) await sleep(20);
    assert.ok(await store.load(), "saved after the fetch");
    const good = await store.load();
    const orig = h.server.listeners("request")[0] as (...a: unknown[]) => void;
    h.server.removeAllListeners("request");
    h.server.on("request", (req, res) => {
      if (req.url === "/v1/sandbox-token-keys") return void res.writeHead(200, { "content-type": "application/json" }).end('{"keys":[]}');
      orig(req, res);
    });
    assert.equal(await a.refreshKeys(false), false, "an empty key set is a failed fetch");
    await sleep(100);
    assert.equal(await store.load(), good, "and is never stored over the last good set");
    h.server.removeAllListeners("request");
    h.server.on("request", orig);
    const tok = h.token();
    h.outage = "reset";
    const b = new SandboxTokens(hermitGetter(url, "ks"), { store });
    assert.equal(await b.refreshKeys(false), false, "hermit away");
    assert.equal(b.usable, false);
    assert.equal(await b.loadStored(), true);
    assert.equal(b.usable, true);
    assert.equal(b.loaded, false);
    assert.ok("claims" in (await b.verify(tok.token, "127.0.0.1")), "a valid token verifies with the stored set");
    h.outage = "up";
    assert.equal(await b.refreshKeys(false), true);
    assert.equal(b.loaded, true);
  } finally {
    h.close();
  }
});

test("proxy credential: 407 without one (JSON with Proxy-Authenticate on a fixed name), 403 not_allowed for a bad one", async () => {
  const none = await http("http://magpie/v1/sandbox/x");
  assert.equal(none.status, 407);
  assert.match(String(none.headers["proxy-authenticate"]), /^Basic realm="aviary-gateway"/);
  assert.equal(JSON.parse(none.body).code, "proxy_auth_required");
  for (const cred of [basic("hst1.1.AA.AA"), "Basic " + Buffer.from(`root:${sb.token}`).toString("base64"), basic(hermit.token({ key: signingKey(masterKey(5)) }).token)]) {
    const r = await http("http://magpie/v1/sandbox/x", { headers: { "proxy-authorization": cred } });
    assert.equal(r.status, 403, cred);
    assert.equal(JSON.parse(r.body).code, "not_allowed");
  }
  assert.equal(magpie.seen.length, 0, "nothing forwarded");
  // internet requests keep their plain-text refusals
  const inet = await http(`http://allowed.test:${upPort}/plain`);
  assert.equal(inet.status, 407);
  assert.equal(inet.body, "egress: proxy authentication required\n");
});

// ---- the fixed names --------------------------------------------------------------------------------------------

const FORGED = {
  authorization: "Bearer sandbox-sent-authorization",
  "x-api-key": "sandbox-sent-key",
  "aviary-person": "forged@example.org",
  "aviary-admin": "forged@example.org",
  "aviary-sandbox-id": "00000000-0000-7000-8000-000000000000",
  "aviary-sandbox-person": "forged@example.org",
};

test("magpie and hermit: /v1/sandbox/** only, the verified identity and the pair bearer added, the client's stripped, no log row", async () => {
  const s = hermit.token({ person: "Staff.Mixed@Example.ORG" });
  const rid = uuidv7();
  magpie.seen.length = 0;
  const r = await http("http://magpie/v1/sandbox/x?a=1", {
    headers: { "proxy-authorization": basic(s.token), ...FORGED, "aviary-request": rid, "aviary-protocol": "magpie-sandbox=1", "aviary-lease": "0190f6c2-0000-4000-8000-00000000c0de", "x-other": "kept" },
  });
  assert.equal(r.status, 200);
  const got = magpie.seen[0]!;
  assert.equal(got.url, "/v1/sandbox/x?a=1");
  assert.equal(got.headers.host, "magpie");
  assert.equal(got.headers.authorization, `Bearer ${MAGPIE_BEARER}`);
  assert.equal(got.headers["aviary-sandbox-id"], s.claims.sandboxId);
  assert.equal(got.headers["aviary-sandbox-conversation"], s.claims.conversationId);
  assert.equal(got.headers["aviary-sandbox-person"], "staff.mixed@example.org");
  assert.equal(got.headers["aviary-request"], rid);
  assert.equal(got.headers["aviary-protocol"], "magpie-sandbox=1");
  assert.equal(got.headers["aviary-lease"], "0190f6c2-0000-4000-8000-00000000c0de");
  assert.equal(got.headers["x-other"], "kept");
  for (const h of ["x-api-key", "proxy-authorization", "aviary-person", "aviary-admin"]) assert.equal(got.headers[h], undefined, h);

  await http("http://magpie/v1/sandbox/x", { headers: { ...pa(), "aviary-request": "not-a-v7-id" } });
  assert.ok(isRequestId(String(magpie.seen.at(-1)!.headers["aviary-request"])), "an invalid request id is replaced");

  for (const p of ["http://magpie/v1/conversations", "http://magpie/v1/sandbox/%2e%2e/conversations", "http://hermit/v1/people"]) {
    const n = magpie.seen.length;
    const x = await http(p, { headers: pa() });
    assert.equal(x.status, 403, p);
    assert.equal(JSON.parse(x.body).code, "not_allowed");
    assert.equal(magpie.seen.length, n);
  }
  const hs = await http("http://hermit/v1/sandbox/self", { headers: { ...pa(), ...FORGED } });
  assert.equal(hs.status, 200);
  assert.equal(JSON.parse(hs.body).name, "hermit");
  const hh = hermit.seen.at(-1)!;
  assert.equal(hh.headers.authorization, `Bearer ${HERMIT_BEARER}`);
  assert.equal(hh.headers["aviary-sandbox-person"], "staff@example.org");
  assert.equal(hh.headers["aviary-person"], undefined);
  const logged = (await rows()).filter((x) => x.host === "magpie" || x.host === "hermit" || String(x.path).startsWith("/v1/sandbox"));
  assert.equal(logged.length, 0, "magpie and hermit calls write no egress_log row");
  const m = egress.metrics().join("\n");
  assert.match(m, /egress_fixed_requests_total\{route="magpie",status="200"\} [1-9]/);
  assert.match(m, /egress_fixed_requests_total\{route="hermit",status="403"\} 1/);
  assert.match(m, /egress_fixed_bytes_total\{route="magpie",dir="down"\} [1-9]/);
});

test("magpie and hermit: a gateway-side failure is logged with request, conversation and person key, rate-limited, never the path", async () => {
  const dead = createServer();
  await new Promise<void>((r) => dead.listen(0, "127.0.0.1", r));
  const deadPort = (dead.address() as AddressInfo).port;
  await new Promise<void>((r) => dead.close(() => r()));
  const e = new Egress(options({ db: t.db, cacheDir: join(tmp, "cache-hot"), internal: { magpie: `http://127.0.0.1:${deadPort}`, hermit: `http://127.0.0.1:${hermit.port}` } }));
  const p = await e.listen(0, "127.0.0.1");
  const lines: string[] = [];
  const write = process.stderr.write.bind(process.stderr);
  process.stderr.write = ((c: string | Uint8Array, ...a: unknown[]) => {
    if (String(c).includes("egress fixed-name upstream")) lines.push(String(c));
    return (write as (...x: unknown[]) => boolean)(c, ...a);
  }) as typeof process.stderr.write;
  try {
    const s = hermit.token({ person: "Staff@Example.org" });
    const rid = uuidv7();
    const r = await http("http://magpie/v1/sandbox/secret-path?q=private", { port: p, headers: { "proxy-authorization": basic(s.token), "aviary-request": rid } });
    assert.equal(r.status, 502);
    for (let i = 0; i < 3; i++) assert.equal((await http("http://magpie/v1/sandbox/x", { port: p, headers: pa() })).status, 502);
    assert.equal(lines.length, 1, "at most one line per route per second");
    const l = JSON.parse(lines[0]!);
    assert.equal(l.route, "magpie");
    assert.equal(l.status, 502);
    assert.equal(l.request, rid);
    assert.equal(l.conversation, s.claims.conversationId);
    assert.match(l.person_key, /^u[0-9a-f]{24}$/);
    assert.doesNotMatch(lines[0]!, /secret-path|private|staff@example\.org/i, "never the path or the email");
    await sleep(1100);
    await http("http://magpie/v1/sandbox/x", { port: p, headers: pa() });
    assert.equal(lines.length, 2);
    assert.equal(JSON.parse(lines[1]!).notLogged, 3);
  } finally {
    process.stderr.write = write;
    await e.drain();
    await e.log.close(1000);
  }
});

test("limits: 64 MiB to magpie delivered, 64 MiB + 1 refused 413 too_large", async () => {
  const ok = await http("http://magpie/v1/sandbox/blob", { method: "PUT", headers: { ...pa(), "content-type": "application/octet-stream" }, body: Buffer.alloc(FIXED_BODY_MAX, 97) });
  assert.equal(ok.status, 200);
  assert.equal(JSON.parse(ok.body).bytes, FIXED_BODY_MAX);
  const over = await http("http://magpie/v1/sandbox/blob", { method: "PUT", headers: { ...pa(), "content-type": "application/octet-stream" }, body: Buffer.alloc(FIXED_BODY_MAX + 1, 97) });
  assert.equal(over.status, 413);
  assert.equal(JSON.parse(over.body).code, "too_large");
});

test("upload cap: it counts a CONNECT tunnel, but not 40 MiB of PUTs to magpie on one kept-alive connection", async () => {
  // the cap here is 256 KiB; 40 x 1 MiB through magpie on one connection all arrive
  const agent = new Agent({ keepAlive: true, maxSockets: 1 });
  const n = magpie.seen.length;
  for (let i = 0; i < 40; i++) {
    const r = await http("http://magpie/v1/sandbox/blob", { method: "PUT", headers: { ...pa(), "content-type": "application/octet-stream" }, body: Buffer.alloc(1024 * 1024, 98), agent });
    assert.equal(r.status, 200, `PUT ${i + 1}`);
    if (i > 0) assert.ok(r.reused, `PUT ${i + 1} on the same connection`);
  }
  agent.destroy();
  const puts = magpie.seen.slice(n);
  assert.equal(puts.length, 40);
  assert.ok(puts.every((p) => p.bytes === 1024 * 1024));

  // a tunnel past the cap is cut and logged deny:upload
  const { status, sock } = await connectVia(`allowed.test:${tlsPort}`, basic(sb.token));
  assert.equal(status, 200);
  await new Promise<void>((resolve) => {
    const c = tlsConnect({ socket: sock, servername: "allowed.test", rejectUnauthorized: false }, () => {
      const chunk = Buffer.alloc(64 * 1024, 120);
      for (let i = 0; i < 8; i++) if (!c.destroyed) c.write(chunk);
    });
    c.on("data", () => {});
    c.on("error", () => resolve());
    c.on("close", () => resolve());
  });
  await sleep(200);
  const cut = (await rows()).filter((x) => x.decision === "deny:upload" && x.kind === "connect");
  assert.equal(cut.length, 1);
  assert.ok(Number(cut[0].bytes_up) > 256 * 1024);
  assert.equal(cut[0].person, "staff@example.org");
});

test("model route: the real key injected, the sandbox's credentials and Aviary-* dropped, cost logged with person and request (JSON and SSE)", async () => {
  const rid = uuidv7();
  const r = await http("http://model/v1/messages", {
    method: "POST",
    headers: { ...pa(), ...FORGED, cookie: "c=1", "content-type": "application/json", "aviary-request": rid, "aviary-protocol": "gateway=1" },
    body: JSON.stringify({ model: "vendor/claude-x" }),
  });
  assert.equal(r.status, 200);
  assert.equal(JSON.parse(r.body).echo.model, "vendor/claude-x");
  assert.equal(upLastHeaders.authorization, "Bearer sk-real");
  for (const h of ["x-api-key", "cookie", "proxy-authorization", "aviary-sandbox-person", "aviary-sandbox-id", "aviary-person", "aviary-request", "aviary-protocol"]) assert.equal(upLastHeaders[h], undefined, h);
  const s = await http("http://model/v1/messages", { method: "POST", headers: { ...pa(), accept: "text/event-stream" }, body: "{}" });
  assert.match(s.body, /message_delta/);
  const ct = await http("http://model/v1/messages/count_tokens", { method: "POST", headers: pa(), body: JSON.stringify({ model: "claude-x" }) });
  assert.equal(ct.status, 200, "count_tokens is relayed; the id without its provider prefix is the model too");
  await sleep(50);
  const models = (await rows()).filter((x) => x.kind === "model" && x.decision === "allow");
  assert.deepEqual(models.map((x) => Number(x.cost_usd)), [0.0123, 0.0042, 0.0123]);
  assert.ok(models.every((x) => x.sandbox_id === sb.claims.sandboxId && x.conversation_id === sb.claims.conversationId && x.person === "staff@example.org"));
  assert.equal(models[0].request, rid);
  assert.deepEqual(models.map((x) => x.model), ["vendor/claude-x", null, "claude-x"]);
  const other = await http("http://model/v1/models", { headers: pa() });
  assert.equal(other.status, 403);
  const refused = await http("http://model/v1/messages", { method: "POST", headers: pa(), body: JSON.stringify({ model: "vendor/other" }) });
  assert.equal(refused.status, 400);
  assert.match(JSON.parse(refused.body).error.message, /not offered/);
});

test("model route: zero data retention is forced into every request body, whatever the caller sends", async () => {
  const send = (body: string) => http("http://model/v1/messages", { method: "POST", headers: { ...pa(), "content-type": "application/json" }, body });
  const plain = await send(JSON.stringify({ model: "vendor/claude-x" }));
  assert.equal(plain.status, 200);
  assert.deepEqual(JSON.parse(plain.body).echo.provider, { zdr: true, data_collection: "deny" });
  const loose = await send(JSON.stringify({ model: "vendor/claude-x", provider: { zdr: false, data_collection: "allow", order: ["a"] } }));
  assert.deepEqual(JSON.parse(loose.body).echo.provider, { zdr: true, data_collection: "deny", order: ["a"] });
  const odd = await send(JSON.stringify({ model: "vendor/claude-x", provider: "anything" }));
  assert.deepEqual(JSON.parse(odd.body).echo.provider, { zdr: true, data_collection: "deny" });
  // the configured thinking level for a chat turn (a request with tools)
  const turn = await send(JSON.stringify({ model: "vendor/claude-x", tools: [{ name: "t" }] }));
  assert.ok(JSON.stringify(JSON.parse(turn.body).echo).includes("high"));
  for (const bad of ["not json", "[1]", "null", ""]) {
    const r = await send(bad);
    assert.equal(r.status, 400, `body ${JSON.stringify(bad)} must be refused`);
  }
  const refused = (await rows()).filter((x) => x.kind === "model" && x.decision === "deny:body");
  assert.equal(refused.length, 4);
});

test("model route: a body of many MB passes rewritten, one too large is refused before it is read", async () => {
  const image = "A".repeat(12 * 1024 * 1024);
  const body = JSON.stringify({ model: "vendor/claude-x", messages: [{ role: "user", content: [{ type: "image", source: { type: "base64", media_type: "image/png", data: image } }] }], provider: { zdr: false } });
  const rs = await Promise.all([1, 2, 3].map(() => http("http://model/v1/messages", { method: "POST", headers: { ...pa(), "content-type": "application/json" }, body })));
  for (const r of rs) {
    assert.equal(r.status, 200);
    const echo = JSON.parse(r.body).echo;
    assert.deepEqual(echo.provider, { zdr: true, data_collection: "deny" });
    assert.equal(echo.messages[0].content[0].source.data.length, image.length);
  }
  const head = await new Promise<string>((resolve, reject) => {
    const s = netConnect(port, "127.0.0.1");
    let buf = "";
    s.on("error", reject);
    s.on("data", (d) => (buf += d.toString("latin1")));
    s.on("close", () => resolve(buf));
    s.write(`POST http://model/v1/messages HTTP/1.1\r\nHost: model\r\nProxy-Authorization: ${basic(sb.token)}\r\nContent-Type: application/json\r\nContent-Length: ${70 * 1024 * 1024}\r\n\r\n{"model":`);
  });
  assert.match(head, /^HTTP\/1\.1 413 /);
  assert.match(head, /limited to 67108864 bytes/);
});

test("model route: GET http://model/v1/aviary/models answers the R18 shape from egress.json", async () => {
  const r = await http("http://model/v1/aviary/models", { headers: pa() });
  assert.equal(r.status, 200);
  assert.deepEqual(JSON.parse(r.body), { default: "vendor/claude-x", small: "vendor/claude-x", title: "vendor/claude-x", contextTokens: 1048576, models: [{ id: "vendor/claude-x", label: "X", reasoning: "high" }] });
});

test("tools route: the tools pair bearer and the verified person, never the sandbox's credential; one log row", async () => {
  const r = await http("http://tools/mcp", { method: "POST", headers: { ...pa(), ...FORGED, "content-type": "application/json" }, body: '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' });
  assert.equal(r.status, 200);
  const h = JSON.parse(r.body).headers;
  assert.equal(h.authorization, `Bearer ${TOOLS_BEARER}`);
  assert.equal(h["aviary-sandbox-person"], "staff@example.org");
  assert.equal(h["aviary-sandbox-id"], sb.claims.sandboxId);
  assert.equal(h["x-api-key"], undefined);
  assert.equal(h["aviary-person"], undefined);
  assert.equal((await http("http://tools/other", { method: "POST", headers: pa() })).status, 403);
  const t2 = (await rows()).filter((x) => x.kind === "tools");
  assert.deepEqual(t2.map((x) => [x.status, x.decision, x.person]), [[200, "allow", "staff@example.org"], [403, "deny:path", "staff@example.org"]]);
});

// ---- the internet -----------------------------------------------------------------------------------------------

test("CONNECT: identity, allowlist, ports and addresses are enforced", async () => {
  assert.equal((await connectVia(`allowed.test:${tlsPort}`)).status, 407);
  assert.equal((await connectVia(`allowed.test:${tlsPort}`, basic("wrong"))).status, 403);
  const denied = await connectVia(`other.test:${tlsPort}`, basic(sb.token));
  assert.equal(denied.status, 403);
  assert.ok(denied.head.includes(denyText("other.test").trim()), denied.head);
  assert.match(denied.head, /cannot be enabled from the chat/);
  assert.doesNotMatch(denied.head, /request_network_access|ask the user|permission/i);
  assert.equal((await connectVia("allowed.test:22", basic(sb.token))).status, 403);
  const evil = await connectVia(`evil.test:${tlsPort}`, basic(sb.token));
  assert.equal(evil.status, 403);
  assert.match(evil.head, /non-public/);
  assert.equal((await connectVia(`10.0.0.1:${tlsPort}`, basic(sb.token))).status, 403);
  // a fixed name is not an internet host: CONNECT to one is refused like any unlisted host
  assert.equal((await connectVia("magpie:443", basic(sb.token))).status, 403);
});

test("CONNECT: an allowed host gets a TLS tunnel, and the call is logged with bytes", async () => {
  const { status, sock } = await connectVia(`allowed.test:${tlsPort}`, basic(sb.token));
  assert.equal(status, 200);
  const reply = await new Promise<string>((resolve, reject) => {
    const c = tlsConnect({ socket: sock, servername: "allowed.test", rejectUnauthorized: false }, () => c.write("hi"));
    c.on("data", (d) => {
      resolve(d.toString());
      c.end();
    });
    c.on("error", reject);
  });
  assert.equal(reply, "echo:hi");
  await sleep(100);
  const r = (await rows()).filter((x) => x.kind === "connect" && x.decision === "allow");
  assert.equal(r.at(-1).host, "allowed.test");
  assert.equal(r.at(-1).conversation_id, sb.claims.conversationId);
  assert.ok(Number(r.at(-1).bytes_up) > 0 && Number(r.at(-1).bytes_down) > 0);
  const denials = (await rows()).filter((x) => x.kind === "connect" && x.decision.startsWith("deny:")).map((x) => x.decision);
  for (const d of ["deny:auth", "deny:host", "deny:port", "deny:address"]) assert.ok(denials.includes(d), d);
});

test("the sandbox allowlist through the proxy: suffix allow, deny list, config deny, threat DNS, all logged", async () => {
  const ok = await connectVia(`www.ranked.test:${tlsPort}`, basic(sb.token));
  assert.equal(ok.status, 200);
  ok.sock.destroy();
  const paste = await connectVia(`paste.ranked.test:${tlsPort}`, basic(sb.token));
  assert.equal(paste.status, 403);
  assert.match(paste.head, /not on the sandbox's internet allowlist/);
  assert.equal((await connectVia(`nope.ranked.test:${tlsPort}`, basic(sb.token))).status, 403);
  const mal = await connectVia(`malware.test:${tlsPort}`, basic(sb.token));
  assert.equal(mal.status, 403);
  assert.match(mal.head, /security threat/);
  const h = await http(`http://unknown.test:${upPort}/plain`, { headers: pa() });
  assert.equal(h.status, 403);
  assert.match(h.body, /cannot be enabled from the chat/);
  const r = await rows();
  const decision = (host: string) => r.filter((x) => x.host === host).map((x) => x.decision);
  assert.deepEqual(decision("paste.ranked.test"), ["deny:denylist"]);
  assert.deepEqual(decision("nope.ranked.test"), ["deny:config"]);
  assert.deepEqual(decision("malware.test"), ["deny:threat"]);
  assert.deepEqual(decision("unknown.test"), ["deny:host"]);
});

test("absolute-URI HTTP is forwarded; apt-style GETs are cached", async () => {
  const a = await http(`http://allowed.test:${upPort}/plain`, { headers: pa() });
  assert.equal(a.status, 200);
  assert.equal(a.body, "DEB");
  assert.equal(upLastHeaders["proxy-authorization"], undefined, "proxy credentials never go upstream");
  const p = `http://apt.test:${upPort}/pool/main/x.deb`;
  assert.equal((await http(p, { headers: pa() })).body, "DEB");
  await sleep(50);
  const second = await http(p, { headers: pa() });
  assert.equal(second.body, "DEB");
  assert.equal(second.headers["x-egress-cache"], "hit");
  assert.equal(upHits["/pool/main/x.deb"], 1);
  assert.equal((await http(`http://other.test:${upPort}/plain`, { headers: pa() })).status, 403);
});

// ---- self hosts: the package mirror -----------------------------------------------------------------------------

test("self hosts: any port of a listed host, 127.0.0.1 and origin form are served locally, never refused as private", async () => {
  for (const target of ["http://vip.egress.test:3128/conformance/probe", "http://vip.egress.test/conformance/probe", "http://127.0.0.1:3128/x", "/x"]) {
    const r = await http(target, { headers: pa() });
    assert.equal(r.status, 404, target);
    assert.equal(r.body, "egress: not found\n");
  }
  assert.equal((await http("/mirror/pypi/simple/foo/")).status, 407);
  // pip and npm send the index URL's userinfo as Authorization on self-host paths
  assert.equal((await http("/mirror/pypi/simple/foo/", { headers: { authorization: basic(sb.token) } })).status, 200);
});

const run = (cmd: string, args: string[], env: Record<string, string>): Promise<{ code: number; out: string }> =>
  new Promise((resolve) => {
    execFile(cmd, args, { env: { PATH: process.env.PATH!, HOME: tmp, ...env }, timeout: 120_000 }, (e, stdout, stderr) => resolve({ code: e ? Number((e as { code?: number }).code ?? 1) : 0, out: `${stdout}${stderr}` }));
  });

test("pip download and npm view through http://sbx:<token>@<vip>:3128/mirror/… with the proxy environment set", async () => {
  const proxy = `http://sbx:${sb.token}@127.0.0.1:${port}`;
  const env = { http_proxy: proxy, https_proxy: proxy, HTTP_PROXY: proxy, HTTPS_PROXY: proxy, NO_PROXY: "localhost,127.0.0.1,::1", no_proxy: "localhost,127.0.0.1,::1" };
  const vip = `sbx:${sb.token}@vip.egress.test:3128`;
  const dest = mkdtempSync(join(tmp, "pip-"));
  const pip = await run("python3", ["-m", "pip", "download", "--no-deps", "--no-cache-dir", "--disable-pip-version-check", "--trusted-host", "vip.egress.test", "--index-url", `http://${vip}/mirror/pypi/simple`, "-d", dest, "foo"], env);
  assert.equal(pip.code, 0, pip.out);
  assert.deepEqual(readdirSync(dest), ["foo-1.0-py3-none-any.whl"]);
  assert.deepEqual(readFileSync(join(dest, "foo-1.0-py3-none-any.whl")), wheel);
  const npm = await run("npm", ["view", "left-pad", "version", "--registry", `http://vip.egress.test:3128/mirror/npm/`, "--cache", join(tmp, "npm-cache"), "--no-update-notifier"], env);
  assert.equal(npm.code, 0, npm.out);
  assert.equal(npm.out.trim().split("\n").at(-1), "1.0.0");
  const r = (await rows()).filter((x) => x.kind === "mirror" && x.decision.startsWith("allow"));
  assert.ok(r.some((x) => x.path.startsWith("/mirror/pypi/simple/foo")), "the pypi index through the mirror");
  assert.ok(r.some((x) => x.path.startsWith("/mirror/pypi/files/")), "the wheel through the mirror");
  assert.ok(r.some((x) => x.path === "/mirror/npm/left-pad"), "the packument through the mirror");
  assert.ok(r.every((x) => x.person === "staff@example.org"));
});

test("pypi mirror: index links rewritten, files cached; a gzipped .metadata arrives whole", async () => {
  const idx = await http("http://vip.egress.test:3128/mirror/pypi/simple/foo/", { headers: pa() });
  assert.equal(idx.status, 200);
  assert.match(idx.body, /href="\/mirror\/pypi\/files\/packages\/foo-1\.0-py3-none-any\.whl#sha256=/);
  const before = upHits["/files/packages/foo-1.0-py3-none-any.whl"] ?? 0;
  for (let i = 0; i < 2; i++) {
    const f = await http("http://vip.egress.test:3128/mirror/pypi/files/packages/foo-1.0-py3-none-any.whl", { headers: pa() });
    assert.equal(f.status, 200);
    await sleep(50);
  }
  assert.ok((upHits["/files/packages/foo-1.0-py3-none-any.whl"] ?? 0) <= Math.max(before, 1), "served from the cache");
  const want = "Metadata-Version: 2.1\nName: foo\n".repeat(50);
  const md = await http("http://vip.egress.test:3128/mirror/pypi/files/packages/foo-1.0-py3-none-any.whl.metadata", { headers: pa() });
  assert.equal(md.body, want);
  const m = await http("http://vip.egress.test:3128/mirror/npm/left-pad", { headers: { ...pa(), host: "vip.egress.test:3128" } });
  assert.equal(JSON.parse(m.body).versions["1.0.0"].dist.tarball, "http://vip.egress.test:3128/mirror/npm/left-pad/-/left-pad-1.0.0.tgz");
});

test("CostSniffer reads the last cost in a stream split across chunks", () => {
  const s = new CostSniffer();
  s.contentType = "text/event-stream";
  for (const c of ['data: {"type":"message_delta","us', 'age":{"cost":0.5}}\n', "\n", 'data: {"usage":{"cost":0.7}}\n']) s.push(Buffer.from(c));
  assert.equal(s.cost(), 0.7);
});

// ---- a database outage ------------------------------------------------------------------------------------------

test("database outage: model calls answer from the last budget status plus this replica's costs; log rows wait in memory, in order", async () => {
  const d = await freshEgressDb("egress_outage", true);
  let cost = 4;
  const u = createServer((req, res) => {
    req.resume();
    req.on("end", () => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, usage: { cost } }));
    });
  });
  await new Promise<void>((r) => u.listen(0, "127.0.0.1", r));
  const uPort = (u.address() as AddressInfo).port;
  const c = config(`http://127.0.0.1:${uPort}`, { budget: { limitUsd: 10, since: new Date(Date.now() - 60_000).toISOString(), ownerName: "Deyao" } });
  const gate = new BudgetGate(d.db, c.budget, 0);
  const log = new EgressLog(d.db);
  gate.attach(log);
  const e = new Egress(options({ db: d.db, cacheDir: join(tmp, "cache-outage"), config: c, log, budget: gate }));
  const p = await e.listen(0, "127.0.0.1");
  const call = () => http("http://model/v1/messages", { method: "POST", port: p, headers: pa(), body: JSON.stringify({ model: "vendor/claude-x" }) });
  try {
    await d.db.query("insert into egress_log (kind, host, decision, cost_usd) values ('model', 'x', 'allow', 1)");
    assert.equal((await call()).status, 200); // spend 1 + 4 = 5
    await sleep(100);
    d.relay!.cut();
    const t0 = Date.now();
    assert.equal((await call()).status, 200, "5 < 10: answered from the last status");
    assert.equal((await call()).status, 200, "5 + 4 (this replica, unwritten) < 10");
    const refused = await call();
    assert.equal(refused.status, 402, "5 + 4 + 4 >= 10: the replica's own costs count");
    assert.equal(JSON.parse(refused.body).error.type, "budget_exceeded");
    assert.ok(Date.now() - t0 < 4000, `no waiting on the database (${Date.now() - t0} ms)`);
    const tunnel = await connectVia(`allowed.test:${tlsPort}`, basic(sb.token), p);
    assert.equal(tunnel.status, 200, "internet requests answer too");
    tunnel.sock.destroy();
    await sleep(100);
    assert.ok(log.buffered >= 4, `rows wait in memory (${log.buffered})`);
    assert.match(e.metrics().join("\n"), /egress_log_buffered [4-9]/);
    d.relay!.restore();
    for (let i = 0; i < 50 && log.buffered; i++) await sleep(200);
    assert.equal(log.buffered, 0, "written once the database is back");
    const got = (await d.db.query("select kind, decision, cost_usd from egress_log order by id")).rows;
    assert.deepEqual(got.map((x) => [x.kind, x.decision]), [["model", "allow"], ["model", "allow"], ["model", "allow"], ["model", "allow"], ["model", "deny:budget"], ["connect", "allow"]]);
    const s = (await gate.status())!;
    assert.equal(s.spentUsd, 13);
    assert.equal(s.exhausted, true);
    cost = 0;
  } finally {
    await e.drain();
    await log.close(1000);
    u.close();
    await d.drop();
  }
});

test("database outage that hangs (accepts, never answers): model calls answer at once from the last status", async () => {
  const d = await freshEgressDb("egress_stall", true);
  const u = createServer((req, res) => {
    req.resume();
    req.on("end", () => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, usage: { cost: 4 } }));
    });
  });
  await new Promise<void>((r) => u.listen(0, "127.0.0.1", r));
  const c = config(`http://127.0.0.1:${(u.address() as AddressInfo).port}`, { budget: { limitUsd: 10, since: new Date(Date.now() - 60_000).toISOString() } });
  const gate = new BudgetGate(d.db, c.budget, 0);
  const log = new EgressLog(d.db);
  gate.attach(log);
  const e = new Egress(options({ db: d.db, cacheDir: join(tmp, "cache-stall"), config: c, log, budget: gate }));
  const p = await e.listen(0, "127.0.0.1");
  const call = async () => {
    const t0 = Date.now();
    const r = await http("http://model/v1/messages", { method: "POST", port: p, headers: pa(), body: JSON.stringify({ model: "vendor/claude-x" }) });
    return [r.status, Date.now() - t0];
  };
  try {
    assert.equal((await call())[0], 200); // spent 4
    await sleep(100);
    d.relay!.stall();
    const times: number[] = [];
    const statuses: number[] = [];
    for (let i = 0; i < 3; i++) {
      const [st, ms] = await call();
      statuses.push(st!);
      times.push(ms!);
    }
    assert.deepEqual(statuses, [200, 200, 402], "4 + 4 (this replica, unwritten) < 10, then 4 + 4 + 4 >= 10");
    assert.ok(times[0]! < 600, `the first call waits at most the read wait (${times})`);
    assert.ok(times.slice(1).every((ms) => ms < 150), `the others do not wait at all (${times})`);
    assert.ok(log.buffered >= 3);
    d.relay!.restore();
    for (let i = 0; i < 100 && log.buffered; i++) await sleep(200);
    assert.equal(log.buffered, 0, "written once the database answers again");
  } finally {
    await e.drain();
    await log.close(1000);
    u.close();
    await d.drop();
  }
});

test("budget: with no status ever known (database away since start) a replica counts its own costs from zero", async () => {
  const d = await freshEgressDb("egress_unknown", true);
  try {
    d.relay!.cut();
    const gate = new BudgetGate(d.db, { limitUsd: 5, since: new Date(Date.now() - 60_000).toISOString() }, 0);
    await gate.seed().catch(() => {});
    const s0 = (await gate.status())!;
    assert.equal(s0.spentUsd, 0);
    assert.equal(s0.exhausted, false);
    gate.noteCost(3);
    gate.noteCost(2.5);
    const s1 = (await gate.status())!;
    assert.equal(s1.spentUsd, 5.5);
    assert.equal(s1.exhausted, true, "the limit still holds against this replica's own costs");
    assert.ok(gate.unknownServed >= 2, "counted for /metrics");
  } finally {
    await d.drop();
  }
});

test("the log at exit: the final flush keeps retrying while the database is away, within its time", async () => {
  const d = await freshEgressDb("egress_close", true);
  try {
    d.relay!.cut();
    const log = new EgressLog(d.db);
    log.add({ kind: "model", host: "m", decision: "allow", cost_usd: 1 });
    log.add({ kind: "connect", host: "h", decision: "allow" });
    setTimeout(() => d.relay!.restore(), 1500);
    const t0 = Date.now();
    await log.close(10_000);
    assert.equal(log.buffered, 0, "written once the database came back");
    assert.ok(Date.now() - t0 < 8000);
    assert.equal((await d.db.query("select count(*)::int as n from egress_log")).rows[0].n, 2);
  } finally {
    await d.drop();
  }
});

test("the log buffer: bounded, the oldest non-model row dropped first, never a model row", async () => {
  const d = await freshEgressDb("egress_buffer", true);
  try {
    d.relay!.cut();
    const log = new EgressLog(d.db, 5);
    log.add({ kind: "model", host: "m", decision: "allow", cost_usd: 1 });
    for (let i = 0; i < 6; i++) log.add({ kind: "connect", host: `h${i}`, decision: "allow" });
    log.add({ kind: "model", host: "m2", decision: "allow" });
    await sleep(200);
    assert.equal(log.buffered, 5);
    assert.deepEqual(log.dropped, { connect: 3 });
    d.relay!.restore();
    await log.flush();
    const got = (await d.db.query("select kind, host from egress_log order by id")).rows.map((x) => x.host);
    assert.deepEqual(got, ["m", "h3", "h4", "h5", "m2"]);
  } finally {
    await d.drop();
  }
});

test("egress_log: a CONNECT to port 99999999999 and a model name with NUL are logged cleaned; the rows after them are written", async () => {
  const d = await freshEgressDb("egress_poison");
  const e = new Egress(options({ db: d.db, cacheDir: join(tmp, "cache-poison") }));
  const p = await e.listen(0, "127.0.0.1");
  try {
    const c = await connectVia("example.com:99999999999", undefined, p);
    assert.equal(c.status, 407, "no credential: deny:auth, logged");
    c.sock.destroy();
    const f = await http("http://allowed.test:99999999999/x", { port: p, headers: pa() });
    assert.equal(f.status, 403);
    const m = await http("http://model/v1/messages", { port: p, method: "POST", headers: { ...pa(), "content-type": "application/json" }, body: '{"model":"x\\u0000y"}' });
    assert.equal(m.status, 400, "deny:model");
    await http("http://allowed.test:" + upPort + "/plain/after", { port: p, headers: pa() });
    await e.log.flush();
    assert.equal(e.log.buffered, 0, "nothing stuck in memory");
    assert.equal(e.log.rejected, 0);
    const got = (await d.db.query("select kind, host, port, model, decision from egress_log order by id")).rows;
    assert.deepEqual(got.map((r) => [r.kind, r.port, r.decision]), [["connect", null, "deny:auth"], ["http", null, "deny:port"], ["model", null, "deny:model"], ["http", upPort, "allow"]]);
    assert.equal(got[2].model, "xy", "NUL stripped");
  } finally {
    await e.drain();
    await d.drop();
  }
});

test("egress_log: a row Postgres refuses as data is dropped and counted; its batch and the rows after it are written, in order", async () => {
  const d = await freshEgressDb("egress_refused");
  try {
    await d.db.query("alter table egress_log add constraint test_poison check (host <> 'poison')");
    const log = new EgressLog(d.db);
    log.add({ kind: "connect", host: "a", decision: "allow" });
    log.add({ kind: "http", host: "poison", decision: "allow" });
    log.add({ kind: "model", host: "b", decision: "allow", cost_usd: 1 });
    await log.flush();
    log.add({ kind: "connect", host: "c", decision: "allow" });
    await log.flush();
    assert.equal(log.buffered, 0);
    assert.equal(log.rejected, 1);
    assert.deepEqual(log.dropped, { http: 1 });
    assert.deepEqual((await d.db.query("select host from egress_log order by id")).rows.map((r) => r.host), ["a", "b", "c"]);
  } finally {
    await d.drop();
  }
});

test("egress_log rows are cleaned: ports outside 1-65535, NUL in text, long text, bad uuids", async () => {
  const { cleanRow, isDataError } = await import("../src/egress/egresslog.ts");
  for (const port of [0, -1, 65536, 99999999999, 1.5, NaN]) assert.equal(cleanRow({ kind: "connect", host: "h", decision: "allow", port }).port, null, String(port));
  assert.equal(cleanRow({ kind: "connect", host: "h", decision: "allow", port: 443 }).port, 443);
  const r = cleanRow({ kind: "model", host: "\u0000", decision: "deny:\u0000model", model: "m".repeat(500) + "\u0000", path: "/p\u0000", person: "a\u0000@b", request: "r\u0000", method: "PO\u0000ST", conversation_id: "nope", sandbox_id: randomUUID() });
  assert.equal(r.host, "-");
  assert.equal(r.decision, "deny:model");
  assert.equal(r.model!.length, 200);
  assert.equal(r.path, "/p");
  assert.equal(r.person, "a@b");
  assert.equal(r.method, "POST");
  assert.equal(r.request, "r");
  assert.equal(r.conversation_id, null);
  assert.match(r.sandbox_id!, /^[0-9a-f-]{36}$/);
  assert.equal(isDataError({ code: "22003" }), true);
  assert.equal(isDataError({ code: "23514" }), true);
  assert.equal(isDataError({ code: "08006" }), false);
  assert.equal(isDataError(new Error("Connection terminated")), false);
});

test("egress migrations: a database at a newer migration than this build serves; an edited migration is a start failure", async () => {
  const { MIGRATIONS, migrate } = await import("../src/egress/db.ts");
  const d = await freshEgressDb("egress_migr");
  try {
    // a newer build ran one more (expand-only) migration
    assert.deepEqual(await migrate(d.db, [...MIGRATIONS, "create table newer_thing (id int)"]), [MIGRATIONS.length + 1]);
    assert.deepEqual(await migrate(d.db), [], "this (older) build starts on the newer schema");
    // rows recorded by a build before sha256 was recorded get it filled in, then checked
    await d.db.query("update egress_migrations set sha256 = null where version = 1");
    assert.deepEqual(await migrate(d.db), []);
    assert.equal((await d.db.query("select count(*)::int as n from egress_migrations where sha256 is null")).rows[0].n, 0);
    const edited = [...MIGRATIONS];
    edited[1] = edited[1] + "\n-- edited";
    await assert.rejects(migrate(d.db, edited), /egress migration 2 was edited/);
  } finally {
    await d.drop();
  }
});

// ---- the drain --------------------------------------------------------------------------------------------------

test("drain: no new connections, idle ones closed, answers carry Connection: close, each kind cut at its own bound and counted", async () => {
  const bounds = { fixed: 1000, tunnel: 1800, tools: 2600, model: 3400 };
  const e = new Egress(options({ db: t.db, cacheDir: join(tmp, "cache-drain"), drainBounds: bounds }));
  const p = await e.listen(0, "127.0.0.1");
  const idleAgent = new Agent({ keepAlive: true, maxSockets: 1 });
  assert.equal((await http("http://magpie/v1/sandbox/x", { port: p, headers: pa(), agent: idleAgent })).status, 200);
  const idleSock = [...Object.values(idleAgent.freeSockets).flat()][0]!;
  const idleClosed = new Promise<void>((r) => idleSock.once("close", () => r()));
  const short = http("http://magpie/v1/sandbox/poll?ms=700", { port: p, headers: pa() });
  // one piece of work of each kind that outlives its bound; each resolves with when it was cut
  let t0 = 0;
  const cutAt = (pr: Promise<unknown>) => pr.then(() => "answered", () => Date.now() - t0);
  const fixed = cutAt(http("http://magpie/v1/sandbox/poll?ms=20000", { port: p, headers: pa() }));
  const tools = cutAt(http("http://tools/mcp", { port: p, method: "POST", headers: { ...pa(), "x-stall": "head", "content-type": "application/json" }, body: "{}" }));
  const model = cutAt(http("http://model/v1/messages", { port: p, method: "POST", headers: { ...pa(), "x-stall": "mid", "content-type": "application/json" }, body: '{"model":"vendor/claude-x"}' }));
  const tun = await connectVia(`allowed.test:${tlsPort}`, basic(sb.token), p);
  assert.equal(tun.status, 200);
  const tunnel = new Promise<number>((r) => tun.sock.once("close", () => r(Date.now() - t0)));

  // a model answer that began before the drain (its head has no Connection: close) and ends during it: the client's
  // next request on that kept-alive connection is served, answered with Connection: close
  const ka = netConnect(p, "127.0.0.1");
  let kaBuf = "";
  ka.on("data", (d) => (kaBuf += d.toString("latin1")));
  const until = async (re: RegExp, what: string) => {
    for (let i = 0; i < 100 && !re.test(kaBuf); i++) await sleep(20);
    assert.match(kaBuf, re, what);
  };
  const body = '{"model":"vendor/claude-x"}';
  ka.write(`POST http://model/v1/messages HTTP/1.1\r\nHost: model\r\nProxy-Authorization: ${basic(sb.token)}\r\nX-Stall: end:600\r\nContent-Type: application/json\r\nContent-Length: ${body.length}\r\n\r\n${body}`);
  await until(/^HTTP\/1\.1 200[^]*\r\n\r\n/, "the model answer's head");
  assert.doesNotMatch(kaBuf.slice(0, kaBuf.indexOf("\r\n\r\n")), /connection: close/i);

  await sleep(200);
  assert.deepEqual(e.inFlight(), { model: 2, tunnel: 1, fixed: 2, tools: 1 });
  t0 = Date.now();
  const drained = e.drain();
  await sleep(50);
  await assert.rejects(new Promise((resolve, reject) => netConnect(p, "127.0.0.1").on("connect", resolve).on("error", reject)), "a new connection is refused");
  await idleClosed;

  await until(/\r\n0\r\n\r\n$/, "the model answer ended");
  kaBuf = "";
  ka.write(`GET http://magpie/v1/sandbox/x HTTP/1.1\r\nHost: magpie\r\nProxy-Authorization: ${basic(sb.token)}\r\n\r\n`);
  await until(/^HTTP\/1\.1 200[^]*\r\n\r\n/, "a new request on the kept-alive connection is served");
  assert.match(kaBuf.slice(0, kaBuf.indexOf("\r\n\r\n")), /\r\nconnection: close/i, "with Connection: close");
  await new Promise<void>((r) => (ka.destroyed ? r() : ka.once("close", () => r())));

  const s = await short;
  assert.equal(s.status, 200, "an answer within its bound arrives");
  assert.equal(s.headers.connection, "close");
  const cut = await drained;
  assert.deepEqual(cut, { model: 1, tunnel: 1, fixed: 1, tools: 1 });
  const at = { fixed: await fixed, tunnel: await tunnel, tools: await tools, model: await model };
  for (const k of ["fixed", "tunnel", "tools", "model"] as const) {
    assert.equal(typeof at[k], "number", `${k} was cut`);
    const ms = at[k] as number;
    assert.ok(ms >= bounds[k] - 50 && ms < bounds[k] + 600, `${k} cut at ${ms} ms, its bound is ${bounds[k]} ms`);
  }
  const m = e.metrics().join("\n");
  for (const k of ["fixed", "tunnel", "tools", "model"]) assert.match(m, new RegExp(`egress_drain_cut_total\\{kind="${k}"\\} 1`));
  idleAgent.destroy();
});

test("model route: an upstream silent for the idle timeout is cut (504 before the head, the stream ended after it); a stream that keeps sending has no total timeout", async () => {
  const e = new Egress(options({ db: t.db, cacheDir: join(tmp, "cache-idle"), modelIdleMs: 500 }));
  const p = await e.listen(0, "127.0.0.1");
  try {
    const ids: string[] = [];
    const hdr = (stall: string) => {
      ids.push(uuidv7());
      return { ...pa(), "x-stall": stall, "content-type": "application/json", "aviary-request": ids.at(-1)! };
    };
    const body = '{"model":"vendor/claude-x"}';
    let t0 = Date.now();
    const head = await http("http://model/v1/messages", { port: p, method: "POST", headers: hdr("head"), body });
    assert.equal(head.status, 504);
    assert.equal(JSON.parse(head.body).code, "upstream_timeout");
    assert.ok(Date.now() - t0 >= 450 && Date.now() - t0 < 2000, `${Date.now() - t0} ms`);
    t0 = Date.now();
    await assert.rejects(http("http://model/v1/messages", { port: p, method: "POST", headers: hdr("mid"), body }), "a stream gone quiet is ended");
    assert.ok(Date.now() - t0 >= 450 && Date.now() - t0 < 2000, `${Date.now() - t0} ms`);
    t0 = Date.now();
    const drip = await http("http://model/v1/messages", { port: p, method: "POST", headers: hdr("drip"), body });
    assert.equal(drip.status, 200);
    assert.ok(Date.now() - t0 >= 1400, "1.5 s of events, three times the idle timeout, all relayed");
    assert.ok(drip.body.split("event: ping").length - 1 >= 7);
    const got = await rows(e.log);
    const mine = ids.map((id) => got.find((r) => r.request === id));
    assert.deepEqual(mine.map((r) => r?.status), [504, 200, 200]);
  } finally {
    await e.drain();
  }
});

// ---- the in-cluster API -----------------------------------------------------------------------------------------

test("in-cluster API: models for pigeon, the ops summary for hermit, the egress log for admin; 401 and 403 wrong_caller", async () => {
  const accept = acceptLists({ EGRESS_ACCEPT_PIGEON: "pig-next, pig", EGRESS_ACCEPT_HERMIT: "her", EGRESS_ACCEPT_ADMIN: "adm" });
  assert.throws(() => acceptLists({ EGRESS_ACCEPT_PIGEON: "p", EGRESS_ACCEPT_HERMIT: "h" }), /EGRESS_ACCEPT_ADMIN/);
  const cfgB = config(`http://127.0.0.1:${upPort}`, { budget: { limitUsd: 100, since: new Date(Date.now() - 86_400_000).toISOString(), ownerName: "Deyao" } });
  const api = new EgressApi({ db: t.db, config: cfgB, budget: new BudgetGate(t.db, cfgB.budget), accept });
  const p = await api.listen(0, "127.0.0.1");
  const get = (path: string, bearer?: string) => http(path, { port: p, headers: bearer ? { authorization: `Bearer ${bearer}` } : {} });
  try {
    const m = await get("/v1/aviary/models", "pig");
    assert.equal(m.status, 200);
    assert.equal(JSON.parse(m.body).default, "vendor/claude-x");
    assert.equal((await get("/v1/aviary/models", "pig-next")).status, 200, "both tokens of a rotation");
    const no = await get("/v1/aviary/models");
    assert.equal(no.status, 401);
    assert.equal(no.headers["www-authenticate"], "Bearer");
    assert.equal(JSON.parse(no.body).code, "unauthenticated");
    const wrong = await get("/v1/aviary/models", "her");
    assert.equal(wrong.status, 403);
    assert.equal(JSON.parse(wrong.body).code, "wrong_caller");
    assert.equal((await get("/v1/ops/summary", "pig")).status, 403);

    await egress.log.flush();
    await t.db.query("insert into egress_log (kind, host, decision, status, cost_usd, model, owner) values ('model', 'openrouter.ai', 'allow', 200, 0.5, 'vendor/claude-x', ' Old.Relay@Example.org ')");
    await t.db.query("insert into egress_log (kind, host, decision, status, cost_usd) values ('model', 'openrouter.ai', 'allow', 200, 0.25)");
    const sres = await get("/v1/ops/summary?tz=Asia%2FHong_Kong&recentMinutes=15", "her");
    assert.equal(sres.status, 200);
    const sum = JSON.parse(sres.body);
    for (const k of ["status", "total", "byUser", "byModel", "byDay"]) assert.ok(k in sum.budget, k);
    for (const k of ["byStatus", "recent", "deniedHosts"]) assert.ok(k in sum.egress, k);
    for (const k of ["modelErrors", "blocked", "list"]) assert.ok(k in sum.recent, k);
    assert.equal(sum.budget.status.limitUsd, 100);
    assert.equal(typeof sum.budget.status.spentUsd, "number");
    const owners = [...sum.budget.byUser.map((u: { owner: string }) => u.owner), ...sum.egress.recent.map((r: { owner: string | null }) => r.owner), ...sum.recent.list.map((r: { owner: string | null }) => r.owner)].filter(Boolean);
    assert.ok(owners.length > 0);
    for (const o of owners) assert.ok(o.includes("@") && o === o.trim().toLowerCase(), o);
    assert.ok(sum.budget.byUser.some((u: { owner: string }) => u.owner === "old.relay@example.org"));
    assert.ok(sum.egress.byStatus.every((x: { status: unknown }) => x.status === null || typeof x.status === "number"));
    assert.ok(sum.budget.total.callsMonth >= 2);

    const log = await get(`/v1/admin/egress-log?conversation=${sb.claims.conversationId}&limit=5`, "adm");
    assert.equal(log.status, 200);
    const lr = JSON.parse(log.body).rows;
    assert.ok(lr.length > 0 && lr.length <= 5);
    assert.ok(lr.every((r: { conversationId: string; owner: string }) => r.conversationId === sb.claims.conversationId && r.owner === "staff@example.org"));
    for (const k of ["at", "kind", "method", "host", "path", "status", "decision", "bytesUp", "bytesDown", "ms", "costUsd", "owner", "conversationId", "sandboxId"]) assert.ok(k in lr[0], k);
    assert.equal((await get("/v1/admin/egress-log", "her")).status, 403);
    assert.equal((await get("/v1/admin/egress-log?conversation=x", "adm")).status, 400);
  } finally {
    await api.drain(100);
  }
});

// ---- the allowlist and the address rules (unchanged) ------------------------------------------------------------

test("allowlist: exact and suffix entries, most specific wins, an equal deny wins, config denyHosts wins", () => {
  const r = new HostRules(["pypi.org", ".example.com", "*.wild.test"]);
  assert.ok(r.match("pypi.org") > 0);
  assert.equal(r.match("evilpypi.org"), 0);
  assert.equal(r.match("www.pypi.org"), 0, "an exact entry does not cover subdomains");
  assert.ok(r.match("a.b.example.com") > 0);
  assert.ok(r.match("Example.COM.") > 0);
  assert.equal(r.match("badexample.com"), 0);
  assert.ok(r.match("x.wild.test") > 0);
  const p = new HostPolicy({
    allow: [".google.com", "raw.githubusercontent.com", ".gov.hk", ".tie.test"],
    deny: [".drive.google.com", ".githubusercontent.com", ".tie.test"],
    configDeny: [".gov.hk"],
  });
  assert.deepEqual(p.check("www.google.com"), { allow: true });
  assert.deepEqual(p.check("drive.google.com"), { allow: false, reason: "deny:denylist" });
  assert.deepEqual(p.check("raw.githubusercontent.com"), { allow: true }, "an exact allow beats a broader deny");
  assert.deepEqual(p.check("evil.githubusercontent.com"), { allow: false, reason: "deny:denylist" });
  assert.deepEqual(p.check("a.tie.test"), { allow: false, reason: "deny:denylist" }, "equally specific: deny wins");
  assert.deepEqual(p.check("data.gov.hk"), { allow: false, reason: "deny:config" });
  assert.deepEqual(p.check("unknown.example"), { allow: false, reason: "deny:host" });
});

test("allowlist: the generated lists shipped with the image", () => {
  const p = HostPolicy.load({ defaultAllowedHosts: ["extra.example"], denyHosts: ["www.bbc.com"], generated: true });
  assert.ok(p.allow.size > 40_000 && p.deny.size > 1000, `${p.allow.size} / ${p.deny.size}`);
  assert.match(p.version, /^\d{4}-\d{2}-\d{2} [0-9a-f]{16}$/);
  const allowed = ["pypi.org", "files.pythonhosted.org", "registry.npmjs.org", "deb.debian.org", "github.com", "raw.githubusercontent.com",
    "registry-1.docker.io", "auth.docker.io", "production.cloudflare.docker.com", "ghcr.io", "pkg-containers.githubusercontent.com",
    "www.scmp.com", "news.rthk.hk", "data.gov.hk", "www.censtatd.gov.hk", "www.info.gov.hk", "www.reuters.com", "extra.example"];
  for (const h of allowed) assert.deepEqual(p.check(h), { allow: true }, h);
  const denied = ["pastebin.com", "transfer.sh", "webhook.site", "drive.google.com", "docs.google.com", "mega.nz", "dropbox.com",
    "foo.workers.dev", "x.ngrok-free.app", "s3.amazonaws.com", "bit.ly", "gist.github.com", "discord.com", "random-unknown-zz9q.com", "www.bbc.com"];
  for (const h of denied) assert.equal(p.check(h).allow, false, h);
  assert.equal(p.check("www.bbc.com").allow === false && (p.check("www.bbc.com") as { reason: string }).reason, "deny:config");
  const off = HostPolicy.load({ defaultAllowedHosts: ["pypi.org"], denyHosts: [], generated: false });
  assert.equal(off.check("github.com").allow, false);
  assert.equal(off.check("pypi.org").allow, true);
});

test("threat DNS: 0.0.0.0 is a threat, NXDOMAIN is refused, a normal answer passes, answers are cached", async () => {
  const dns = await fakeDns({ "ok.test": "1.2.3.4", "malware.test": "0.0.0.0" });
  try {
    const r = new ThreatResolver([`127.0.0.1:${dns.port}`], 500);
    assert.deepEqual(await r.resolve("ok.test"), ["1.2.3.4"]);
    await assert.rejects(r.resolve("malware.test"), (e: unknown) => e instanceof DnsRefusal && e.reason === "deny:threat");
    await assert.rejects(r.resolve("nx.test"), (e: unknown) => e instanceof DnsRefusal && e.reason === "deny:nxdomain");
    assert.deepEqual(await r.resolve("1.1.1.1"), ["1.1.1.1"]);
    const before = dns.queries;
    await r.resolve("ok.test");
    assert.equal(dns.queries, before, "cached");
  } finally {
    dns.close();
  }
  const dead = new ThreatResolver(["127.0.0.1:9"], 200);
  await assert.rejects(dead.resolve("ok.test"), (e: unknown) => e instanceof DnsRefusal && e.reason === "deny:dns", "fails closed");
});

test("netguard: address classes", () => {
  const g = new AddressGuard(["2a01:4f8:c015:24a1::/64"]);
  for (const ip of ["10.1.2.3", "127.0.0.1", "169.254.169.254", "100.64.0.1", "172.20.0.1", "192.168.1.1", "::1", "fd42::5", "fe80::1", "::ffff:10.0.0.1", "::ffff:127.0.0.1", "2a01:4f8:c015:24a1::1", "0.0.0.0", "not-an-ip"])
    assert.ok(g.blocked(ip), ip);
  for (const ip of ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111", "::ffff:1.1.1.1"]) assert.ok(!g.blocked(ip), ip);
});

/** A UDP DNS server for A/AAAA questions: names in `a` answer that IPv4 address (AAAA: no data), others NXDOMAIN. */
async function fakeDns(a: Record<string, string>): Promise<{ port: number; queries: number; close(): void }> {
  const sock = createSocket("udp4");
  const state = { port: 0, queries: 0, close: () => sock.close() };
  sock.on("message", (q, from) => {
    state.queries++;
    let i = 12;
    const labels: string[] = [];
    while (q[i]) {
      labels.push(q.subarray(i + 1, i + 1 + q[i]!).toString());
      i += q[i]! + 1;
    }
    const qEnd = i + 5;
    const qtype = q.readUInt16BE(i + 1);
    const name = labels.join(".");
    const ip = a[name];
    const head = Buffer.from(q.subarray(0, 12));
    head.writeUInt16BE(0x8180 | (ip ? 0 : 3), 2); // response, RD, RA; rcode 3 = NXDOMAIN
    head.writeUInt16BE(1, 4);
    const answer = ip && qtype === 1 ? Buffer.concat([Buffer.from([0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]), Buffer.from(ip.split(".").map(Number))]) : Buffer.alloc(0);
    head.writeUInt16BE(answer.length ? 1 : 0, 6);
    head.writeUInt16BE(0, 8);
    head.writeUInt16BE(0, 10);
    sock.send(Buffer.concat([head, q.subarray(12, qEnd), answer]), from.port, from.address);
  });
  await new Promise<void>((r) => sock.bind(0, "127.0.0.1", r));
  state.port = sock.address().port;
  return state;
}

test("entry point: the first argument picks the role; with none JARVIS_ROLE still picks web or tools, never egress", async () => {
  const main = join(import.meta.dirname, "..", "src", "main.ts");
  const runMain = (args: string[], env: Record<string, string>): Promise<{ code: number; err: string }> =>
    new Promise((resolve) => {
      const { JARVIS_ROLE: _r, ...base } = process.env;
      execFile(process.execPath, ["--disable-warning=ExperimentalWarning", main, ...args], { env: { ...base, ...env }, timeout: 20_000 }, (e, _o, err) =>
        resolve({ code: e ? ((e as { code?: number }).code ?? 1) : 0, err: String(err) }),
      );
    });
  const none = await runMain([], {});
  assert.equal(none.code, 2);
  assert.match(none.err, /first argument must be web, egress or tools/);
  const oldEgress = await runMain([], { JARVIS_ROLE: "egress" });
  assert.equal(oldEgress.code, 2);
  assert.match(oldEgress.err, /JARVIS_ROLE=egress: this image's egress is aviary's sandbox gateway/);
  // tools from JARVIS_ROLE alone gets as far as its own config (missing here)
  const tools = await runMain([], { JARVIS_ROLE: "tools", JARVIS_CONFIG: "/nonexistent/config.json" });
  assert.notEqual(tools.code, 0);
  assert.doesNotMatch(tools.err, /first argument must be/);
  assert.match(tools.err, /nonexistent/);
  const bad = await runMain(["nope"], { JARVIS_ROLE: "tools" });
  assert.equal(bad.code, 2, "an argument wins over JARVIS_ROLE");
});
