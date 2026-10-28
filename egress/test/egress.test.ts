import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, request, type IncomingMessage, type Server } from "node:http";
import { connect as netConnect, type AddressInfo, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { createServer as createTlsServer, connect as tlsConnect } from "node:tls";
import { randomUUID } from "node:crypto";
import { gzipSync } from "node:zlib";
import { sha256hex } from "../src/common/tokens.ts";
import { Identities } from "../src/egress/identity.ts";
import { createSocket } from "node:dgram";
import { HostPolicy, HostRules } from "../src/egress/allowlist.ts";
import { AddressGuard } from "../src/egress/netguard.ts";
import { CostSniffer, denyText, Egress } from "../src/egress/proxy.ts";
import { DnsRefusal, ThreatResolver } from "../src/egress/threatdns.ts";
import { freshDb } from "./pg.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let egress: Egress;
let port: number;
let up: Server;
let upPort: number;
const upHits: Record<string, number> = {};
let upLastHeaders: IncomingMessage["headers"] = {};
let tls: ReturnType<typeof createTlsServer>;
let tlsPort: number;
let tmp: string;
const conv = randomUUID();
const sbToken = "sandbox-token-1";
let sandboxId = randomUUID();

const RESOLVE: Record<string, string[]> = {
  "allowed.test": ["127.0.0.1"],
  "apt.test": ["127.0.0.1"],
  "www.ranked.test": ["127.0.0.1"],
  "paste.ranked.test": ["127.0.0.1"],
  "nope.ranked.test": ["127.0.0.1"],
  "unknown.test": ["127.0.0.1"],
  "evil.test": ["10.0.0.5", "fd00::1"],
};

before(async () => {
  t = await freshDb("egress");
  await t.db.query(
    "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at) values ($1,$2,'sbx-1',$3,'x','ready', now() + interval '1 hour')",
    [sandboxId, conv, sha256hex(sbToken)],
  );

  up = createServer((req, res) => {
    const p = req.url || "/";
    upHits[p] = (upHits[p] ?? 0) + 1;
    upLastHeaders = req.headers;
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      if (p.startsWith("/simple/foo/")) {
        res.writeHead(200, { "content-type": "text/html" });
        return res.end(`<a href="http://127.0.0.1:${upPort}/files/packages/foo-1.0-py3-none-any.whl#sha256=ab">foo</a>`);
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
      if (p.startsWith("/files/") || p.startsWith("/npm/left-pad/-/")) {
        res.writeHead(200, { "content-type": "application/octet-stream" });
        return res.end("WHEELBYTES");
      }
      if (p === "/npm/left-pad") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ versions: { "1.0.0": { dist: { tarball: `http://127.0.0.1:${upPort}/npm/left-pad/-/left-pad-1.0.0.tgz` } } } }));
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
      if (p === "/sent/snapshot") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ conversation: req.headers["x-jarvis-conversation"] ?? null, sandbox: req.headers["x-jarvis-sandbox"] ?? null, auth: req.headers.authorization ?? null }));
      }
      if (p === "/api/v1/messages") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ echo: JSON.parse(body || "{}"), usage: { cost: 0.0123 } }));
      }
      res.writeHead(404).end();
    });
  });
  await new Promise<void>((r) => up.listen(0, "127.0.0.1", r));
  upPort = (up.address() as AddressInfo).port;

  tmp = mkdtempSync(join(tmpdir(), "egress-"));
  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", join(tmp, "k.pem"), "-out", join(tmp, "c.pem"), "-days", "1", "-subj", "/CN=allowed.test"], { stdio: "ignore" });
  tls = createTlsServer({ key: readFileSync(join(tmp, "k.pem")), cert: readFileSync(join(tmp, "c.pem")) }, (s) => s.on("data", (d) => s.write(`echo:${d}`)));
  await new Promise<void>((r) => tls.listen(0, "127.0.0.1", r));
  tlsPort = (tls.address() as AddressInfo).port;

  egress = new Egress({
    db: t.db,
    policy: new HostPolicy({
      allow: ["allowed.test", "evil.test", ".apt.test", ".ranked.test", "malware.test"],
      deny: [".paste.ranked.test"],
      configDeny: ["nope.ranked.test"],
    }),
    maxUploadBytes: 256 * 1024,
    routes: [
      { name: "claude", upstream: `http://127.0.0.1:${upPort}/api`, model: true, inject: { Authorization: "Bearer ${REAL_KEY}" } },
      { name: "sent-files", upstream: `http://127.0.0.1:${upPort}/sent`, identity: true, inject: { Authorization: "Bearer ${REAL_KEY}" } },
    ],
    cacheDir: join(tmp, "cache"),
    cacheBytes: 50 * 1024 * 1024,
    selfHosts: ["egress.test"],
    guard: new AddressGuard([], ["127.0.0.1/32"]),
    allowedPorts: [80, 443, tlsPort, upPort],
    resolver: async (h) => {
      if (h === "malware.test") throw new DnsRefusal("deny:threat", "malware.test is blocked as a security threat (malware or phishing)");
      return RESOLVE[h] ?? [];
    },
    upstreams: { pypi: `http://127.0.0.1:${upPort}`, pypiFiles: `http://127.0.0.1:${upPort}/files`, npm: `http://127.0.0.1:${upPort}/npm` },
    aptHosts: ["apt.test"],
    env: { REAL_KEY: "sk-real" },
    identities: new Identities(t.db, 50),
  });
  port = await egress.listen(0, "127.0.0.1");
});

after(async () => {
  await egress.drain(100);
  up.close();
  tls.close();
  await t.drop();
  rmSync(tmp, { recursive: true, force: true });
});

const basic = (tok: string) => "Basic " + Buffer.from(`sbx:${tok}`).toString("base64");

function connectVia(target: string, auth?: string): Promise<{ status: number; head: string; sock: Socket }> {
  return new Promise((resolve, reject) => {
    const s = netConnect(port, "127.0.0.1");
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

function http(path: string, o: { headers?: Record<string, string>; method?: string; body?: string } = {}): Promise<{ status: number; headers: IncomingMessage["headers"]; body: string }> {
  return new Promise((resolve, reject) => {
    const r = request({ host: "127.0.0.1", port, path, method: o.method ?? "GET", headers: o.headers }, (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
    });
    r.on("error", reject);
    r.end(o.body);
  });
}

const rows = async () => {
  await egress.log.flush();
  return (await t.db.query("select * from egress_log order by id")).rows;
};

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

test("CONNECT: identity, allowlist, ports and addresses are enforced", async () => {
  assert.equal((await connectVia(`allowed.test:${tlsPort}`)).status, 407);
  assert.equal((await connectVia(`allowed.test:${tlsPort}`, basic("wrong"))).status, 407);
  const denied = await connectVia(`other.test:${tlsPort}`, basic(sbToken));
  assert.equal(denied.status, 403);
  assert.ok(denied.head.includes(denyText("other.test").trim()), denied.head);
  assert.match(denied.head, /cannot be enabled from the chat/);
  assert.doesNotMatch(denied.head, /request_network_access|ask the user|permission/i);
  assert.equal((await connectVia("allowed.test:22", basic(sbToken))).status, 403);
  const evil = await connectVia(`evil.test:${tlsPort}`, basic(sbToken));
  assert.equal(evil.status, 403);
  assert.match(evil.head, /non-public/);
  assert.equal((await connectVia(`10.0.0.1:${tlsPort}`, basic(sbToken))).status, 403);
});

test("CONNECT: an allowed host gets a TLS tunnel, and the call is logged with bytes", async () => {
  const { status, sock } = await connectVia(`allowed.test:${tlsPort}`, basic(sbToken));
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
  await new Promise((r) => setTimeout(r, 100));
  const r = (await rows()).filter((x) => x.kind === "connect" && x.decision === "allow");
  assert.equal(r.length, 1);
  assert.equal(r[0].host, "allowed.test");
  assert.equal(r[0].conversation_id, conv);
  assert.ok(Number(r[0].bytes_up) > 0 && Number(r[0].bytes_down) > 0);
  const denials = (await rows()).filter((x) => x.decision.startsWith("deny:")).map((x) => x.decision);
  assert.deepEqual(new Set(denials), new Set(["deny:auth", "deny:host", "deny:port", "deny:address"]));
});

test("the sandbox allowlist through the proxy: suffix allow, deny list, config deny, threat DNS, all logged", async () => {
  const ok = await connectVia(`www.ranked.test:${tlsPort}`, basic(sbToken));
  assert.equal(ok.status, 200);
  ok.sock.destroy();
  const paste = await connectVia(`paste.ranked.test:${tlsPort}`, basic(sbToken));
  assert.equal(paste.status, 403);
  assert.match(paste.head, /not on the sandbox's internet allowlist/);
  assert.equal((await connectVia(`nope.ranked.test:${tlsPort}`, basic(sbToken))).status, 403);
  const mal = await connectVia(`malware.test:${tlsPort}`, basic(sbToken));
  assert.equal(mal.status, 403);
  assert.match(mal.head, /security threat/);
  const h = await http(`http://unknown.test:${upPort}/plain`, { headers: { "proxy-authorization": basic(sbToken) } });
  assert.equal(h.status, 403);
  assert.match(h.body, /cannot be enabled from the chat/);
  const r = await rows();
  const decision = (host: string) => r.filter((x) => x.host === host).map((x) => x.decision);
  assert.deepEqual(decision("paste.ranked.test"), ["deny:denylist"]);
  assert.deepEqual(decision("nope.ranked.test"), ["deny:config"]);
  assert.deepEqual(decision("malware.test"), ["deny:threat"]);
  assert.deepEqual(decision("unknown.test"), ["deny:host"]);
  assert.ok(r.filter((x) => x.decision.startsWith("deny:")).every((x) => x.conversation_id === conv || x.decision === "deny:auth"));
});

test("upload cap: a tunnel that sends more than maxUploadBytes is cut and logged deny:upload", async () => {
  const { status, sock } = await connectVia(`allowed.test:${tlsPort}`, basic(sbToken));
  assert.equal(status, 200);
  await new Promise<void>((resolve) => {
    const c = tlsConnect({ socket: sock, servername: "allowed.test", rejectUnauthorized: false }, () => {
      const chunk = Buffer.alloc(64 * 1024, 120);
      const pump = () => {
        for (let i = 0; i < 8; i++) if (!c.destroyed) c.write(chunk);
      };
      pump();
    });
    c.on("data", () => {});
    c.on("error", () => resolve());
    c.on("close", () => resolve());
  });
  await new Promise((r) => setTimeout(r, 200));
  const cut = (await rows()).filter((x) => x.decision === "deny:upload");
  assert.equal(cut.length, 1);
  assert.ok(Number(cut[0].bytes_up) > 256 * 1024);
});

test("absolute-URI HTTP is forwarded; apt-style GETs are cached", async () => {
  const h = { "proxy-authorization": basic(sbToken) };
  const a = await http(`http://allowed.test:${upPort}/plain`, { headers: h });
  assert.equal(a.status, 200);
  assert.equal(a.body, "DEB");
  assert.equal(upLastHeaders["proxy-authorization"], undefined, "proxy credentials never go upstream");
  const p = `http://apt.test:${upPort}/pool/main/x.deb`;
  assert.equal((await http(p, { headers: h })).body, "DEB");
  await new Promise((r) => setTimeout(r, 50));
  const second = await http(p, { headers: h });
  assert.equal(second.body, "DEB");
  assert.equal(second.headers["x-egress-cache"], "hit");
  assert.equal(upHits["/pool/main/x.deb"], 1);
  assert.equal((await http(`http://other.test:${upPort}/plain`, { headers: h })).status, 403);
  assert.equal((await http(`http://allowed.test:${upPort}/plain`)).status, 407);
});

test("pypi mirror: index links rewritten, files cached, works through the proxy form too", async () => {
  assert.equal((await http("/mirror/pypi/simple/foo/")).status, 401);
  const idx = await http("/mirror/pypi/simple/foo/", { headers: { authorization: basic(sbToken) } });
  assert.equal(idx.status, 200);
  assert.match(idx.body, /href="\/mirror\/pypi\/files\/packages\/foo-1\.0-py3-none-any\.whl#sha256=ab"/);
  for (let i = 0; i < 2; i++) {
    const f = await http("/mirror/pypi/files/packages/foo-1.0-py3-none-any.whl", { headers: { authorization: basic(sbToken) } });
    assert.equal(f.body, "WHEELBYTES");
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.equal(upHits["/files/packages/foo-1.0-py3-none-any.whl"], 1);
  // pip with HTTP_PROXY set sends the mirror request in proxy form to egress itself
  const viaProxy = await http("http://egress.test:3128/mirror/pypi/simple/foo/", { headers: { "proxy-authorization": basic(sbToken), authorization: basic(sbToken) } });
  assert.equal(viaProxy.status, 200);
  assert.ok((await rows()).some((x) => x.kind === "mirror" && x.decision === "allow:cache"));
});

test("pypi mirror: a file upstream gzips (PEP 658 .metadata) arrives whole on the first, uncached request", async () => {
  const want = "Metadata-Version: 2.1\nName: foo\n".repeat(50);
  const r = await http("/mirror/pypi/files/packages/foo-1.0-py3-none-any.whl.metadata", { headers: { authorization: basic(sbToken) } });
  assert.equal(r.status, 200);
  assert.equal(r.body, want);
});

test("npm mirror: tarball URLs point back at the mirror", async () => {
  const m = await http("/mirror/npm/left-pad", { headers: { authorization: basic(sbToken), host: "egress.test:3128" } });
  assert.equal(m.status, 200);
  assert.equal(JSON.parse(m.body).versions["1.0.0"].dist.tarball, "http://egress.test:3128/mirror/npm/left-pad/-/left-pad-1.0.0.tgz");
  const tgz = await http("/mirror/npm/left-pad/-/left-pad-1.0.0.tgz", { headers: { authorization: basic(sbToken) } });
  assert.equal(tgz.body, "WHEELBYTES");
});

test("model route: sandbox token in, real key injected, caller auth dropped, cost logged (JSON and SSE)", async () => {
  const bad = await http("/x/claude/v1/messages", { method: "POST", headers: { authorization: "Bearer nope", "content-type": "application/json" }, body: "{}" });
  assert.equal(bad.status, 401);
  const r = await http("/x/claude/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${sbToken}`, "x-api-key": sbToken, cookie: "c=1", "content-type": "application/json" },
    body: JSON.stringify({ model: "claude-x" }),
  });
  assert.equal(r.status, 200);
  assert.equal(JSON.parse(r.body).echo.model, "claude-x");
  assert.equal(upLastHeaders.authorization, "Bearer sk-real");
  assert.equal(upLastHeaders["x-api-key"], undefined);
  assert.equal(upLastHeaders.cookie, undefined);
  const s = await http("/x/claude/v1/messages", { method: "POST", headers: { authorization: `Bearer ${sbToken}`, accept: "text/event-stream" }, body: "{}" });
  assert.match(s.body, /message_delta/);
  await new Promise((r2) => setTimeout(r2, 50));
  const models = (await rows()).filter((x) => x.kind === "model" && x.decision === "allow");
  assert.deepEqual(models.map((x) => Number(x.cost_usd)), [0.0123, 0.0042]);
  assert.ok(models.every((x) => x.sandbox_id === sandboxId && x.conversation_id === conv));
  // the model each call asked for (the operator page's spend per model); none in the body: null
  assert.deepEqual(models.map((x) => x.model), ["claude-x", null]);
  assert.equal((await http("/x/nope/v1")).status, 404);
});

test("model route: zero data retention is forced into every request body, whatever the caller sends", async () => {
  const send = (body: string) =>
    http("/x/claude/v1/messages", { method: "POST", headers: { authorization: `Bearer ${sbToken}`, "content-type": "application/json" }, body });
  const plain = await send(JSON.stringify({ model: "claude-x" }));
  assert.equal(plain.status, 200);
  assert.deepEqual(JSON.parse(plain.body).echo.provider, { zdr: true, data_collection: "deny" });
  const loose = await send(JSON.stringify({ model: "claude-x", provider: { zdr: false, data_collection: "allow", order: ["a"] } }));
  assert.deepEqual(JSON.parse(loose.body).echo.provider, { zdr: true, data_collection: "deny", order: ["a"] });
  const odd = await send(JSON.stringify({ model: "claude-x", provider: "anything" }));
  assert.deepEqual(JSON.parse(odd.body).echo.provider, { zdr: true, data_collection: "deny" });
  for (const bad of ["not json", "[1]", "null", ""]) {
    const r = await send(bad);
    assert.equal(r.status, 400, `body ${JSON.stringify(bad)} must be refused`);
  }
  const refused = (await rows()).filter((x) => x.kind === "model" && x.decision === "deny:body");
  assert.equal(refused.length, 4);
});

test("identity route: the conversation and sandbox egress verified, never the caller's own copies", async () => {
  const r = await http("/x/sent-files/snapshot", {
    method: "POST",
    headers: { authorization: `Bearer ${sbToken}`, "content-type": "application/json", "x-jarvis-conversation": "someone-elses", "x-jarvis-sandbox": "forged" },
    body: "{}",
  });
  assert.equal(r.status, 200);
  const got = JSON.parse(r.body);
  assert.equal(got.conversation, conv);
  assert.equal(got.sandbox, sandboxId);
  assert.equal(got.auth, "Bearer sk-real", "the route's own credential, not the sandbox token");
  // the model route gets no identity headers, and a caller's copies are dropped there too
  const m = await http("/x/claude/v1/messages", { method: "POST", headers: { authorization: `Bearer ${sbToken}`, "x-jarvis-conversation": "forged" }, body: JSON.stringify({ model: "claude-x" }) });
  assert.equal(m.status, 200);
});

test("model route: a body of many MB passes rewritten, one too large is refused before it is read", async () => {
  // a Claude Code turn that has read page images: the conversation's base64 is resent on every call
  const image = "A".repeat(12 * 1024 * 1024);
  const body = JSON.stringify({ model: "claude-x", messages: [{ role: "user", content: [{ type: "image", source: { type: "base64", media_type: "image/png", data: image } }] }], provider: { zdr: false } });
  const send = () => http("/x/claude/v1/messages", { method: "POST", headers: { authorization: `Bearer ${sbToken}`, "content-type": "application/json" }, body });
  const rs = await Promise.all([send(), send(), send()]);
  for (const r of rs) {
    assert.equal(r.status, 200);
    const echo = JSON.parse(r.body).echo;
    assert.deepEqual(echo.provider, { zdr: true, data_collection: "deny" });
    assert.equal(echo.messages[0].content[0].source.data.length, image.length);
  }
  // past the 64 MiB limit by its Content-Length: 413 at once, nothing read or sent upstream
  const head = await new Promise<string>((resolve, reject) => {
    const s = netConnect(port, "127.0.0.1");
    let buf = "";
    s.on("error", reject);
    s.on("data", (d) => (buf += d.toString("latin1")));
    s.on("close", () => resolve(buf));
    s.write(`POST /x/claude/v1/messages HTTP/1.1\r\nHost: egress.test\r\nAuthorization: Bearer ${sbToken}\r\nContent-Type: application/json\r\nContent-Length: ${70 * 1024 * 1024}\r\n\r\n{"model":`);
  });
  assert.match(head, /^HTTP\/1\.1 413 /);
  assert.match(head, /limited to 67108864 bytes/);
  const big = (await rows()).filter((x) => x.kind === "model" && x.decision === "deny:upload");
  assert.equal(big.length, 1);
});

test("the wire: a hosted sandbox's local proxy reaches egress over one WebSocket per connection", async () => {
  // the real sandbox agent in JARVIS_INIT mode, as E2B's template starts it, then configured by POST /init
  const { spawn } = await import("node:child_process");
  const freePort = async () => {
    const s = createServer();
    await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
    const p = (s.address() as AddressInfo).port;
    await new Promise((r) => s.close(r));
    return p;
  };
  const agentPort = await freePort();
  const wirePort = await freePort();
  const work = mkdtempSync(join(tmpdir(), "wire-work-"));
  const agent = spawn(process.execPath, [join(import.meta.dirname, "../sandbox/agent.mjs")], {
    env: { PATH: process.env.PATH!, JARVIS_INIT: "1", JARVIS_WORKSPACE: work, PORT: String(agentPort), JARVIS_WIRE_PORT: String(wirePort) },
    stdio: ["ignore", "pipe", "inherit"],
  });
  try {
    await new Promise<void>((resolve) => agent.stdout!.once("data", () => resolve()));
    const a = `http://127.0.0.1:${agentPort}`;
    assert.equal((await fetch(`${a}/livez`)).status, 200);
    assert.equal((await fetch(`${a}/healthz`)).status, 503, "not ready before /init");
    assert.equal((await fetch(`${a}/healthz`, { headers: { "x-jarvis-exec-token": "nope" } })).status, 503);
    // a wrong token is refused at the upgrade
    const bad = await new Promise<number>((resolve) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/wire`, { headers: { authorization: "Bearer nope" } } as any);
      ws.onerror = () => resolve(401);
      ws.onopen = () => resolve(101);
    });
    assert.equal(bad, 401);
    const init = await fetch(`${a}/init`, {
      method: "POST",
      body: JSON.stringify({ token: "exec-token-wire-0001", env: { MARK: "set-by-init" }, wire: { url: `ws://127.0.0.1:${port}/wire`, headers: { authorization: `Bearer ${sbToken}` } } }),
    });
    assert.equal(init.status, 200);
    assert.equal((await fetch(`${a}/init`, { method: "POST", body: JSON.stringify({ token: "x".repeat(20) }) })).status, 409, "only once");
    assert.equal((await fetch(`${a}/healthz`)).status, 200);

    // the mirror in proxy form (what pip sends with HTTP_PROXY=http://sbx:<token>@127.0.0.1:3128)
    const mirror = await new Promise<{ status: number; body: string }>((resolve, reject) => {
      const r = request({ host: "127.0.0.1", port: wirePort, path: "http://127.0.0.1:3128/mirror/pypi/simple/foo/", headers: { "proxy-authorization": basic(sbToken) } }, (res) => {
        let body = "";
        res.on("data", (c) => (body += c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, body }));
      });
      r.on("error", reject);
      r.end();
    });
    assert.equal(mirror.status, 200);
    assert.match(mirror.body, /\/mirror\/pypi\/files\//);
    // CONNECT through the wire to an allowed host, TLS inside
    const s = netConnect(wirePort, "127.0.0.1");
    s.write(`CONNECT allowed.test:${tlsPort} HTTP/1.1\r\nHost: allowed.test\r\nProxy-Authorization: ${basic(sbToken)}\r\n\r\n`);
    const head = await new Promise<string>((resolve) => {
      let b = "";
      const on = (d: Buffer) => {
        b += d.toString("latin1");
        if (b.includes("\r\n\r\n")) {
          s.off("data", on);
          resolve(b);
        }
      };
      s.on("data", on);
    });
    assert.match(head, /^HTTP\/1\.1 200/);
    const reply = await new Promise<string>((resolve, reject) => {
      const c = tlsConnect({ socket: s, servername: "allowed.test", rejectUnauthorized: false }, () => c.write("wire"));
      c.on("data", (d) => {
        resolve(d.toString());
        c.end();
      });
      c.on("error", reject);
    });
    assert.equal(reply, "echo:wire");
    // and the allowlist still applies inside the wire
    const s2 = netConnect(wirePort, "127.0.0.1");
    s2.write(`CONNECT other.test:443 HTTP/1.1\r\nHost: other.test\r\nProxy-Authorization: ${basic(sbToken)}\r\n\r\n`);
    const denied = await new Promise<string>((resolve) => {
      let b = "";
      s2.on("data", (d) => (b += d));
      s2.on("close", () => resolve(b));
    });
    assert.match(denied, /^HTTP\/1\.1 403/);
  } finally {
    agent.kill("SIGKILL");
    rmSync(work, { recursive: true, force: true });
  }
});

test("a released sandbox's token stops working", async () => {
  await t.db.query("update sandboxes set status = 'gone' where id = $1", [sandboxId]);
  await new Promise((r) => setTimeout(r, 100));
  assert.equal((await connectVia(`allowed.test:${tlsPort}`, basic(sbToken))).status, 407);
  await t.db.query("update sandboxes set status = 'ready' where id = $1", [sandboxId]);
  await new Promise((r) => setTimeout(r, 100));
});

test("CostSniffer reads the last cost in a stream split across chunks", () => {
  const s = new CostSniffer();
  s.contentType = "text/event-stream";
  for (const c of ['data: {"type":"message_delta","us', 'age":{"cost":0.5}}\n', "\n", 'data: {"usage":{"cost":0.7}}\n']) s.push(Buffer.from(c));
  assert.equal(s.cost(), 0.7);
});

test("drain: stops accepting, waits for an open tunnel, then exits", async () => {
  const e2 = new Egress({
    db: t.db,
    policy: new HostPolicy({ allow: ["allowed.test"] }),
    routes: [],
    cacheDir: join(tmp, "cache2"),
    cacheBytes: 1024 * 1024,
    selfHosts: [],
    guard: new AddressGuard([], ["127.0.0.1/32"]),
    allowedPorts: [tlsPort],
    resolver: async (h) => RESOLVE[h] ?? [],
  });
  const p2 = await e2.listen(0, "127.0.0.1");
  const s = netConnect(p2, "127.0.0.1");
  await new Promise<void>((r) => s.on("connect", () => r()));
  s.write(`CONNECT allowed.test:${tlsPort} HTTP/1.1\r\nProxy-Authorization: ${basic(sbToken)}\r\n\r\n`);
  await new Promise((r) => s.once("data", r));
  const t0 = Date.now();
  const d = e2.drain(400);
  await new Promise((r) => setTimeout(r, 50));
  await assert.rejects(
    new Promise((resolve, reject) => {
      const c = netConnect(p2, "127.0.0.1");
      c.on("connect", resolve);
      c.on("error", reject);
    }),
  );
  await d;
  assert.ok(Date.now() - t0 >= 350, "waited for the open tunnel up to the grace");
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
