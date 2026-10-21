// The web role end to end on fakes: real Postgres, the real sandbox agent (started by a fake Kubernetes API), the
// CloudCLI UI's calls under /internal/cc/* and a process run in the sandbox through the agent's /proc.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { request } from "node:http";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { janitorPass } from "../src/web/janitor.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { userKey } from "../src/web/sandbox.ts";
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const B = "bob@example.org";
const CC_TOKEN = "cc-token-test";

let pg: Awaited<ReturnType<typeof freshDb>>;
let k8s: FakeK8s;
let web: WebServer;
let deps: WebDeps;
let base: string;

before(async () => {
  process.env.JARVIS_CC_TOKEN = CC_TOKEN;
  pg = await freshDb("web");
  k8s = new FakeK8s();
  const work = mkdtempSync(join(tmpdir(), "chat-web-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant.");
  const config = parseConfig({
    product: { name: "YYC AI", locale: "zh-HK" },
    publicUrl: "https://ai.example.org",
    access: { teamDomain: "t.cloudflareaccess.com", audience: "aud" },
    users: [A, B],
    admins: [A],
    models: [{ id: "anthropic/claude-sonnet-5.5", label: "Sonnet" }],
    defaultModel: "anthropic/claude-sonnet-5.5",
    systemPromptFile: promptFile,
    sandbox: { image: "sandbox:test", maxActive: 2 },
  });
  deps = { config, db: pg.db, dbUrl: pg.url, sandboxProvider: undefined as any, namespace: "jarvis", drainSeconds: 5 };
  deps.sandboxProvider = new KubernetesProvider({ k8s, config, egressNamespace: "jarvis", agentUrlFor: k8s.agentUrlFor });
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  await web.close();
  await k8s.closeAll();
  await pg.drop();
});

async function cc(path: string, body: unknown, token = CC_TOKEN, method = "POST") {
  const r = await fetch(base + path, { method, headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: method === "GET" ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let json: any = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: r.status, json, text };
}

// the agent's /proc frames: 1 byte type, 4 bytes big-endian length, payload (chat/sandbox/agent.mjs)
const PROC_STDIN = 0, PROC_STDOUT = 1, PROC_EXIT = 3, PROC_STDIN_END = 4;
const frame = (type: number, payload: string) => {
  const b = Buffer.from(payload);
  const h = Buffer.alloc(5);
  h[0] = type;
  h.writeUInt32BE(b.length, 1);
  return Buffer.concat([h, b]);
};
function frameReader(on: (type: number, payload: Buffer) => void): (c: Buffer) => void {
  let buf = Buffer.alloc(0);
  return (c) => {
    buf = Buffer.concat([buf, c]);
    while (buf.length >= 5 && buf.length >= 5 + buf.readUInt32BE(1)) {
      const n = buf.readUInt32BE(1);
      on(buf[0]!, buf.subarray(5, 5 + n));
      buf = buf.subarray(5 + n);
    }
  };
}

/** Runs argv in the sandbox through the agent's /proc (as the CloudCLI UI does), with `input` on stdin. */
function runInSandbox(sb: { url: string; token: string; headers: Record<string, string> }, args: string[], input = ""): Promise<{ stdout: string; exit: { code: number | null } }> {
  return new Promise((resolve, reject) => {
    const u = new URL("/proc", sb.url);
    const req = request({
      hostname: u.hostname,
      port: u.port,
      path: u.pathname,
      headers: {
        ...sb.headers,
        "x-jarvis-exec-token": sb.token,
        connection: "Upgrade",
        upgrade: "jarvis-proc",
        "x-proc": Buffer.from(JSON.stringify({ args })).toString("base64"),
      },
    });
    req.on("response", (res) => reject(new Error(`no upgrade: HTTP ${res.statusCode}`)));
    req.on("error", reject);
    req.on("upgrade", (_res, socket) => {
      let stdout = "";
      socket.on("data", frameReader((type, payload) => {
        if (type === PROC_STDOUT) stdout += payload.toString("utf8");
        if (type === PROC_EXIT) resolve({ stdout, exit: JSON.parse(payload.toString("utf8")) });
      }));
      if (input) socket.write(frame(PROC_STDIN, input));
      socket.write(frame(PROC_STDIN_END, ""));
    });
    req.end();
  });
}

test("/internal/cc/*: the shared token only, POST only, a uuid conversation, instance users only", async () => {
  const conv = randomUUID();
  assert.equal((await cc("/internal/cc/sandbox", { owner: A, conversation: conv }, "wrong")).status, 401);
  assert.equal((await cc("/internal/cc/sandbox", undefined, CC_TOKEN, "GET")).status, 405);
  assert.equal((await cc("/internal/cc/sandbox", { owner: A, conversation: "nope" })).status, 400);
  assert.equal((await cc("/internal/cc/sandbox", { owner: "eve@example.org", conversation: conv })).status, 403);
  assert.equal((await cc("/internal/cc/nothing", { conversation: conv })).status, 404);
  // nothing else is served: no UI, no API
  assert.equal((await fetch(`${base}/`)).status, 404);
  assert.equal((await fetch(`${base}/api/conversations`)).status, 404);
  assert.equal((await fetch(`${base}/healthz`)).status, 200);
});

test("a sandbox per conversation: made once, the owner's ~/.claude key, Claude Code runs through /proc, reset after a release", async () => {
  const conv = randomUUID();
  const first = await cc("/internal/cc/sandbox", { owner: A.toUpperCase(), conversation: conv });
  assert.equal(first.status, 200, first.text);
  assert.equal(first.json.reset, false);
  const row = (await pg.db.query("select * from sandboxes where id = $1", [first.json.id])).rows[0];
  assert.equal(row.owner, A);
  assert.equal(row.user_key, userKey(A));
  assert.equal(row.status, "ready");
  // the same sandbox while it lives
  const again = await cc("/internal/cc/sandbox", { owner: A, conversation: conv });
  assert.equal(again.json.id, first.json.id);
  assert.equal(again.json.reset, false);

  // the UI runs a process in it: stdin in, stdout and the exit status out
  const ws = k8s.workOf(row.pod);
  const r = await runInSandbox(first.json, ["sh", "-c", "read x; echo got:$x; pwd"], "hello\n");
  assert.equal(r.exit.code, 0);
  assert.equal(r.stdout, `got:hello\n${ws}\n`);

  // touch: its idle time starts again
  await pg.db.query("update sandboxes set last_active_at = now() - interval '2 hours' where id = $1", [row.id]);
  assert.equal((await cc("/internal/cc/touch", { conversation: conv })).status, 200);
  assert.ok(Date.now() - new Date((await pg.db.query("select last_active_at from sandboxes where id = $1", [row.id])).rows[0].last_active_at).getTime() < 60_000);

  // idle: released by the janitor; the next call makes a new one and says so
  await pg.db.query("update sandboxes set last_active_at = now() - interval '2 hours' where id = $1", [row.id]);
  await janitorPass(deps, web.app.sandboxes);
  assert.deepEqual((await pg.db.query("select status, release_reason from sandboxes where id = $1", [row.id])).rows[0], { status: "gone", release_reason: "idle" });
  assert.equal(k8s.pods.has(row.pod), false, "the pod is deleted");
  const next = await cc("/internal/cc/sandbox", { owner: A, conversation: conv });
  assert.notEqual(next.json.id, first.json.id);
  assert.equal(next.json.reset, true);

  // release (the conversation was deleted): gone at once
  assert.equal((await cc("/internal/cc/release", { conversation: conv })).status, 200);
  assert.equal((await pg.db.query("select 1 from sandboxes where conversation_id = $1 and status <> 'gone'", [conv])).rows.length, 0);
});

test("a full pool frees the least recently used idle sandbox, never one in use", async () => {
  const [c1, c2, c3] = [randomUUID(), randomUUID(), randomUUID()];
  await cc("/internal/cc/sandbox", { owner: A, conversation: c1 });
  await cc("/internal/cc/sandbox", { owner: B, conversation: c2 });
  await pg.db.query("update sandboxes set last_active_at = now() - interval '10 minutes' where conversation_id = $1", [c1]);
  const third = await cc("/internal/cc/sandbox", { owner: A, conversation: c3 });
  assert.equal(third.status, 200, third.text);
  const live = (await pg.db.query("select conversation_id from sandboxes where status <> 'gone'")).rows.map((r) => r.conversation_id);
  assert.deepEqual(live.sort(), [c2, c3].sort());
  for (const c of [c2, c3]) await cc("/internal/cc/release", { conversation: c });
});

test("janitor: orphan pods go, rows whose sandbox vanished are marked gone, max lifetime", async () => {
  k8s.ghosts.set("sbx-orphan", { metadata: { name: "sbx-orphan", creationTimestamp: new Date(Date.now() - 600_000).toISOString() }, status: { phase: "Running" } });
  const vanished = randomUUID();
  const v = await cc("/internal/cc/sandbox", { owner: A, conversation: vanished });
  const pod = (await pg.db.query("select pod from sandboxes where id = $1", [v.json.id])).rows[0].pod;
  await k8s.deletePod("", pod);
  const old = randomUUID();
  const o = await cc("/internal/cc/sandbox", { owner: B, conversation: old });
  await pg.db.query("update sandboxes set expires_at = now() - interval '1 minute' where id = $1", [o.json.id]);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal(k8s.ghosts.has("sbx-orphan"), false);
  assert.deepEqual((await pg.db.query("select status, release_reason from sandboxes where id = $1", [v.json.id])).rows[0], { status: "gone", release_reason: "sandbox vanished" });
  assert.deepEqual((await pg.db.query("select status, release_reason from sandboxes where id = $1", [o.json.id])).rows[0], { status: "gone", release_reason: "max-lifetime" });
});

test("drain: not ready, but the calls in flight are answered", async () => {
  web.app.draining = true;
  try {
    assert.equal((await fetch(`${base}/healthz`)).status, 503);
    const conv = randomUUID();
    const r = await cc("/internal/cc/sandbox", { owner: A, conversation: conv });
    assert.equal(r.status, 200);
    await web.app.drain(1000);
    assert.equal(web.app.active, 0);
    await cc("/internal/cc/release", { conversation: conv });
  } finally {
    web.app.draining = false;
  }
});
