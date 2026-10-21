#!/usr/bin/env node
// A fake claw for the web role's tests: speaks the event stream (claw-code docs/event-stream.md) and acts on markers
// in the prompt:
//   BASH:<command>     one bash tool call through $CLAW_BASH_COMMAND (the real jarvis-bash)
//   WRITE:<path>=<txt> writes a file in the workdir (cwd), as the file tools would (every such marker)
//   PRESENT:<json>     one present_files call (input {files: <json>}) through the MCP bridge in CLAW_CONFIG_DIR
//   PRESENTSTR:<json> the same with the list sent as a JSON string
//   AFTER:<path>=<txt> writes a file after the PRESENT call (a later version of a presented file)
//   RM:<path>          deletes a file after the PRESENT call
//   VIEW:<path>        a read_file of an image: tool_result with `images` [{path, sha256, media_type}] as claw sends
//   SLEEP              keeps running until killed
//   FAIL               emits an error and exits 1
//   MODELCALL          after its first words, one model call to $ANTHROPIC_BASE_URL/v1/messages; an error answer ends
//                      the run the way claw does (error event "api returned <status> (<type>): <message>", exit 1)
// It also emits a `debug` event with its args, cwd and the CLAW_*/JARVIS_* env it was given, and keeps a session file
// (one line per turn) so resume is visible as "turn N".
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const out = (o) => process.stdout.write(JSON.stringify(o) + "\n");
const args = process.argv.slice(2);
const prompt = args[args.indexOf("prompt") + 1] ?? "";
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => /^(CLAW_|JARVIS_|ANTHROPIC_)/.test(k)));

const sessionPath = process.env.CLAW_RESUME_SESSION || join(process.env.CLAW_SESSIONS_DIR || ".", `fake-${process.pid}.jsonl`);
mkdirSync(dirname(sessionPath), { recursive: true });
appendFileSync(sessionPath, JSON.stringify({ prompt }) + "\n");
const turnNo = readFileSync(sessionPath, "utf8").trim().split("\n").length;

out({ type: "session", session_id: "s", path: sessionPath, model: args[args.indexOf("--model") + 1], cwd: process.cwd(), resumed: existsSync(sessionPath), permission_mode: "danger-full-access" });
const systemPrompt = process.env.CLAW_SYSTEM_PROMPT_FILE ? readFileSync(process.env.CLAW_SYSTEM_PROMPT_FILE, "utf8") : "";
out({ type: "debug", args, cwd: process.cwd(), env, prompt, systemPrompt });

if (prompt.includes("FAIL")) {
  out({ type: "error", message: "the model refused" });
  process.exit(1);
}

let text = "";
const say = (t) => {
  text += t;
  out({ type: "text_delta", text: t });
};
out({ type: "message_start" });
say("Hello ");
say(`turn ${turnNo}`);

if (prompt.includes("MODELCALL")) {
  const r = await fetch(`${process.env.ANTHROPIC_BASE_URL}/v1/messages`, {
    method: "POST",
    headers: { authorization: `Bearer ${process.env.ANTHROPIC_AUTH_TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({ model: args[args.indexOf("--model") + 1], max_tokens: 10, messages: [{ role: "user", content: prompt }] }),
  });
  if (!r.ok) {
    const body = await r.text();
    let e = null;
    try {
      e = JSON.parse(body).error;
    } catch {}
    out({ type: "error", message: e ? `api returned ${r.status} ${r.statusText} (${e.type}): ${e.message}` : `api returned ${r.status}: ${body}` });
    process.exit(1);
  }
  await r.text();
}

const writeFile_ = (path, txt) => {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, txt);
};
for (const w of prompt.matchAll(/(?<![A-Z])WRITE:([^=\s]+)=(\S*)/g)) writeFile_(w[1], w[2]);

const present = /PRESENT(STR)?:(\[.*?\])(?:\s|$)/.exec(prompt);
if (present) {
  const settings = JSON.parse(readFileSync(join(process.env.CLAW_CONFIG_DIR, "settings.json"), "utf8"));
  const srv = settings.mcpServers.tools;
  // PRESENTSTR: the list as a JSON string, as a model sent it live
  const input = { files: present[1] ? present[2] : JSON.parse(present[2]) };
  out({ type: "tool_use", id: "p1", name: "mcp__tools__present_files", input });
  const answer = await new Promise((resolve) => {
    const c = spawn(srv.command, srv.args, { env: { ...process.env, ...srv.env }, stdio: ["pipe", "pipe", "inherit"] });
    let o = "";
    c.stdout.on("data", (d) => {
      o += d;
      if (o.includes("\n")) {
        resolve(JSON.parse(o.split("\n")[0]));
        c.stdin.end();
      }
    });
    c.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 7, method: "tools/call", params: { name: "present_files", arguments: input } }) + "\n");
  });
  const r = answer.result ?? { content: [{ type: "text", text: answer.error?.message ?? "?" }], isError: true };
  out({ type: "tool_result", id: "p1", name: "mcp__tools__present_files", output: r.content.map((x) => x.text).join(""), is_error: !!r.isError });
}
for (const w of prompt.matchAll(/AFTER:([^=\s]+)=(\S*)/g)) writeFile_(w[1], w[2]);
for (const w of prompt.matchAll(/RM:(\S+)/g)) rmSync(w[1], { force: true });


const bash = /BASH:(.*)$/m.exec(prompt);
if (bash) {
  out({ type: "tool_use", id: "t1", name: "bash", input: { command: bash[1] } });
  out({ type: "message_stop" });
  const r = await new Promise((resolve) => {
    const c = spawn(process.env.CLAW_BASH_COMMAND, [], { stdio: ["pipe", "pipe", "pipe"] });
    let o = "";
    let e = "";
    c.stdout.on("data", (d) => (o += d));
    c.stderr.on("data", (d) => (e += d));
    c.on("close", (code) => resolve({ code, o, e }));
    c.stdin.end(JSON.stringify({ command: bash[1], timeout: 30000 }));
  });
  out({ type: "tool_result", id: "t1", name: "bash", output: r.code === 0 ? r.o : r.e, is_error: r.code !== 0 });
  out({ type: "message_start" });
  say(" after bash");
}

const view = /VIEW:(\S+)/.exec(prompt);
if (view) {
  const sha256 = createHash("sha256").update(readFileSync(view[1])).digest("hex");
  out({ type: "tool_use", id: "v1", name: "read_file", input: { path: view[1] } });
  out({ type: "tool_result", id: "v1", name: "read_file", output: `Image ${view[1]}: 2x2 px (shown to you below)`, is_error: false, images: [{ path: view[1], sha256, media_type: "image/png", width: 2, height: 2 }] });
}

if (prompt.includes("SLEEP")) {
  process.on("SIGTERM", () => process.exit(143));
  await new Promise(() => setInterval(() => {}, 1000));
}

out({ type: "message_stop" });
out({ type: "result", message: text, usage: { input_tokens: 10, output_tokens: 5 }, iterations: 1, estimated_cost: 0.001, session_id: "s" });
