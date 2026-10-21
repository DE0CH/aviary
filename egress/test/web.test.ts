// The web role end to end on fakes: real Postgres, the real jarvis-bash and sandbox agent, a fake claw, a fake S3,
// a fake Kubernetes API and a fake egress/tools endpoint.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { truncate } from "node:fs/promises";
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
import { FILES_NOTE, imageAttachments, MAX_PROMPT_IMAGES, RESET_NOTE, SANDBOX_NETWORK_NOTE, sandboxInstallNote } from "../src/web/turns.ts";
import { createToolsServer } from "../src/tools/server.ts";
import { legacyPresented } from "../src/web/app.ts";
import { mergePresented } from "../src/common/presented.ts";
import { AUD, fakeAccess, TEAM } from "./fakes/access.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
import { fakeS3 } from "./fakes/s3.ts";
import { freshDb } from "./pg.ts";
import { png } from "./fakes/png.ts";

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
let realToolsUrl = "";
let realTools: { server: import("node:http").Server } | null = null;
let upstream: { close(): Promise<void>; titleCalls: number; port: number; modelCalls: number; budgetOut: string | null };

async function fakeUpstream(): Promise<typeof upstream> {
  // budgetOut: answer model calls as egress does once the budget is used up (402 budget_exceeded with this message)
  const state = { titleCalls: 0, modelCalls: 0, budgetOut: null as string | null };
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    if (req.url === "/mcp") {
      // tools/call goes to a real tools server (present_files checks and stores for real); the list stays fixed
      const msg = JSON.parse(Buffer.concat(chunks).toString() || "{}");
      if (msg.method === "tools/call" && realToolsUrl) {
        const r = await fetch(realToolsUrl, { method: "POST", headers: { authorization: String(req.headers.authorization ?? ""), "content-type": "application/json" }, body: JSON.stringify(msg) });
        res.writeHead(r.status, { "content-type": "application/json" });
        return void res.end(await r.text());
      }
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
      // titles never think: a thinking-by-default model spent the whole budget on thinking and gave no title
      const titleBody = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      assert.deepEqual(titleBody.thinking, { type: "disabled" });
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
  realTools = createToolsServer({ config, db: pg.db, env: {}, fetch: globalThis.fetch }, []);
  await new Promise<void>((r) => realTools!.server.listen(0, "127.0.0.1", r));
  realToolsUrl = `http://127.0.0.1:${(realTools.server.address() as { port: number }).port}/mcp`;
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  realTools?.server.close();
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
  assert.equal(me.json.firstName, null, "no identity name: no name in the greeting (never the address)");
  assert.equal((await call(B, "GET", "/api/me")).json.isAdmin, false);
  assert.equal((await call(null, "GET", "/healthz")).status, 200);
  // cross-origin writes are refused
  assert.equal((await call(A, "POST", "/api/conversations", {}, { origin: "https://evil.example" })).status, 403);
  // admin views are invisible to others
  assert.equal((await call(B, "GET", "/api/admin/usage")).status, 404);
  assert.equal((await call(A, "GET", "/api/admin/usage")).status, 200);
});

test("a full turn: upload, typing, a lazy sandbox at the first bash, streaming, files back, title, session saved, resume", async () => {
  const conv = await newConversation();
  const up = await call(A, "POST", `/api/conversations/${conv}/files?name=${encodeURIComponent("報告 data.csv")}`, "a,b\n1,2\n");
  assert.equal(up.status, 201, up.text);
  assert.equal(up.json.path, "uploads/報告 data.csv");

  // typing makes no sandbox: it only tells the scaler someone may need one soon
  const made = k8s.created.length;
  assert.equal((await call(A, "POST", "/api/typing", { conversationId: conv })).status, 204);
  assert.equal((await pg.db.query("select 1 from sandboxes where conversation_id = $1", [conv])).rows.length, 0);
  assert.equal(k8s.created.length, made);
  assert.equal((await pg.db.query("select 1 from kv where key = $1", [`sandbox_demand:${A}`])).rows.length, 1);

  // a turn without bash never gets a sandbox
  const other = await newConversation();
  const plain = await runTurn(other, "just talk");
  assert.equal(plain.events.at(-1).data.status, "done");
  assert.ok(!plain.events.some((e) => e.type === "sandbox"));
  assert.equal((await pg.db.query("select 1 from sandboxes where conversation_id = $1", [other])).rows.length, 0);
  assert.equal(k8s.created.length, made);
  const titles = upstream.titleCalls;

  const { turnId, events } = await runTurn(conv, "please BASH:cat 'uploads/報告 data.csv' | wc -l > lines.txt; echo made", ["uploads/報告 data.csv"]);
  // the first bash command made the sandbox
  assert.deepEqual(events.filter((e) => e.type === "sandbox").map((e) => e.data.status), ["starting", "ready"]);
  assert.equal(k8s.created.length, made + 1);
  const pod = k8s.created.at(-1);
  assert.equal(pod.spec.runtimeClassName, "gvisor");
  assert.equal(pod.spec.automountServiceAccountToken, false);
  assert.equal(pod.spec.securityContext.runAsNonRoot, true);
  const env = Object.fromEntries(pod.spec.containers[0].env.map((e: any) => [e.name, e.value]));
  assert.match(env.HTTPS_PROXY, /^http:\/\/sbx:[^@]+@egress\.jarvis\.svc\.cluster\.local:3128$/);
  assert.match(env.PIP_INDEX_URL, /\/mirror\/pypi\/simple$/);
  assert.equal(Object.keys(env).some((k) => /OPENROUTER|S3_|DATABASE/.test(k)), false, "no secrets in a sandbox");
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
  // the instance's prompt, then the runtime's note on the sandbox's internet access
  assert.ok(dbg.systemPrompt.startsWith(readFileSync(deps.config.systemPromptFile, "utf8").trimEnd()), dbg.systemPrompt);
  // ... then how files reach the user (present_files only) and that images can be seen
  assert.ok(dbg.systemPrompt.includes(SANDBOX_NETWORK_NOTE));
  assert.ok(dbg.systemPrompt.endsWith(FILES_NOTE));
  assert.match(dbg.systemPrompt, /call present_files with the\ndeliverables only \(reports, charts, data the user asked for\) — never scripts, fonts or intermediate files/);
  assert.equal(dbg.env.CLAW_IMAGES, "1");
  assert.equal(dbg.env.CLAW_PROMPT_IMAGES, undefined, "a CSV is not an image");
  // between them, how to install software (the test config's provider is kubernetes: no apt)
  assert.ok(dbg.systemPrompt.includes(`${SANDBOX_NETWORK_NOTE}\n${sandboxInstallNote("kubernetes", false)}\n${FILES_NOTE}`), dbg.systemPrompt);
  assert.match(dbg.systemPrompt, /no one can enable a site from the chat/);
  assert.equal(dbg.env.ANTHROPIC_BASE_URL, `${deps.egressBase}/x/model`);
  assert.equal(dbg.env.CLAW_RESUME_SESSION, undefined);
  assert.ok(dbg.args.includes("danger-full-access"));
  const allowed = dbg.args[dbg.args.indexOf("--allowedTools") + 1];
  assert.equal(allowed, "read,write,edit,glob,grep,bash,mcp__tools__web_search,mcp__tools__news_search");
  assert.match(dbg.prompt, /Attached files, in the workspace: uploads\/報告 data\.csv/);
  const settings = JSON.parse(readFileSync(join(dbg.env.CLAW_CONFIG_DIR, "settings.json"), "utf8"));
  assert.equal(settings.mcpServers.tools.env.JARVIS_TURN_TOKEN, dbg.env.ANTHROPIC_AUTH_TOKEN);
  assert.equal(settings.mcpServers.tools.env.JARVIS_WORKDIR, dbg.cwd);

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
  assert.equal(upstream.titleCalls, titles + 1, "the title is made once");

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
  // another person's conversation id in a typing signal is ignored (no sandbox kept, nothing revealed)
  assert.equal((await call(B, "POST", "/api/typing", { conversationId: conv })).status, 204);
  assert.equal((await call(B, "GET", `/api/conversations/${conv}/files`)).status, 404);
  assert.equal((await call(B, "PATCH", `/api/conversations/${conv}`, { title: "x" })).status, 404);
  assert.equal((await call(B, "DELETE", `/api/conversations/${conv}`)).status, 404);
  // a conversation with no message yet is not listed
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

test("no confirmations: the endpoints are gone; a stored conversation with old confirmation events still loads", async () => {
  const conv = await newConversation();
  const r = await runTurn(conv, "hello");
  assert.equal(r.events.at(-1).data.status, "done");
  const turnId = r.turnId;
  const id = crypto.randomUUID();
  // what an older version stored: a request and its answer
  await pg.db.query("insert into events (conversation_id, turn_id, seq, type, data) values ($1, $2, 1000, 'confirmation_request', $3), ($1, $2, 1001, 'confirmation_resolved', $4)", [
    conv, turnId, JSON.stringify({ id, kind: "network_access", summary: "Allow data.gov.hk?", detail: { host: "data.gov.hk", reason: "x" } }), JSON.stringify({ id, status: "approved" }),
  ]);
  const got = await call(A, "GET", `/api/conversations/${conv}`);
  assert.equal(got.status, 200);
  assert.equal(got.json.pendingConfirmations, undefined);
  assert.ok(got.json.turns[0].events.some((e: any) => e.type === "confirmation_request"));
  assert.equal((await call(A, "POST", `/api/confirmations/${id}`, { approve: true })).status, 404);
  const internal = await fetch(`${base}/internal/events`, { method: "POST", headers: { authorization: "Bearer x", "content-type": "application/json" }, body: "{}" });
  assert.notEqual(internal.status, 201);
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
  assert.ok(next.events.some((e) => e.type === "sandbox" && e.data.status === "reset"));
  // the model hears of the reset with the first command's output in the new sandbox
  const out = JSON.parse(next.events.find((e) => e.type === "tool_result").data.output).stdout;
  assert.ok(out.startsWith(RESET_NOTE), out);
  assert.equal(out.slice(RESET_NOTE.length).trim(), "late", "files are restored into the new sandbox");

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

test("janitor: orphan pods, turns of dead replicas", async () => {
  k8s.ghosts.set("sbx-orphan", { metadata: { name: "sbx-orphan", creationTimestamp: new Date(Date.now() - 600_000).toISOString() }, status: { phase: "Running" } });
  const conv = await newConversation();
  const turnId = crypto.randomUUID();
  await pg.db.query("insert into turns (id, conversation_id, status, token_hash, model, replica) values ($1, $2, 'running', $3, 'm', 'web_dead')", [turnId, conv, "h" + turnId]);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal(k8s.ghosts.has("sbx-orphan"), false);
  assert.equal((await pg.db.query("select status from turns where id = $1", [turnId])).rows[0].status, "error");
});

test("janitor: sandboxes of deleted and never-used conversations are released; DELETE releases at once", async () => {
  // a leftover sandbox in a conversation that never had a turn (an older replica warmed on typing)
  const unused = await newConversation();
  await web.app.sandboxes.ensure(unused);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal((await pg.db.query("select status from sandboxes where conversation_id = $1", [unused])).rows[0].status, "ready", "young: kept");
  await pg.db.query("update sandboxes set created_at = now() - interval '6 minutes' where conversation_id = $1", [unused]);
  await janitorPass(deps, web.app.sandboxes);
  assert.deepEqual((await pg.db.query("select status, release_reason from sandboxes where conversation_id = $1", [unused])).rows[0], { status: "gone", release_reason: "unused" });

  // a conversation deleted while its replica died before releasing
  const deleted = await newConversation();
  await runTurn(deleted, "BASH:echo hi");
  await pg.db.query("update conversations set deleted_at = now() where id = $1", [deleted]);
  await janitorPass(deps, web.app.sandboxes);
  assert.deepEqual((await pg.db.query("select status, release_reason from sandboxes where conversation_id = $1", [deleted])).rows[0], { status: "gone", release_reason: "deleted" });

  // DELETE releases its sandbox right away
  const del = await newConversation();
  await runTurn(del, "BASH:echo hi");
  assert.equal((await call(A, "DELETE", `/api/conversations/${del}`)).status, 204);
  for (let i = 0; i < 50; i++) {
    if ((await pg.db.query("select 1 from sandboxes where conversation_id = $1 and status <> 'gone'", [del])).rows.length === 0) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.equal((await pg.db.query("select 1 from sandboxes where conversation_id = $1 and status <> 'gone'", [del])).rows.length, 0);
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

const sha = (b: string | Uint8Array) => createHash("sha256").update(b).digest("hex");

const MINI_PDF = "%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";

test("present_files: only presented files are shown, checked in the workspace, copies once, the latest version wins, survives reload", async () => {
  const conv = await newConversation();
  // a refused call: nothing is shown, every bad path is named
  const bad = JSON.stringify([{ path: "missing.pdf" }, { path: "../etc/passwd" }, { path: "/etc/passwd" }, { path: "node_modules/x.js" }]);
  const t0 = await runTurn(conv, `WRITE:node_modules/x.js=1 PRESENT:${bad}`);
  const r0 = t0.events.find((e) => e.type === "tool_result");
  assert.equal(r0.data.is_error, true);
  for (const p of ["missing.pdf: no such file", "../etc/passwd: not a path inside", "/etc/passwd: not a path inside", "node_modules/x.js: files in that folder are not kept"]) assert.ok(r0.data.output.includes(p), r0.data.output);
  assert.ok(!t0.events.some((e) => e.type === "files_presented"));
  const got0 = await call(A, "GET", `/api/conversations/${conv}`);
  assert.deepEqual(got0.json.turns[0].presented, [], "a new-style turn that presents nothing shows no cards");

  // helper files and a copy of the PDF (the duplicate bug: build/r.pdf and r.pdf); the sandbox path /work/x is accepted
  const files = JSON.stringify([{ path: "build/r.pdf" }, { path: "/work/r.pdf", caption: "The report" }, { path: "./chart.png" }]);
  const t1 = await runTurn(conv, `WRITE:build/r.pdf=v1 WRITE:r.pdf=v1 WRITE:build/fonts.ttf=f WRITE:build/main.py=p WRITE:chart.png=c PRESENT:${files} AFTER:r.pdf=v22`);
  const res = t1.events.find((e) => e.type === "tool_result");
  assert.equal(res.data.is_error, false, res.data.output);
  assert.match(res.data.output, /Shown to the user under your answer: build\/r\.pdf, r\.pdf, chart\.png/);
  const shown = t1.events.filter((e) => e.type === "files_presented");
  assert.equal(shown.length, 2, "at the call, then the final versions");
  assert.deepEqual(shown[0].data.files, [
    { path: "r.pdf", caption: "The report", sha256: sha("v1"), size: 2 },
    { path: "chart.png", sha256: sha("c"), size: 1 },
  ]);
  const final = [
    { path: "r.pdf", caption: "The report", sha256: sha("v22"), size: 3 },
    { path: "chart.png", sha256: sha("c"), size: 1 },
  ];
  assert.deepEqual(shown[1].data.files, final);
  // the turn's files are all stored (the agent keeps using them) but only the presented ones are shown
  const all = (await call(A, "GET", `/api/conversations/${conv}/files`)).json.map((f: any) => f.path);
  for (const p of ["build/fonts.ttf", "build/main.py", "build/r.pdf"]) assert.ok(all.includes(p));
  const got = await call(A, "GET", `/api/conversations/${conv}`);
  assert.deepEqual(got.json.turns[1].presented, final, "stored with the turn (reload)");
  // the card's link is that version
  const dl = await call(A, "GET", `/api/conversations/${conv}/files/r.pdf?v=${sha("v22")}`);
  assert.equal(dl.text, "v22");
  assert.match(dl.headers.get("content-disposition")!, /^attachment/);
  assert.match(dl.headers.get("cache-control")!, /immutable/);
  // tool steps name the files
  assert.equal(t1.events.find((e) => e.type === "tool_use").data.name, "mcp__tools__present_files");

  // a later turn may change r.pdf: the earlier card keeps its version
  await runTurn(conv, "WRITE:r.pdf=v333");
  assert.equal((await call(A, "GET", `/api/conversations/${conv}/files/r.pdf?v=${sha("v22")}`)).text, "v22");
  assert.equal((await call(A, "GET", `/api/conversations/${conv}/files/r.pdf`)).text, "v333");
  // someone else's conversation: 404 whatever the version
  assert.equal((await call(B, "GET", `/api/conversations/${conv}/files/r.pdf?v=${sha("v22")}`)).status, 404);
});

test("present_files: a file over 50 MB is refused; a presented file deleted before the turn ends is dropped", async () => {
  const conv = await newConversation();
  // the sandbox never sends a file over 50 MB back (X-Skipped), so it can only come from elsewhere: put one there
  await runTurn(conv, "hi");
  await truncate(join(deps.workRoot, conv, "work", "big.bin"), 51 * 1024 * 1024).catch(async () => {
    writeFileSync(join(deps.workRoot, conv, "work", "big.bin"), "");
    await truncate(join(deps.workRoot, conv, "work", "big.bin"), 51 * 1024 * 1024);
  });
  const t2 = await runTurn(conv, `PRESENT:${JSON.stringify([{ path: "big.bin" }])}`);
  const r = t2.events.find((e) => e.type === "tool_result");
  assert.equal(r.data.is_error, true);
  assert.match(r.data.output, /big\.bin: larger than 50 MB/);
  const t3 = await runTurn(conv, `WRITE:gone.csv=1 PRESENT:${JSON.stringify([{ path: "gone.csv" }])} RM:gone.csv`);
  const shown = t3.events.filter((e) => e.type === "files_presented");
  assert.deepEqual(shown.at(-1).data.files, []);
});

test("old conversations: a turn from before present_files shows the files it changed, as before, copies once", async () => {
  const conv = await newConversation();
  const turn = randomUUID();
  const start = new Date(Date.now() - 600_000);
  const end = new Date(start.getTime() + 60_000);
  await pg.db.query("insert into turns (id, conversation_id, status, token_hash, model, started_at, finished_at) values ($1,$2,'done',$3,'m',$4,$5)", [turn, conv, sha(turn), start, end]);
  await pg.db.query("insert into messages (conversation_id, role, content, turn_id) values ($1,'user','make a report',$2)", [conv, turn]);
  const row = (path: string, content: string, at: Date, source = "agent") =>
    pg.db.query("insert into files (conversation_id, path, sha256, size, source, updated_at) values ($1,$2,$3,$4,$5,$6)", [conv, path, sha(content), content.length, source, at]);
  await row("build/香港 簡報.pdf", "pdf", new Date(start.getTime() + 30_000));
  await row("香港 簡報.pdf", "pdf", new Date(start.getTime() + 31_000));
  await row("build/main.py", "py", new Date(start.getTime() + 20_000));
  await row("uploads/in.csv", "csv", new Date(start.getTime() + 10_000), "upload");
  await row("later.txt", "later", new Date(end.getTime() + 60_000));
  const got = await call(A, "GET", `/api/conversations/${conv}`);
  assert.deepEqual(got.json.turns[0].presented.map((f: any) => f.path), ["build/main.py", "香港 簡報.pdf"]);
  // the rule itself
  assert.deepEqual(legacyPresented([], start, null), []);
  assert.deepEqual(mergePresented([{ path: "a", sha256: "1", size: 1 }], [{ path: "b", sha256: "2", size: 1 }, { path: "a", sha256: "3", size: 1 }]).map((f) => f.path), ["b", "a"]);
});

test("images: attached images go to claw as prompt images (at most 20), a viewed image's thumbnail and a PDF preview load", async () => {
  const conv = await newConversation();
  const img = png();
  const up = await call(A, "POST", `/api/conversations/${conv}/files?name=photo.png`, img);
  assert.equal(up.status, 201);
  const up2 = await call(A, "POST", `/api/conversations/${conv}/files?name=IMG_1.HEIC`, "heic-bytes");
  await call(A, "POST", `/api/conversations/${conv}/files?name=notes.txt`, "n");
  const t1 = await runTurn(conv, "what is in this photo?", ["uploads/photo.png", up2.json.path, "uploads/notes.txt"]);
  const dbg = t1.events.find((e) => e.type === "debug").data;
  assert.deepEqual(JSON.parse(dbg.env.CLAW_PROMPT_IMAGES), ["uploads/photo.png", "uploads/IMG_1.HEIC"]);
  // an uploaded image shows inline in the staff member's own message
  const inl = await call(A, "GET", `/api/conversations/${conv}/files/uploads/photo.png?inline=1`);
  assert.match(inl.headers.get("content-disposition")!, /^inline/);
  assert.equal(inl.headers.get("content-type"), "image/png");
  // an SVG is never inline (scripts)
  await call(A, "POST", `/api/conversations/${conv}/files?name=x.svg`, "<svg/>");
  assert.match((await call(A, "GET", `/api/conversations/${conv}/files/uploads/x.svg?inline=1`)).headers.get("content-disposition")!, /^attachment/);

  // the model looks at a chart the sandbox made: the bytes are in the bucket by the time the step's event arrives
  const r = await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "VIEW:chart.png\nBASH:printf '%s' 'chart-bytes' > chart.png", attachments: [] });
  const events = await streamUntil(A, conv, (e) => e.type === "tool_result" && e.data.name === "read_file");
  const viewed = events.at(-1).data.images[0];
  assert.equal(viewed.path, "chart.png");
  assert.equal(viewed.sha256, sha("chart-bytes"));
  const thumb = await call(A, "GET", `/api/conversations/${conv}/files/chart.png?v=${viewed.sha256}&preview=1`);
  assert.equal(thumb.status, 200, "loads mid-turn");
  assert.equal(thumb.text, "chart-bytes");
  await streamUntil(A, conv, turnEnd(r.json.turnId));

  // a PDF's first page as a small PNG (poppler in the web image; skipped where pdftoppm is missing)
  const hasPdftoppm = spawnSync("pdftoppm", ["-v"]).error === undefined;
  await runTurn(conv, `WRITE:doc.pdf=x`);
  await pg.db.query("update files set sha256 = $3, size = $4 where conversation_id = $1 and path = $2", [conv, "doc.pdf", sha(MINI_PDF), MINI_PDF.length]);
  await deps.blobs.put(`jarvis/files/${conv}/${sha(MINI_PDF)}`, Buffer.from(MINI_PDF));
  const prev = await fetch(`${base}/api/conversations/${conv}/files/doc.pdf?v=${sha(MINI_PDF)}&preview=1`, { headers: { "cf-access-jwt-assertion": await access.token(A) } });
  if (hasPdftoppm) {
    assert.equal(prev.status, 200);
    assert.equal(prev.headers.get("content-type"), "image/png");
    assert.equal(Buffer.from(await prev.arrayBuffer()).subarray(1, 4).toString(), "PNG");
  }
  assert.equal((await call(A, "GET", `/api/conversations/${conv}/files/notes.txt?preview=1`)).status, 404);

  // the cap and the filter, directly
  const dir = mkdtempSync(join(tmpdir(), "imgs-"));
  const names = Array.from({ length: 25 }, (_, i) => `p${i}.jpg`);
  for (const n of names) writeFileSync(join(dir, n), "x");
  assert.equal((await imageAttachments(dir, [...names, "a.txt"])).length, MAX_PROMPT_IMAGES);
  assert.deepEqual(await imageAttachments(dir, ["missing.png", "a.txt"]), []);
});
