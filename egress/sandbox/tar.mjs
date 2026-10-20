// A small tar (ustar + PAX long names) reader and writer for regular files only: the sandbox agent's PUT /files and
// GET /changes, and anything on the server side that talks to it. No dependencies.
import { Readable } from "node:stream";

const BLOCK = 512;

function octal(n, width) {
  return n.toString(8).padStart(width - 1, "0") + "\0";
}

function header(name, size, mode, mtime, type = "0") {
  const h = Buffer.alloc(BLOCK);
  h.write(name.slice(0, 100), 0, "utf8");
  h.write(octal(mode & 0o7777, 8), 100);
  h.write(octal(0, 8), 108);
  h.write(octal(0, 8), 116);
  h.write(octal(size, 12), 124);
  h.write(octal(Math.floor(mtime), 12), 136);
  h.write("        ", 148);
  h.write(type, 156);
  h.write("ustar\0", 257);
  h.write("00", 263);
  let sum = 0;
  for (const b of h) sum += b;
  h.write(octal(sum, 7) + " ", 148);
  return h;
}

function pad(size) {
  const r = size % BLOCK;
  return r ? Buffer.alloc(BLOCK - r) : Buffer.alloc(0);
}

function paxRecord(key, value) {
  const body = ` ${key}=${value}\n`;
  let len = Buffer.byteLength(body);
  let total = len + String(len).length;
  if (String(total).length !== String(len).length) total = len + String(total).length;
  return `${total}${body}`;
}

/** The header blocks for one regular file (a PAX header first when the name is long or not ASCII). */
export function entryHeader(name, size, mode = 0o644, mtimeSec = Date.now() / 1000) {
  const parts = [];
  if (Buffer.byteLength(name) > 100 || /[^\x20-\x7e]/.test(name)) {
    const pax = Buffer.from(paxRecord("path", name));
    parts.push(header("PaxHeader", pax.length, 0o644, mtimeSec, "x"), pax, pad(pax.length));
  }
  parts.push(header(name, size, mode, mtimeSec));
  return Buffer.concat(parts);
}

export const entryPadding = pad;
export const END = Buffer.alloc(BLOCK * 2);

/** A tar of in-memory files: [{ name, data: Buffer|string, mode?, mtime? }]. */
export function packBuffers(files) {
  const parts = [];
  for (const f of files) {
    const data = Buffer.isBuffer(f.data) ? f.data : Buffer.from(f.data);
    parts.push(entryHeader(f.name, data.length, f.mode ?? 0o644, f.mtime ?? Date.now() / 1000), data, pad(data.length));
  }
  parts.push(END);
  return Buffer.concat(parts);
}

function parseOctal(buf) {
  const s = buf.toString("latin1").replace(/\0.*$/s, "").trim();
  return s ? parseInt(s, 8) : 0;
}

function parsePax(buf) {
  const out = {};
  let i = 0;
  while (i < buf.length) {
    const sp = buf.indexOf(0x20, i);
    if (sp < 0) break;
    const len = parseInt(buf.toString("latin1", i, sp), 10);
    if (!len) break;
    const rec = buf.subarray(sp + 1, i + len - 1).toString("utf8");
    const eq = rec.indexOf("=");
    out[rec.slice(0, eq)] = rec.slice(eq + 1);
    i += len;
  }
  return out;
}

/**
 * Reads a tar stream and calls onFile({ name, mode, mtime, size }, body: Readable) for each regular file, one at a time
 * (the next entry waits until the body has been consumed). Other entry types are skipped.
 */
export async function unpack(stream, onFile) {
  let buf = Buffer.alloc(0);
  let pending = null; // { need, resolve }
  let ended = false;
  let failed = null;
  const wake = () => {
    if (pending && (buf.length >= pending.need || ended || failed)) {
      const p = pending;
      pending = null;
      p.resolve();
    }
  };
  stream.on("data", (c) => {
    buf = buf.length ? Buffer.concat([buf, c]) : c;
    if (buf.length > 4 * 1024 * 1024) stream.pause();
    wake();
  });
  stream.on("end", () => {
    ended = true;
    wake();
  });
  stream.on("error", (e) => {
    failed = e;
    wake();
  });
  const need = async (n) => {
    while (buf.length < n) {
      if (failed) throw failed;
      if (ended) return false;
      stream.resume();
      await new Promise((resolve) => (pending = { need: n, resolve }));
    }
    return true;
  };
  const take = (n) => {
    const out = buf.subarray(0, n);
    buf = buf.subarray(n);
    if (buf.length < 1024 * 1024) stream.resume();
    return out;
  };
  let paxName = null;
  for (;;) {
    if (!(await need(BLOCK))) return;
    const h = take(BLOCK);
    if (h.every((b) => b === 0)) return;
    const size = parseOctal(h.subarray(124, 136));
    const type = String.fromCharCode(h[156] || 48);
    const prefix = h.subarray(345, 500).toString("utf8").replace(/\0.*$/s, "");
    let name = h.subarray(0, 100).toString("utf8").replace(/\0.*$/s, "");
    if (prefix && h.subarray(257, 262).toString("latin1") === "ustar") name = `${prefix}/${name}`;
    const padded = size + ((BLOCK - (size % BLOCK)) % BLOCK);
    if (type === "x" || type === "g") {
      if (!(await need(padded))) throw new Error("tar: truncated PAX header");
      const pax = parsePax(take(padded).subarray(0, size));
      if (type === "x" && pax.path) paxName = pax.path;
      continue;
    }
    if (paxName) {
      name = paxName;
      paxName = null;
    }
    if (type !== "0" && type !== "\0" && type !== "7") {
      // directories, links, devices: skip their data
      let left = padded;
      while (left > 0) {
        if (!(await need(1))) throw new Error("tar: truncated");
        left -= take(Math.min(left, buf.length)).length;
      }
      continue;
    }
    let left = size;
    const body = new Readable({ read() {} });
    const done = onFile({ name, size, mode: parseOctal(h.subarray(100, 108)), mtime: parseOctal(h.subarray(136, 148)) }, body);
    while (left > 0) {
      if (!(await need(1))) {
        body.destroy(new Error("tar: truncated file"));
        throw new Error("tar: truncated file");
      }
      const chunk = take(Math.min(left, buf.length));
      left -= chunk.length;
      body.push(chunk);
    }
    body.push(null);
    await done;
    let skip = padded - size;
    while (skip > 0) {
      if (!(await need(1))) break;
      skip -= take(Math.min(skip, buf.length)).length;
    }
  }
}
