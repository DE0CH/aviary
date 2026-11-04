// The old stack's web role (jarvis3 chat `web`, /internal/cc/*) and its sandbox agent (GET /proc), for the LU rig
// (WP-54): a stand-in good enough for the real ui image (router + CloudCLI) to run real Claude Code turns against the
// rig's mock model. Plain Node, no dependencies; runs in a container of the ui image itself (its Claude Code binary),
// as root, so each process gets its own mount namespace with /workspace bound to /jfs/ws/<conversation> and then runs
// as uid 1000 (Claude Code refuses to skip permissions as root).
//
// Every sandbox is this one agent: POST /internal/cc/sandbox answers its own URL with the conversation in a header
// (CloudCLI keeps `headers` and drops the URL's path). Calls are logged as JSON lines on stdout and kept for
// GET /__rig/calls (the rig's checks: which conversations ran, touches, warms, releases).
//
// Env: PORT (17070), JARVIS_CC_TOKEN (the ui's bearer), JFS (/jfs), MOCK_URL (the mock model), CLAUDE_BIN.
import { spawn } from "node:child_process";
import { mkdirSync, chownSync } from "node:fs";
import http from "node:http";
import path from "node:path";

import { userKey } from "/app/jarvis/router.mjs";

const PORT = Number(process.env.PORT || 17070);
const TOKEN = process.env.JARVIS_CC_TOKEN || "cc-rig-token";
const AGENT_TOKEN = "agent-rig-token";
const JFS = process.env.JFS || "/jfs";
const CLAUDE = process.env.CLAUDE_BIN || "/app/node_modules/@anthropic-ai/claude-agent-sdk-linux-x64/claude";
const SELF = process.env.SELF_URL || `http://lu-old:${PORT}`;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const calls = [];
const procs = new Map(); // id -> {conversation, args, startedAt, exited}
const owners = new Map(); // conversation -> owner email (from /internal/cc/sandbox)
let nextProc = 1;
const log = (o) => { const e = { t: new Date().toISOString(), ...o }; calls.push(e); if (calls.length > 5000) calls.shift(); console.log(JSON.stringify(e)); };
const frame = (t, b) => { const p = Buffer.isBuffer(b) ? b : Buffer.from(b ?? ""); const h = Buffer.alloc(5); h[0] = t; h.writeUInt32BE(p.length, 1); return Buffer.concat([h, p]); };

const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    const json = (status, o) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(o)); };
    if (req.url === "/healthz") return json(200, { ok: true, draining: false });
    if (req.url === "/__rig/calls") return json(200, calls);
    if (req.url === "/__rig/procs") return json(200, [...procs.entries()].map(([id, p]) => ({ id, ...p, child: undefined })));
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return json(401, { error: "unauthorized" });
    let b = {};
    try { b = JSON.parse(body || "{}"); } catch { return json(400, { error: "bad json" }); }
    log({ call: req.url, body: b });
    if (req.url === "/internal/cc/sandbox") {
      if (!UUID.test(String(b.conversation))) return json(400, { error: "conversation must be a uuid" });
      owners.set(b.conversation, b.owner);
      return json(200, { id: `sbx-${b.conversation.slice(0, 8)}`, url: SELF, token: AGENT_TOKEN, headers: { "x-rig-conversation": b.conversation } });
    }
    if (req.url.startsWith("/internal/cc/")) return json(200, { ok: true, hinted: false });
    if (req.url.startsWith("/internal/ops/")) return json(200, { ok: true });
    return json(404, { error: "not found" });
  });
});

server.on("upgrade", (req, sock) => {
  const given = req.headers["x-jarvis-exec-token"] || (/^Bearer\s+(\S+)$/i.exec(req.headers.authorization || "") || [])[1];
  const conversation = String(req.headers["x-rig-conversation"] || "");
  if (given !== AGENT_TOKEN || req.url !== "/proc" || !UUID.test(conversation)) { sock.end("HTTP/1.1 401 Unauthorized\r\n\r\n"); return; }
  let spec;
  try { spec = JSON.parse(Buffer.from(String(req.headers["x-proc"]), "base64").toString("utf8")); } catch { sock.end("HTTP/1.1 400 Bad Request\r\n\r\n"); return; }
  const owner = owners.get(conversation);
  if (!owner) { sock.end("HTTP/1.1 409 Conflict\r\n\r\n"); return; }
  const ws = path.join(JFS, "ws", conversation);
  // ~/.claude is the person's folder on JuiceFS, as in the old sandboxes (and the ui's own ~/.claude)
  const claudeDir = path.join(JFS, "users", userKey(owner), "claude");
  for (const d of [ws, claudeDir]) { mkdirSync(d, { recursive: true }); chownSync(d, 1000, 1000); }
  const args = [...spec.args];
  if (args[0] === "claude") args[0] = CLAUDE;
  const id = nextProc++;
  procs.set(id, { conversation, args: spec.args.slice(0, 4), startedAt: new Date().toISOString(), exited: null });
  log({ proc: id, conversation, args: spec.args.slice(0, 3), env: Object.keys(spec.env || {}) });
  sock.write("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: jarvis-proc\r\n\r\n");
  // a mount namespace of its own: /workspace is this conversation's folder, then uid 1000
  const child = spawn("unshare", ["-m", "--propagation", "private", "sh", "-c",
    'mount --bind "$0" /workspace && mkdir -p /home/sandbox/.claude && mount --bind "$1" /home/sandbox/.claude && shift && cd /workspace && exec setpriv --reuid=1000 --regid=1000 --clear-groups "$@"', ws, claudeDir, ...args], {
    env: {
      PATH: process.env.PATH, HOME: "/home/sandbox", LANG: "C.UTF-8",
      ANTHROPIC_BASE_URL: process.env.MOCK_URL, ANTHROPIC_API_KEY: "sk-ant-rig", ANTHROPIC_MODEL: "claude-sonnet-5-5",
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1", DISABLE_AUTOUPDATER: "1", ...(spec.env || {}),
    },
  });
  child.stdout.on("data", (d) => { if (!sock.destroyed) sock.write(frame(1, d)); });
  child.stderr.on("data", (d) => { if (!sock.destroyed) sock.write(frame(2, d)); });
  child.on("exit", (code, signal) => {
    procs.get(id).exited = { code, signal, at: new Date().toISOString() };
    log({ proc: id, exit: code, signal });
    if (!sock.destroyed) sock.end(frame(3, JSON.stringify({ code, signal })));
  });
  child.on("error", (e) => log({ proc: id, error: String(e.message) }));
  let buf = Buffer.alloc(0);
  sock.on("data", (c) => {
    buf = Buffer.concat([buf, c]);
    while (buf.length >= 5) {
      const n = buf.readUInt32BE(1);
      if (buf.length < 5 + n) break;
      const t = buf[0]; const p = buf.subarray(5, 5 + n); buf = buf.subarray(5 + n);
      if (t === 0) child.stdin.write(p); else if (t === 4) child.stdin.end(); else if (t === 5) child.kill(p.toString() || "SIGTERM");
    }
  });
  sock.on("error", () => {});
  // the agent's own rule: a dropped /proc connection kills the process (what the switch must never cause)
  sock.on("close", () => { if (child.exitCode === null && child.signalCode === null) { log({ proc: id, cut: true, conversation }); child.kill("SIGTERM"); } });
});

server.listen(PORT, "0.0.0.0", () => console.log(JSON.stringify({ listening: PORT })));
