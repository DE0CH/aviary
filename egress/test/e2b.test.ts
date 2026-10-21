// The E2B sandbox provider end to end on fakes: real Postgres, the real web role and sandbox agent (in JARVIS_INIT mode,
// as E2B's template starts it), a fake E2B API and port proxy.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { janitorPass } from "../src/web/janitor.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { E2bProvider } from "../src/web/sandbox-e2b.ts";
import { FAKE_E2B_KEY, FakeE2b } from "./fakes/e2b.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const INSTANCE = "ai.example.org";

let pg: Awaited<ReturnType<typeof freshDb>>;
let e2b: FakeE2b;
let web: WebServer;
let deps: WebDeps;

const baseConfig = {
  product: { name: "YYC AI", locale: "zh-HK" },
  publicUrl: `https://${INSTANCE}`,
  access: { teamDomain: "t.cloudflareaccess.com", audience: "aud" },
  users: [A],
  models: [{ id: "m1", label: "M" }],
  defaultModel: "m1",
  systemPromptFile: "/dev/null",
};

before(async () => {
  pg = await freshDb("e2b");
  e2b = await new FakeE2b().start();
  const work = mkdtempSync(join(tmpdir(), "chat-e2b-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant.");
  const config = parseConfig({
    ...baseConfig,
    systemPromptFile: promptFile,
    sandbox: { provider: "e2b", maxActive: 3, idleMinutes: 30, e2b: { template: "jarvis3-sandbox:abc123", egressUrl: "wss://egress.example.org/wire", apiUrl: e2b.apiUrl } },
  });
  const e = config.sandbox.e2b!;
  deps = {
    config,
    db: pg.db,
    dbUrl: pg.url,
    sandboxProvider: new E2bProvider({ ...e, apiKey: FAKE_E2B_KEY, egressHeaders: { "cf-access-client-id": "id-x" }, agentUrlFor: e2b.agentUrlFor }),
    namespace: "jarvis",
    drainSeconds: 5,
  };
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
});

after(async () => {
  await web.close();
  await e2b.close();
  await pg.drop();
});

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

test("an E2B sandbox locked to the egress host, configured by /init, healthy through the port proxy", async () => {
  const conv = randomUUID();
  await web.app.sandboxes.ensure(conv, A);
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

  // keep-alive (every touch, at most every 30 s per sandbox) sets E2B's timeout
  await deps.sandboxProvider.keepAlive(row, new Date(Date.now() + 600_000));
  assert.ok(Math.abs(sb.timeouts.at(-1)! - 600) <= 1, String(sb.timeouts));

  // idle: the janitor kills the sandbox
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
  const conv = randomUUID();
  const saved = (p as any).create;
  (p as any).create = async () => {
    throw new Error("e2b POST /v2/sandboxes: HTTP 429 rate limited");
  };
  try {
    await assert.rejects(web.app.sandboxes.ensure(conv, A), /429/);
  } finally {
    (p as any).create = saved;
  }
  const r = (await pg.db.query("select status, release_reason from sandboxes where conversation_id = $1", [conv])).rows[0];
  assert.equal(r.status, "gone");
  assert.match(r.release_reason, /start failed: .*429/);
});
