// tools' own database `tools` (aviary platform spec 17.2 tools item 5; TOOLS_DATABASE_URL): `tool_calls`, with
// migrations of this table only, under tools' own advisory lock. Never edit a shipped migration: add the next one.
//
// A database outage never stops a call: a row that cannot be written is dropped and counted (platform 11, A6.F30),
// and nothing waits on the write (queries time out in seconds).
import { createHash } from "node:crypto";
import pg from "pg";
import { log, logError } from "../common/log.ts";

export type Db = pg.Pool;

/** tools' migration lock (the chat runtime's shared schema uses 4_242_001, egress 7_310_001). */
const MIGRATION_LOCK = 7_320_001;

export const MIGRATIONS: string[] = [
  // one row per tools/call: today's columns of the chat schema's tool_calls (turn_id stays for rows copied from the
  // old stack), plus the sandbox and the request id the gateway named
  `
create table tool_calls (id bigserial primary key, at timestamptz not null default now(), turn_id uuid,
  conversation_id uuid, user_email text, tool text not null, args jsonb, ok boolean, ms int, error text,
  cost_usd numeric, sandbox_id uuid, request text);
create index tool_calls_at on tool_calls (at);
create index tool_calls_conv on tool_calls (conversation_id, at);
`,
];

export function connect(url: string): Db {
  const pool = new pg.Pool({
    connectionString: url,
    application_name: `tools:${process.env.HOSTNAME || "local"}`.slice(0, 63),
    max: 4,
    idleTimeoutMillis: 30_000,
    connectionTimeoutMillis: 5000,
    query_timeout: 10_000,
  });
  pool.on("error", (e) => console.error(JSON.stringify({ t: new Date().toISOString(), at: "pg pool", error: e.message })));
  return pool;
}

export const migrationSha = (sql: string): string => createHash("sha256").update(sql).digest("hex");

/** Applies the missing migrations (two replicas starting at once apply each exactly once). A database at a newer
 * migration than this build knows is accepted (expand-only within a release); an edited history (a recorded
 * migration this build knows whose sha256 differs) is a start failure. Returns what it applied. */
export async function migrate(db: Db, migrations: readonly string[] = MIGRATIONS): Promise<number[]> {
  const c = await db.connect();
  const applied: number[] = [];
  try {
    await c.query("select pg_advisory_lock($1)", [MIGRATION_LOCK]);
    await c.query("create table if not exists tools_migrations (version int primary key, sha256 text not null, applied_at timestamptz not null default now())");
    const { rows } = await c.query<{ version: number; sha256: string }>("select version, sha256 from tools_migrations order by version");
    const have = new Map(rows.map((r) => [r.version, r.sha256]));
    for (const [v, sha] of have) {
      if (v > migrations.length) continue;
      const mine = migrationSha(migrations[v - 1]!);
      if (sha !== mine) throw new Error(`tools migration ${v} was edited: the database recorded sha256 ${sha}, this build has ${mine}`);
    }
    const newest = rows.at(-1)?.version ?? 0;
    if (newest > migrations.length) log("tools database is at a newer migration than this build knows; serving (expand-only)", { database: newest, build: migrations.length });
    for (let v = 1; v <= migrations.length; v++) {
      if (have.has(v)) continue;
      await c.query("begin");
      try {
        await c.query(migrations[v - 1]!);
        await c.query("insert into tools_migrations (version, sha256) values ($1, $2)", [v, migrationSha(migrations[v - 1]!)]);
        await c.query("commit");
        applied.push(v);
      } catch (e) {
        await c.query("rollback");
        throw e;
      }
    }
    return applied;
  } finally {
    await c.query("select pg_advisory_unlock($1)", [MIGRATION_LOCK]).catch(() => {});
    c.release();
  }
}

export interface CallRow {
  conversationId: string;
  sandboxId: string;
  person: string;
  request: string | null;
  tool: string;
  args: unknown;
  ok: boolean;
  ms: number;
  error: string | null;
  costUsd: number | null;
}

/** The tool_calls writer: fire and forget; a failed insert is logged and counted, never retried or waited on. */
export class CallLog {
  readonly #db: Db;
  readonly #pending = new Set<Promise<void>>();
  dropped = 0;
  constructor(db: Db) {
    this.#db = db;
  }

  write(r: CallRow): void {
    const p = this.#db
      .query(
        `insert into tool_calls (conversation_id, sandbox_id, user_email, request, tool, args, ok, ms, error, cost_usd) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)`,
        [r.conversationId, r.sandboxId, r.person, r.request, r.tool, JSON.stringify(r.args), r.ok, r.ms, r.error, r.costUsd],
      )
      .then(
        () => {},
        (e) => {
          this.dropped++;
          logError("tool_calls insert", e, { tool: r.tool, request: r.request });
        },
      )
      .finally(() => this.#pending.delete(p));
    this.#pending.add(p);
  }

  /** Waits for the writes in flight (each bounded by the connection and query timeouts, 5 + 10 s, but queued behind
   * the pool's 4 connections), at most `boundMs` when given; returns how many were still unwritten then, which are
   * counted as dropped (the process is about to exit). */
  async flush(boundMs?: number): Promise<number> {
    const all = Promise.all([...this.#pending]).then(() => {});
    if (boundMs === undefined) {
      await all;
      return 0;
    }
    let timer: NodeJS.Timeout | undefined;
    await Promise.race([all, new Promise<void>((r) => (timer = setTimeout(r, boundMs)))]);
    clearTimeout(timer);
    const left = this.#pending.size;
    if (left) {
      this.dropped += left;
      logError("tool_calls flush", new Error(`${left} rows unwritten at exit`), { boundMs });
    }
    return left;
  }
}
