// A conversation's files: bytes in the bucket under their sha256, metadata in `files`. The server's local workdir
// is a cache of them, reconciled before each turn and stored back after it (docs/CHAT.md, "Turn runner").
import { createHash } from "node:crypto";
import { createWriteStream } from "node:fs";
import { mkdir, readFile, rm } from "node:fs/promises";
import type { IncomingMessage } from "node:http";
import { join } from "node:path";
import { pipeline } from "node:stream/promises";
import { Transform } from "node:stream";
import { blobKey, type WebDeps } from "./ctx.ts";
import { HttpError } from "./http.ts";
import { excluded, manifestOf, readJson, removeFrom, writeInto, writeJsonAtomic, type Manifest } from "./workspace.ts";

export interface FileRow {
  path: string;
  sha256: string;
  size: string;
  source: "upload" | "agent";
  updated_at: Date;
}

export const dirs = (deps: WebDeps, conv: string) => {
  const base = join(deps.workRoot, conv);
  return {
    base,
    work: join(base, "work"),
    sessions: join(base, "sessions"),
    clawConfig: join(base, "claw-config"),
    syncState: join(base, "sync.json"),
    stored: join(base, "stored.json"),
  };
};

export async function liveFiles(deps: WebDeps, conv: string): Promise<FileRow[]> {
  const { rows } = await deps.db.query<FileRow>(
    "select path, sha256, size, source, updated_at from files where conversation_id = $1 and not deleted order by path",
    [conv],
  );
  return rows;
}

/** Make the local workdir match `files` (download what differs, delete what the table doesn't have). */
export async function restoreWorkdir(deps: WebDeps, conv: string): Promise<void> {
  const d = dirs(deps, conv);
  await mkdir(d.work, { recursive: true });
  const prev = await readJson<Manifest>(d.stored, {});
  const local = await manifestOf(d.work, prev);
  const rows = await liveFiles(deps, conv);
  const want = new Map(rows.map((r) => [r.path, r]));
  for (const r of rows) {
    if (local[r.path]?.sha256 === r.sha256) continue;
    const data = await deps.blobs.get(blobKey(deps, conv, r.sha256));
    if (!data) throw new Error(`file ${r.path} is missing from the bucket`);
    await writeInto(d.work, r.path, data);
  }
  for (const rel of Object.keys(local)) if (!want.has(rel)) await removeFrom(d.work, rel);
  await writeJsonAtomic(d.stored, await manifestOf(d.work, local));
}

/**
 * Store what changed in the workdir: new/changed files go to the bucket and `files` (source agent); files the turn
 * removed are marked deleted, unless their row changed after `since` (an upload during the turn).
 */
export async function storeWorkdir(deps: WebDeps, conv: string, since: Date): Promise<string[]> {
  const d = dirs(deps, conv);
  const prev = await readJson<Manifest>(d.stored, {});
  const local = await manifestOf(d.work, prev);
  const rows = new Map((await liveFiles(deps, conv)).map((r) => [r.path, r]));
  const changed: string[] = [];
  for (const [rel, e] of Object.entries(local)) {
    if (rows.get(rel)?.sha256 === e.sha256) continue;
    await deps.blobs.putFile(blobKey(deps, conv, e.sha256), join(d.work, rel), e.size);
    await deps.db.query(
      `insert into files (conversation_id, path, sha256, size, source, updated_at, deleted) values ($1, $2, $3, $4, 'agent', now(), false)
       on conflict (conversation_id, path) do update set sha256 = excluded.sha256, size = excluded.size, source = 'agent', updated_at = now(), deleted = false`,
      [conv, rel, e.sha256, e.size],
    );
    changed.push(rel);
  }
  for (const [rel, r] of rows) {
    if (!local[rel] && r.updated_at < since) {
      await deps.db.query("update files set deleted = true, updated_at = now() where conversation_id = $1 and path = $2", [conv, rel]);
      changed.push(rel);
    }
  }
  await writeJsonAtomic(d.stored, local);
  return changed;
}

/** Saves files pulled from a sandbox straight to the bucket (the janitor's final save before a release). */
export async function storeEntries(deps: WebDeps, conv: string, entries: { path: string; data: Uint8Array }[]): Promise<number> {
  const rows = new Map((await liveFiles(deps, conv)).map((r) => [r.path, r]));
  let n = 0;
  for (const e of entries) {
    if (excluded(e.path) || e.path.startsWith(".jobs/")) continue;
    const sha = createHash("sha256").update(e.data).digest("hex");
    if (rows.get(e.path)?.sha256 === sha) continue;
    await deps.blobs.put(blobKey(deps, conv, sha), e.data);
    await deps.db.query(
      `insert into files (conversation_id, path, sha256, size, source, updated_at, deleted) values ($1, $2, $3, $4, 'agent', now(), false)
       on conflict (conversation_id, path) do update set sha256 = excluded.sha256, size = excluded.size, source = 'agent', updated_at = now(), deleted = false`,
      [conv, e.path, sha, e.data.length],
    );
    n++;
  }
  return n;
}

const MAX_UPLOAD = 100 * 1024 * 1024;

export function cleanName(name: string): string {
  const base = name.split(/[\\/]/).pop() || "";
  const clean = base.replace(/[\u0000-\u001f\u007f]/g, "").replace(/^\.+/, "").trim().slice(0, 180);
  return clean || "file";
}

/** An upload from the staff member: streamed to a temp file while hashed, then to the bucket; path uploads/<name>. */
export async function saveUpload(deps: WebDeps, conv: string, name: string, req: IncomingMessage): Promise<{ path: string; size: number }> {
  const declared = Number(req.headers["content-length"] || 0);
  if (declared > MAX_UPLOAD) throw new HttpError(413, "files are limited to 100 MB");
  const d = dirs(deps, conv);
  await mkdir(d.base, { recursive: true });
  const tmp = join(d.base, `upload-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`);
  const h = createHash("sha256");
  let size = 0;
  try {
    await pipeline(
      req,
      new Transform({
        transform(chunk: Buffer, _enc, cb) {
          size += chunk.length;
          if (size > MAX_UPLOAD) return cb(new HttpError(413, "files are limited to 100 MB"));
          h.update(chunk);
          cb(null, chunk);
        },
      }),
      createWriteStream(tmp),
    );
    const sha = h.digest("hex");
    const existing = new Set((await liveFiles(deps, conv)).map((r) => r.path));
    const clean = cleanName(name);
    const dot = clean.lastIndexOf(".");
    const [stem, ext] = dot > 0 ? [clean.slice(0, dot), clean.slice(dot)] : [clean, ""];
    let path = `uploads/${clean}`;
    for (let i = 2; existing.has(path); i++) path = `uploads/${stem} (${i})${ext}`;
    await deps.blobs.putFile(blobKey(deps, conv, sha), tmp, size);
    await deps.db.query(
      `insert into files (conversation_id, path, sha256, size, source, updated_at, deleted) values ($1, $2, $3, $4, 'upload', now(), false)
       on conflict (conversation_id, path) do update set sha256 = excluded.sha256, size = excluded.size, source = 'upload', updated_at = now(), deleted = false`,
      [conv, path, sha, size],
    );
    return { path, size };
  } finally {
    await rm(tmp, { force: true });
  }
}

const TYPES: Record<string, string> = {
  pdf: "application/pdf", txt: "text/plain; charset=utf-8", md: "text/markdown; charset=utf-8", csv: "text/csv; charset=utf-8",
  json: "application/json", html: "text/html; charset=utf-8", png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg",
  gif: "image/gif", webp: "image/webp", svg: "image/svg+xml",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  zip: "application/zip", mp3: "audio/mpeg", mp4: "video/mp4",
};

export function contentType(path: string): string {
  return TYPES[path.split(".").pop()?.toLowerCase() || ""] || "application/octet-stream";
}

export async function readLocal(path: string): Promise<Buffer> {
  return readFile(path);
}
