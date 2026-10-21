// The E2B sandbox provider end to end on fakes: real Postgres, the real web role, jarvis-bash and sandbox agent (in
// JARVIS_INIT mode, as E2B's template starts it), a fake E2B API and port proxy, a fake claw and S3.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { AccessVerifier } from "../src/common/access.ts";
import { parseConfig } from "../src/common/config.ts";
import { Listener } from "../src/common/db.ts";
import { S3BlobStore } from "../src/common/s3.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { janitorPass } from "../src/web/janitor.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { E2bProvider } from "../src/web/sandbox-e2b.ts";
import { AUD, fakeAccess, TEAM } from "./fakes/access.ts";
import { FAKE_E2B_KEY, FakeE2b } from "./fakes/e2b.ts";
import { fakeS3 } from "./fakes/s3.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const INSTANCE = "ai.example.org";

let pg: Awaited<ReturnType<typeof freshDb>>;
let s3: Awaited<ReturnType<typeof fakeS3>>;
let access: Awaited<ReturnType<typeof fakeAccess>>;
let e2b: FakeE2b;
let web: WebServer;
let deps: WebDeps;
let base: string;
let mcp: import("node:http").Server;

const baseConfig = {
  product: { name: "YYC AI", locale: "zh-HK" },
  publicUrl: `https://${INSTANCE}`,
  access: { teamDomain: TEAM, audience: AUD },
  users: [A],
  models: [{ id: "m1", label: "M" }],
  defaultModel: "m1",
  systemPromptFile: "/dev/null",
};

before(async () => {
  pg = await freshDb("e2b");
  s3 = await fakeS3();
  access = await fakeAccess();
  e2b = await new FakeE2b().start();
  mcp = createServer((req, res) => {
    req.resume();
    res.writeHead(req.url === "/mcp" ? 200 : 404, { "content-type": "application/json" });
    res.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { tools: [] } }));
  });
  await new Promise<void>((r) => mcp.listen(0, "127.0.0.1", r));
  const mcpPort = (mcp.address() as { port: number }).port;
  const work = mkdtempSync(join(tmpdir(), "chat-e2b-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant.");
  const config = parseConfig({
    ...baseConfig,
    systemPromptFile: promptFile,
    sandbox: { provider: "e2b", maxActive: 3, idleMinutes: 30, e2b: { template: "jarvis3-sandbox:abc123", egressUrl: "wss://egress.example.org/wire", apiUrl: e2b.apiUrl } },
    s3: { endpoint: s3.url, region: "fsn1", bucket: "b", prefix: "jarvis/" },
  });
  const e = config.sandbox.e2b!;
  deps = {
    config,
    db: pg.db,
    dbUrl: pg.url,
    listener: new Listener(pg.url),
    blobs: new S3BlobStore({ ...config.s3, accessKeyId: "k", secretAccessKey: "s" }),
    sandboxProvider: new E2bProvider({ ...e, apiKey: FAKE_E2B_KEY, egressHeaders: { "cf-access-client-id": "id-x" }, agentUrlFor: e2b.agentUrlFor }),
    verifier: new AccessVerifier({ teamDomain: TEAM, audience: AUD, jwks: access.jwks }),
    replica: "web_e2b",
    namespace: "jarvis",
    workRoot: join(work, "work"),
    clawPath: here("./fakes/claw.mjs"),
    bashPath: here("../src/bin/jarvis-bash"),
    mcpBridgePath: here("../src/bin/jarvis-mcp-bridge"),
    egressBase: `http://127.0.0.1:${mcpPort}`,
    toolsUrl: `http://127.0.0.1:${mcpPort}/mcp`,
    uiDir: join(work, "ui"),
    brandDir: join(work, "brand"),
    drainSeconds: 5,
  };
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  await web.close();
  await deps.listener.close();
  await e2b.close();
  await s3.close();
  mcp.close();
  await pg.drop();
});

async function call(method: string, path: string, body?: unknown) {
  const h: Record<string, string> = { "cf-access-jwt-assertion": await access.token(A) };
  if (body !== undefined) h["content-type"] = "application/json";
  const r = await fetch(base + path, { method, headers: h, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let json: any = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: r.status, json, text };
}

async function runTurn(conv: string, text: string): Promise<any[]> {
  const r = await call("POST", `/api/conversations/${conv}/messages`, { text });
  assert.equal(r.status, 202, r.text);
  for (let i = 0; i < 300; i++) {
    const c = await call("GET", `/api/conversations/${conv}`);
    const t = c.json.turns.find((x: any) => x.id === r.json.turnId);
    if (t && ["done", "error", "cancelled"].includes(t.status)) return t.events;
    await new Promise((res) => setTimeout(res, 100));
  }
  throw new Error("turn did not finish");
}

test("config: provider e2b needs a template and a wss egress URL, not an image", () => {
  assert.throws(() => parseConfig({ ...baseConfig, sandbox: { provider: "e2b", e2b: { egressUrl: "wss://e/wire" } } }), /sandbox\.e2b\.template/);
  assert.throws(() => parseConfig({ ...baseConfig, sandbox: { provider: "e2b", e2b: { template: "t", egressUrl: "https://e/x" } } }), /wss/);
  assert.throws(() => parseConfig({ ...baseConfig, sandbox: { provider: "lambda" } }), /provider/);
  assert.throws(() => parseConfig({ ...baseConfig, sandbox: {} }), /sandbox\.image/);
  const c = parseConfig({ ...baseConfig, sandbox: { provider: "e2b", e2b: { template: "t", egressUrl: "wss://e.example/wire" } } });
  assert.deepEqual(
    { ...c.sandbox.e2b, egressHeaders: undefined },
    { template: "t", egressUrl: "wss://e.example/wire", egressHeaders: undefined, apiKeyEnv: "E2B_API_KEY", apiUrl: "https://api.e2b.app", domain: "e2b.app", instance: INSTANCE },
  );
});

test("first bash → an E2B sandbox locked to the egress host, configured by /init; bash runs through the port proxy", async () => {
  const conv = (await call("POST", "/api/conversations", {})).json.id;
  await web.app.sandboxes.ensure(conv);
  const row: any = (await pg.db.query("select * from sandboxes where conversation_id = $1", [conv])).rows[0];
  assert.equal(row.status, "ready");
  const sb = e2b.sandboxes.get(row.pod);
  assert.ok(sb, "sandboxes.pod holds E2B's sandbox id");
  assert.equal(sb.body.templateID, "jarvis3-sandbox:abc123");
  assert.deepEqual(sb.body.network, { allowPublicTraffic: false, allowOut: ["egress.example.org"], denyOut: ["0.0.0.0/0"] });
  assert.deepEqual(sb.body.metadata, { jarvis: INSTANCE, sandbox: row.id, conversation: conv });
  assert.equal(sb.body.envVars, undefined, "nothing secret in the create call");
  assert.deepEqual(row.agent.headers, { "e2b-traffic-access-token": sb.traffic });
  assert.ok(sb.timeouts.length >= 1 && sb.timeouts[0]! > 30 * 60, `the timeout is set past the idle release (${sb.timeouts})`);

  const events = await runTurn(conv, "BASH:echo $HTTPS_PROXY | sed 's/sbx:[^@]*@/sbx:T@/'; echo $PIP_INDEX_URL | sed 's/sbx:[^@]*@/sbx:T@/'; echo hi > made.txt");
  const result = events.find((e: any) => e.type === "tool_result");
  assert.equal(result.data.is_error, false, JSON.stringify(result.data));
  const out = JSON.parse(result.data.output).stdout.trim().split("\n");
  assert.deepEqual(out, ["http://sbx:T@127.0.0.1:3128", "http://sbx:T@127.0.0.1:3128/mirror/pypi/simple"]);
  const files = await call("GET", `/api/conversations/${conv}/files`);
  assert.ok(files.json.some((f: any) => f.path === "made.txt"));
  // keep-alive (typing, turns, every minute of a running turn; at most every 30 s per sandbox) sets E2B's timeout
  await deps.sandboxProvider.keepAlive(row, new Date(Date.now() + 600_000));
  assert.ok(Math.abs(sb.timeouts.at(-1)! - 600) <= 1, String(sb.timeouts));

  // idle: the janitor saves what changed and kills the sandbox
  await pg.db.query("update sandboxes set last_active_at = now() - interval '2 hours' where id = $1", [row.id]);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal((await pg.db.query("select status from sandboxes where id = $1", [row.id])).rows[0].status, "gone");
  assert.ok(e2b.killed.includes(row.pod));
});

test("janitor: kills this instance's E2B sandboxes nobody owns, never another instance's", async () => {
  const mine = await e2b.add({ templateID: "t", metadata: { jarvis: INSTANCE, sandbox: "x" } });
  mine.startedAt = new Date(Date.now() - 600_000).toISOString();
  const theirs = await e2b.add({ templateID: "t", metadata: { jarvis: "other.example.org" } });
  theirs.startedAt = mine.startedAt;
  const fresh = await e2b.add({ templateID: "t", metadata: { jarvis: INSTANCE } });
  await janitorPass(deps, web.app.sandboxes);
  assert.ok(e2b.killed.includes(mine.id));
  assert.ok(!e2b.killed.includes(theirs.id));
  assert.ok(!e2b.killed.includes(fresh.id), "a sandbox younger than a minute may still be starting");
});

test("a create that fails leaves no row behind and says why", async () => {
  const p = deps.sandboxProvider as E2bProvider;
  const conv = (await call("POST", "/api/conversations", {})).json.id;
  const saved = (p as any).create;
  (p as any).create = async () => {
    throw new Error("e2b POST /v2/sandboxes: HTTP 429 rate limited");
  };
  try {
    await assert.rejects(web.app.sandboxes.ensure(conv), /429/);
  } finally {
    (p as any).create = saved;
  }
  const r = (await pg.db.query("select status, release_reason from sandboxes where conversation_id = $1", [conv])).rows[0];
  assert.equal(r.status, "gone");
  assert.match(r.release_reason, /start failed: .*429/);
});
