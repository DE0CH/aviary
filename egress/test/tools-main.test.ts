// The tools role's main end to end (src/tools/main.ts `run`): the real start and signal path in a child process of
// its own, with its config and catalogue in a temp dir, ports picked by the OS, a fake hermit, a fake upstream MCP
// server with a slow tool, and its own `tools` database on the local Postgres (TEST_DATABASE_URL). main() itself
// always runs `run(SITE)` with the pod's fixed paths and ports; only where it runs differs here.
// Covers: unready until migrations, catalogue and upstreams are done; on SIGTERM /readyz turns 503 (draining), new
// connections are refused, the in-flight call finishes with Connection: close and its tool_calls row is written
// before the exit, exit 0 with log `drained`; a second SIGTERM exits at once; a missing variable exits 2.
// Every child is killed on teardown and every fake server closes its connections, so nothing outlives a test.
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { Agent, createServer, request as httpRequest, type IncomingHttpHeaders, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import pg from "pg";
import type { Site } from "../src/tools/main.ts";

const MAIN = new URL("../src/tools/main.ts", import.meta.url).href;
const GW = "gateway-pair-token-main";
const HERMIT_BEARER = "tools-to-hermit-main";
const owner = "staff@example.com";
const admin = process.env.TEST_DATABASE_URL || "postgres://claude:dev@127.0.0.1:5432/postgres";
const dbName = `tools_test_mainproc_${process.pid}`;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let dir = "";
let dbUrl = "";
let hermit: Server;
let hermitUrl = "";
let upstream: Server;
let upstreamUrl = "";
const sbx = { id: randomUUID(), conversationId: randomUUID(), person: owner };

async function adminQuery(sql: string, url = admin): Promise<pg.QueryResult> {
  const c = new pg.Client({ connectionString: url });
  await c.connect();
  try {
    return await c.query(sql);
  } finally {
    await c.end();
  }
}

async function listen(s: Server): Promise<string> {
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  return `http://127.0.0.1:${(s.address() as AddressInfo).port}`;
}

function close(s: Server): Promise<void> {
  s.closeAllConnections();
  return new Promise((r) => s.close(() => r()));
}

before(async () => {
  dir = mkdtempSync(join(tmpdir(), "tools-main-test-"));
  await adminQuery(`drop database if exists ${dbName} with (force)`);
  await adminQuery(`create database ${dbName}`);
  const u = new URL(admin);
  u.pathname = `/${dbName}`;
  dbUrl = u.toString();

  // hermit's GET /v1/sandbox-tokens/{id}: the one sandbox is ready
  hermit = createServer((req, res) => {
    if (req.headers.authorization !== `Bearer ${HERMIT_BEARER}`) return void res.writeHead(401).end('{"code":"unauthenticated"}');
    if (req.url !== `/v1/sandbox-tokens/${sbx.id}`) return void res.writeHead(404, { "content-type": "application/json" }).end('{"code":"unknown_sandbox"}');
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ sandboxId: sbx.id, state: "ready", live: true, conversationId: sbx.conversationId, person: sbx.person, expiresAt: new Date(Date.now() + 3600_000).toISOString() }));
  });
  hermitUrl = await listen(hermit);

  // an upstream REST API with one tool, `wait`, that answers after `ms`
  upstream = createServer((req, res) => {
    let b = "";
    req.on("data", (c) => (b += c));
    req.on("end", async () => {
      const answer = (v: unknown) => {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(v));
      };
      if (req.url === "/api/openapi.json") return answer({ openapi: "3.1.0", paths: { "/api/wait": { post: { summary: "waits" } } } });
      if (req.url === "/api/wait") {
        const ms = Number(JSON.parse(b || "{}").ms ?? 0);
        for (const w of upstreamCallWaiters.get(ms)?.splice(0) ?? []) w();
        // unref'd: a call the test leaves hanging never holds this test process open
        await new Promise((r) => setTimeout(r, ms).unref());
        return answer({ waited: true });
      }
      res.writeHead(404).end();
    });
  });
  upstreamUrl = await listen(upstream);

  writeFileSync(join(dir, "tools.json"), JSON.stringify({ upstreams: [{ prefix: "up", url: `${upstreamUrl}/api`, tools: ["wait"], listAs: "lister@example.com" }] }));
  writeFileSync(join(dir, "catalogue.json"), JSON.stringify({ users: ["@example.com"], admins: [] }));
});

after(async () => {
  for (const c of children) if (c.proc.exitCode === null && c.proc.signalCode === null) c.proc.kill("SIGKILL");
  await Promise.all(children.map((c) => c.exited));
  await close(hermit);
  await close(upstream);
  await adminQuery(`drop database if exists ${dbName} with (force)`);
  rmSync(dir, { recursive: true, force: true });
});

interface Child {
  proc: ChildProcess;
  /** the JSON log lines so far (stdout and stderr) */
  lines: Array<Record<string, any>>;
  exited: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  /** resolves with the first log line whose `at` is `at` */
  logged(at: string, ms?: number): Promise<Record<string, any>>;
}
const children: Child[] = [];

/** waiters for the upstream to receive a tools/call with this `ms`: then the call is in flight in tools */
const upstreamCallWaiters = new Map<number, Array<() => void>>();
function upstreamReceives(ms: number): Promise<void> {
  return new Promise((r) => {
    const list = upstreamCallWaiters.get(ms) ?? [];
    list.push(r);
    upstreamCallWaiters.set(ms, list);
  });
}

function start(env: Record<string, string> = { TOOLS_DATABASE_URL: "", TOOLS_ACCEPT_GATEWAY: GW, TOOLS_TOKEN_HERMIT: HERMIT_BEARER }): Child {
  const site: Site = { configPath: join(dir, "tools.json"), cataloguePath: join(dir, "catalogue.json"), apiPort: 0, healthPort: 0, hermitUrl };
  const script = `const { run } = await import(${JSON.stringify(MAIN)}); await run(JSON.parse(process.argv.at(-1)));`;
  const vars: Record<string, string> = { PATH: process.env.PATH ?? "", ...env };
  if (vars.TOOLS_DATABASE_URL === "") vars.TOOLS_DATABASE_URL = dbUrl;
  const proc = spawn(process.execPath, ["--disable-warning=ExperimentalWarning", "--input-type=module", "-e", script, JSON.stringify(site)], { env: vars, stdio: ["ignore", "pipe", "pipe"] });
  const lines: Array<Record<string, any>> = [];
  const waiters: Array<() => void> = [];
  const onData = (buf: string) => (chunk: Buffer) => {
    buf += chunk.toString();
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const l = buf.slice(0, i);
      buf = buf.slice(i + 1);
      try {
        lines.push(JSON.parse(l));
      } catch {
        lines.push({ at: "raw", text: l });
      }
      for (const w of waiters.splice(0)) w();
    }
  };
  proc.stdout!.on("data", onData(""));
  proc.stderr!.on("data", onData(""));
  const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((r) => proc.on("close", (code, signal) => r({ code, signal })));
  const c: Child = {
    proc,
    lines,
    exited,
    async logged(at, ms = 15_000) {
      const deadline = Date.now() + ms;
      for (;;) {
        const l = lines.find((x) => x.at === at);
        if (l) return l;
        if (Date.now() > deadline || proc.exitCode !== null) throw new Error(`no log "${at}" (got ${JSON.stringify(lines)})`);
        await Promise.race([new Promise<void>((r) => waiters.push(r)), sleep(200)]);
      }
    },
  };
  children.push(c);
  return c;
}

async function ready(c: Child): Promise<{ api: string; health: string }> {
  const api = `http://127.0.0.1:${(await c.logged("tools listening")).port}`;
  const health = `http://127.0.0.1:${(await c.logged("tools health listening")).port}`;
  const deadline = Date.now() + 20_000;
  for (;;) {
    const r = await fetch(`${health}/readyz`).catch(() => null);
    if (r?.status === 200) return { api, health };
    await r?.text();
    assert.ok(Date.now() < deadline, `never ready: ${JSON.stringify(c.lines)}`);
    await sleep(100);
  }
}

function call(api: string, ms: number, request: string, agent?: Agent): Promise<{ status: number; headers: IncomingHttpHeaders; body: string }> {
  const u = new URL(`${api}/v1/tools/up_wait`);
  const headers = {
    authorization: `Bearer ${GW}`,
    "aviary-sandbox-id": sbx.id,
    "aviary-sandbox-conversation": sbx.conversationId,
    "aviary-sandbox-person": sbx.person,
    "aviary-request": request,
    "content-type": "application/json",
  };
  return new Promise((resolve, reject) => {
    const req = httpRequest({ hostname: u.hostname, port: u.port, path: u.pathname, method: "POST", headers, agent }, (res) => {
      let b = "";
      res.on("data", (x) => (b += x));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: b }));
    });
    req.on("error", reject);
    req.end(JSON.stringify({ ms }));
  });
}

test("main: SIGTERM drains (readyz 503, no new connections, the in-flight call finishes and is logged), then exit 0 `drained`", { timeout: 60_000 }, async () => {
  const c = start();
  const { api, health } = await ready(c);
  const v = (await (await fetch(`${health}/version`)).json()) as { name: string };
  assert.equal(v.name, "tools");
  assert.equal((await call(api, 0, "warm")).status, 200);

  const agent = new Agent({ keepAlive: true, maxSockets: 1 });
  try {
    const request = `drain-${randomUUID()}`;
    const received = upstreamReceives(1500);
    const inflight = call(api, 1500, request, agent);
    await received; // the call is in flight, not still connecting, when SIGTERM arrives
    c.proc.kill("SIGTERM");
    await c.logged("tools draining");
    const r = await fetch(`${health}/readyz`);
    assert.equal(r.status, 503);
    assert.ok(((await r.json()) as { why: string[] }).why.includes("draining"));
    await assert.rejects(call(api, 0, "late"), "no new connections during the drain");
    const done = await inflight;
    assert.equal(done.status, 200);
    assert.match(done.body, /waited/);
    assert.equal(done.headers.connection, "close");
    const exit = await c.exited;
    assert.deepEqual(exit, { code: 0, signal: null });
    const drained = await c.logged("drained");
    assert.equal(drained.cut, 0);
    assert.equal(drained.unwritten, 0);
    const { rows } = await adminQuery(`select ok, tool, user_email from tool_calls where request = '${request}'`, dbUrl);
    assert.deepEqual(rows, [{ ok: true, tool: "up_wait", user_email: owner }], "the in-flight call's row is written before the exit");
  } finally {
    agent.destroy();
  }
});

test("main: a second SIGTERM exits at once, even with a call in flight", { timeout: 60_000 }, async () => {
  const c = start();
  const { api } = await ready(c);
  const received = upstreamReceives(60_000);
  const inflight = call(api, 60_000, "long").catch((e) => e);
  await received;
  c.proc.kill("SIGTERM");
  await c.logged("tools draining");
  const t0 = Date.now();
  c.proc.kill("SIGTERM");
  assert.deepEqual(await c.exited, { code: 0, signal: null });
  assert.ok(Date.now() - t0 < 5000, "exits at once");
  await c.logged("tools exit at once (second SIGTERM)");
  assert.ok((await inflight) instanceof Error, "the call is cut with the process");
});

test("main: a missing variable stops the start with exit 2", { timeout: 30_000 }, async () => {
  const c = start({ TOOLS_DATABASE_URL: "", TOOLS_ACCEPT_GATEWAY: GW });
  assert.deepEqual(await c.exited, { code: 2, signal: null });
  assert.ok(c.lines.some((l) => l.at === "tools start" && /TOOLS_TOKEN_HERMIT/.test(l.error)));
});
