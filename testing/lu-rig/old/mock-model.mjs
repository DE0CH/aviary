// The rig's model (Anthropic Messages API), no cost: a turn's last user text decides the answer.
//   "[[slow N]]"  a long old turn: N Bash tool calls in a row, each `sleep 1; echo chunk <k>` (so the turn streams a
//                 row a second and runs a real process in the sandbox: across a move, Stop ends it), then "done N"
//   "[[ask]]"     an AskUserQuestion call (one question, two options), then "asked" once it has its result
//   "[[bg N]]"    one Bash call run in the background (`sleep N; echo bg done`), then "started": Claude Code holds
//                 its process open after the result until the task ends (a hold, DESIGN 9.9)
//   anything else  "Echo: <the first 80 characters of the prompt>" in one chunk
// Every request is kept for GET /__log ({at, stream, prompt}); a title or small-model request (no tools) answers
// "Rig title". Plain Node, no dependencies.
import http from "node:http";

const PORT = Number(process.env.PORT || 18080);
const log = [];

/** The text of the last user message (strings and text blocks), without system reminders. */
function lastUserText(messages) {
  for (let i = (messages || []).length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "user") continue;
    const parts = typeof m.content === "string" ? [m.content] : (m.content || []).filter((b) => b.type === "text").map((b) => b.text);
    const t = parts.filter((p) => !p.startsWith("<system-reminder>")).join("\n").trim();
    if (t) return t;
  }
  return "";
}

/** One tool call as the whole answer (streamed when asked): Bash `command` unless another tool is named. */
function toolUse(res, p, command, name = "Bash", given = null) {
  const id = `toolu_rig_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
  const input = given ?? { command, description: "rig step" };
  const msg = { id: `msg_rig_${Date.now()}`, type: "message", role: "assistant", model: p.model || "claude-rig", content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 1 } };
  if (!p.stream) {
    res.setHeader("content-type", "application/json");
    return res.end(JSON.stringify({ ...msg, content: [{ type: "tool_use", id, name, input }], stop_reason: "tool_use" }));
  }
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  const ev = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
  ev("message_start", { message: msg });
  ev("content_block_start", { index: 0, content_block: { type: "tool_use", id, name, input: {} } });
  ev("content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(input) } });
  ev("content_block_stop", { index: 0 });
  ev("message_delta", { delta: { stop_reason: "tool_use", stop_sequence: null }, usage: { output_tokens: 20 } });
  ev("message_stop", {});
  res.end();
}

http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    if (req.url === "/__log") { res.setHeader("content-type", "application/json"); return res.end(JSON.stringify(log)); }
    if (req.url === "/healthz") return res.end("ok");
    let p = {};
    try { p = JSON.parse(body || "{}"); } catch { /* empty */ }
    if (req.url.startsWith("/v1/messages/count_tokens")) { res.setHeader("content-type", "application/json"); return res.end('{"input_tokens":10}'); }
    if (!req.url.startsWith("/v1/messages")) { res.writeHead(404); return res.end("{}"); }
    const prompt = lastUserText(p.messages);
    const main = Array.isArray(p.tools) && p.tools.length > 0;
    const lastUser = [...(p.messages || [])].reverse().find((m) => m.role === "user");
    log.push({ at: new Date().toISOString(), stream: !!p.stream, main, prompt: prompt.slice(0, 200), user: JSON.stringify(lastUser?.content ?? "").slice(0, 4000) });
    const slow = main ? /\[\[slow (\d+)\]\]/.exec(prompt) : null;
    const ask = main && /\[\[ask\]\]/.test(prompt);
    const bg = main ? /\[\[bg (\d+)\]\]/.exec(prompt) : null;
    // the tool results after the prompt's message: how far the turn is
    const msgs = p.messages || [];
    let k = 0;
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      const blocks = Array.isArray(m.content) ? m.content : [];
      if (m.role === "user" && blocks.some((b) => b.type === "tool_result")) { k++; continue; }
      if (m.role === "user") break;
    }
    const has = (name) => (p.tools || []).some((t) => t.name === name);
    if (slow && k < Number(slow[1]) && has("Bash")) return toolUse(res, p, `sleep 1; echo chunk ${k + 1}`);
    if (ask && k === 0 && has("AskUserQuestion")) {
      return toolUse(res, p, "", "AskUserQuestion", { questions: [{ question: "Persona question: go on?", header: "Persona", multiSelect: false,
        options: [{ label: "Yes", description: "go on" }, { label: "No", description: "stop" }] }] });
    }
    if (bg && k === 0 && has("Bash")) {
      return toolUse(res, p, "", "Bash", { command: `sleep ${bg[1]}; echo bg done`, description: "rig background step", run_in_background: true });
    }
    // a title names the persona token of the conversation's text when it has one (the persona drivers find it in
    // the list by it)
    const tag = /persona-[0-9a-f]{8}/.exec(JSON.stringify(p.messages || []))?.[0];
    const chunks = slow ? [`done ${slow[1]}`] : ask ? ["asked"] : bg ? ["started"]
      : [main ? `Echo: ${prompt.slice(0, 80)}` : tag ? `Rig ${tag}` : "Rig title"];
    const msg = { id: `msg_rig_${Date.now()}`, type: "message", role: "assistant", model: p.model || "claude-rig", content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 1 } };
    if (!p.stream) {
      res.setHeader("content-type", "application/json");
      return res.end(JSON.stringify({ ...msg, content: [{ type: "text", text: chunks.join("") }], stop_reason: "end_turn" }));
    }
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    const ev = (type, data) => { if (!res.destroyed) res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`); };
    ev("message_start", { message: msg });
    ev("content_block_start", { index: 0, content_block: { type: "text", text: "" } });
    let i = 0;
    const next = () => {
      if (res.destroyed) return;
      if (i < chunks.length) { ev("content_block_delta", { index: 0, delta: { type: "text_delta", text: chunks[i++] } }); return void setImmediate(next); }
      ev("content_block_stop", { index: 0 });
      ev("message_delta", { delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: chunks.length } });
      ev("message_stop", {});
      res.end();
    };
    next();
  });
}).listen(PORT, "0.0.0.0", () => console.log(JSON.stringify({ listening: PORT })));
