// One turn through the web role with the REAL claw pinned in chat/claw.json (host mode, claw-code
// docs/event-stream.md): a fake Anthropic-format model server (canned SSE: one bash tool_use, then a text answer), the
// real jarvis-bash with the real sandbox agent (fake Kubernetes), and the real jarvis-mcp-bridge with a fake tools server.
// The binary comes from $CLAW_BIN, or is downloaded per claw.json (sha256 checked) into a cache in the temp dir.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdtempSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { AccessVerifier } from "../src/common/access.ts";
import { parseConfig } from "../src/common/config.ts";
import { Listener } from "../src/common/db.ts";
import { S3BlobStore } from "../src/common/s3.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { AUD, fakeAccess, TEAM } from "./fakes/access.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { fakeS3 } from "./fakes/s3.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const MODEL = "anthropic/claude-opus-5.5";

async function clawBinary(): Promise<string> {
  if (process.env.CLAW_BIN) return process.env.CLAW_BIN;
  const pin = JSON.parse(readFileSync(here("../claw.json"), "utf8")) as { url: string; sha256: string };
  const path = join(tmpdir(), `claw-${pin.sha256.slice(0, 16)}`);
  const sha = (b: Buffer) => createHash("sha256").update(b).digest("hex");
  if (existsSync(path) && sha(readFileSync(path)) === pin.sha256) return path;
  const r = await fetch(pin.url);
  assert.equal(r.status, 200, `downloading ${pin.url}`);
  const bin = Buffer.from(await r.arrayBuffer());
  assert.equal(sha(bin), pin.sha256, "claw.json's sha256 does not match the release asset");
  writeFileSync(`${path}.part`, bin);
  chmodSync(`${path}.part`, 0o755);
  renameSync(`${path}.part`, path);
  return path;
}

interface Seen {
  model: { headers: Record<string, string | string[] | undefined>; body: any }[];
  tools: { method: string; auth: string | undefined }[];
}

/** The model API (egress /x/model) and the tools server (/mcp) on one port. */
async function fakeUpstream() {
  const seen: Seen = { model: [], tools: [] };
  const sse = (res: any, events: [string, unknown][]) => {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    for (const [event, data] of events) res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
    res.end();
  };
  const usage = { input_tokens: 20, output_tokens: 5, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
  const start = (id: string) => ["message_start", { type: "message_start", message: { id, type: "message", role: "assistant", model: "claude-opus-5.5", content: [], stop_reason: null, stop_sequence: null, usage } }] as [string, unknown];
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const raw = Buffer.concat(chunks).toString("utf8");
    const body = raw ? JSON.parse(raw) : {};
    if (req.url === "/mcp") {
      seen.tools.push({ method: body.method, auth: req.headers.authorization });
      const ok = (result: unknown) => {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: body.id, result }));
      };
      if (body.id === undefined || body.id === null) return void res.writeHead(202).end();
      if (body.method === "initialize") return ok({ protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "fake-tools", version: "1" } });
      if (body.method === "tools/list") {
        return ok({ tools: [{ name: "web_search", description: "Searches the web.", inputSchema: { type: "object", properties: { query: { type: "string" } }, required: ["query"] } }] });
      }
      if (body.method === "tools/call") return ok({ content: [{ type: "text", text: "no results" }] });
      res.writeHead(200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ jsonrpc: "2.0", id: body.id, error: { code: -32601, message: "no such method" } }));
    }
    if (req.url === "/x/model/v1/messages") {
      if (!body.stream) {
        // the title call
        res.writeHead(200, { "content-type": "application/json" });
        return void res.end(JSON.stringify({ content: [{ type: "text", text: "Line count" }] }));
      }
      seen.model.push({ headers: req.headers, body });
      const last = body.messages.at(-1);
      const hasResult = Array.isArray(last?.content) && last.content.some((b: any) => b.type === "tool_result");
      if (!hasResult) {
        return sse(res, [
          start("msg_1"),
          ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "toolu_1", name: "bash", input: {} } }],
          ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify({ command: "printf 'a\\nb\\n' > lines.txt; wc -l < lines.txt" }) } }],
          ["content_block_stop", { type: "content_block_stop", index: 0 }],
          ["message_delta", { type: "message_delta", delta: { stop_reason: "tool_use", stop_sequence: null }, usage: { output_tokens: 12 } }],
          ["message_stop", { type: "message_stop" }],
        ]);
      }
      return sse(res, [
        start("msg_2"),
        ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "The file has " } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "2 lines." } }],
        ["content_block_stop", { type: "content_block_stop", index: 0 }],
        ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 6 } }],
        ["message_stop", { type: "message_stop" }],
      ]);
    }
    res.writeHead(404).end();
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as { port: number }).port;
  return { seen, port, close: () => new Promise<void>((r) => server.close(() => r())) };
}

let pg: Awaited<ReturnType<typeof freshDb>>;
let s3: Awaited<ReturnType<typeof fakeS3>>;
let access: Awaited<ReturnType<typeof fakeAccess>>;
let k8s: FakeK8s;
let web: WebServer;
let deps: WebDeps;
let base: string;
let upstream: Awaited<ReturnType<typeof fakeUpstream>>;

before(async () => {
  const clawPath = await clawBinary();
  pg = await freshDb("claw");
  s3 = await fakeS3();
  access = await fakeAccess();
  k8s = new FakeK8s();
  upstream = await fakeUpstream();
  const work = mkdtempSync(join(tmpdir(), "chat-claw-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant. MARKER-SYSTEM-PROMPT");
  const config = parseConfig({
    product: { name: "YYC AI", locale: "zh-HK" },
    publicUrl: "https://ai.example.org",
    access: { teamDomain: TEAM, audience: AUD },
    users: [A],
    admins: [A],
    models: [{ id: MODEL, label: "Opus" }],
    defaultModel: MODEL,
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
    // relative on purpose: claw runs in the workdir, so the runner must hand it absolute paths
    workRoot: relative(process.cwd(), join(work, "work")),
    clawPath,
    bashPath: relative(process.cwd(), here("../src/bin/jarvis-bash")),
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
  await web?.close();
  await deps?.listener.close();
  await k8s?.closeAll();
  await upstream?.close();
  await s3?.close();
  await pg?.drop();
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

async function waitTurn(turnId: string, timeoutMs = 60_000): Promise<any> {
  const until = Date.now() + timeoutMs;
  for (;;) {
    const { rows } = await pg.db.query("select status, error from turns where id = $1", [turnId]);
    if (rows[0] && !["queued", "running", "waiting"].includes(rows[0].status)) return rows[0];
    if (Date.now() > until) throw new Error(`turn ${turnId} still ${rows[0]?.status}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

test("the real claw: one turn with a bash call in the sandbox, MCP tools through the bridge", async () => {
  const c = await call("POST", "/api/conversations", {});
  assert.equal(c.status, 201, c.text);
  const conv = c.json.id;
  const m = await call("POST", `/api/conversations/${conv}/messages`, { text: "How many lines? Use bash.", attachments: [] });
  assert.equal(m.status, 202, m.text);
  const turnId = m.json.turnId;
  const turn = await waitTurn(turnId);
  const { rows: evs } = await pg.db.query("select type, data from events where turn_id = $1 order by id", [turnId]);
  const dump = JSON.stringify(evs.map((e: any) => [e.type, e.data])).slice(0, 4000);
  assert.equal(turn.status, "done", `${turn.error}\n${dump}`);

  // the model calls: Anthropic format, the turn token as a bearer, prompt caching, the host's system prompt, the tools
  assert.equal(upstream.seen.model.length, 2, dump);
  const first = upstream.seen.model[0]!;
  const token = String(first.headers.authorization).replace(/^Bearer /, "");
  assert.ok(token.length > 10, "a bearer token");
  assert.equal(first.headers["x-api-key"], undefined, "no ANTHROPIC_API_KEY");
  assert.equal(first.body.model, "claude-opus-5.5");
  const system = Array.isArray(first.body.system) ? first.body.system : [{ type: "text", text: String(first.body.system) }];
  assert.match(system.map((b: any) => b.text).join("\n"), /MARKER-SYSTEM-PROMPT/);
  assert.doesNotMatch(system.map((b: any) => b.text).join("\n"), /CLAUDE\.md/);
  assert.deepEqual(system.at(-1).cache_control, { type: "ephemeral" });
  const toolNames = first.body.tools.map((t: any) => t.name).sort();
  assert.ok(toolNames.includes("bash"), toolNames.join(","));
  assert.ok(toolNames.includes("mcp__tools__web_search"), `the bridge's tool reached the model: ${toolNames.join(",")}`);
  assert.ok(!toolNames.some((n: string) => /AskUserQuestion|REPL|Agent|WebFetch|WebSearch/i.test(n)), toolNames.join(","));
  assert.deepEqual(first.body.tools.at(-1).cache_control, { type: "ephemeral" });

  // the bridge reached the tools server with the turn token
  const methods = upstream.seen.tools.map((t) => t.method);
  assert.ok(methods.includes("initialize") && methods.includes("tools/list"), methods.join(","));
  assert.ok(upstream.seen.tools.every((t) => t.auth === `Bearer ${token}`));

  // bash ran in the sandbox through jarvis-bash, and claw reported its output
  const second = upstream.seen.model[1]!;
  const resultBlock = second.body.messages.at(-1).content.find((b: any) => b.type === "tool_result");
  const resultText = typeof resultBlock.content === "string" ? resultBlock.content : resultBlock.content.map((b: any) => b.text).join("");
  assert.equal(resultBlock.is_error ?? false, false, resultText);
  assert.match(resultText, /\b2\b/);
  const types = evs.map((e: any) => e.type);
  for (const t of ["turn_started", "tool_use", "tool_result", "text_delta", "result", "turn_finished"]) assert.ok(types.includes(t), `${t} in ${types.join(",")}`);

  // the answer, the file the sandbox wrote, and claw's session for resume
  const { rows: msgs } = await pg.db.query("select content from messages where turn_id = $1 and role = 'assistant'", [turnId]);
  assert.equal(msgs[0]?.content, "The file has 2 lines.");
  const files = await call("GET", `/api/conversations/${conv}/files`);
  assert.ok(files.json.some((f: any) => f.path === "lines.txt"), files.text);
  const { rows: sess } = await pg.db.query("select content from claw_sessions where conversation_id = $1", [conv]);
  assert.ok(sess[0]?.content.includes("toolu_1"), "claw's session is stored");

  // a second turn resumes the session (CLAW_RESUME_SESSION): the model sees the first turn's messages
  const m2 = await call("POST", `/api/conversations/${conv}/messages`, { text: "Again, please.", attachments: [] });
  assert.equal(m2.status, 202, m2.text);
  const turn2 = await waitTurn(m2.json.turnId);
  assert.equal(turn2.status, "done", turn2.error);
  assert.equal(upstream.seen.model.length, 4);
  const resumed = upstream.seen.model[2]!.body.messages;
  assert.match(JSON.stringify(resumed[0]), /How many lines\? Use bash\./);
  assert.match(JSON.stringify(resumed.at(-1)), /Again, please\./);
  assert.ok(resumed.length >= 5, JSON.stringify(resumed).slice(0, 2000));
});

test("jarvis-mcp-bridge: Content-Length framing in and out", async () => {
  const child = spawn(here("../src/bin/jarvis-mcp-bridge"), [], {
    env: { ...process.env, JARVIS_TOOLS_URL: deps.toolsUrl, JARVIS_TURN_TOKEN: "tok" },
    stdio: ["pipe", "pipe", "inherit"],
  });
  const frame = (o: unknown) => {
    const b = Buffer.from(JSON.stringify(o));
    return Buffer.concat([Buffer.from(`Content-Length: ${b.length}\r\n\r\n`), b]);
  };
  let out = Buffer.alloc(0);
  const got = new Promise<any[]>((resolve) => {
    child.stdout!.on("data", (c: Buffer) => {
      out = Buffer.concat([out, c]);
      const msgs: any[] = [];
      let rest = out;
      for (;;) {
        const end = rest.indexOf("\r\n\r\n");
        if (end < 0) break;
        const len = Number(/^Content-Length: (\d+)$/.exec(rest.subarray(0, end).toString())?.[1]);
        if (!len || rest.length < end + 4 + len) break;
        msgs.push(JSON.parse(rest.subarray(end + 4, end + 4 + len).toString()));
        rest = rest.subarray(end + 4 + len);
      }
      if (msgs.length === 2) resolve(msgs);
    });
  });
  child.stdin!.write(frame({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }));
  child.stdin!.write(frame({ jsonrpc: "2.0", method: "notifications/initialized" }));
  child.stdin!.write(frame({ jsonrpc: "2.0", id: "two", method: "tools/list", params: {} }));
  const msgs = await got;
  child.stdin!.end();
  const byId = Object.fromEntries(msgs.map((m) => [m.id, m]));
  assert.equal(byId[1].result.serverInfo.name, "fake-tools");
  assert.equal(byId.two.result.tools[0].name, "web_search");
  assert.equal(await new Promise((r) => child.on("close", r)), 0);
});
