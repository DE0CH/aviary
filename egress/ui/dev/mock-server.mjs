// Mock of the web role's public API for developing and screenshotting the UI (docs/CHAT.md, "Public API").
// Serves ../ui-dist with the SPA fallback. A sent message plays a canned turn: text, a web search, a bash command,
// a sandbox download the egress allowlist refuses, a table and a produced file. Nothing ever asks the user anything.
//   node dev/mock-server.mjs [port]     env MOCK_DENY=1 → every API call is 403 (the not-on-the-list page)
//                                       env MOCK_LOCALE=en → the instance locale is English
//                                       env MOCK_SEED=1 → two finished conversations to start with
//                                       env MOCK_ONE_MODEL=1 → one model only (no picker)
//                                       env MOCK_BUDGET=1 → the shared budget is used up (every turn refused)
//                                       env MOCK_NO_NAME=1 → no identity first name (greeting without a name)
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { readFileSync, existsSync } from "node:fs";
import { dirname, extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "..", "..", "ui-dist");
const port = Number(process.argv[2] || 5199);
const SPEED = Number(process.env.MOCK_SPEED || 1);

const me = {
  email: "staff@example.org",
  name: "Chan Tai Man",
  firstName: process.env.MOCK_NO_NAME ? null : "Tai Man",
  isAdmin: false,
  product: { name: "YYC AI", logoUrl: null, locale: process.env.MOCK_LOCALE || "zh-HK" },
  models: [
    { id: "anthropic/claude-opus-5.5", label: "Opus" },
    { id: "anthropic/claude-sonnet-5.5", label: "Sonnet" },
  ],
  defaultModel: "anthropic/claude-opus-5.5",
};
if (process.env.MOCK_ONE_MODEL) {
  me.models = [{ id: "anthropic/claude-sonnet-5.5", label: "Sonnet 5.5" }];
  me.defaultModel = "anthropic/claude-sonnet-5.5";
}

const convs = new Map();
let eventId = 0;
const subs = new Map(); // conv id -> Set<res>

function newConv(model, title = "") {
  const id = randomUUID();
  const now = new Date().toISOString();
  const c = { id, title, model: model || me.defaultModel, messages: [], turns: [], files: [], createdAt: now, updatedAt: now };
  convs.set(id, c);
  return c;
}

function emit(c, turn, type, data) {
  const e = { id: ++eventId, turnId: turn.id, seq: turn.events.length, type, data, createdAt: new Date().toISOString() };
  turn.events.push(e);
  c.updatedAt = e.createdAt;
  for (const res of subs.get(c.id) ?? []) res.write(`id: ${e.id}\ndata: ${JSON.stringify(e)}\n\n`);
  return e;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms / SPEED));

async function streamText(c, turn, text, chunk = 6) {
  for (let i = 0; i < text.length; i += chunk) {
    if (turn.status === "cancelled") return false;
    emit(c, turn, "text_delta", { text: text.slice(i, i + chunk) });
    await sleep(35);
  }
  return true;
}

async function playTurn(c, turn, prompt) {
  const fin = (status, error) => {
    turn.status = status;
    turn.finishedAt = new Date().toISOString();
    emit(c, turn, "turn_finished", { turnId: turn.id, status, ...(error ? { error } : {}) });
  };
  turn.status = "running";
  emit(c, turn, "turn_started", { turnId: turn.id });
  if (process.env.MOCK_BUDGET) {
    turn.status = "error";
    turn.finishedAt = new Date().toISOString();
    const error = "Deyao 設定了 30 美元的使用上限，目前已用完。";
    emit(c, turn, "turn_finished", { turnId: turn.id, status: "error", error, code: "budget", limitUsd: 30, ownerName: "Deyao" });
    return;
  }
  emit(c, turn, "sandbox", { status: "starting" });
  await sleep(500);
  emit(c, turn, "sandbox", { status: "ready" });
  emit(c, turn, "session", { session_id: "s1" });
  emit(c, turn, "message_start", {});
  emit(c, turn, "thinking_delta", { text: "" });
  await sleep(700);
  if (!(await streamText(c, turn, "好的，我先搜尋最近的相關報道，再整理重點。"))) return fin("cancelled");
  emit(c, turn, "message_stop", {});
  emit(c, turn, "tool_use", { id: "t1", name: "mcp__tools__web_search", input: { query: "香港 金融服務 新聞 2026年10月", num_results: 10 } });
  await sleep(1200);
  emit(c, turn, "tool_result", {
    id: "t1",
    name: "mcp__tools__web_search",
    is_error: false,
    output: JSON.stringify({ results: [{ title: "金管局公布新措施", url: "https://www.hkej.com/x" }, { title: "Banks report Q3 results", url: "https://www.scmp.com/y" }] }, null, 2),
  });
  emit(c, turn, "tool_use", { id: "t2", name: "mcp__tools__news_search", input: { query: "金融服務 保險", from: "2026-10-13", to: "2026-10-20" } });
  await sleep(900);
  emit(c, turn, "tool_result", { id: "t2", name: "mcp__tools__news_search", is_error: false, output: "共 42 篇報道：明報 9、經濟日報 8、信報 7、星島 6、其他 12" });
  emit(c, turn, "tool_use", { id: "t3", name: "bash", input: { command: "python3 count.py coverage.csv > summary.xlsx", description: "按媒體統計報道數量" } });
  await sleep(900);
  emit(c, turn, "tool_result", { id: "t3", name: "bash", is_error: false, output: "wrote summary.xlsx (5 rows)" });
  emit(c, turn, "tool_use", { id: "t4", name: "bash", input: { command: "curl -fsS https://files.example-share.net/coverage-2026.csv -o extra.csv", description: "下載額外數據" } });
  await sleep(600);
  emit(c, turn, "tool_result", {
    id: "t4",
    name: "bash",
    is_error: true,
    output: "curl: (56) CONNECT tunnel failed, response 403\negress: files.example-share.net is not on the sandbox's internet allowlist, and it cannot be enabled from the chat. The sandbox reaches package registries, code hosts and widely used public sites only. To read a web page, use the web_fetch and web_search tools instead of the sandbox.",
  });
  emit(c, turn, "message_start", {});
  const reply = `\n\n以下是本星期的重點：\n\n| 媒體 | 篇數 | 主要角度 |\n|---|---:|---|\n| 明報 | 9 | 監管新措施 |\n| 經濟日報 | 8 | 銀行業績 |\n| 信報 | 7 | 保險業前景 |\n\n1. **金管局**公布新的跨境理財通安排，多份報章以頭版報道。\n2. 保險業界對新規反應正面，但有評論關注中小型公司的合規成本。\n\n統計表已存為 \`summary.xlsx\`，可以在下面下載。`;
  if (!(await streamText(c, turn, reply, 8))) return fin("cancelled");
  emit(c, turn, "message_stop", {});
  c.files.push({ path: "summary.xlsx", size: 18_432, source: "agent", updatedAt: new Date().toISOString() });
  emit(c, turn, "result", { usage: { input_tokens: 31000, output_tokens: 900 }, estimated_cost: 0.12 });
  c.messages.push({ id: ++eventId, role: "assistant", content: reply.trim(), turnId: turn.id, attachments: [], createdAt: new Date().toISOString() });
  fin("done");
  if (!c.title) {
    c.title = prompt.slice(0, 18);
    emit(c, turn, "title", { title: c.title });
  }
}

function seed() {
  const a = newConv(me.defaultModel, "本週金融服務報道摘要");
  const t = { id: randomUUID(), status: "done", startedAt: new Date(Date.now() - 3_600_000).toISOString(), finishedAt: null, events: [] };
  a.turns.push(t);
  a.messages.push({ id: ++eventId, role: "user", content: "總結本週金融服務業的報道", turnId: t.id, attachments: ["uploads/coverage.csv"], createdAt: t.startedAt });
  emit(a, t, "turn_started", {});
  emit(a, t, "tool_use", { id: "x", name: "mcp__tools__yyc_posts_search", input: { query: "金融服務", stream: "FSHK" } });
  emit(a, t, "tool_result", { id: "x", name: "mcp__tools__yyc_posts_search", output: "18 posts", is_error: false });
  // what an older version stored: a confirmation and its answer (the UI shows nothing for them now)
  const cid = randomUUID();
  emit(a, t, "confirmation_request", { id: cid, kind: "network_access", summary: "允許沙盒連接 data.gov.hk？", detail: { host: "data.gov.hk", reason: "下載數據" } });
  emit(a, t, "confirmation_resolved", { id: cid, status: "approved" });
  emit(a, t, "text_delta", { text: "本週共有 **18** 篇相關報道，以監管和銀行業績為主。" });
  emit(a, t, "turn_finished", { status: "done" });
  t.finishedAt = new Date().toISOString();
  const b = newConv(me.defaultModel, "新聞稿草擬：可持續基金");
  b.updatedAt = new Date(Date.now() - 2 * 86_400_000).toISOString();
}
if (process.env.MOCK_SEED) seed();

const json = (res, code, body) => {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(body === undefined ? "" : JSON.stringify(body));
};
const readBody = (req) =>
  new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks)));
  });

const types = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".webmanifest": "application/manifest+json" };

createServer(async (req, res) => {
  const u = new URL(req.url, "http://x");
  const p = u.pathname;
  if (p.startsWith("/api/")) {
    if (process.env.MOCK_DENY) return json(res, 403, { error: "not on the list" });
    if (p === "/api/me") return json(res, 200, me);
    if (p === "/api/typing") return json(res, 204);
    if (p === "/api/conversations" && req.method === "GET")
      return json(
        res,
        200,
        [...convs.values()]
          .filter((c) => c.messages.length)
          .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
          .map((c) => ({ id: c.id, title: c.title, updatedAt: c.updatedAt, running: c.turns.some((t) => ["queued", "running", "waiting"].includes(t.status)) })),
      );
    if (p === "/api/conversations" && req.method === "POST") {
      const body = JSON.parse((await readBody(req)).toString() || "{}");
      return json(res, 200, { id: newConv(body.model).id });
    }
    let m;
    if ((m = /^\/api\/conversations\/([^/]+)(\/.*)?$/.exec(p))) {
      const c = convs.get(m[1]);
      if (!c) return json(res, 404, { error: "not found" });
      const rest = m[2] || "";
      if (!rest && req.method === "GET") return json(res, 200, { id: c.id, title: c.title, model: c.model, messages: c.messages, turns: c.turns });
      if (!rest && req.method === "PATCH") {
        Object.assign(c, JSON.parse((await readBody(req)).toString()));
        return json(res, 204);
      }
      if (!rest && req.method === "DELETE") {
        convs.delete(c.id);
        return json(res, 204);
      }
      if (rest === "/messages" && req.method === "POST") {
        const body = JSON.parse((await readBody(req)).toString());
        if (c.turns.some((t) => ["queued", "running", "waiting"].includes(t.status))) return json(res, 409, { error: "busy" });
        const turn = { id: randomUUID(), status: "queued", startedAt: new Date().toISOString(), finishedAt: null, events: [] };
        c.turns.push(turn);
        c.messages.push({ id: ++eventId, role: "user", content: body.text, turnId: turn.id, attachments: body.attachments || [], createdAt: new Date().toISOString() });
        setTimeout(() => void playTurn(c, turn, body.text), 300 / SPEED);
        return json(res, 200, { turnId: turn.id });
      }
      if (rest === "/stream") {
        const after = Number(u.searchParams.get("after") || req.headers["last-event-id"] || 0);
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
        for (const t of c.turns) for (const e of t.events) if (e.id > after) res.write(`id: ${e.id}\ndata: ${JSON.stringify(e)}\n\n`);
        let set = subs.get(c.id);
        if (!set) subs.set(c.id, (set = new Set()));
        set.add(res);
        const hb = setInterval(() => res.write(": hb\n\n"), 15_000);
        req.on("close", () => {
          clearInterval(hb);
          set.delete(res);
        });
        return;
      }
      if (rest === "/files" && req.method === "GET") return json(res, 200, c.files);
      if (rest === "/files" && req.method === "POST") {
        const buf = await readBody(req);
        const path = `uploads/${u.searchParams.get("name")}`;
        c.files.push({ path, size: buf.length, source: "upload", updatedAt: new Date().toISOString() });
        return json(res, 200, { path });
      }
      if (rest.startsWith("/files/")) {
        res.writeHead(200, { "content-type": "application/octet-stream" });
        return res.end("mock file");
      }
    }
    if ((m = /^\/api\/turns\/([^/]+)\/cancel$/.exec(p))) {
      for (const c of convs.values()) for (const t of c.turns) if (t.id === m[1]) t.status = "cancelled";
      return json(res, 204);
    }
    return json(res, 404, { error: "no route" });
  }
  let file = join(dist, p === "/" ? "index.html" : p);
  if (!file.startsWith(dist) || !existsSync(file) || p.startsWith("/c/")) file = join(dist, "index.html");
  if (p.startsWith("/brand/")) {
    res.writeHead(404);
    return res.end();
  }
  res.writeHead(200, { "content-type": types[extname(file)] || "application/octet-stream" });
  res.end(readFileSync(file));
}).listen(port, () => console.log(`mock on http://127.0.0.1:${port}`));
