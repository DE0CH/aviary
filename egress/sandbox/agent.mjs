#!/usr/bin/env node
// The sandbox agent (docs/CHAT.md, "sandbox-agent"): the only process the server talks to inside a sandbox pod.
// It writes the files the server pushes, runs commands in /work, and reports which files changed since a cursor so
// the server can copy them back into the conversation's workdir. Plain Node, no dependencies.
//
// Env: JARVIS_EXEC_TOKEN (removed from the environment commands see), JARVIS_WORK_DIR (default /work), PORT (default
// 7070). The pod runs it as uid 1000 under tini (which reaps orphaned processes).
//
// JARVIS_INIT=1 instead of a token (a hosted provider's template, docs/CHAT.md "Sandbox providers"): the agent starts
// when the template is built, before any sandbox exists, so it waits for one `POST /init` {token, env, wire?} from the
// server: the exec token, the env every command gets (proxy, mirrors) and, with `wire` {url, headers}, the local proxy
// on 127.0.0.1:3128 that carries each TCP connection to egress over a WebSocket. Until then /healthz answers 503;
// /livez always answers 200 (the template's ready check).
import { spawn } from "node:child_process";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { closeSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, statSync, chmodSync } from "node:fs";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { createWriteStream } from "node:fs";
import { dirname, join, normalize, sep } from "node:path";
import { END, entryHeader, entryPadding, unpack } from "./tar.mjs";

let TOKEN = process.env.JARVIS_EXEC_TOKEN || "";
delete process.env.JARVIS_EXEC_TOKEN;
const AWAIT_INIT = !TOKEN && process.env.JARVIS_INIT === "1";
delete process.env.JARVIS_INIT;
const WORK = realpathSync(process.env.JARVIS_WORK_DIR || "/work");
const PORT = Number(process.env.PORT || 7070);
const BOOT_ID = randomBytes(8).toString("hex");
const MAX_OUT = 1024 * 1024;
const MAX_FILE = 50 * 1024 * 1024;
const EXCLUDED_DIRS = new Set(["node_modules", ".venv", "venv", "__pycache__", ".cache"]);
const HEADER_MAX = 8 * 1024;
export const META_ENTRY = ".jarvis-meta/changes.json";

if (!TOKEN && !AWAIT_INIT) {
  console.error("JARVIS_EXEC_TOKEN is not set (or JARVIS_INIT=1 to wait for POST /init)");
  process.exit(2);
}

// ---- paths ---------------------------------------------------------------------------------------------------

/** The absolute path for a workspace-relative path, or throws (absolute paths, "..", escapes through symlinks). */
function inWork(rel) {
  if (typeof rel !== "string" || !rel || rel.includes("\0") || rel.startsWith("/")) throw new Error(`bad path ${JSON.stringify(rel)}`);
  const n = normalize(rel);
  if (n === "." || n.startsWith("..") || n.split(sep).includes("..")) throw new Error(`bad path ${JSON.stringify(rel)}`);
  return join(WORK, n);
}

/** Creates the parent directories and checks that the real parent is still inside /work. */
function safeParent(abs) {
  const parent = dirname(abs);
  mkdirSync(parent, { recursive: true });
  const real = realpathSync(parent);
  if (real !== WORK && !real.startsWith(WORK + sep)) throw new Error(`path escapes the workspace: ${abs.slice(WORK.length + 1)}`);
}

// ---- change tracking -----------------------------------------------------------------------------------------

/** path -> signature, for every regular file under /work outside the excluded directories */
function scan() {
  const files = new Map();
  const walk = (dir, rel) => {
    let names;
    try {
      names = readdirSync(dir);
    } catch {
      return;
    }
    for (const name of names) {
      const abs = join(dir, name);
      const r = rel ? `${rel}/${name}` : name;
      let st;
      try {
        st = lstatSync(abs);
      } catch {
        continue;
      }
      if (st.isDirectory()) {
        if (EXCLUDED_DIRS.has(name) || r === ".git/objects" || r.endsWith("/.git/objects")) continue;
        walk(abs, r);
      } else if (st.isFile()) {
        files.set(r, { sig: `${st.mtimeMs}:${st.size}:${st.ctimeMs}`, size: st.size });
      }
    }
  };
  walk(WORK, "");
  return files;
}

const snapshots = new Map(); // seq -> Map(path -> { sig, size })
let seq = 0;

function remember(files) {
  seq++;
  snapshots.set(seq, files);
  for (const k of snapshots.keys()) if (k <= seq - 8) snapshots.delete(k);
  return `${BOOT_ID}:${seq}`;
}

/** After the server wrote or deleted files, it already has them: keep them out of the next /changes. */
function absorb(written, deleted) {
  for (const snap of snapshots.values()) {
    for (const rel of written) {
      try {
        const st = statSync(join(WORK, rel));
        snap.set(rel, { sig: `${st.mtimeMs}:${st.size}:${st.ctimeMs}`, size: st.size });
      } catch {
        snap.delete(rel);
      }
    }
    for (const rel of deleted) for (const k of [...snap.keys()]) if (k === rel || k.startsWith(rel + "/")) snap.delete(k);
  }
}

// ---- processes -----------------------------------------------------------------------------------------------

function tail() {
  let chunks = [];
  let size = 0;
  let truncated = false;
  return {
    push(c) {
      chunks.push(c);
      size += c.length;
      while (size > MAX_OUT && chunks.length) {
        const over = size - MAX_OUT;
        const first = chunks[0];
        truncated = true;
        if (first.length <= over) {
          chunks.shift();
          size -= first.length;
        } else {
          chunks[0] = first.subarray(over);
          size -= over;
        }
      }
    },
    text() {
      return Buffer.concat(chunks).toString("utf8");
    },
    get truncated() {
      return truncated;
    },
  };
}

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

function runForeground(command, timeoutMs) {
  return new Promise((resolve) => {
    const t0 = Date.now();
    const child = spawn("bash", ["-lc", command], { cwd: WORK, detached: true, stdio: ["ignore", "pipe", "pipe"], env: process.env });
    const out = tail();
    const err = tail();
    child.stdout.on("data", (c) => out.push(c));
    child.stderr.on("data", (c) => err.push(c));
    let interrupted = false;
    let killTimer = null;
    const timer = setTimeout(() => {
      interrupted = true;
      killGroup(child.pid, "SIGTERM");
      killTimer = setTimeout(() => killGroup(child.pid, "SIGKILL"), 2000);
    }, timeoutMs);
    child.on("error", (e) => {
      clearTimeout(timer);
      resolve({ stdout: "", stderr: String(e.message), exit_code: 127, interrupted: false, ms: Date.now() - t0, truncated: false });
    });
    child.on("exit", (code, signal) => {
      clearTimeout(timer);
      if (killTimer) clearTimeout(killTimer);
      // a process the command left running may hold the pipes open: give them a moment, then stop reading
      const finish = () => {
        child.stdout.destroy();
        child.stderr.destroy();
        const signum = signal ? { SIGHUP: 1, SIGINT: 2, SIGKILL: 9, SIGTERM: 15 }[signal] ?? 0 : 0;
        resolve({
          stdout: out.text(),
          stderr: err.text(),
          exit_code: code ?? (signal ? 128 + signum : -1),
          interrupted,
          ms: Date.now() - t0,
          truncated: out.truncated || err.truncated,
        });
      };
      let left = 2;
      const one = () => --left === 0 && finish();
      child.stdout.once("close", one);
      child.stderr.once("close", one);
      setTimeout(() => left > 0 && ((left = 0), finish()), 200);
    });
  });
}

function runBackground(command) {
  const id = randomBytes(6).toString("hex");
  mkdirSync(join(WORK, ".jobs"), { recursive: true });
  const fd = openSync(join(WORK, ".jobs", `${id}.log`), "a");
  const child = spawn("bash", ["-lc", command], { cwd: WORK, detached: true, stdio: ["ignore", fd, fd], env: process.env });
  closeSync(fd);
  child.on("error", () => {});
  child.unref();
  return { background_id: id, pid: child.pid, log: `.jobs/${id}.log` };
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

async function putFiles(req, res) {
  let deletes = [];
  try {
    deletes = JSON.parse(String(req.headers["x-delete"] || "[]"));
    if (!Array.isArray(deletes)) throw new Error();
  } catch {
    return json(res, 400, { ok: false, error: "X-Delete must be a JSON array of paths" });
  }
  const written = [];
  const deleted = [];
  try {
    for (const rel of deletes) {
      const abs = inWork(rel);
      safeParent(abs);
      rmSync(abs, { recursive: true, force: true });
      deleted.push(normalize(rel));
    }
    await unpack(req, async (h, body) => {
      const abs = inWork(h.name);
      safeParent(abs);
      try {
        const st = lstatSync(abs);
        if (st.isDirectory()) rmSync(abs, { recursive: true, force: true });
        else if (!st.isFile()) rmSync(abs, { force: true });
      } catch {
        /* new file */
      }
      const tmp = `${abs}.jarvis-${randomBytes(4).toString("hex")}`;
      await new Promise((resolve, reject) => {
        const ws = createWriteStream(tmp, { mode: 0o600 });
        body.on("error", reject);
        ws.on("error", reject);
        ws.on("finish", resolve);
        body.pipe(ws);
      });
      chmodSync(tmp, (h.mode & 0o777) | 0o600);
      renameSync(tmp, abs);
      written.push(normalize(h.name));
    });
  } catch (e) {
    return json(res, 400, { ok: false, error: String(e.message), written: written.length, deleted: deleted.length });
  }
  absorb(written, deleted);
  json(res, 200, { ok: true, written: written.length, deleted: deleted.length });
}

async function changes(req, res, since) {
  const [boot, n] = String(since || "").split(":");
  const prev = boot === BOOT_ID ? snapshots.get(Number(n)) : undefined;
  const now = scan();
  const changed = [];
  const skipped = [];
  for (const [rel, f] of now) {
    const p = prev?.get(rel);
    if (p && p.sig === f.sig) continue;
    if (f.size > MAX_FILE) skipped.push(rel);
    else changed.push(rel);
  }
  const deleted = prev ? [...prev.keys()].filter((k) => !now.has(k)) : [];
  const cursor = remember(now);
  const delHeader = JSON.stringify(deleted);
  const skipHeader = JSON.stringify(skipped);
  const headers = { "content-type": "application/x-tar", "x-cursor": cursor, "x-full": prev ? "0" : "1" };
  let meta = null;
  if (delHeader.length + skipHeader.length > HEADER_MAX) {
    // too long for a header: the lists go first in the tar, as .jarvis-meta/changes.json
    headers["x-meta-entry"] = META_ENTRY;
    meta = Buffer.from(JSON.stringify({ deleted, skipped }));
  } else {
    headers["x-deleted"] = delHeader;
    headers["x-skipped"] = skipHeader;
  }
  res.writeHead(200, headers);
  const write = (b) => (res.write(b) ? Promise.resolve() : new Promise((r) => res.once("drain", r)));
  if (meta) await write(Buffer.concat([entryHeader(META_ENTRY, meta.length), meta, entryPadding(meta.length)]));
  for (const rel of changed) {
    let data;
    let mode = 0o644;
    let mtime = Date.now() / 1000;
    try {
      const st = lstatSync(join(WORK, rel));
      if (!st.isFile()) continue;
      data = readFileSync(join(WORK, rel));
      mode = st.mode;
      mtime = st.mtimeMs / 1000;
    } catch {
      continue; // gone since the scan: the next /changes reports it deleted
    }
    await write(Buffer.concat([entryHeader(rel, data.length, mode, mtime), data, entryPadding(data.length)]));
  }
  res.end(END);
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
  socket.on("close", () => {
    if (exited) return;
    killGroup(child.pid, "SIGTERM");
    setTimeout(() => !exited && killGroup(child.pid, "SIGKILL"), 2000).unref();
  });
}

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url || "/", "http://agent");
    if (url.pathname === "/livez") return json(res, 200, { ok: true });
    if (url.pathname === "/healthz") return TOKEN ? json(res, 200, { ok: true, boot_id: BOOT_ID }) : json(res, 503, { ok: false, waiting: "init" });
    if (req.method === "POST" && url.pathname === "/init") return await init(req, res);
    if (!authorized(req)) return json(res, 401, { error: "unauthorized" });
    if (req.method === "PUT" && url.pathname === "/files") return await putFiles(req, res);
    if (req.method === "GET" && url.pathname === "/changes") return await changes(req, res, url.searchParams.get("since"));
    if (req.method === "POST" && url.pathname === "/exec") {
      const b = await readJson(req);
      if (typeof b.command !== "string" || !b.command.trim()) return json(res, 400, { error: "command is required" });
      if (b.background) return json(res, 200, runBackground(b.command));
      const timeout = Math.min(Math.max(Number(b.timeout_ms) || 120_000, 1000), 600_000);
      return json(res, 200, await runForeground(b.command, timeout));
    }
    json(res, 404, { error: "not found" });
  } catch (e) {
    if (!res.headersSent) json(res, 500, { error: String(e?.message || e) });
    else res.destroy();
  }
});
server.on("upgrade", onProcUpgrade);
server.requestTimeout = 0; // /exec may run for up to 10 minutes
server.headersTimeout = 60_000;
// longer than any client's idle keep-alive, so a reused connection is never closed under a request in flight
server.keepAliveTimeout = 120_000;
server.listen(PORT, "::", () => console.log(JSON.stringify({ at: "sandbox agent listening", port: PORT, work: WORK, boot_id: BOOT_ID })));
process.on("SIGTERM", () => server.close(() => process.exit(0)));
