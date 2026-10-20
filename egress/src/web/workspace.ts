// A conversation's workspace on the server: walking it (with the sync excludes), hashing, and the small JSON
// manifests that let the server, the sandbox and the bucket exchange only what changed (docs/CHAT.md).
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

/** Never synced or stored: dependency trees, caches, git objects, claw's own state. Files over MAX_FILE_BYTES too. */
const EXCLUDED_DIRS = new Set(["node_modules", ".venv", "venv", "__pycache__", ".cache", ".claw", ".jobs"]);
export const MAX_FILE_BYTES = 50 * 1024 * 1024;

export function excluded(rel: string): boolean {
  const parts = rel.split("/");
  for (let i = 0; i < parts.length - 1; i++) {
    const p = parts[i]!;
    if (EXCLUDED_DIRS.has(p)) return true;
    if (p === ".git" && parts[i + 1] === "objects") return true;
  }
  return false;
}

export interface FileStat {
  size: number;
  mtimeMs: number;
}

/** Regular files under `root` (relative paths, `/`-separated), excluded ones left out. Symlinks are not followed. */
export async function walk(root: string): Promise<Map<string, FileStat>> {
  const out = new Map<string, FileStat>();
  const visit = async (dir: string, rel: string): Promise<void> => {
    let names: string[];
    try {
      names = await readdir(dir);
    } catch {
      return;
    }
    for (const name of names) {
      const r = rel ? `${rel}/${name}` : name;
      const st = await lstat(join(dir, name)).catch(() => null);
      if (!st) continue;
      if (st.isDirectory()) {
        if (EXCLUDED_DIRS.has(name) || (name === "objects" && rel.endsWith(".git"))) continue;
        await visit(join(dir, name), r);
      } else if (st.isFile() && !excluded(r) && st.size <= MAX_FILE_BYTES) {
        out.set(r, { size: st.size, mtimeMs: Math.floor(st.mtimeMs) });
      }
    }
  };
  await visit(root, "");
  return out;
}

export function sha256File(path: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const h = createHash("sha256");
    createReadStream(path)
      .on("data", (c) => h.update(c))
      .on("end", () => resolve(h.digest("hex")))
      .on("error", reject);
  });
}

export const sha256Buf = (b: Uint8Array): string => createHash("sha256").update(b).digest("hex");

export interface ManifestEntry extends FileStat {
  sha256: string;
}
export type Manifest = Record<string, ManifestEntry>;

/** Current manifest of `root`, hashing only files whose size or mtime differ from `prev`. */
export async function manifestOf(root: string, prev: Manifest = {}): Promise<Manifest> {
  const out: Manifest = {};
  for (const [rel, st] of await walk(root)) {
    const p = prev[rel];
    out[rel] = p && p.size === st.size && p.mtimeMs === st.mtimeMs ? p : { ...st, sha256: await sha256File(join(root, rel)) };
  }
  return out;
}

export async function readJson<T>(path: string, fallback: T): Promise<T> {
  try {
    return JSON.parse(await readFile(path, "utf8")) as T;
  } catch {
    return fallback;
  }
}

export async function writeJsonAtomic(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const tmp = `${path}.${process.pid}.${Date.now()}.tmp`;
  await writeFile(tmp, JSON.stringify(value));
  await rename(tmp, path);
}

/** Writes `data` at root/rel (creating directories), refusing paths that climb out of root. */
export async function writeInto(root: string, rel: string, data: Uint8Array, mtimeMs?: number): Promise<void> {
  if (rel.split("/").some((x) => x === ".." || x === "") || rel.startsWith("/")) throw new Error(`bad path ${rel}`);
  const p = join(root, rel);
  await mkdir(dirname(p), { recursive: true });
  // replace whatever is there (a symlink must not redirect the write)
  await rm(p, { force: true, recursive: false }).catch(() => {});
  await writeFile(p, data);
  if (mtimeMs) {
    const { utimes } = await import("node:fs/promises");
    await utimes(p, mtimeMs / 1000, mtimeMs / 1000).catch(() => {});
  }
}

export async function removeFrom(root: string, rel: string): Promise<void> {
  if (rel.split("/").some((x) => x === ".." || x === "") || rel.startsWith("/")) return;
  await rm(join(root, rel), { force: true });
}
