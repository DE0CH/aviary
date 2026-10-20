#!/usr/bin/env node
// The sandbox agent (docs/CHAT.md, "sandbox-agent"): the only process the server talks to inside a sandbox pod.
// It writes the files the server pushes, runs commands in /work, and reports which files changed since a cursor so
// the server can copy them back into the conversation's workdir. Plain Node, no dependencies.
//
// Env: JARVIS_EXEC_TOKEN (required; removed from the environment commands see), JARVIS_WORK_DIR (default /work),
// PORT (default 7070). The pod runs it as uid 1000 under tini (which reaps orphaned processes).
import { spawn } from "node:child_process";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { closeSync, lstatSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, statSync, chmodSync } from "node:fs";
import { createServer } from "node:http";
import { createWriteStream } from "node:fs";
import { dirname, join, normalize, sep } from "node:path";
import { END, entryHeader, entryPadding, unpack } from "./tar.mjs";

const TOKEN = process.env.JARVIS_EXEC_TOKEN || "";
delete process.env.JARVIS_EXEC_TOKEN;
const WORK = realpathSync(process.env.JARVIS_WORK_DIR || "/work");
const PORT = Number(process.env.PORT || 7070);
const BOOT_ID = randomBytes(8).toString("hex");
const MAX_OUT = 1024 * 1024;
const MAX_FILE = 50 * 1024 * 1024;
const EXCLUDED_DIRS = new Set(["node_modules", ".venv", "venv", "__pycache__", ".cache"]);
const HEADER_MAX = 8 * 1024;
export const META_ENTRY = ".jarvis-meta/changes.json";

if (!TOKEN) {
  console.error("JARVIS_EXEC_TOKEN is not set");
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

// ---- HTTP ----------------------------------------------------------------------------------------------------

function authorized(req) {
  const m = /^Bearer\s+(\S+)$/i.exec(req.headers.authorization || "");
  if (!m) return false;
  const a = createHash("sha256").update(m[1]).digest();
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

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url || "/", "http://agent");
    if (url.pathname === "/healthz") return json(res, 200, { ok: true, boot_id: BOOT_ID });
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
server.requestTimeout = 0; // /exec may run for up to 10 minutes
server.headersTimeout = 60_000;
// longer than any client's idle keep-alive, so a reused connection is never closed under a request in flight
server.keepAliveTimeout = 120_000;
server.listen(PORT, "::", () => console.log(JSON.stringify({ at: "sandbox agent listening", port: PORT, work: WORK, boot_id: BOOT_ID })));
process.on("SIGTERM", () => server.close(() => process.exit(0)));
