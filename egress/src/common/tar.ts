// A minimal tar (ustar + PAX path records) writer and reader for regular files only: what moves a workspace between
// the server and a sandbox (docs/CHAT.md, "sandbox-agent"). Directories are implied by paths; links, devices and
// absolute or `..` paths are refused on read.
const BLOCK = 512;

function octal(n: number, width: number): string {
  return n.toString(8).padStart(width - 1, "0") + "\0";
}

function header(name: string, size: number, mode: number, mtimeSec: number, type: string, prefix = ""): Buffer {
  const h = Buffer.alloc(BLOCK, 0);
  h.write(name, 0, 100, "utf8");
  h.write(octal(mode & 0o7777, 8), 100, 8, "ascii");
  h.write(octal(1000, 8), 108, 8, "ascii");
  h.write(octal(1000, 8), 116, 8, "ascii");
  h.write(octal(size, 12), 124, 12, "ascii");
  h.write(octal(Math.max(0, Math.floor(mtimeSec)), 12), 136, 12, "ascii");
  h.write("        ", 148, 8, "ascii");
  h.write(type, 156, 1, "ascii");
  h.write("ustar\0", 257, 6, "ascii");
  h.write("00", 263, 2, "ascii");
  h.write(prefix, 345, 155, "utf8");
  let sum = 0;
  for (const b of h) sum += b;
  h.write(octal(sum, 7) + " ", 148, 8, "ascii");
  return h;
}

const pad = (n: number): Buffer => Buffer.alloc((BLOCK - (n % BLOCK)) % BLOCK, 0);

export interface TarEntry {
  path: string;
  data: Uint8Array;
  mode?: number;
  mtimeMs?: number;
}

export function tarPack(entries: TarEntry[]): Buffer {
  const parts: Buffer[] = [];
  for (const e of entries) {
    const mtime = (e.mtimeMs ?? Date.now()) / 1000;
    const nameBytes = Buffer.byteLength(e.path);
    if (nameBytes > 100) {
      // PAX extended header with the full path
      const rec = (k: string, v: string) => {
        const body = ` ${k}=${v}\n`;
        const n = Buffer.byteLength(body);
        let len = n + 1;
        while (String(len).length + n !== len) len = String(len).length + n;
        return `${len}${body}`;
      };
      const pax = Buffer.from(rec("path", e.path), "utf8");
      parts.push(header("PaxHeader", pax.length, 0o644, mtime, "x"), pax, pad(pax.length));
    }
    const short = nameBytes > 100 ? Buffer.from(e.path).subarray(0, 99).toString("utf8").replace(/�+$/, "") : e.path;
    parts.push(header(short, e.data.length, e.mode ?? 0o644, mtime, "0"), Buffer.from(e.data), pad(e.data.length));
  }
  parts.push(Buffer.alloc(BLOCK * 2, 0));
  return Buffer.concat(parts);
}

function str(b: Buffer, off: number, len: number): string {
  const s = b.subarray(off, off + len);
  const z = s.indexOf(0);
  return (z >= 0 ? s.subarray(0, z) : s).toString("utf8");
}

function num(b: Buffer, off: number, len: number): number {
  const s = str(b, off, len).trim();
  return s ? parseInt(s, 8) : 0;
}

/** A safe relative path ("a/b.txt"), or null for anything absolute, empty or climbing out. */
export function safeRelPath(p: string): string | null {
  const parts = p.replace(/\\/g, "/").split("/").filter((x) => x && x !== ".");
  if (!parts.length || parts.some((x) => x === "..") || p.startsWith("/")) return null;
  return parts.join("/");
}

export function tarUnpack(buf: Buffer): TarEntry[] {
  const out: TarEntry[] = [];
  let off = 0;
  let paxPath: string | null = null;
  let longName: string | null = null;
  while (off + BLOCK <= buf.length) {
    const h = buf.subarray(off, off + BLOCK);
    if (h.every((x) => x === 0)) break;
    const size = num(h, 124, 12);
    const type = String.fromCharCode(h[156] || 0x30);
    const dataStart = off + BLOCK;
    const data = buf.subarray(dataStart, dataStart + size);
    off = dataStart + size + ((BLOCK - (size % BLOCK)) % BLOCK);
    if (type === "x") {
      const text = data.toString("utf8");
      for (const m of text.matchAll(/\d+ ([^=]+)=([^\n]*)\n/g)) if (m[1] === "path") paxPath = m[2]!;
      continue;
    }
    if (type === "L") {
      longName = str(data, 0, data.length);
      continue;
    }
    if (type === "g") continue;
    const prefix = str(h, 345, 155);
    let name = paxPath ?? longName ?? (prefix ? `${prefix}/${str(h, 0, 100)}` : str(h, 0, 100));
    paxPath = null;
    longName = null;
    if (type !== "0" && type !== "\0" && type !== "7") continue; // regular files only
    const rel = safeRelPath(name);
    if (!rel) throw new Error(`tar: refused path ${JSON.stringify(name)}`);
    name = rel;
    out.push({ path: name, data: Buffer.from(data), mode: num(h, 100, 8), mtimeMs: num(h, 136, 12) * 1000 });
  }
  return out;
}
