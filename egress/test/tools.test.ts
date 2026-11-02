// The tools role on the new contract (aviary platform spec 17.2 tools), end to end on the real local Postgres (its own
// `tools` database), with every outside service faked (test/fakes/tools-upstreams.ts; hermit's liveness here): only
// the gateway's bearers, the owner from Aviary-Sandbox-Person, the staff catalogue, hermit's liveness with its stale
// view during an outage, the MCP methods, each built-in tool, upstream proxying with its read-only allowlist, tool_calls
// logging (dropped, never waited on, while the database is away) and the drain.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, request as httpRequest, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import pg from "pg";
import { hermitGetter } from "../src/egress/tokens.ts";
import { allowedBy, Catalogue, parseCatalogue } from "../src/tools/catalogue.ts";
import { checkConfigEnv, parseToolsConfig } from "../src/tools/config.ts";
import { CallLog, connect, migrate, MIGRATIONS } from "../src/tools/db.ts";
import { Liveness } from "../src/tools/liveness.ts";
import { isPrivateAddress, htmlToText } from "../src/tools/netguard.ts";
import { iso } from "../src/tools/providers.ts";
import { acceptList, createToolsServer, DRAIN_MS, FLUSH_MS, POOL_END_MS, toolList, type ToolsServer } from "../src/tools/server.ts";
import type { Deps, ToolDef } from "../src/tools/types.ts";
import { pickResponse, readOnlyAllowed, startRefresh, Upstream } from "../src/tools/upstream.ts";
import { startFakes, type Fakes } from "./fakes/tools-upstreams.ts";

const GW = "gateway-pair-token-current";
const GW_NEXT = "gateway-pair-token-next";
const HERMIT_BEARER = "tools-to-hermit";
const owner = "staff@example.com";

/** A fresh, migrated `tools` database on the local Postgres (TEST_DATABASE_URL). */
async function freshToolsDb(name: string): Promise<{ db: pg.Pool; url: string; drop(): Promise<void> }> {
  const admin = process.env.TEST_DATABASE_URL || "postgres://claude:dev@127.0.0.1:5432/postgres";
  const dbName = `tools_test_${name}_${process.pid}`;
  const c = new pg.Client({ connectionString: admin });
  await c.connect();
  await c.query(`drop database if exists ${dbName} with (force)`);
  await c.query(`create database ${dbName}`);
  await c.end();
  const u = new URL(admin);
  u.pathname = `/${dbName}`;
  const db = connect(u.toString());
  await migrate(db);
  return {
    db,
    url: u.toString(),
    async drop() {
      await db.end();
      const c2 = new pg.Client({ connectionString: admin });
      await c2.connect();
      await c2.query(`drop database if exists ${dbName} with (force)`);
      await c2.end();
    },
  };
}

type Live = { state: string; live: boolean; conversationId: string; person: string } | "unknown";

/** hermit's `GET /v1/sandbox-tokens/{id}` (hermit-api TokenLiveness), with an outage switch. */
class FakeHermit {
  readonly answers = new Map<string, Live>();
  readonly lookups = new Map<string, number>();
  outage: "up" | "reset" | "hang" | "unavailable" | "auth" = "up";
  /** each answer waits this long */
  delayMs = 0;
  server: Server;
  url = "";
  constructor() {
    this.server = createServer(async (req, res) => {
      if (this.delayMs) await new Promise((r) => setTimeout(r, this.delayMs));
      if (this.outage === "auth") return void res.writeHead(401).end('{"code":"unauthenticated"}');
      if (this.outage === "reset") return void req.socket.destroy();
      if (this.outage === "hang") return;
      if (this.outage === "unavailable") return void res.writeHead(503).end('{"code":"unavailable"}');
      if (req.headers.authorization !== `Bearer ${HERMIT_BEARER}`) return void res.writeHead(401).end('{"code":"unauthenticated"}');
      const m = /^\/v1\/sandbox-tokens\/([0-9a-f-]{36})$/.exec(req.url ?? "");
      if (!m) return void res.writeHead(404).end();
      const id = m[1]!;
      this.lookups.set(id, (this.lookups.get(id) ?? 0) + 1);
      const a = this.answers.get(id);
      if (!a || a === "unknown") return void res.writeHead(404, { "content-type": "application/json" }).end('{"code":"unknown_sandbox"}');
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ sandboxId: id, ...a, expiresAt: new Date(Date.now() + 3600_000).toISOString() }));
    });
  }
  async listen(): Promise<void> {
    await new Promise<void>((r) => this.server.listen(0, "127.0.0.1", r));
    this.url = `http://127.0.0.1:${(this.server.address() as AddressInfo).port}`;
  }
}

interface Sbx {
  id: string;
  conversationId: string;
  person: string;
}

let t: Awaited<ReturnType<typeof freshToolsDb>>;
let fakes: Fakes;
let hermit: FakeHermit;
let deps: Deps;
let url = "";
let srv: ToolsServer;
let clock = Date.now();
let liveness: Liveness;
let calls: CallLog;
let dir = "";
let sbx: Sbx;

/** A sandbox hermit knows (ready unless said otherwise). */
function sandbox(o: Partial<Sbx> & { state?: string; live?: boolean } = {}): Sbx {
  const s = { id: o.id ?? randomUUID(), conversationId: o.conversationId ?? randomUUID(), person: o.person ?? owner };
  hermit.answers.set(s.id, { state: o.state ?? "ready", live: o.live ?? (o.state ?? "ready") !== "gone", conversationId: s.conversationId, person: s.person });
  return s;
}

/** The headers the gateway sends on a `tools` call for a verified sandbox (platform 3.11.3). */
function gw(s: Sbx, bearer = GW, request = "0190f6c2-0000-7000-8000-000000000001"): Record<string, string> {
  return {
    authorization: `Bearer ${bearer}`,
    "aviary-sandbox-id": s.id,
    "aviary-sandbox-conversation": s.conversationId,
    "aviary-sandbox-person": s.person,
    "aviary-request": request,
    "content-type": "application/json",
  };
}

async function rpc(method: string, params: Record<string, unknown> = {}, s: Sbx = sbx, headers?: Record<string, string>): Promise<{ status: number; body: any }> {
  const r = await fetch(`${url}/mcp`, { method: "POST", headers: headers ?? gw(s), body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) });
  const text = await r.text();
  return { status: r.status, body: text ? JSON.parse(text) : null };
}

/** A query on the tools database once the tool_calls rows in flight are written. */
const q = async (sql: string) => {
  await calls.flush();
  return t.db.query(sql);
};
const call = async (name: string, args: Record<string, unknown>) => (await rpc("tools/call", { name, arguments: args })).body.result;
const textOf = (r: any) => r.content.map((c: any) => c.text).join("");

/** A slow tool (for the drain tests): answers after `ms`. */
const slowUpstream = {
  cfg: { prefix: "slow" },
  tools: (): ToolDef[] => [
    {
      name: "slow_wait",
      description: "waits",
      inputSchema: { type: "object" },
      run: async (args) => {
        await new Promise((r) => setTimeout(r, Number(args.ms ?? 0)));
        return { content: [{ type: "text", text: "waited" }] };
      },
    },
  ],
} as unknown as Upstream;

const catalogueFile = () => join(dir, "catalogue.json");

async function startServer(o: { calls?: CallLog } = {}): Promise<{ srv: ToolsServer; url: string }> {
  const catalogue = new Catalogue(catalogueFile());
  assert.equal(await catalogue.reload(), true);
  const ups = deps.config.upstreams.map((u) => new Upstream(u, deps));
  for (const u of ups) await u.refresh();
  const s = createToolsServer(deps, [...ups, slowUpstream], { accept: acceptList({ TOOLS_ACCEPT_GATEWAY: `${GW},${GW_NEXT}` }), liveness, catalogue, calls: o.calls ?? calls });
  await new Promise<void>((r) => s.server.listen(0, "127.0.0.1", r));
  return { srv: s, url: `http://127.0.0.1:${(s.server.address() as AddressInfo).port}` };
}

before(async () => {
  t = await freshToolsDb("main");
  fakes = await startFakes();
  hermit = new FakeHermit();
  await hermit.listen();
  dir = mkdtempSync(join(tmpdir(), "tools-test-"));
  writeFileSync(catalogueFile(), JSON.stringify({ users: ["@example.com", "guest@example.org"], admins: ["admin@example.com"] }));
  const config = parseToolsConfig({
    exa: { keyEnv: "EXA_KEY", baseUrl: `${fakes.url}/exa` },
    tikhub: { keyEnv: "TIK_KEY", baseUrl: `${fakes.url}/tikhub` },
    anyapi: { keyEnv: "ANY_KEY", baseUrl: `${fakes.url}/anyapi`, denySkus: ["facebook.marketplace"] },
    capture: { url: `${fakes.url}/capture`, tokenEnv: "CAP", name: "YYC" },
    upstreams: [{ prefix: "yyc", url: `${fakes.url}/mcp-up`, headers: { Authorization: "Bearer ${UP_TOKEN}", "X-YYC-User": "${USER_EMAIL}" }, tools: ["posts_search", "post_delete"], listAs: "Lister@example.com" }],
  });
  deps = {
    config,
    env: { EXA_KEY: "exa-key", TIK_KEY: "tik-key", ANY_KEY: "any-key", UP_TOKEN: "up", CAP: "cap-token" },
    fetch: globalThis.fetch,
    resolve: async () => ({ address: "127.0.0.1", family: 4 }),
    fetchPorts: "any",
  };
  liveness = new Liveness(hermitGetter(hermit.url, HERMIT_BEARER), () => clock);
  calls = new CallLog(t.db);
  ({ srv, url } = await startServer());
  sbx = sandbox();
});

after(async () => {
  await srv.drain(1000);
  await calls.flush();
  await fakes.close();
  hermit.server.closeAllConnections();
  hermit.server.close();
  await t.drop();
  rmSync(dir, { recursive: true, force: true });
});

test("caller: only the gateway's bearers (either during a rotation); the Aviary-Sandbox-* headers are required", async () => {
  const h = gw(sbx);
  assert.equal((await rpc("tools/list", {}, sbx, { ...h, authorization: "" })).status, 401);
  const wrong = await rpc("tools/list", {}, sbx, { ...h, authorization: "Bearer nope" });
  assert.equal(wrong.status, 401);
  assert.equal(wrong.body.code, "unauthenticated");
  // the old credential (a sandbox's own egress token) is not a gateway bearer
  assert.equal((await rpc("tools/list", {}, sbx, { ...h, authorization: `Bearer ${randomUUID()}` })).status, 401);
  assert.equal((await rpc("tools/list")).status, 200);
  assert.equal((await rpc("tools/list", {}, sbx, gw(sbx, GW_NEXT))).status, 200, "the next bearer of a rotation");
  for (const k of ["aviary-sandbox-id", "aviary-sandbox-conversation", "aviary-sandbox-person"]) {
    const { [k]: _, ...rest } = h;
    const r = await rpc("tools/list", {}, sbx, rest);
    assert.equal(r.status, 400, k);
    assert.equal(r.body.code, "bad_request");
  }
  assert.throws(() => acceptList({}), /TOOLS_ACCEPT_GATEWAY is not set/);
  assert.throws(() => acceptList({ TOOLS_ACCEPT_GATEWAY: "a," }), /empty entry/);
});

test("owner: Aviary-Sandbox-Person (lowercased) fills tool_calls; the catalogue and hermit's state decide who is served", async () => {
  const s = sandbox({ person: owner });
  const request = "0190f6c2-0000-7000-8000-0000000000aa";
  const r = await rpc("tools/call", { name: "web_fetch", arguments: { url: "http://127.0.0.1:1/" } }, s, { ...gw(s, GW, request), "aviary-sandbox-person": "Staff@EXAMPLE.com" });
  assert.equal(r.status, 200);
  await calls.flush();
  const { rows } = await q("select conversation_id, sandbox_id, user_email, request, turn_id from tool_calls order by id desc limit 1");
  assert.deepEqual(rows[0], { conversation_id: s.conversationId, sandbox_id: s.id, user_email: owner, request, turn_id: null });

  // only starting and ready sandboxes are served
  assert.equal((await rpc("tools/list", {}, sandbox({ state: "starting" }))).status, 200);
  for (const state of ["releasing", "paused", "gone"]) {
    const res = await rpc("tools/list", {}, sandbox({ state, live: state !== "gone" }));
    assert.equal(res.status, 403, state);
    assert.equal(res.body.code, "not_allowed");
    assert.match(res.body.message, new RegExp(state));
  }
  // hermit does not know the sandbox (404), or it says another conversation or person
  const unknown = sandbox();
  hermit.answers.set(unknown.id, "unknown");
  assert.equal((await rpc("tools/list", {}, unknown)).status, 403);
  const theirs = sandbox();
  assert.equal((await rpc("tools/list", {}, { ...theirs, person: "staff2@example.com" })).status, 403, "another person");
  assert.equal((await rpc("tools/list", {}, { ...theirs, conversationId: randomUUID() })).status, 403, "another conversation");
  // a person the catalogue does not list
  const outsider = sandbox({ person: "someone@else.example" });
  const res = await rpc("tools/list", {}, outsider);
  assert.equal(res.status, 403);
  assert.match(res.body.message, /may not use/);
  assert.equal((await rpc("tools/list", {}, sandbox({ person: "guest@example.org" }))).status, 200, "an explicit address");
});

test("liveness: cached 30 s; a hermit outage serves the last answer whatever its age; revocation lands after the TTL", async () => {
  const s = sandbox();
  assert.equal((await rpc("tools/list", {}, s)).status, 200);
  assert.equal(hermit.lookups.get(s.id), 1);
  assert.equal((await rpc("tools/list", {}, s)).status, 200);
  assert.equal(hermit.lookups.get(s.id), 1, "cached");
  for (const outage of ["reset", "unavailable", "hang"] as const) {
    hermit.outage = outage;
    clock += 3_600_000; // far past the TTL: the stale answer is served, a refresh runs in the background and fails
    const t0 = Date.now();
    const r = await rpc("tools/call", { name: "web_fetch", arguments: { url: "http://127.0.0.1:1/" } }, s);
    assert.equal(r.status, 200, outage);
    assert.ok(Date.now() - t0 < 900, `${outage}: the call does not wait for hermit`);
    await new Promise((r) => setTimeout(r, 1100)); // let the background lookup time out
    clock += 2500; // past the retry pause
  }
  // a sandbox first seen during the outage: admitted on the gateway's verification (the lookup times out at 1 s)
  hermit.outage = "reset";
  assert.equal((await rpc("tools/list", {}, sandbox())).status, 200);
  hermit.outage = "up";
  // hermit back, the sandbox released: refused once the TTL has passed and the background refresh has landed
  hermit.answers.set(s.id, { state: "releasing", live: true, conversationId: s.conversationId, person: s.person });
  clock += 31_000;
  await rpc("tools/list", {}, s); // starts the refresh (served on the old answer)
  await new Promise((r) => setTimeout(r, 200));
  assert.equal((await rpc("tools/list", {}, s)).status, 403);
  assert.ok(liveness.failures >= 3);
});

test("liveness: calls that arrive together for a sandbox not yet looked up all wait for the one lookup", async () => {
  hermit.delayMs = 300;
  try {
    const gone = sandbox({ state: "gone", live: false });
    const first = rpc("tools/list", {}, gone);
    await new Promise((r) => setTimeout(r, 10));
    const second = rpc("tools/list", {}, gone);
    await new Promise((r) => setTimeout(r, 10));
    const third = rpc("tools/call", { name: "slow_wait", arguments: { ms: 0 } }, gone);
    const rs = await Promise.all([first, second, third]);
    assert.deepEqual(rs.map((r) => r.status), [403, 403, 403]);
    assert.equal(hermit.lookups.get(gone.id), 1, "one lookup for all three");
    const ok = sandbox();
    const both = await Promise.all([rpc("tools/list", {}, ok), rpc("tools/list", {}, ok)]);
    assert.deepEqual(both.map((r) => r.status), [200, 200]);
  } finally {
    hermit.delayMs = 0;
  }
});

test("liveness: while hermit hangs, only a sandbox's first lookup is waited for; later calls are admitted at once and retry in the background", async () => {
  const s = sandbox();
  hermit.outage = "hang";
  const failed = () => liveness.failuresBy.unreachable;
  const before = failed();
  try {
    let t0 = Date.now();
    assert.equal((await rpc("tools/list", {}, s)).status, 200, "admitted on the gateway's verification");
    assert.ok(Date.now() - t0 >= 900, "the first lookup is waited for, up to its 1 s timeout");
    assert.equal(failed() - before, 1);
    for (let i = 0; i < 4; i++) {
      clock += 2500; // past the retry pause: the first of these calls starts a retry, the rest find it running
      t0 = Date.now();
      assert.equal((await rpc("tools/call", { name: "slow_wait", arguments: { ms: 0 } }, s)).status, 200);
      assert.ok(Date.now() - t0 < 300, `call ${i + 2} does not wait for hermit (${Date.now() - t0} ms)`);
    }
    await new Promise((r) => setTimeout(r, 1100)); // the background retries time out
    assert.equal(failed() - before, 2, "one retry, none started while it runs");
  } finally {
    hermit.outage = "up";
  }
  // hermit back: the next retry lands and the answer is used
  clock += 2500;
  await rpc("tools/list", {}, s);
  await new Promise((r) => setTimeout(r, 200));
  hermit.answers.set(s.id, { state: "releasing", live: true, conversationId: s.conversationId, person: s.person });
  clock += 31_000;
  await rpc("tools/list", {}, s);
  await new Promise((r) => setTimeout(r, 200));
  assert.equal((await rpc("tools/list", {}, s)).status, 403);
});

test("liveness: hermit refusing tools' bearer (401/403) is a configuration fault, counted apart; calls are still served", async () => {
  const s = sandbox();
  const before = liveness.failuresBy.auth;
  hermit.outage = "auth";
  try {
    assert.equal((await rpc("tools/list", {}, s)).status, 200, "a hermit problem never refuses a call the gateway verified");
    assert.equal(liveness.failuresBy.auth, before + 1);
    assert.equal(liveness.authFault, true);
    // hermit also unreachable for a moment: still a fault (only a 200/404 answer clears it)
    hermit.outage = "reset";
    clock += 2500;
    assert.equal((await rpc("tools/list", {}, sandbox())).status, 200);
    assert.equal(liveness.authFault, true, "an outage does not clear it");
  } finally {
    hermit.outage = "up";
  }
  clock += 2500; // past the retry pause
  const later = sandbox({ state: "releasing" });
  assert.equal((await rpc("tools/list", {}, later)).status, 403);
  assert.equal(liveness.authFault, false, "cleared by the next answer");
});

test("tool_calls: a database outage drops the row and never delays the call", async () => {
  const dead = connect("postgres://tools:x@127.0.0.1:1/tools");
  const deadLog = new CallLog(dead);
  const other = await startServer({ calls: deadLog });
  try {
    const s = sandbox();
    const t0 = Date.now();
    const r = await fetch(`${other.url}/mcp`, { method: "POST", headers: gw(s), body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "slow_wait", arguments: { ms: 0 } } }) });
    assert.equal(r.status, 200);
    assert.equal(textOf(((await r.json()) as any).result), "waited");
    assert.ok(Date.now() - t0 < 1000);
    await deadLog.flush();
    assert.equal(deadLog.dropped, 1);
  } finally {
    await other.srv.drain(1000);
    await dead.end();
  }
});

test("tool_calls: the final flush is bounded, so a blackholed database cannot hold the exit past the grace period", async () => {
  assert.equal(DRAIN_MS + FLUSH_MS + POOL_END_MS + 10_000, 170_000, "preStop + drain + flush + pool end, 10 s inside the 180 s grace");
  const { createServer: tcp } = await import("node:net");
  const blackhole = tcp(() => {});
  await new Promise<void>((r) => blackhole.listen(0, "127.0.0.1", r));
  const db = connect(`postgres://tools:x@127.0.0.1:${(blackhole.address() as AddressInfo).port}/tools`);
  const log = new CallLog(db);
  try {
    const s = sandbox();
    log.write({ conversationId: s.conversationId, sandboxId: s.id, person: s.person, request: null, tool: "web_fetch", args: {}, ok: true, ms: 1, error: null, costUsd: null });
    const t0 = Date.now();
    assert.equal(await log.flush(300), 1, "one row unwritten at the bound");
    assert.ok(Date.now() - t0 < 1000);
    assert.equal(log.dropped, 1);
  } finally {
    blackhole.close();
    void db.end().catch(() => {});
  }
});

/** One request on a kept-alive connection of `agent` (node:http, so Connection headers are visible). */
function post(base: string, s: Sbx, body: unknown, agent: import("node:http").Agent): Promise<{ status: number; headers: import("node:http").IncomingHttpHeaders; body: string }> {
  const u = new URL(`${base}/mcp`);
  return new Promise((resolve, reject) => {
    const req = httpRequest({ hostname: u.hostname, port: u.port, path: u.pathname, method: "POST", headers: gw(s), agent }, (res) => {
      let b = "";
      res.on("data", (c) => (b += c));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: b }));
    });
    req.on("error", reject);
    req.end(JSON.stringify(body));
  });
}

test("drain: 150 s bound; in-flight calls finish, a kept-alive request is answered with Connection: close, the rest is cut", async () => {
  assert.equal(DRAIN_MS, 150_000);
  const { Agent } = await import("node:http");
  const s = sandbox();
  const one = await startServer();
  const agent = new Agent({ keepAlive: true, maxSockets: 2 });
  try {
    const slowCall = (ms: number) => ({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "slow_wait", arguments: { ms } } });
    // two kept-alive connections: one busy with a 600 ms call, one idle after a quick call
    await post(one.url, s, slowCall(0), agent);
    const inflight = post(one.url, s, slowCall(600), agent);
    await new Promise((r) => setTimeout(r, 50));
    const drained = one.srv.drain(5000);
    const r = await inflight;
    assert.equal(r.status, 200);
    assert.match(r.body, /waited/);
    assert.equal(r.headers.connection, "close", "the answer during the drain closes its connection");
    assert.equal(await drained, 0, "nothing cut");
    await assert.rejects(fetch(`${one.url}/mcp`, { method: "POST", headers: gw(s), body: "{}" }), "no new connections after the drain");
  } finally {
    agent.destroy();
  }
  // a call longer than the bound is cut and counted
  const two = await startServer();
  const p = fetch(`${two.url}/mcp`, { method: "POST", headers: gw(s), body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "slow_wait", arguments: { ms: 5000 } } }) }).catch((e) => e);
  await new Promise((r) => setTimeout(r, 100));
  const cutRequest = "0190f6c2-0000-7000-8000-0000000000cc";
  const p2 = fetch(`${two.url}/mcp`, { method: "POST", headers: gw(s, GW, cutRequest), body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "slow_wait", arguments: { ms: 5000 } } }) }).catch((e) => e);
  await new Promise((r) => setTimeout(r, 100));
  assert.equal(await two.srv.drain(300), 2);
  assert.ok((await p) instanceof Error);
  assert.ok((await p2) instanceof Error);
  assert.ok(two.srv.metrics().includes("tools_drain_cut_total 2"));
  // the cut calls' rows are in the log's pending writes when drain returns (main flushes them before it exits)
  const { rows } = await q(`select ok, error from tool_calls where request = '${cutRequest}'`);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].ok, false);
  assert.match(rows[0].error, /cut by the drain/);
});

test("upstreams: the first tool-list read is bounded, so an upstream that hangs never holds the pod unready", async () => {
  const hang = createServer(() => {});
  await new Promise<void>((r) => hang.listen(0, "127.0.0.1", r));
  try {
    const cfg = parseToolsConfig({ upstreams: [{ prefix: "stuck", url: `http://127.0.0.1:${(hang.address() as AddressInfo).port}/mcp`, tools: ["x"], listAs: "lister@example.org" }] });
    const ups = cfg.upstreams.map((u) => new Upstream(u, { ...deps, config: cfg }));
    const t0 = Date.now();
    const r = startRefresh(ups, 600_000, 300);
    await r.ready;
    r.stop();
    assert.ok(Date.now() - t0 < 2000, "ready without the upstream's answer");
    assert.deepEqual(ups[0]!.tools(), []);
  } finally {
    hang.closeAllConnections();
    hang.close();
  }
});

test("upstreams: an upstream whose first read failed is read again soon (backoff), not at the next 10-minute refresh", async () => {
  let seen = 0;
  const flaky = createServer((req, res) => {
    let b = "";
    req.on("data", (c) => (b += c));
    req.on("end", () => {
      if (++seen === 1) return void req.socket.destroy(); // down for the first read
      const { id } = JSON.parse(b);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ jsonrpc: "2.0", id, result: { tools: [{ name: "x", annotations: { readOnlyHint: true } }] } }));
    });
  });
  await new Promise<void>((r) => flaky.listen(0, "127.0.0.1", r));
  try {
    const cfg = parseToolsConfig({ upstreams: [{ prefix: "flaky", url: `http://127.0.0.1:${(flaky.address() as AddressInfo).port}/mcp`, tools: ["x"], listAs: "lister@example.org" }] });
    const ups = cfg.upstreams.map((u) => new Upstream(u, { ...deps, config: cfg }));
    const r = startRefresh(ups, 600_000, 300, [50, 100]);
    await r.ready;
    assert.equal(ups[0]!.count, 0);
    assert.equal(ups[0]!.listed, false);
    for (let i = 0; i < 40 && !ups[0]!.listed; i++) await new Promise((r) => setTimeout(r, 50));
    r.stop();
    assert.equal(ups[0]!.listed, true, "read again within the backoff, long before the 10-minute refresh");
    assert.deepEqual(ups[0]!.tools().map((t) => t.name), ["flaky_x"]);
    assert.equal(seen, 2);
  } finally {
    flaky.closeAllConnections();
    flaky.close();
  }
});

// hermit's own cases, byte for byte (aviary crates/hermit/fixtures/catalogue-cases.json; chat.yml checks the copy), so
// tools and hermit (and YYC's CI, `hermit catalogue-check`) accept and refuse the same files
test("catalogue: hermit's shared cases", () => {
  type Case = { name: string; file?: unknown; text?: string; allowed?: string[]; denied?: string[]; admins?: string[] };
  const cases = JSON.parse(readFileSync(new URL("./fixtures/catalogue-cases.json", import.meta.url), "utf8")) as { accept: Case[]; refuse: Case[] };
  const text = (c: Case) => c.text ?? JSON.stringify(c.file);
  assert.ok(cases.accept.length > 5 && cases.refuse.length > 20);
  for (const c of cases.accept) {
    const r = parseCatalogue(text(c));
    assert.ok(!("refused" in r), `${c.name}: refused: ${"refused" in r ? r.refused : ""}`);
    for (const e of c.allowed!) assert.ok(allowedBy(r, e), `${c.name}: ${JSON.stringify(e)} should be allowed`);
    for (const e of c.denied!) assert.ok(!allowedBy(r, e), `${c.name}: ${JSON.stringify(e)} should be denied`);
    assert.deepEqual([...r.admins].sort(), [...c.admins!].sort(), c.name);
  }
  for (const c of cases.refuse) assert.ok("refused" in parseCatalogue(text(c)), `${c.name}: accepted`);
});

test("config, catalogue and migrations", async () => {
  assert.throws(() => parseToolsConfig({ upstreams: [{ prefix: "y", url: "http://u", headers: {} }] }), /listAs/);
  assert.throws(() => parseToolsConfig({ upstreams: [{ prefix: "y", url: "http://u", headers: {}, listAs: "a@b.c" }, { prefix: "y", url: "http://v", headers: {}, listAs: "a@b.c" }] }), /twice/);
  assert.equal(parseToolsConfig({ upstreams: [{ prefix: "y", url: "http://u", headers: {}, listAs: " A@B.c " }] }).upstreams[0]!.listAs, "a@b.c");
  assert.throws(() => checkConfigEnv(deps.config, {}), /EXA_KEY/);
  assert.throws(() => checkConfigEnv(deps.config, { ...deps.env, UP_TOKEN: "" }), /UP_TOKEN/);
  checkConfigEnv(deps.config, deps.env);

  assert.deepEqual(parseCatalogue('{"users": ["@example.com", "A@x.org"], "admins": ["a@x.org"]}'), {
    users: new Set(["a@x.org"]),
    domains: new Set(["example.com"]),
    admins: new Set(["a@x.org"]),
  });
  const many = Array.from({ length: 10_001 }, (_, i) => `u${i}@example.org`);
  assert.ok("refused" in parseCatalogue(JSON.stringify({ users: many, admins: [] })), "more than 10000 entries");
  // a refused file keeps the last good catalogue
  const c = new Catalogue(catalogueFile());
  assert.equal(await c.reload(), true);
  writeFileSync(catalogueFile(), '{"users": []}');
  assert.equal(await c.reload(), true);
  assert.equal(c.allowed(owner), true);
  assert.equal(c.refused, 1);
  assert.equal(c.allowed("x@sub.example.com"), false, "no subdomain matching");
  writeFileSync(catalogueFile(), JSON.stringify({ users: ["@example.com", "guest@example.org"], admins: ["admin@example.com"] }));
  assert.equal(new Catalogue(join(dir, "missing.json")).loaded, false);
  assert.equal(await new Catalogue(join(dir, "missing.json")).reload(), false);

  // migrations: applied once, idempotent, an edited history refused, a newer database accepted
  assert.deepEqual(await migrate(t.db), []);
  await assert.rejects(migrate(t.db, [MIGRATIONS[0] + "-- edited"]), /was edited/);
  assert.deepEqual(await migrate(t.db, []), []);
});

test("a body over tools' 2 MB cap is answered 413 too_large with Connection: close, not a reset", async () => {
  const big = JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "slow_wait", arguments: { pad: "x".repeat(2_100_000) } } });
  const { Agent } = await import("node:http");
  const agent = new Agent({ keepAlive: true });
  try {
    const r = await new Promise<{ status: number; headers: import("node:http").IncomingHttpHeaders; body: string }>((resolve, reject) => {
      const u = new URL(`${url}/mcp`);
      const req = httpRequest({ hostname: u.hostname, port: u.port, path: u.pathname, method: "POST", headers: gw(sbx), agent }, (res) => {
        let b = "";
        res.on("data", (c) => (b += c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: b }));
      });
      req.on("error", reject);
      req.end(big);
    });
    assert.equal(r.status, 413);
    assert.equal(JSON.parse(r.body).code, "too_large");
    assert.equal(r.headers.connection, "close");
  } finally {
    agent.destroy();
  }
  // the server goes on serving
  assert.equal((await rpc("ping")).status, 200);
});

test("MCP: initialize, ping, list, unknown method, notifications, batch", async () => {
  const init = (await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claude-code" } })).body;
  assert.equal(init.result.protocolVersion, "2025-06-18");
  assert.ok(init.result.capabilities.tools);
  assert.equal((await rpc("initialize", { protocolVersion: "1999-01-01" })).body.result.protocolVersion, "2025-06-18");
  assert.deepEqual((await rpc("ping")).body.result, {});
  assert.equal((await rpc("nope/nope")).body.error.code, -32601);
  const names = (await rpc("tools/list")).body.result.tools.map((x: any) => x.name);
  assert.deepEqual(names, ["web_search", "web_fetch", "social_search", "anyapi_search", "anyapi_describe", "anyapi_run", "anyapi_result", "yyc_posts_search", "slow_wait"]);
  for (const n of names) assert.match(n, /^[a-z0-9_]+$/, "lowercase names");
  const note = await fetch(`${url}/mcp`, { method: "POST", headers: gw(sbx), body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) });
  assert.equal(note.status, 202);
  const batch = await fetch(`${url}/mcp`, {
    method: "POST",
    headers: gw(sbx),
    body: JSON.stringify([{ jsonrpc: "2.0", id: 7, method: "ping" }, { jsonrpc: "2.0", method: "notifications/initialized" }]),
  });
  assert.deepEqual(await batch.json(), [{ jsonrpc: "2.0", id: 7, result: {} }]);
});

test("web_search: Exa results as text, cost logged in tool_calls", async () => {
  const r = await call("web_search", { query: "施政報告", since: "2026-10-01", include_domains: ["https://www.info.gov.hk/x"] });
  assert.match(textOf(r), /施政報告 2026[\s\S]*https:\/\/www\.info\.gov\.hk\/a[\s\S]*Published: 2026-10-15/);
  const req = fakes.seen.filter((s) => s.path === "/exa/search").at(-1)!;
  assert.equal(req.body.startPublishedDate, "2026-10-01T00:00:00.000Z");
  assert.deepEqual(req.body.includeDomains, ["www.info.gov.hk"]);
  const { rows } = await q("select * from tool_calls where tool = 'web_search' order by id desc limit 1");
  assert.equal(rows[0].user_email, owner);
  assert.equal(rows[0].ok, true);
  assert.equal(Number(rows[0].cost_usd), 0.007);
  assert.equal(rows[0].conversation_id, sbx.conversationId);
  const bad = await call("web_search", { query: "x", since: "yesterday" });
  assert.equal(bad.isError, true);
  assert.match(textOf(bad), /YYYY-MM-DD/);
});

test("web_fetch: the instance's own copy or capture first, labelled; original: true, none and a failure read the page", async () => {
  const cached = await call("web_fetch", { url: "https://news.example/known-article" });
  assert.match(textOf(cached), /^Source: YYC's stored copy \(captured 2026-10-27 10:00\)/);
  assert.match(textOf(cached), /存檔全文/);
  assert.match(textOf(cached), /YYC's page for it: https:\/\/yyc\.example\/item\/7/);
  assert.equal((cached as any).structuredContent.via, "own-cache");
  const captured = await call("web_fetch", { url: "https://news.example/known-site/x" });
  assert.match(textOf(captured), /^Source: YYC's capture, read just now/);
  assert.equal((captured as any).structuredContent.via, "own-capture");
  // asked for as it is: the page itself (Exa here), no label
  const orig = await call("web_fetch", { url: "https://news.example/known-article", original: true });
  assert.match(textOf(orig), /全文內容/);
  assert.doesNotMatch(textOf(orig), /^Source:/);
  // not ours, or the capture down: read as before
  assert.match(textOf(await call("web_fetch", { url: "https://other.example/a" })), /全文內容/);
  assert.match(textOf(await call("web_fetch", { url: "https://news.example/capture-down" })), /全文內容/);
});

test("web_fetch: Exa text first, then the page itself; private addresses refused", async () => {
  fakes.exaHasText = true;
  assert.match(textOf(await call("web_fetch", { url: "https://example.org/a" })), /全文內容/);
  fakes.exaHasText = false;
  // Exa has nothing: the fake server itself is fetched (deps.resolve sends every host to 127.0.0.1)
  const direct = await call("web_fetch", { url: `${fakes.url.replace("127.0.0.1", "page.example")}/nothing-here` });
  assert.match(textOf(direct), /HTTP 404/);
  fakes.exaHasText = true;
  const saved = deps.resolve;
  delete deps.resolve; // the real check
  try {
    const r = await call("web_fetch", { url: "http://127.0.0.1:9/x", raw_html: true });
    assert.equal(r.isError, true);
    assert.match(textOf(r), /non-public address/);
  } finally {
    deps.resolve = saved;
  }
  assert.equal((await call("web_fetch", { url: "file:///etc/passwd" })).isError, true);
});

test("anyapi: find an API, read its input, run it (cost logged), re-read the result free; refusals", async () => {
  const found = textOf(await call("anyapi_search", { query: "facebook page posts" }));
  assert.match(found, /facebook\.profile_posts — Facebook Profile Posts/);
  assert.match(found, /up to USD 0\.0012 per request \(up to USD 0\.0067 when it fails over/);
  const described = textOf(await call("anyapi_describe", { api: "facebook.profile_posts" }));
  assert.match(described, /"inputSchema"/);
  assert.doesNotMatch(described, /lanes|latency/, "source lanes and latency are left out");
  const ran = await call("anyapi_run", { api: "facebook.profile_posts", input: { url: "https://facebook.com/hkgov" }, max_items: 2 });
  assert.equal(ran.isError, undefined);
  assert.match(textOf(ran), /Cost: USD 0\.0012/);
  assert.match(textOf(ran), /result_id: res_123/);
  assert.equal((textOf(ran).match(/帖文/g) ?? []).length, 2, "max_items reached AnyAPI");
  const run = fakes.seen.findLast((x) => x.path.startsWith("/anyapi/v1/run/"))!;
  assert.equal(run.path, "/anyapi/v1/run/facebook.profile_posts?max_items=2");
  assert.deepEqual(run.body, { url: "https://facebook.com/hkgov" });
  const { rows } = await q("select cost_usd, ok from tool_calls where tool = 'anyapi_run' order by id desc limit 1");
  assert.deepEqual(rows[0], { cost_usd: "0.0012", ok: true });
  const again = await call("anyapi_result", { result_id: "res_123", fields: "text" });
  assert.match(textOf(again), /re-read, free/);
  assert.doesNotMatch(textOf(again), /Cost:/, "a re-read costs nothing and is not counted again");
  assert.equal((await q("select cost_usd from tool_calls where tool = 'anyapi_result' order by id desc limit 1")).rows[0].cost_usd, null);
  // refusals: a bad id, a denied API, AnyAPI's own input error, an empty balance
  assert.equal((await call("anyapi_run", { api: "../v1/balance", input: {} })).isError, true);
  assert.match(textOf(await call("anyapi_run", { api: "facebook.marketplace", input: {} })), /not offered here/);
  assert.match(textOf(await call("anyapi_run", { api: "facebook.profile_posts", input: {} })), /input\.url is required/);
  fakes.anyapiBroke = true;
  try {
    assert.match(textOf(await call("anyapi_search", { query: "x" })), /balance has run out/);
  } finally {
    fakes.anyapiBroke = false;
  }
});

test("social_search: TikHub posts, fallback endpoint, platform check", async () => {
  const th = await call("social_search", { platform: "threads", query: "香港" });
  assert.match(textOf(th), /hkuser[\s\S]*Threads 帖子[\s\S]*threads\.com\/@hkuser\/post\/ABC[\s\S]*likes 12/);
  const wb = await call("social_search", { platform: "weibo", query: "香港" });
  assert.match(textOf(wb), /博主 · 2026-10-15 02:00 UTC[\s\S]*微博 內容[\s\S]*likes 12000/);
  assert.equal((await call("social_search", { platform: "myspace", query: "x" })).isError, true);
  const { rows } = await q("select cost_usd from tool_calls where tool = 'social_search' and ok order by id desc limit 1");
  assert.equal(Number(rows[0].cost_usd), 0.001);
});

test("upstream MCP: listed read-only tools are proxied with the staff member's identity; everything else is hidden", async () => {
  const r = await call("yyc_posts_search", { query: "x" });
  // the owner the gateway named (Aviary-Sandbox-Person) reaches yyc-mcp as X-YYC-User
  assert.equal(textOf(r), `called posts_search for ${owner}`);
  const up = fakes.seen.filter((s) => s.path === "/mcp-up" && s.body?.method === "tools/call").at(-1)!;
  assert.equal(up.headers.authorization, "Bearer up");
  assert.equal(up.headers["x-yyc-user"], owner);
  // another person's sandbox: their own email, never the listing identity or another caller's
  const other = sandbox({ person: "other@example.com" });
  const r2 = (await rpc("tools/call", { name: "yyc_posts_search", arguments: { query: "y" } }, other)).body.result;
  assert.equal(textOf(r2), "called posts_search for other@example.com");
  // the tool list is read as the upstream's listAs
  const listing = fakes.seen.filter((s) => s.path === "/mcp-up" && s.body?.method === "tools/list").at(-1)!;
  assert.equal(listing.headers["x-yyc-user"], "lister@example.com");

  // not listed (Send-Mail, a new read-only tool) or listed but marked as writing (post_delete): never offered, never run
  const before = fakes.seen.filter((s) => s.body?.method === "tools/call").length;
  for (const name of ["yyc_send_mail", "yyc_stream_items", "yyc_post_delete"]) {
    const res = await rpc("tools/call", { name, arguments: {} });
    assert.equal(res.body.error.code, -32602, name);
    assert.match(res.body.error.message, /unknown tool/);
  }
  assert.equal(fakes.seen.filter((s) => s.body?.method === "tools/call").length, before, "never forwarded");

  // default deny: an upstream without a `tools` list offers nothing
  const bare: { tools?: string[] } = {};
  assert.equal(readOnlyAllowed(bare, { name: "posts_search", annotations: { readOnlyHint: true } }), false);
  const listed = { ...bare, tools: ["a"] };
  assert.equal(readOnlyAllowed(listed, { name: "a" }), true);
  assert.equal(readOnlyAllowed(listed, { name: "a", annotations: { readOnlyHint: false } }), false);
  assert.equal(readOnlyAllowed(listed, { name: "a", annotations: { destructiveHint: true } }), false);
  assert.equal(readOnlyAllowed(listed, { name: "b", annotations: { readOnlyHint: true } }), false);
});

test("helpers: private addresses, hostnames, dates, SSE answers, HTML text", () => {
  for (const a of ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "100.64.0.1", "169.254.169.254", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "fd42::5"])
    assert.equal(isPrivateAddress(a), true, a);
  for (const a of ["8.8.8.8", "1.1.1.1", "2a01:4f8::1", "::ffff:8.8.8.8"]) assert.equal(isPrivateAddress(a), false, a);
  assert.equal(iso(1792000000), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("1792000000000"), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("Wed Oct 15 10:00:00 +0800 2026"), "2026-10-15T02:00:00.000Z");
  assert.equal(iso("garbage"), null);
  assert.equal(pickResponse('event: message\ndata: {"jsonrpc":"2.0","id":4,"result":{"a":1}}\n\n', "text/event-stream", 4).result.a, 1);
  assert.deepEqual(htmlToText("<html><title>T &amp; U</title><script>x()</script><p>一</p><p>二&nbsp;三</p></html>"), { title: "T & U", text: "T & U 一\n二 三" });
  assert.deepEqual(toolList({ ...deps, config: { upstreams: [] } }, []).map((d) => d.name), ["web_fetch"]);
});
