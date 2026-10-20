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
import { sha256hex } from "../src/common/tokens.ts";
import { Identities } from "../src/egress/identity.ts";
import { AddressGuard, hostAllowed } from "../src/egress/netguard.ts";
import { CostSniffer, DENY_HELP, Egress } from "../src/egress/proxy.ts";
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
const turnToken = "turn-token-1";
let sandboxId = randomUUID();
let turnId = randomUUID();

const RESOLVE: Record<string, string[]> = {
  "allowed.test": ["127.0.0.1"],
  "apt.test": ["127.0.0.1"],
  "later.test": ["127.0.0.1"],
  "evil.test": ["10.0.0.5", "fd00::1"],
};

before(async () => {
  t = await freshDb("egress");
  await t.db.query("insert into users (email) values ('a@x.org')");
  await t.db.query("insert into conversations (id, owner, model) values ($1, 'a@x.org', 'm')", [conv]);
  await t.db.query(
    "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at) values ($1,$2,'sbx-1',$3,'x','ready', now() + interval '1 hour')",
    [sandboxId, conv, sha256hex(sbToken)],
  );
  await t.db.query("insert into turns (id, conversation_id, status, token_hash, model) values ($1,$2,'running',$3,'m')", [turnId, conv, sha256hex(turnToken)]);

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
    defaultAllowedHosts: ["allowed.test", "evil.test", ".apt.test"],
    routes: [{ name: "model", upstream: `http://127.0.0.1:${upPort}/api`, auth: "turn", inject: { Authorization: "Bearer ${REAL_KEY}" } }],
    cacheDir: join(tmp, "cache"),
    cacheBytes: 50 * 1024 * 1024,
    selfHosts: ["egress.test"],
    guard: new AddressGuard([], ["127.0.0.1/32"]),
    allowedPorts: [80, 443, tlsPort, upPort],
    resolver: async (h) => RESOLVE[h] ?? [],
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

test("netguard: host matching and address classes", () => {
  assert.ok(hostAllowed("pypi.org", ["pypi.org"]));
  assert.ok(!hostAllowed("evilpypi.org", ["pypi.org"]));
  assert.ok(hostAllowed("a.b.example.com", [".example.com"]));
  assert.ok(hostAllowed("example.com.", [".example.com"]));
  assert.ok(!hostAllowed("badexample.com", [".example.com"]));
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
  assert.ok(denied.head.includes(DENY_HELP), denied.head);
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

test("a host the user allows for the conversation works on the next try", async () => {
  assert.equal((await connectVia(`later.test:${tlsPort}`, basic(sbToken))).status, 403);
  await t.db.query("insert into allowed_hosts (conversation_id, host, added_by) values ($1, 'later.test', 'a@x.org')", [conv]);
  await new Promise((r) => setTimeout(r, 2100));
  const ok = await connectVia(`later.test:${tlsPort}`, basic(sbToken));
  assert.equal(ok.status, 200);
  ok.sock.destroy();
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

test("npm mirror: tarball URLs point back at the mirror", async () => {
  const m = await http("/mirror/npm/left-pad", { headers: { authorization: basic(sbToken), host: "egress.test:3128" } });
  assert.equal(m.status, 200);
  assert.equal(JSON.parse(m.body).versions["1.0.0"].dist.tarball, "http://egress.test:3128/mirror/npm/left-pad/-/left-pad-1.0.0.tgz");
  const tgz = await http("/mirror/npm/left-pad/-/left-pad-1.0.0.tgz", { headers: { authorization: basic(sbToken) } });
  assert.equal(tgz.body, "WHEELBYTES");
});

test("model route: turn token in, real key injected, caller auth dropped, cost logged (JSON and SSE)", async () => {
  const bad = await http("/x/model/v1/messages", { method: "POST", headers: { authorization: "Bearer nope", "content-type": "application/json" }, body: "{}" });
  assert.equal(bad.status, 401);
  const r = await http("/x/model/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${turnToken}`, "x-api-key": turnToken, cookie: "c=1", "content-type": "application/json" },
    body: JSON.stringify({ model: "claude-x" }),
  });
  assert.equal(r.status, 200);
  assert.equal(JSON.parse(r.body).echo.model, "claude-x");
  assert.equal(upLastHeaders.authorization, "Bearer sk-real");
  assert.equal(upLastHeaders["x-api-key"], undefined);
  assert.equal(upLastHeaders.cookie, undefined);
  const s = await http("/x/model/v1/messages", { method: "POST", headers: { "x-api-key": turnToken, accept: "text/event-stream" }, body: "{}" });
  assert.match(s.body, /message_delta/);
  await new Promise((r2) => setTimeout(r2, 50));
  const models = (await rows()).filter((x) => x.kind === "model" && x.decision === "allow");
  assert.deepEqual(models.map((x) => Number(x.cost_usd)), [0.0123, 0.0042]);
  assert.ok(models.every((x) => x.turn_id === turnId && x.conversation_id === conv));
  assert.equal((await http("/x/nope/v1")).status, 404);
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
    defaultAllowedHosts: ["allowed.test"],
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
