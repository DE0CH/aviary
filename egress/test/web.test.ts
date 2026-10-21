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
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
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
let upstream: { close(): Promise<void>; titleCalls: number; port: number; modelCalls: number; budgetOut: string | null };

async function fakeUpstream(): Promise<typeof upstream> {
  // budgetOut: answer model calls as egress does once the budget is used up (402 budget_exceeded with this message)
  const state = { titleCalls: 0, modelCalls: 0, budgetOut: null as string | null };
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    if (req.url === "/mcp") {
      res.writeHead(200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { tools: [{ name: "web_search" }, { name: "news_search" }] } }));
    }
    if (req.url === "/x/model/v1/messages") {
      if (state.budgetOut) {
        res.writeHead(402, { "content-type": "application/json" });
        return void res.end(JSON.stringify({ type: "error", error: { type: "budget_exceeded", message: state.budgetOut } }));
      }
      if (JSON.parse(Buffer.concat(chunks).toString() || "{}").max_tokens === 10) {
        state.modelCalls++;
        res.writeHead(200, { "content-type": "application/json" });
        return void res.end(JSON.stringify({ content: [{ type: "text", text: "ok" }] }));
      }
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
    models: [{ id: "anthropic/claude-opus-5.5", label: "Opus" }, { id: "anthropic/claude-sonnet-5.5", label: "Sonnet" }],
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
    sandboxProvider: undefined as any,
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
  };
  deps.sandboxProvider = new KubernetesProvider({ k8s, config, egressNamespace: "jarvis", agentUrlFor: k8s.agentUrlFor });
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

const turnEnd = (turnId: string) => (e: any) => e.type === "turn_finished" && e.turnId === turnId;

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
  assert.deepEqual(types.filter((t) => ["turn_started", "tool_use", "tool_result", "result", "turn_finished"].includes(t)), ["turn_started", "tool_use", "tool_result", "result", "turn_finished"]);
  assert.equal(events.find((e) => e.type === "result").data.estimated_cost, 0.001);
  assert.ok(events[0].createdAt);
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
  // a conversation with no message yet (made on the first keystroke, to warm it) is not listed
  assert.ok(!(await call(A, "GET", "/api/conversations")).json.some((c: any) => c.id === conv));
  await pg.db.query("insert into messages (conversation_id, role, content) values ($1, 'user', 'hi')", [conv]);
  assert.ok(!(await call(B, "GET", "/api/conversations")).json.some((c: any) => c.id === conv));
  assert.ok((await call(A, "GET", "/api/conversations")).json.some((c: any) => c.id === conv));
  assert.equal((await call(A, "PATCH", `/api/conversations/${conv}`, { title: "  Mine  " })).status, 204);
  assert.equal((await call(A, "GET", `/api/conversations/${conv}`)).json.title, "Mine");
  assert.equal((await call(A, "PATCH", `/api/conversations/${conv}`, { model: "nope" })).status, 400);
  assert.equal((await call(A, "PATCH", `/api/conversations/${conv}`, { model: "anthropic/claude-sonnet-5.5" })).status, 204);
  const patched = (await call(A, "GET", `/api/conversations/${conv}`)).json;
  assert.deepEqual([patched.title, patched.model], ["Mine", "anthropic/claude-sonnet-5.5"]);
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
  assert.equal((await call(A, "POST", `/api/turns/${r.json.turnId}/cancel`)).status, 204);
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
  assert.equal((await call(A, "POST", `/api/confirmations/${id}`, { approve: true })).status, 204);
  assert.equal((await pg.db.query("select status from confirmations where id = $1", [id])).rows[0].status, "approved");
  assert.equal((await call(A, "POST", `/api/confirmations/${id}`, { approve: false })).status, 409);
  const resolved = await streamUntil(A, conv, (e) => e.type === "confirmation_resolved");
  assert.deepEqual(resolved.at(-1).data, { id, status: "approved" });
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
  assert.ok(next.events.some((e) => e.type === "sandbox" && e.data.status === "reset"));
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

test("the web manifest carries the product name; static files and the SPA fallback", async () => {
  const { mkdirSync, writeFileSync } = await import("node:fs");
  mkdirSync(deps.uiDir, { recursive: true });
  writeFileSync(join(deps.uiDir, "manifest.webmanifest"), JSON.stringify({ name: "AI", short_name: "AI", start_url: "/", icons: [] }));
  writeFileSync(join(deps.uiDir, "index.html"), "<!doctype html><title>app</title>");
  const m = await call(A, "GET", "/manifest.webmanifest");
  assert.equal(m.status, 200);
  assert.deepEqual([m.json.name, m.json.short_name, m.json.start_url], ["YYC AI", "YYC AI", "/"]);
  assert.match((await call(A, "GET", "/c/123")).text, /<title>app<\/title>/);
  assert.equal((await call(A, "GET", "/assets/missing.js")).status, 404);
  assert.equal((await call(STRANGER, "GET", "/manifest.webmanifest")).status, 403);
});

test("the stream resumes from the greater of ?after and Last-Event-ID", async () => {
  const conv = await newConversation();
  const { events } = await runTurn(conv, "hello");
  const ids = events.map((e) => e.id);
  const mid = ids[Math.floor(ids.length / 2)]!;
  const read = async (after: number, lastEventId: number) => {
    const ctl = new AbortController();
    const r = await fetch(`${base}/api/conversations/${conv}/stream?after=${after}`, {
      headers: { "cf-access-jwt-assertion": await access.token(A), "last-event-id": String(lastEventId) },
      signal: ctl.signal,
    });
    let buf = "";
    for await (const chunk of r.body as unknown as AsyncIterable<Uint8Array>) {
      buf += Buffer.from(chunk).toString("utf8");
      if (buf.includes(`id: ${ids.at(-1)}\n`)) break;
    }
    ctl.abort();
    return [...buf.matchAll(/^id: (\d+)$/gm)].map((m) => Number(m[1]));
  };
  assert.equal((await read(mid, 0))[0], ids[ids.indexOf(mid) + 1]);
  assert.equal((await read(0, mid))[0], ids[ids.indexOf(mid) + 1]);
  assert.equal((await read(ids[0]!, mid))[0], ids[ids.indexOf(mid) + 1]);
});

test("admin monitor: admins only (404 for staff: API, page and bundle), a snapshot of every section", async () => {
  const { mkdirSync, writeFileSync } = await import("node:fs");
  mkdirSync(deps.uiDir, { recursive: true });
  writeFileSync(join(deps.uiDir, "admin.html"), "<!doctype html><title>Jarvis 3 monitor</title>");
  writeFileSync(join(deps.uiDir, "admin-abc.js"), "/* admin */");
  for (const p of ["/api/admin/monitor", "/admin/monitor", "/admin", "/admin-abc.js"]) {
    const r = await call(B, "GET", p);
    assert.equal(r.status, 404, p);
    assert.doesNotMatch(r.text, /Jarvis/);
    assert.equal((await call(null, "GET", p)).status, 401, p);
    assert.equal((await call(STRANGER, "GET", p)).status, 403, p);
  }
  const page = await call(A, "GET", "/admin/monitor");
  assert.equal(page.status, 200);
  assert.match(page.text, /Jarvis 3 monitor/);
  assert.equal((await call(A, "GET", "/admin-abc.js")).status, 200);

  const conv = await newConversation();
  await runTurn(conv, "hello");
  await web.app.sandboxes.ensure(conv);
  await pg.db.query(
    "insert into egress_log (conversation_id, kind, host, decision) values ($1, 'connect', 'evil.example', 'deny:not on the allowlist'), ($1, 'mirror', 'mirror', 'allow:cache'), ($1, 'mirror', 'mirror', 'allow')",
    [conv],
  );
  await pg.db.query("insert into tool_calls (conversation_id, user_email, tool, ok, ms) values ($1, $2, 'web_search', true, 100), ($1, $2, 'web_search', false, 300)", [conv, A]);
  // presence of another role, with an error line
  await pg.db.query("insert into kv (key, value) values ('presence:egress:egress-1', $1) on conflict (key) do update set value = excluded.value", [
    JSON.stringify({ role: "egress", replica: "egress-1", commit: "abc", startedAt: new Date().toISOString(), at: new Date().toISOString(), errors: [{ t: new Date().toISOString(), at: "x", error: "boom" }] }),
  ]);
  const r = await call(A, "GET", "/api/admin/monitor?tz=Asia/Hong_Kong");
  assert.equal(r.status, 200, r.text);
  const d = r.json;
  assert.deepEqual(d.errors, {});
  assert.equal(d.tz, "Asia/Hong_Kong");
  assert.equal(d.provider, "kubernetes");
  const sbx = d.sandboxes.list.find((s: any) => s.conversationId === conv);
  assert.ok(sbx, "the live sandbox is listed");
  assert.equal(sbx.owner, A);
  assert.equal(sbx.provider?.state, "Running");
  assert.deepEqual([sbx.provider.host, sbx.provider.cpuUsed, sbx.provider.cpuLimit, sbx.provider.memUsed], ["node-1", 0.25, 1, 100 * 1024 ** 2]);
  assert.deepEqual(d.hosts.list.map((h: any) => [h.name, h.cpus, h.cpuUsed, h.memTotal, h.reachable]), [["node-1", 4, 1.5, 8 * 1024 ** 3, true]]);
  assert.ok(d.hosts.list[0].sandboxes >= 1);
  assert.ok(d.hosts.notes.some((n: string) => /runtime class: .*404/.test(n)));
  assert.ok(d.sandboxes.starts24h.ok >= 1 && d.sandboxes.starts24h.p50Ms != null);
  assert.equal(d.sandboxes.pool.max, 2);
  assert.ok(d.load.perHour.length === 24 && d.load.turnsToday >= 1 && d.load.usersNow >= 1);
  assert.ok(d.models.byModel.some((m: any) => m.model === "anthropic/claude-opus-5.5" && m.in7d >= 10));
  const ws = d.tools.byTool.find((t: any) => t.tool === "web_search");
  assert.deepEqual([ws.service, ws.calls, ws.errors], ["Exa", 2, 1]);
  assert.equal(d.tools.pendingConfirmations, undefined);
  assert.deepEqual(d.egress.deniedTop[0], { ...d.egress.deniedTop[0], host: "evil.example", why: "not on the allowlist", n: 1 });
  assert.equal(d.egress.mirror.rate, 0.5);
  assert.equal(d.egress.perMinute.length, 60);
  const roles = Object.fromEntries(d.health.roles.map((x: any) => [x.role, x]));
  assert.ok(roles.web.replicas.some((x: any) => x.replica === "web_1" && x.up));
  assert.equal(roles.egress.replicas[0].commit, "abc");
  assert.ok(d.health.errorLines.some((e: any) => e.error === "boom" && e.role === "egress"));
  assert.ok(d.health.postgres.ms >= 0 && d.health.postgres.total >= 1);
  assert.equal(d.health.s3.ok, true);
  assert.equal(d.health.leaders[0].name, "janitor");
  // no message contents anywhere in the snapshot
  assert.doesNotMatch(r.text, /hello/);
});

test("models: only configured ids; a conversation on a model no longer offered continues on the default", async () => {
  assert.equal((await call(A, "POST", "/api/conversations", { model: "anthropic/claude-opus-4" })).status, 400);
  const conv = await newConversation();
  assert.equal((await call(A, "PATCH", `/api/conversations/${conv}`, { model: "openai/gpt-x" })).status, 400);
  await pg.db.query("update conversations set model = 'anthropic/claude-opus-4' where id = $1", [conv]);
  assert.equal((await call(A, "GET", `/api/conversations/${conv}`)).json.model, deps.config.defaultModel);
  const { events } = await runTurn(conv, "hello");
  const dbg = events.find((e) => e.type === "debug").data;
  assert.equal(dbg.args[dbg.args.indexOf("--model") + 1], deps.config.defaultModel);
  const { rows } = await pg.db.query("select model from conversations where id = $1", [conv]);
  assert.equal(rows[0].model, deps.config.defaultModel);
});

test("budget: a used-up budget refuses the turn before claw runs; a running turn stops at its next model call; admin reset/raise", async () => {
  await pg.db.query("delete from egress_log; delete from kv where key = 'budget:override'");
  const since = new Date(Date.now() - 60_000).toISOString();
  deps.config.budget = { limitUsd: 1, since, ownerName: "Deyao" };
  try {
    // staff never see the budget API
    assert.equal((await call(B, "GET", "/api/admin/budget")).status, 404);
    assert.equal((await call(B, "POST", "/api/admin/budget", { action: "reset" })).status, 404);
    const st = await call(A, "GET", "/api/admin/budget");
    assert.equal(st.status, 200);
    assert.deepEqual([st.json.budget.spentUsd, st.json.budget.limitUsd, st.json.budget.remainingUsd, st.json.budget.exhausted], [0, 1, 1, false]);

    // used up: the turn is refused at once, with the message, and claw never runs
    await pg.db.query("insert into egress_log (kind, host, decision, cost_usd) values ('model', 'openrouter.ai', 'allow', 1.2)");
    const conv = await newConversation(B);
    const refused = await runTurn(conv, "hello", [], B);
    assert.ok(!refused.events.some((e) => e.type === "debug"), "no claw process");
    const fin = refused.events.at(-1);
    assert.equal(fin.data.status, "error");
    assert.equal(fin.data.code, "budget");
    assert.equal(fin.data.error, "Deyao 設定了 1 美元的使用上限，目前已用完。");
    assert.deepEqual([fin.data.limitUsd, fin.data.ownerName], [1, "Deyao"]);
    const got = (await call(B, "GET", `/api/conversations/${conv}`)).json;
    assert.equal(got.turns.at(-1).status, "error");
    assert.equal(got.turns.at(-1).error, "Deyao 設定了 1 美元的使用上限，目前已用完。");

    // the admin raises the limit: turns run again (audit row written)
    const raised = await call(A, "POST", "/api/admin/budget", { action: "set", limitUsd: 5 });
    assert.equal(raised.status, 200, raised.text);
    assert.deepEqual([raised.json.budget.limitUsd, raised.json.budget.source, raised.json.budget.exhausted], [5, "override", false]);
    assert.equal(raised.json.audit[0].action, "set");
    assert.equal(raised.json.audit[0].actor, A);
    assert.equal((await call(A, "POST", "/api/admin/budget", { action: "set", limitUsd: "lots" })).status, 400);
    assert.equal((await call(A, "POST", "/api/admin/budget", { action: "nope" })).status, 400);

    // a running turn crosses the limit: egress refuses its next model call (402 budget_exceeded), the turn ends with
    // the budget message and keeps what it wrote so far
    upstream.budgetOut = "Deyao 設定了 5 美元的使用上限，目前已用完。";
    const stopped = await runTurn(conv, "MODELCALL please", [], B);
    const err = stopped.events.find((e) => e.type === "error");
    assert.deepEqual([err.data.code, err.data.message, err.data.limitUsd], ["budget", "Deyao 設定了 5 美元的使用上限，目前已用完。", 5]);
    const end = stopped.events.at(-1);
    assert.deepEqual([end.data.status, end.data.code, end.data.error], ["error", "budget", "Deyao 設定了 5 美元的使用上限，目前已用完。"]);
    assert.ok(stopped.events.some((e) => e.type === "text_delta"), "the words before the refused call stay");
    upstream.budgetOut = null;
    const calls = upstream.modelCalls;
    const fine = await runTurn(conv, "MODELCALL again", [], B);
    assert.equal(fine.events.at(-1).data.status, "done");
    assert.equal(upstream.modelCalls, calls + 1);

    // reset: spend counts from now
    const reset = await call(A, "POST", "/api/admin/budget", { action: "reset" });
    assert.deepEqual([reset.json.budget.spentUsd, reset.json.budget.limitUsd], [0, 5]);
    // back to config: used up again (the 1.2 spent counts from config's since)
    const cleared = await call(A, "POST", "/api/admin/budget", { action: "clear" });
    assert.deepEqual([cleared.json.budget.source, cleared.json.budget.exhausted], ["config", true]);
    assert.deepEqual(cleared.json.audit.slice(0, 3).map((a: any) => a.action), ["clear", "reset", "set"]);
  } finally {
    deps.config.budget = undefined;
    upstream.budgetOut = null;
    await pg.db.query("delete from kv where key = 'budget:override'");
  }
});
