// The web role end to end on fakes: real Postgres, the real jarvis-bash and sandbox agent, a fake claw, a fake S3,
// a fake Kubernetes API and a fake egress/tools endpoint.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
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
import { RESET_NOTE } from "../src/web/turns.ts";
import { AUD, fakeAccess, TEAM } from "./fakes/access.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { fakeS3 } from "./fakes/s3.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const B = "bob@example.org";
const STRANGER = "eve@example.org";
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

let pg: Awaited<ReturnType<typeof freshDb>>;
let s3: Awaited<ReturnType<typeof fakeS3>>;
let access: Awaited<ReturnType<typeof fakeAccess>>;
let k8s: FakeK8s;
let web: WebServer;
let deps: WebDeps;
let base: string;
let upstream: { close(): Promise<void>; titleCalls: number; port: number };

async function fakeUpstream(): Promise<typeof upstream> {
  const state = { titleCalls: 0 };
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    if (req.url === "/mcp") {
      res.writeHead(200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { tools: [{ name: "web_search" }, { name: "news_search" }] } }));
    }
    if (req.url === "/x/model/v1/messages") {
      state.titleCalls++;
      res.writeHead(200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ content: [{ type: "text", text: "「A test title」" }] }));
    }
    res.writeHead(404).end();
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as { port: number }).port;
  return Object.assign(state, { port, close: () => new Promise<void>((r) => server.close(() => r())) }) as typeof upstream;
}

before(async () => {
  pg = await freshDb("web");
  s3 = await fakeS3();
  access = await fakeAccess();
  k8s = new FakeK8s();
  upstream = await fakeUpstream();
  const work = mkdtempSync(join(tmpdir(), "chat-web-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant.");
  const config = parseConfig({
    product: { name: "YYC AI", locale: "zh-HK" },
    publicUrl: "https://ai.example.org",
    access: { teamDomain: TEAM, audience: AUD },
    users: [A, B],
    admins: [A],
    models: [{ id: "anthropic/claude-opus-5.5", label: "Opus" }],
    defaultModel: "anthropic/claude-opus-5.5",
    titleModel: "anthropic/claude-haiku-5.5",
    systemPromptFile: promptFile,
    sandbox: { image: "sandbox:test", maxActive: 2 },
    s3: { endpoint: s3.url, region: "fsn1", bucket: "b", prefix: "jarvis/" },
  });
  deps = {
    config,
    db: pg.db,
    dbUrl: pg.url,
    listener: new Listener(pg.url),
    blobs: new S3BlobStore({ ...config.s3, accessKeyId: "k", secretAccessKey: "s" }),
    k8s,
    verifier: new AccessVerifier({ teamDomain: TEAM, audience: AUD, jwks: access.jwks }),
    replica: "web_1",
    namespace: "jarvis",
    workRoot: join(work, "work"),
    clawPath: here("./fakes/claw.mjs"),
    bashPath: here("../src/bin/jarvis-bash"),
    mcpBridgePath: here("../src/bin/jarvis-mcp-bridge"),
    egressBase: `http://127.0.0.1:${upstream.port}`,
    toolsUrl: `http://127.0.0.1:${upstream.port}/mcp`,
    uiDir: join(work, "ui"),
    brandDir: join(work, "brand"),
    drainSeconds: 5,
    agentUrlFor: k8s.agentUrlFor,
  };
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  await web.close();
  await deps.listener.close();
  await k8s.closeAll();
  await upstream.close();
  await s3.close();
  await pg.drop();
});

async function call(email: string | null, method: string, path: string, body?: unknown, headers: Record<string, string> = {}) {
  const h: Record<string, string> = { ...headers };
  if (email) h["cf-access-jwt-assertion"] = await access.token(email);
  let b: any;
  if (body instanceof Uint8Array || typeof body === "string") b = body;
  else if (body !== undefined) {
    b = JSON.stringify(body);
    h["content-type"] = "application/json";
  }
  const r = await fetch(base + path, { method, headers: h, body: b });
  const text = await r.text();
  let json: any = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: r.status, json, text, headers: r.headers };
}

/** Reads the SSE stream until `until` returns true for an event (or times out). */
async function streamUntil(email: string, conv: string, until: (e: any) => boolean, after = 0, timeoutMs = 30_000): Promise<any[]> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  const r = await fetch(`${base}/api/conversations/${conv}/stream?after=${after}`, {
    headers: { "cf-access-jwt-assertion": await access.token(email) },
    signal: ctl.signal,
  });
  assert.equal(r.status, 200);
  const events: any[] = [];
  let buf = "";
  let found = false;
  try {
    for await (const chunk of r.body as unknown as AsyncIterable<Uint8Array>) {
      buf += Buffer.from(chunk).toString("utf8");
      let i: number;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, i);
        buf = buf.slice(i + 2);
        const data = block.split("\n").find((l) => l.startsWith("data: "));
        if (!data) continue;
        const ev = JSON.parse(data.slice(6));
        events.push(ev);
        if (until(ev)) {
          found = true;
          ctl.abort();
          break;
        }
      }
      if (found) break;
    }
  } catch (e) {
    if (!ctl.signal.aborted) throw e;
  } finally {
    clearTimeout(timer);
  }
  if (found) return events;
  throw new Error(`stream ended without the awaited event; got ${events.map((e) => e.type).join(",")}`);
}

const turnEnd = (turnId: string) => (e: any) => e.type === "turn_end" && e.turnId === turnId;

async function newConversation(email = A): Promise<string> {
  const r = await call(email, "POST", "/api/conversations", {});
  assert.equal(r.status, 201);
  return r.json.id;
}

async function runTurn(conv: string, text: string, attachments: string[] = [], email = A): Promise<{ turnId: string; events: any[] }> {
  const r = await call(email, "POST", `/api/conversations/${conv}/messages`, { text, attachments });
  assert.equal(r.status, 202, r.text);
  const events = await streamUntil(email, conv, turnEnd(r.json.turnId));
  return { turnId: r.json.turnId, events: events.filter((e) => e.turnId === r.json.turnId) };
}

test("Access and the allowlist", async () => {
  assert.equal((await call(null, "GET", "/api/me")).status, 401);
  const wrongAud = await fetch(`${base}/api/me`, { headers: { "cf-access-jwt-assertion": await access.token(A, { aud: "other" }) } });
  assert.equal(wrongAud.status, 401);
  const stranger = await call(STRANGER, "GET", "/api/me");
  assert.equal(stranger.status, 403);
  const page = await call(STRANGER, "GET", "/");
  assert.equal(page.status, 403);
  assert.match(page.text, /not on the list/);
  assert.match(page.text, /eve@example\.org/);
  const me = await call(A, "GET", "/api/me");
  assert.equal(me.status, 200);
  assert.equal(me.json.isAdmin, true);
  assert.equal(me.json.product.name, "YYC AI");
  assert.equal(me.json.defaultModel, "anthropic/claude-opus-5.5");
  assert.equal((await call(B, "GET", "/api/me")).json.isAdmin, false);
  assert.equal((await call(null, "GET", "/healthz")).status, 200);
  // cross-origin writes are refused
  assert.equal((await call(A, "POST", "/api/conversations", {}, { origin: "https://evil.example" })).status, 403);
  // admin views are invisible to others
  assert.equal((await call(B, "GET", "/api/admin/usage")).status, 404);
  assert.equal((await call(A, "GET", "/api/admin/usage")).status, 200);
});

test("a full turn: upload, warm, bash in the sandbox, streaming, files back, title, session saved, resume", async () => {
  const conv = await newConversation();
  const up = await call(A, "POST", `/api/conversations/${conv}/files?name=${encodeURIComponent("報告 data.csv")}`, "a,b\n1,2\n");
  assert.equal(up.status, 201, up.text);
  assert.equal(up.json.path, "uploads/報告 data.csv");

  assert.equal((await call(A, "POST", `/api/conversations/${conv}/warm`)).status, 204);
  for (let i = 0; i < 100; i++) {
    const { rows } = await pg.db.query("select status from sandboxes where conversation_id = $1", [conv]);
    if (rows[0]?.status === "ready") break;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.equal(k8s.created.length >= 1, true);
  const pod = k8s.created.at(-1);
  assert.equal(pod.spec.runtimeClassName, "gvisor");
  assert.equal(pod.spec.automountServiceAccountToken, false);
  assert.equal(pod.spec.securityContext.runAsNonRoot, true);
  const env = Object.fromEntries(pod.spec.containers[0].env.map((e: any) => [e.name, e.value]));
  assert.match(env.HTTPS_PROXY, /^http:\/\/sbx:[^@]+@egress\.jarvis\.svc\.cluster\.local:3128$/);
  assert.match(env.PIP_INDEX_URL, /\/mirror\/pypi\/simple$/);
  assert.equal(Object.keys(env).some((k) => /OPENROUTER|S3_|DATABASE/.test(k)), false, "no secrets in a sandbox");

  const { turnId, events } = await runTurn(conv, "please BASH:cat 'uploads/報告 data.csv' | wc -l > lines.txt; echo made", ["uploads/報告 data.csv"]);
  const types = events.map((e) => e.type);
  assert.deepEqual(types.filter((t) => ["turn_start", "tool_use", "tool_result", "turn_end"].includes(t)), ["turn_start", "tool_use", "tool_result", "turn_end"]);
  const result = events.find((e) => e.type === "tool_result");
  assert.equal(result.data.is_error, false, JSON.stringify(result.data));
  assert.equal(JSON.parse(result.data.output).stdout.trim(), "made");
  assert.equal(events.at(-1).data.status, "done");
  // text deltas are batched: "Hello " + "turn 1" arrive as one event
  assert.equal(events.filter((e) => e.type === "text_delta")[0].data.text, "Hello turn 1");

  const dbg = events.find((e) => e.type === "debug").data;
  assert.equal(dbg.env.CLAW_EVENT_STREAM, "1");
  assert.equal(dbg.env.CLAW_SYSTEM_PROMPT_FILE, deps.config.systemPromptFile);
  assert.equal(dbg.env.ANTHROPIC_BASE_URL, `${deps.egressBase}/x/model`);
  assert.equal(dbg.env.CLAW_RESUME_SESSION, undefined);
  assert.ok(dbg.args.includes("danger-full-access"));
  const allowed = dbg.args[dbg.args.indexOf("--allowedTools") + 1];
  assert.equal(allowed, "read,write,edit,glob,grep,bash,mcp__tools__web_search,mcp__tools__news_search");
  assert.match(dbg.prompt, /Attached files, in the workspace: uploads\/報告 data\.csv/);
  const settings = JSON.parse(readFileSync(join(dbg.env.CLAW_CONFIG_DIR, "settings.json"), "utf8"));
  assert.equal(settings.mcpServers.tools.env.JARVIS_TURN_TOKEN, dbg.env.ANTHROPIC_AUTH_TOKEN);

  const files = await call(A, "GET", `/api/conversations/${conv}/files`);
  const byPath = Object.fromEntries(files.json.map((f: any) => [f.path, f]));
  assert.equal(byPath["lines.txt"].source, "agent");
  assert.equal(byPath["uploads/報告 data.csv"].source, "upload");
  const dl = await call(A, "GET", `/api/conversations/${conv}/files/lines.txt`);
  assert.equal(dl.text.trim(), "2");
  assert.match(dl.headers.get("content-disposition")!, /attachment/);

  const got = await call(A, "GET", `/api/conversations/${conv}`);
  assert.equal(got.json.title, "A test title");
  assert.deepEqual(got.json.messages.map((m: any) => m.role), ["user", "assistant"]);
  assert.equal(got.json.messages[1].content, "Hello turn 1 after bash");
  assert.equal(got.json.turns[0].status, "done");
  assert.equal(got.json.turns[0].id, turnId);
  const { rows: sess } = await pg.db.query("select content from claw_sessions where conversation_id = $1", [conv]);
  assert.equal(sess.length, 1);

  // second turn: claw resumes the saved session; the sandbox is the same one, so no reset note
  const second = await runTurn(conv, "again WRITE:notes/a.md=hi");
  const dbg2 = second.events.find((e) => e.type === "debug").data;
  assert.ok(dbg2.env.CLAW_RESUME_SESSION);
  assert.equal(second.events.filter((e) => e.type === "text_delta")[0].data.text, "Hello turn 2");
  assert.ok(!dbg2.prompt.includes(RESET_NOTE));
  const files2 = await call(A, "GET", `/api/conversations/${conv}/files`);
  assert.ok(files2.json.some((f: any) => f.path === "notes/a.md"));
  assert.equal(upstream.titleCalls, 1, "the title is made once");

  // catch-up: a stream that starts after the first event replays the rest
  const first = second.events[0].id;
  const replay = await streamUntil(A, conv, turnEnd(second.turnId), first);
  assert.equal(replay[0].id > first, true);
});

test("conversations are private to their owner", async () => {
  const conv = await newConversation(A);
  assert.equal((await call(B, "GET", `/api/conversations/${conv}`)).status, 404);
  assert.equal((await call(B, "GET", `/api/conversations/${conv}/stream`)).status, 404);
  assert.equal((await call(B, "POST", `/api/conversations/${conv}/messages`, { text: "hi" })).status, 404);
  assert.equal((await call(B, "POST", `/api/conversations/${conv}/warm`)).status, 404);
  assert.equal((await call(B, "GET", `/api/conversations/${conv}/files`)).status, 404);
  assert.equal((await call(B, "PATCH", `/api/conversations/${conv}`, { title: "x" })).status, 404);
  assert.equal((await call(B, "DELETE", `/api/conversations/${conv}`)).status, 404);
  assert.ok(!(await call(B, "GET", "/api/conversations")).json.some((c: any) => c.id === conv));
  assert.ok((await call(A, "GET", "/api/conversations")).json.some((c: any) => c.id === conv));
  assert.equal((await call(A, "PATCH", `/api/conversations/${conv}`, { title: "  Mine  " })).json.title, "Mine");
  assert.equal((await call(A, "DELETE", `/api/conversations/${conv}`)).status, 204);
  assert.equal((await call(A, "GET", `/api/conversations/${conv}`)).status, 404);
});

test("cancel, one turn at a time, and a failing turn", async () => {
  const conv = await newConversation();
  const r = await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "SLEEP" });
  assert.equal(r.status, 202);
  await streamUntil(A, conv, (e) => e.type === "debug");
  assert.equal((await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "too soon" })).status, 409);
  assert.equal((await call(B, "POST", `/api/turns/${r.json.turnId}/cancel`)).status, 404);
  assert.equal((await call(A, "POST", `/api/turns/${r.json.turnId}/cancel`)).status, 202);
  const events = await streamUntil(A, conv, turnEnd(r.json.turnId));
  assert.equal(events.at(-1).data.status, "cancelled");

  const failed = await runTurn(conv, "FAIL");
  assert.equal(failed.events.at(-1).data.status, "error");
  assert.match(failed.events.at(-1).data.error, /refused/);
});

test("confirmations: the tools role asks through /internal/events, only the owner answers", async () => {
  const conv = await newConversation();
  const r = await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "SLEEP" });
  const [dbg] = (await streamUntil(A, conv, (e) => e.type === "debug")).filter((e) => e.type === "debug");
  const token = dbg.data.env.ANTHROPIC_AUTH_TOKEN;
  assert.equal((await fetch(`${base}/internal/events`, { method: "POST", headers: { authorization: "Bearer nope" }, body: "{}" })).status, 401);

  const id = crypto.randomUUID();
  await pg.db.query("insert into confirmations (id, conversation_id, turn_id, kind, summary, status) values ($1, $2, $3, 'network', 'Allow pypi.example', 'pending')", [id, conv, r.json.turnId]);
  const ev = await fetch(`${base}/internal/events`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ type: "confirmation_request", id, kind: "network", summary: "Allow pypi.example", detail: {} }),
  });
  assert.equal(ev.status, 201);
  assert.equal((await pg.db.query("select status from turns where id = $1", [r.json.turnId])).rows[0].status, "waiting");
  const pending = await call(A, "GET", `/api/conversations/${conv}`);
  assert.equal(pending.json.pendingConfirmations[0].id, id);

  const heard: string[] = [];
  const unlisten = await deps.listener.listen(`confirm_${id.replace(/-/g, "")}`, (p) => p && heard.push(p));
  assert.equal((await call(B, "POST", `/api/confirmations/${id}`, { approve: true })).status, 404);
  const ok = await call(A, "POST", `/api/confirmations/${id}`, { approve: true });
  assert.equal(ok.json.status, "approved");
  assert.equal((await call(A, "POST", `/api/confirmations/${id}`, { approve: false })).status, 409);
  await streamUntil(A, conv, (e) => e.type === "confirmation_decided");
  for (let i = 0; i < 50 && !heard.length; i++) await new Promise((res) => setTimeout(res, 20));
  assert.deepEqual(heard, ["approved"]);
  await unlisten();
  assert.equal((await pg.db.query("select status from turns where id = $1", [r.json.turnId])).rows[0].status, "running");
  await call(A, "POST", `/api/turns/${r.json.turnId}/cancel`);
  await streamUntil(A, conv, turnEnd(r.json.turnId));
});

test("sandbox release: idle release saves background output, the next turn is told about the reset, the pool frees idle sandboxes", async () => {
  const conv = await newConversation();
  await runTurn(conv, "BASH:(sleep 1; echo late > bg.txt) >/dev/null 2>&1 &");
  const { rows } = await pg.db.query("select * from sandboxes where conversation_id = $1 and status = 'ready'", [conv]);
  const sb = rows[0];
  assert.ok(sb);
  await new Promise((r) => setTimeout(r, 1500));
  await pg.db.query("update sandboxes set last_active_at = now() - interval '2 hours' where id = $1", [sb.id]);
  await janitorPass(deps, web.app.sandboxes);
  const after = (await pg.db.query("select status, release_reason from sandboxes where id = $1", [sb.id])).rows[0];
  assert.deepEqual(after, { status: "gone", release_reason: "idle" });
  assert.equal(k8s.pods.has(sb.pod), false, "the pod is deleted");
  const files = await call(A, "GET", `/api/conversations/${conv}/files`);
  assert.ok(files.json.some((f: any) => f.path === "bg.txt"), "the final save kept what the background process wrote");

  const next = await runTurn(conv, "BASH:cat bg.txt");
  const dbg = next.events.find((e) => e.type === "debug").data;
  assert.ok(dbg.prompt.startsWith(RESET_NOTE));
  assert.ok(next.events.some((e) => e.type === "sandbox" && e.data.state === "reset"));
  assert.equal(JSON.parse(next.events.find((e) => e.type === "tool_result").data.output).stdout.trim(), "late", "files are restored into the new sandbox");

  // pool of 2: two other conversations warm; the least recently used idle sandbox makes room
  const c2 = await newConversation();
  const c3 = await newConversation();
  await web.app.sandboxes.ensure(c2);
  const live = (await pg.db.query("select conversation_id from sandboxes where status <> 'gone'")).rows.map((r) => r.conversation_id);
  assert.equal(live.length, 2);
  await web.app.sandboxes.ensure(c3);
  const live2 = (await pg.db.query("select conversation_id from sandboxes where status <> 'gone' order by created_at")).rows.map((r) => r.conversation_id);
  assert.deepEqual(live2.sort(), [c2, c3].sort());
});

test("janitor: orphan pods, turns of dead replicas, unanswered confirmations", async () => {
  k8s.ghosts.set("sbx-orphan", { metadata: { name: "sbx-orphan", creationTimestamp: new Date(Date.now() - 600_000).toISOString() }, status: { phase: "Running" } });
  const conv = await newConversation();
  const turnId = crypto.randomUUID();
  await pg.db.query("insert into turns (id, conversation_id, status, token_hash, model, replica) values ($1, $2, 'running', $3, 'm', 'web_dead')", [turnId, conv, "h" + turnId]);
  const cid = crypto.randomUUID();
  await pg.db.query("insert into confirmations (id, conversation_id, turn_id, kind, summary, status, created_at) values ($1, $2, $3, 'x', 'y', 'pending', now() - interval '1 hour')", [cid, conv, turnId]);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal(k8s.ghosts.has("sbx-orphan"), false);
  assert.equal((await pg.db.query("select status from turns where id = $1", [turnId])).rows[0].status, "error");
  assert.equal((await pg.db.query("select status from confirmations where id = $1", [cid])).rows[0].status, "expired");
});

test("drain: not ready, no new turns, running turns finish or are cancelled", async () => {
  const conv = await newConversation();
  const r = await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "SLEEP" });
  await streamUntil(A, conv, (e) => e.type === "debug");
  const drained = web.app.runner.drain(500);
  assert.equal((await call(null, "GET", "/healthz")).status, 503);
  assert.equal((await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "x" })).status, 503);
  await drained;
  const t = (await pg.db.query("select status from turns where id = $1", [r.json.turnId])).rows[0];
  assert.equal(t.status, "cancelled");
  web.app.runner.draining = false;
});
