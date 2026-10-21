// Shared conversations (docs/CHAT.md, "Sharing and continuing"): a read-only snapshot of a conversation's completed
// turns that any instance user can open by link, and "continue conversation", which makes the viewer a new
// conversation of their own from that snapshot. The snapshot is frozen in `shares` (later turns of the original never
// show); file bytes are never copied: the bucket objects stay where they were first stored, `blobs` says where a
// sha256's bytes are, and `blob_grants` lets a conversation read a sha256 stored under another conversation's prefix.
// A continued conversation keeps no reference to the share or the original: its ids, paths and claw's session are
// rewritten to its own.
import { randomBytes, randomUUID } from "node:crypto";
import type { Readable } from "node:stream";
import { effectiveModel } from "../common/config.ts";
import { mergePresented, presentedOf } from "../common/presented.ts";
import { tx, type Db } from "../common/db.ts";
import type { BlobStore } from "../common/s3.ts";
import { newToken, sha256hex } from "../common/tokens.ts";
import { blobKey, type WebDeps } from "./ctx.ts";
import { publicEvent, type EventRow } from "./events.ts";

const SHA_RE = /^[0-9a-f]{64}$/;
const FINISHED = ["done", "error", "cancelled"];

export interface ShareFile {
  path: string;
  sha256: string;
  size: number;
  source: "upload" | "agent";
  updated_at?: string;
}

/**
 * The file cards of a turn with no presented list (stored before present_files): what the UI shows its owner for it,
 * the agent files whose last change fell between the turn's start and 15 s after its end.
 */
export function legacyPresented(files: ShareFile[], t: Record<string, any>): { path: string; sha256: string; size: number }[] {
  if (!t.started_at || !t.finished_at) return [];
  const from = new Date(t.started_at).getTime() - 1000;
  const to = new Date(t.finished_at).getTime() + 15_000;
  const made = files.filter((f) => f.source === "agent" && f.updated_at && new Date(f.updated_at).getTime() >= from && new Date(f.updated_at).getTime() <= to);
  return mergePresented([], made.map((f) => ({ path: f.path, sha256: f.sha256, size: Number(f.size) })));
}

type Presented = { path: string; sha256: string; size: number; caption?: string }[];

/** Each turn's presented files: its stored list (turns.presented), else its last files_presented event's, else the legacy rule's. */
export function presentedByTurn(turns: Record<string, any>[], events: EventRow[], files: ShareFile[]): Map<string, Presented> {
  const fromEvents = new Map<string, Presented>();
  for (const e of events) if (e.type === "files_presented" && Array.isArray((e.data as { files?: unknown }).files)) fromEvents.set(e.turn_id, (e.data as { files: Presented }).files);
  const out = new Map<string, Presented>();
  for (const t of turns) out.set(t.id, presentedOf(t.presented) ?? fromEvents.get(t.id) ?? legacyPresented(files, t));
  return out;
}

export interface ShareRow {
  id: string;
  conversation_id: string;
  owner: string;
  created_at: Date;
  revoked_at: Date | null;
  title: string;
  model: string;
  messages: Record<string, any>[];
  turns: Record<string, any>[];
  events: EventRow[];
  files: ShareFile[];
  claw_session: string | null;
}

/** A share link's id: 128 random bits, base64url (the link is the only way to it; every request is still signed in). */
export const newShareId = (): string => randomBytes(16).toString("base64url");
export const SHARE_ID_RE = /^[A-Za-z0-9_-]{22}$/;

/** Every sha256 a turn's events or stored row name: presented files (present_files) and images the model viewed. */
function turnShas(turns: Record<string, any>[], events: EventRow[], files: ShareFile[]): { presented: Set<string>; viewed: Set<string>; named: Set<string> } {
  const presented = new Set<string>();
  const viewed = new Set<string>();
  const named = new Set<string>(); // "<sha256> <path>": a version as it was shown, under its own name
  const addList = (v: unknown, into: Set<string>) => {
    if (!Array.isArray(v)) return;
    for (const f of v) {
      const sha = String((f as { sha256?: unknown })?.sha256 ?? "");
      if (!SHA_RE.test(sha)) continue;
      into.add(sha);
      named.add(`${sha} ${String((f as { path?: unknown }).path ?? "")}`);
    }
  };
  for (const list of presentedByTurn(turns, events, files).values()) addList(list, presented);
  for (const e of events) if (e.type === "tool_result") addList((e.data as { images?: unknown }).images, viewed);
  return { presented, viewed, named };
}

/**
 * What a share page may hand out: uploads (and message attachments), presented files and images shown in tool steps.
 * Never other workdir files (scripts, intermediates): those exist in the snapshot only for "continue".
 */
export function shareDownloadable(s: Pick<ShareRow, "messages" | "turns" | "events" | "files">): { paths: Map<string, string>; shas: Set<string>; named: Set<string> } {
  const { presented, viewed, named } = turnShas(s.turns, s.events, s.files);
  const attached = new Set<string>();
  for (const m of s.messages) if (Array.isArray(m.attachments)) for (const a of m.attachments) attached.add(String(a));
  const paths = new Map<string, string>(); // path → sha256 of its current (snapshot) version
  for (const f of s.files) if (f.source === "upload" || attached.has(f.path) || presented.has(f.sha256)) paths.set(f.path, f.sha256);
  const shas = new Set<string>([...presented, ...viewed, ...paths.values()]);
  for (const [path, sha] of paths) named.add(`${sha} ${path}`);
  return { paths, shas, named };
}

/** Where a conversation's sha256 bytes are: its own key, else (a continued conversation) the registry, if granted. */
async function grantedKey(db: Db, conv: string, sha: string): Promise<string | null> {
  const { rows } = await db.query<{ key: string }>(
    "select b.key from blob_grants g join blobs b on b.sha256 = g.sha256 where g.conversation_id = $1 and g.sha256 = $2",
    [conv, sha],
  );
  return rows[0]?.key ?? null;
}

/**
 * The bucket as the web role reads it: a read of `files/<conversation>/<sha256>` that finds nothing there falls back
 * to the bytes' first location when the conversation holds a grant for that sha256 (a continued conversation).
 * Writes go where they always did. So restore, downloads and previews work unchanged for continued conversations.
 */
export function sharedBlobs(inner: BlobStore, db: Db, prefix: string): BlobStore {
  if ((inner as { [SHARED]?: true })[SHARED]) return inner;
  const re = new RegExp(`^${prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}files/([0-9a-f-]{36})/([0-9a-f]{64})$`);
  const fallback = async (key: string): Promise<string | null> => {
    const m = re.exec(key);
    if (!m) return null;
    const k = await grantedKey(db, m[1]!, m[2]!);
    return k && k !== key ? k : null;
  };
  return {
    [SHARED]: true,
    put: (k, b, t) => inner.put(k, b, t),
    putFile: (k, p, s, t) => inner.putFile(k, p, s, t),
    delete: (k) => inner.delete(k),
    async get(key) {
      const r = await inner.get(key);
      if (r) return r;
      const k = await fallback(key);
      return k ? inner.get(k) : null;
    },
    async getStream(key): Promise<Readable | null> {
      const r = await inner.getStream(key);
      if (r) return r;
      const k = await fallback(key);
      return k ? inner.getStream(k) : null;
    },
    async exists(key) {
      if (await inner.exists(key)) return true;
      const k = await fallback(key);
      return k ? inner.exists(k) : false;
    },
  } as BlobStore;
}
const SHARED = Symbol("sharedBlobs");

/** Freezes the conversation's completed turns into a new share. Null when there is nothing finished to share. */
export async function createShare(deps: WebDeps, conv: { id: string; title: string; model: string }, owner: string): Promise<string | null> {
  const db = deps.db;
  const { rows: turns } = await db.query<{ t: Record<string, any> }>(
    "select to_jsonb(t) - 'token_hash' - 'replica' as t from turns t where conversation_id = $1 and status = any($2) order by started_at",
    [conv.id, FINISHED],
  );
  if (!turns.length) return null;
  const ids = turns.map((r) => r.t.id as string);
  const [{ rows: messages }, { rows: events }, { rows: files }, { rows: sess }] = await Promise.all([
    db.query(
      `select id, role, content, turn_id, attachments, created_at from messages
        where conversation_id = $1 and (turn_id is null or turn_id = any($2::uuid[])) order by id`,
      [conv.id, ids],
    ),
    db.query<EventRow>("select id, conversation_id, turn_id, seq, type, data, created_at from events where conversation_id = $1 and turn_id = any($2::uuid[]) order by id", [conv.id, ids]),
    db.query<ShareFile>("select path, sha256, size::float8 as size, source, updated_at from files where conversation_id = $1 and not deleted order by path", [conv.id]),
    db.query<{ content: string }>("select content from claw_sessions where conversation_id = $1", [conv.id]),
  ]);
  // every sha256 the snapshot names goes into the registry (where its bytes are), so a continued conversation and
  // the share page find them without the original: a sha this conversation itself reads through a grant is
  // registered already (grants come from a share), else its bytes are under this conversation's own prefix
  const tShas = turnShas(turns.map((r) => r.t), events, files);
  const shas = new Set<string>([...files.map((f) => f.sha256), ...tShas.presented, ...tShas.viewed]);
  if (shas.size) {
    await db.query(
      `insert into blobs (sha256, key) select s, $2 || s from unnest($3::text[]) s
        where not exists (select 1 from blob_grants g where g.conversation_id = $1 and g.sha256 = s)
       on conflict (sha256) do nothing`,
      [conv.id, blobKey(deps, conv.id, ""), [...shas]],
    );
  }
  const id = newShareId();
  await db.query(
    `insert into shares (id, conversation_id, owner, title, model, messages, turns, events, files, claw_session)
     values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
    [id, conv.id, owner, conv.title, conv.model, JSON.stringify(messages), JSON.stringify(turns.map((r) => r.t)), JSON.stringify(events), JSON.stringify(files), sess[0]?.content ?? null],
  );
  return id;
}

/** A live share (not revoked, its conversation not deleted), or null. */
export async function loadShare(db: Db, id: string): Promise<ShareRow | null> {
  if (!SHARE_ID_RE.test(id)) return null;
  const { rows } = await db.query<ShareRow>(
    `select s.* from shares s join conversations c on c.id = s.conversation_id
      where s.id = $1 and s.revoked_at is null and c.deleted_at is null`,
    [id],
  );
  return rows[0] ?? null;
}

/** The share page's view: the same shape as GET /api/conversations/:id, plus who shared it and what can be downloaded. */
export function shareView(s: ShareRow, viewer: string) {
  const byTurn = new Map<string, unknown[]>();
  for (const e of s.events) {
    const list = byTurn.get(e.turn_id) ?? [];
    list.push(publicEvent(e));
    byTurn.set(e.turn_id, list);
  }
  const { paths } = shareDownloadable(s);
  const presented = presentedByTurn(s.turns, s.events, s.files);
  return {
    id: s.id,
    title: s.title,
    ownerName: s.owner.split("@")[0],
    isOwner: s.owner === viewer,
    createdAt: s.created_at,
    messages: s.messages.map((m) => ({ id: Number(m.id), role: m.role, content: m.content, turnId: m.turn_id, attachments: m.attachments ?? [], createdAt: m.created_at })),
    turns: s.turns.map((t) => ({
      id: t.id,
      status: t.status,
      model: t.model,
      startedAt: t.started_at,
      finishedAt: t.finished_at,
      error: t.error,
      costUsd: null,
      presented: presented.get(t.id) ?? [],
      events: byTurn.get(t.id) ?? [],
    })),
    files: s.files.filter((f) => paths.has(f.path)).map((f) => ({ path: f.path, size: Number(f.size), source: f.source, updatedAt: f.updated_at })),
  };
}

/**
 * Which bytes a share-page download names: a snapshot path (uploads, attachments, presented) or `v` = a version that
 * was shown under that very path (so the caller can't pick another name, and with it another content type).
 */
export function shareBlob(s: ShareRow, path: string, v: string): string | null {
  const { paths, named } = shareDownloadable(s);
  if (v) return SHA_RE.test(v) && named.has(`${v} ${path}`) ? v : null;
  return paths.get(path) ?? null;
}

export async function shareBlobKey(db: Db, sha: string): Promise<string | null> {
  const { rows } = await db.query<{ key: string }>("select key from blobs where sha256 = $1", [sha]);
  return rows[0]?.key ?? null;
}

/** Replaces every occurrence of each old id with its new one (ids are UUIDs: no id is a substring of another). */
function rewriter(map: Map<string, string>): (s: string) => string {
  if (!map.size) return (s) => s;
  const re = new RegExp([...map.keys()].map((k) => k.replace(/[-]/g, "\\-")).join("|"), "gi");
  return (s) => s.replace(re, (m) => map.get(m.toLowerCase()) ?? m);
}

/** claw's session for the continued conversation: its own session id; workdir paths already rewritten. */
export function continuedSession(content: string, rewrite: (s: string) => string): string {
  const fresh = `session-${Date.now()}-${randomBytes(4).readUInt32BE(0)}`;
  return rewrite(content)
    .split("\n")
    .map((line) => {
      if (!line.includes('"session_meta"')) return line;
      try {
        const o = JSON.parse(line);
        if (o?.type !== "session_meta") return line;
        o.session_id = fresh;
        delete o.fork;
        return JSON.stringify(o);
      } catch {
        return line;
      }
    })
    .join("\n");
}

/**
 * "Continue conversation": a new conversation of the viewer's with the snapshot's title, messages, turns and their
 * events, claw's session and every workdir file (new `files` rows on the same bucket objects, plus grants to read
 * them), but no sandbox (a fresh one comes with the first bash command, with the reset note), no egress or tool
 * logs and no spend. Nothing in it names the share or the original.
 */
export async function continueShare(deps: WebDeps, s: ShareRow, viewer: string): Promise<string> {
  const db = deps.db;
  const conv = randomUUID();
  const ids = new Map<string, string>([[s.conversation_id.toLowerCase(), conv]]);
  for (const t of s.turns) ids.set(String(t.id).toLowerCase(), randomUUID());
  const rewrite = rewriter(ids);
  const rewriteJson = (v: unknown) => JSON.parse(rewrite(JSON.stringify(v)));
  const tShas = turnShas(s.turns, s.events, s.files);
  const shas = [...new Set<string>([...s.files.map((f) => f.sha256), ...tShas.presented, ...tShas.viewed])];
  const usedSandbox = s.events.some((e) => e.type === "sandbox" || (e.type === "tool_use" && (e.data as { name?: unknown }).name === "bash"));

  await tx(db, async (client) => {
    await client.query("insert into conversations (id, owner, title, model) values ($1, $2, $3, $4)", [conv, viewer, s.title, effectiveModel(deps.config, s.model)]);
    for (const t of s.turns) {
      // the stored row as it was (any columns this schema has, e.g. presented), with new ids, a fresh unusable token
      // hash, and no usage or cost: spend stays with the turn that spent it
      const row = rewriteJson({ ...t, conversation_id: s.conversation_id, id: t.id });
      row.token_hash = sha256hex(newToken());
      row.replica = null;
      row.usage = null;
      row.cost_usd = null;
      await client.query("insert into turns select * from jsonb_populate_record(null::turns, $1)", [JSON.stringify(row)]);
    }
    for (const m of s.messages) {
      await client.query("insert into messages (conversation_id, role, content, turn_id, attachments, created_at) values ($1, $2, $3, $4, $5, $6)", [
        conv,
        m.role,
        rewrite(String(m.content)),
        m.turn_id ? ids.get(String(m.turn_id).toLowerCase()) ?? null : null,
        JSON.stringify(m.attachments ?? []),
        m.created_at,
      ]);
    }
    for (const e of s.events) {
      await client.query("insert into events (conversation_id, turn_id, seq, type, data, created_at) values ($1, $2, $3, $4, $5, $6)", [
        conv,
        ids.get(String(e.turn_id).toLowerCase()),
        e.seq,
        e.type,
        rewrite(JSON.stringify(e.data)),
        e.created_at,
      ]);
    }
    if (s.claw_session) await client.query("insert into claw_sessions (conversation_id, content) values ($1, $2)", [conv, continuedSession(s.claw_session, rewrite)]);
    for (const f of s.files) {
      // updated_at as it was: a turn from before present_files shows the files it changed by that time
      await client.query("insert into files (conversation_id, path, sha256, size, source, updated_at, deleted) values ($1, $2, $3, $4, $5, coalesce($6::timestamptz, now()), false)", [
        conv,
        f.path,
        f.sha256,
        f.size,
        f.source,
        f.updated_at ?? null,
      ]);
    }
    if (shas.length) await client.query("insert into blob_grants (conversation_id, sha256) select $1, unnest($2::text[]) on conflict do nothing", [conv, shas]);
    // the first bash command lands in a new sandbox: it says so (the reset note), as after any sandbox release
    if (usedSandbox) await client.query("insert into kv (key, value) values ($1, $2)", [`turn_sandbox:${conv}`, JSON.stringify("earlier")]);
    await client.query("update conversations set updated_at = now() where id = $1", [conv]);
  });
  return conv;
}

/** A bucket key a share download may stream (only after shareBlob allowed the sha256). */
export async function shareKey(deps: WebDeps, s: ShareRow, sha: string): Promise<string> {
  return (await shareBlobKey(deps.db, sha)) ?? blobKey(deps, s.conversation_id, sha);
}
