#!/usr/bin/env node
// The sandbox agent (docs/CHAT.md, "The sandbox agent"): the only process the server side talks to inside a
// sandbox. It answers a health check and runs long-lived processes over an upgraded connection (`GET /proc`): the
// CloudCLI UI runs Claude Code in the sandbox through it. Plain Node, no dependencies.
//
// Env: JARVIS_EXEC_TOKEN (removed from the environment processes see), PORT (default 7070). The sandbox runs it as
// uid 1000 under tini (which reaps orphaned processes).
//
// JARVIS_INIT=1 instead of a token (a hosted provider's template, provider e2b): the agent starts when the template is
// built, before any sandbox exists, so it waits for one `POST /init` {token, env, wire?} from the server: the exec
// token, the env every process gets (proxy, mirrors) and, with `wire` {url, headers}, the local proxy on
// 127.0.0.1:3128 that carries each TCP connection to egress over a WebSocket. Until then /healthz answers 503;
// /livez always answers 200 (the template's ready check).
import { spawn } from "node:child_process";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { mkdirSync } from "node:fs";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { normalize } from "node:path";

let TOKEN = process.env.JARVIS_EXEC_TOKEN || "";
delete process.env.JARVIS_EXEC_TOKEN;
const AWAIT_INIT = !TOKEN && process.env.JARVIS_INIT === "1";
delete process.env.JARVIS_INIT;
const PORT = Number(process.env.PORT || 7070);
const BOOT_ID = randomBytes(8).toString("hex");

if (!TOKEN && !AWAIT_INIT) {
  console.error("JARVIS_EXEC_TOKEN is not set (or JARVIS_INIT=1 to wait for POST /init)");
  process.exit(2);
}

// ---- processes -----------------------------------------------------------------------------------------------

function killGroup(pid, sig) {
  try {
    process.kill(-pid, sig);
  } catch {
    try {
      process.kill(pid, sig);
    } catch {
      /* gone */
    }
  }
}

// ---- the wire: local proxy -> egress over WebSocket ------------------------------------------------------------

const WIRE_PORT = Number(process.env.JARVIS_WIRE_PORT || 3128);
const WIRE_HIGH_WATER = 4 * 1024 * 1024;

/** Every TCP connection to 127.0.0.1:3128 becomes one WebSocket to egress's /wire, bytes relayed both ways. */
function startWire(url, headers) {
  const srv = createTcpServer((sock) => {
    sock.pause();
    sock.on("error", () => {});
    let ws;
    try {
      ws = new WebSocket(url, { headers });
    } catch (e) {
      console.error(JSON.stringify({ at: "wire", error: String(e?.message || e) }));
      return sock.destroy();
    }
    ws.binaryType = "arraybuffer";
    const pending = [];
    ws.onopen = () => {
      for (const c of pending.splice(0)) ws.send(c);
      sock.resume();
    };
    sock.on("data", (c) => {
      if (ws.readyState !== WebSocket.OPEN) return void pending.push(c);
      ws.send(c);
      if (ws.bufferedAmount > WIRE_HIGH_WATER) {
        sock.pause();
        const t = setInterval(() => {
          if (ws.readyState !== WebSocket.OPEN || ws.bufferedAmount < WIRE_HIGH_WATER / 4) {
            clearInterval(t);
            sock.resume();
          }
        }, 10);
      }
    });
    ws.onmessage = (e) => sock.write(Buffer.from(e.data));
    ws.onclose = () => sock.destroy();
    ws.onerror = () => sock.destroy();
    sock.on("end", () => ws.readyState <= WebSocket.OPEN && ws.close());
    sock.on("close", () => ws.readyState <= WebSocket.OPEN && ws.close());
  });
  srv.on("error", (e) => console.error(JSON.stringify({ at: "wire listen", error: String(e?.message || e) })));
  srv.listen(WIRE_PORT, "127.0.0.1");
}

/** POST /init: once, before anything else (JARVIS_INIT=1). */
async function init(req, res) {
  if (!AWAIT_INIT || TOKEN) return json(res, 409, { error: "already initialised" });
  const b = await readJson(req);
  if (typeof b.token !== "string" || b.token.length < 16) return json(res, 400, { error: "token is required" });
  const env = b.env && typeof b.env === "object" ? b.env : {};
  for (const [k, v] of Object.entries(env)) if (typeof v === "string") process.env[k] = v;
  if (b.wire) {
    if (typeof b.wire.url !== "string" || !/^wss?:\/\//.test(b.wire.url)) return json(res, 400, { error: "wire.url must be a ws(s) URL" });
    startWire(b.wire.url, b.wire.headers && typeof b.wire.headers === "object" ? b.wire.headers : {});
  }
  TOKEN = b.token;
  json(res, 200, { ok: true, boot_id: BOOT_ID });
}

// ---- HTTP ----------------------------------------------------------------------------------------------------

/** The exec token: `X-Jarvis-Exec-Token` (survives a provider's port proxy) or `Authorization: Bearer`. */
function authorized(req) {
  if (!TOKEN) return false;
  const m = /^Bearer\s+(\S+)$/i.exec(req.headers.authorization || "");
  const given = String(req.headers["x-jarvis-exec-token"] || "") || (m ? m[1] : "");
  if (!given) return false;
  const a = createHash("sha256").update(given).digest();
  const b = createHash("sha256").update(TOKEN).digest();
  return timingSafeEqual(a, b);
}

function json(res, status, body, headers = {}) {
  const s = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(s), ...headers });
  res.end(s);
}

async function readJson(req, limit = 1024 * 1024) {
  let size = 0;
  const chunks = [];
  for await (const c of req) {
    size += c.length;
    if (size > limit) throw new Error("body too large");
    chunks.push(c);
  }
  return chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : {};
}

// ---- long-running processes over an upgraded connection (docs/CLAUDE-CODE.md: Claude Code in the sandbox) ---------
//
// `GET /proc` with `Connection: Upgrade`, `Upgrade: jarvis-proc` and `X-Proc: <base64 JSON {args, cwd?, env?}>`
// (exec token as usual) → 101, then the socket carries frames both ways: 1 byte type, 4 bytes big-endian length,
// payload. Client → agent: 0 stdin bytes, 4 stdin end, 5 signal (payload: its name, e.g. "SIGTERM").
// Agent → client: 1 stdout, 2 stderr, 3 exit (payload JSON {code, signal}); the socket is closed after it.
// `args` is the argv (args[0] is looked up on PATH); cwd must be /workspace or a folder in it (default /workspace);
// env is added to the agent's own environment (the proxy and mirror settings) for that process only. A dropped
// connection kills the process (SIGTERM, SIGKILL 2 s later): the caller owns its life.
export const PROC_STDIN = 0, PROC_STDOUT = 1, PROC_STDERR = 2, PROC_EXIT = 3, PROC_STDIN_END = 4, PROC_SIGNAL = 5;
const WORKSPACE = process.env.JARVIS_WORKSPACE || "/workspace"; // overridable for tests only
const SIGNALS = new Set(["SIGTERM", "SIGINT", "SIGKILL", "SIGHUP", "SIGQUIT"]);

export function frame(type, payload) {
  const b = Buffer.isBuffer(payload) ? payload : Buffer.from(payload ?? "");
  const h = Buffer.alloc(5);
  h[0] = type;
  h.writeUInt32BE(b.length, 1);
  return Buffer.concat([h, b]);
}

/** Splits a byte stream into frames; calls on(type, payload) for each complete one. */
export function frameReader(on) {
  let buf = Buffer.alloc(0);
  return (chunk) => {
    buf = buf.length ? Buffer.concat([buf, chunk]) : chunk;
    while (buf.length >= 5) {
      const n = buf.readUInt32BE(1);
      if (n > 16 * 1024 * 1024) throw new Error("frame too large");
      if (buf.length < 5 + n) break;
      on(buf[0], buf.subarray(5, 5 + n));
      buf = buf.subarray(5 + n);
    }
  };
}

function procSpec(req) {
  let spec;
  try {
    spec = JSON.parse(Buffer.from(String(req.headers["x-proc"] || ""), "base64").toString("utf8"));
  } catch {
    throw new Error("X-Proc must be base64 JSON");
  }
  if (!Array.isArray(spec?.args) || !spec.args.length || !spec.args.every((a) => typeof a === "string" && !a.includes("\0"))) throw new Error("args must be a non-empty array of strings");
  const cwd = normalize(spec.cwd || WORKSPACE);
  if (cwd !== WORKSPACE && !cwd.startsWith(WORKSPACE + "/")) throw new Error("cwd must be /workspace or a folder in it");
  const env = { ...process.env };
  for (const [k, v] of Object.entries(spec.env || {})) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(k) || typeof v !== "string" || v.includes("\0")) throw new Error(`bad env ${k}`);
    env[k] = v;
  }
  return { args: spec.args, cwd, env };
}

function onProcUpgrade(req, socket) {
  const refuse = (status, msg) => {
    socket.end(`HTTP/1.1 ${status}\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n${msg}\n`);
  };
  const url = new URL(req.url || "/", "http://agent");
  if (url.pathname !== "/proc" || String(req.headers.upgrade || "").toLowerCase() !== "jarvis-proc") return refuse("404 Not Found", "not found");
  if (!authorized(req)) return refuse("401 Unauthorized", "unauthorized");
  let spec;
  try {
    spec = procSpec(req);
    mkdirSync(spec.cwd, { recursive: true });
  } catch (e) {
    return refuse("400 Bad Request", String(e.message || e));
  }
  socket.write("HTTP/1.1 101 Switching Protocols\r\nupgrade: jarvis-proc\r\nconnection: upgrade\r\n\r\n");
  socket.setNoDelay(true);
  const child = spawn(spec.args[0], spec.args.slice(1), { cwd: spec.cwd, env: spec.env, detached: true, stdio: ["pipe", "pipe", "pipe"] });
  let exited = false;
  const send = (type, payload) => {
    if (!socket.destroyed) socket.write(frame(type, payload));
  };
  child.stdout.on("data", (c) => send(PROC_STDOUT, c));
  child.stderr.on("data", (c) => send(PROC_STDERR, c));
  child.stdin.on("error", () => {}); // the process closed its stdin; later writes are dropped
  child.on("error", (e) => {
    if (exited) return;
    exited = true;
    send(PROC_STDERR, `${e.message}\n`);
    send(PROC_EXIT, JSON.stringify({ code: 127, signal: null }));
    socket.end();
  });
  child.on("close", (code, signal) => {
    if (exited) return;
    exited = true;
    send(PROC_EXIT, JSON.stringify({ code, signal }));
    socket.end();
  });
  const read = frameReader((type, payload) => {
    if (type === PROC_STDIN) child.stdin.write(payload);
    else if (type === PROC_STDIN_END) child.stdin.end();
    else if (type === PROC_SIGNAL) {
      const sig = payload.toString("utf8");
      if (SIGNALS.has(sig)) killGroup(child.pid, sig);
    }
  });
  socket.on("data", (c) => {
    try {
      read(c);
    } catch {
      socket.destroy();
    }
  });
  const dropped = () => {
    if (exited) return;
    killGroup(child.pid, "SIGTERM");
    setTimeout(() => !exited && killGroup(child.pid, "SIGKILL"), 2000).unref();
  };
  socket.on("close", dropped);
  // the HTTP server's sockets are half-open: a client that goes away only sends FIN ('end'), and 'close' would wait
  // for this side to end too (stdin's end is a frame, never a FIN)
  socket.on("end", () => {
    dropped();
    socket.destroy();
  });
}

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url || "/", "http://agent");
    if (url.pathname === "/livez") return json(res, 200, { ok: true });
    if (url.pathname === "/healthz") return TOKEN ? json(res, 200, { ok: true, boot_id: BOOT_ID }) : json(res, 503, { ok: false, waiting: "init" });
    if (req.method === "POST" && url.pathname === "/init") return await init(req, res);
    if (!authorized(req)) return json(res, 401, { error: "unauthorized" });
    json(res, 404, { error: "not found" });
  } catch (e) {
    if (!res.headersSent) json(res, 500, { error: String(e?.message || e) });
    else res.destroy();
  }
});
server.on("upgrade", onProcUpgrade);
server.headersTimeout = 60_000;
// longer than any client's idle keep-alive, so a reused connection is never closed under a request in flight
server.keepAliveTimeout = 120_000;
server.listen(PORT, "::", () => console.log(JSON.stringify({ at: "sandbox agent listening", port: PORT, boot_id: BOOT_ID })));
process.on("SIGTERM", () => server.close(() => process.exit(0)));
