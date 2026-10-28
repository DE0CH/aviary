#!/usr/bin/env node
// Claude Code's PreToolUse hook on SendUserFile (docs/CLAUDE-CODE.md, "Sent files"): before the files are sent, the
// UI keeps a copy-on-write snapshot of each one outside the sandbox, so a file card keeps the file it showed even when
// the agent later changes or deletes the original. Claude Code waits for this hook, so nothing the agent does next can
// touch a file before its snapshot exists. The request goes through egress (JARVIS_SENT_FILES_URL, authenticated by the
// sandbox's egress token), which adds the conversation it verified. A snapshot that fails blocks the send (exit 2:
// Claude Code shows stderr to the model), so a card never points at a file that was not kept.
import { request } from "node:http";
import { request as httpsRequest } from "node:https";
import path from "node:path";

const url = process.env.JARVIS_SENT_FILES_URL;
const token = process.env.ANTHROPIC_AUTH_TOKEN;

function readStdin() {
  return new Promise((resolve) => {
    const parts = [];
    process.stdin.on("data", (c) => parts.push(c));
    process.stdin.on("end", () => resolve(Buffer.concat(parts).toString("utf8")));
  });
}

function post(target, body) {
  return new Promise((resolve, reject) => {
    const u = new URL(target);
    const data = Buffer.from(JSON.stringify(body));
    const req = (u.protocol === "https:" ? httpsRequest : request)(
      u,
      { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json", "content-length": data.length }, timeout: 45_000 },
      (res) => {
        const parts = [];
        res.on("data", (c) => parts.push(c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, text: Buffer.concat(parts).toString("utf8") }));
      },
    );
    req.on("timeout", () => req.destroy(new Error("no answer within 45 s")));
    req.on("error", reject);
    req.end(data);
  });
}

function block(reason) {
  process.stderr.write(`The files were not sent: they could not be saved for the user (${reason}). Try sending them again.\n`);
  process.exit(2);
}

const input = JSON.parse((await readStdin()) || "{}");
if (!url || input.tool_name !== "SendUserFile") process.exit(0);
const cwd = typeof input.cwd === "string" && input.cwd ? input.cwd : "/workspace";
const files = (Array.isArray(input.tool_input?.files) ? input.tool_input.files : [])
  .filter((f) => typeof f === "string" && f)
  .map((f) => path.resolve(cwd, f.replace(/^file:\/\//, "")));
if (!files.length || typeof input.tool_use_id !== "string") process.exit(0);
try {
  const r = await post(url, { toolUseId: input.tool_use_id, files });
  if (r.status !== 200) block(`HTTP ${r.status}: ${r.text.slice(0, 200)}`);
} catch (e) {
  block(String(e?.message || e));
}
