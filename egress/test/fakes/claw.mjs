#!/usr/bin/env node
// A fake claw for the web role's tests: speaks the event stream (claw-code docs/event-stream.md) and acts on markers
// in the prompt:
//   BASH:<command>     one bash tool call through $CLAW_BASH_COMMAND (the real jarvis-bash)
//   WRITE:<path>=<txt> writes a file in the workdir (cwd), as the file tools would
//   SLEEP              keeps running until killed
//   FAIL               emits an error and exits 1
// It also emits a `debug` event with its args, cwd and the CLAW_*/JARVIS_* env it was given, and keeps a session file
// (one line per turn) so resume is visible as "turn N".
import { spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
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
out({ type: "debug", args, cwd: process.cwd(), env, prompt });

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

const write = /WRITE:([^=\s]+)=(\S*)/.exec(prompt);
if (write) {
  mkdirSync(dirname(write[1]), { recursive: true });
  writeFileSync(write[1], write[2]);
}

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

if (prompt.includes("SLEEP")) {
  process.on("SIGTERM", () => process.exit(143));
  await new Promise(() => setInterval(() => {}, 1000));
}

out({ type: "message_stop" });
out({ type: "result", message: text, usage: { input_tokens: 10, output_tokens: 5 }, iterations: 1, estimated_cost: 0.001, session_id: "s" });
