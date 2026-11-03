// egress's own database `egress` (aviary platform spec 17.2 item 5, DESIGN 5.4): `egress_log` and `budget_state`, with
// migrations of these tables only, under egress's own advisory lock. Never edit a shipped migration: add the next one.
import { createHash } from "node:crypto";
import pg from "pg";
import { log } from "../common/log.ts";

export type Db = pg.Pool;

/** egress's migration lock (the chat runtime's shared schema uses 4_242_001). */
const MIGRATION_LOCK = 7_310_001;

export const MIGRATIONS: string[] = [
  // every request through the proxy that is not a magpie or hermit call: internet (connect, http), the mirror, model
  // and tools calls (today's columns of the chat schema's egress_log, plus person and request). `owner` is the column
  // the old stack's relay calls filled; rows copied from it keep it. No retention: the budget sums every model row
  // since budget.since.
  `
create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
  sandbox_id uuid, turn_id uuid, kind text not null, method text, host text not null, port int, path text,
  status int, decision text not null, bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric, model text,
  owner text, person text, request text);
create index egress_log_at on egress_log (at);
create index egress_log_conv on egress_log (conversation_id, at);
create index egress_log_model_at on egress_log (at) where kind = 'model';
-- the last budget status a replica read, per budget period: what a replica that has not yet read the log itself
-- serves (a database that fails right after its start), and what the ops summary reports as the status's age
create table budget_state (since timestamptz primary key, limit_usd numeric not null, spent_usd numeric not null,
  computed_at timestamptz not null);
`,
  // the last key set hermit published (GET /v1/sandbox-token-keys, public keys only): a replica that starts while
  // hermit is away verifies tokens with it and becomes ready (tokens.ts KeyStore)
  `
create table token_keys (id int primary key default 1 check (id = 1), body text not null, fetched_at timestamptz not null);
`,
  // each row's own id, made once by the writer and kept through its retries: a batch retried after an insert whose
  // outcome was unknown (query_timeout, a connection lost after COMMIT) is not written twice (on conflict do nothing),
  // so a model row's cost counts once toward the budget. Null on rows copied from the old stack and rows of a build
  // before this one (a unique index allows any number of nulls).
  `
alter table egress_log add column row_id uuid;
create unique index egress_log_row_id on egress_log (row_id);
`,
  // the old stack's id of a row `egress import-jarvis` copied (aviary cutover spec 8.2): the copy's idempotency key,
  // so a pass that runs again, is cut short or overlaps another writes each old row once. Null on egress's own rows.
  `
alter table egress_log add column source_id bigint;
create unique index egress_log_source_id on egress_log (source_id);
`,
];

export function connect(url: string | undefined): Db {
  if (!url) throw new Error("EGRESS_DATABASE_URL is not set");
  const pool = new pg.Pool({
    connectionString: url,
    application_name: `egress:${process.env.HOSTNAME || "local"}`.slice(0, 63),
    max: 6,
    idleTimeoutMillis: 30_000,
    // an unreachable database fails a query in seconds instead of holding it (17.2 item 8: nothing waits on it)
    connectionTimeoutMillis: 5000,
    query_timeout: 15_000,
  });
  pool.on("error", (e) => console.error(JSON.stringify({ t: new Date().toISOString(), at: "pg pool", error: e.message })));
  return pool;
}

/** sha256 (hex) of a migration's SQL, recorded with it: a recorded migration whose SQL changed is a start failure. */
export function migrationSha(sql: string): string {
  return createHash("sha256").update(sql).digest("hex");
}

/** Applies the missing migrations (two replicas starting at once apply each exactly once). A database at a newer
 * migration than this build knows is accepted (aviary-pg's rule: migrations are expand-only within a release, so an
 * older build works on a newer schema; a rollback or an old pod restarting mid-rollout serves). An edited history is
 * a start failure: a recorded migration this build knows whose sha256 differs from this build's. Returns what it
 * applied. */
export async function migrate(db: Db, migrations: readonly string[] = MIGRATIONS): Promise<number[]> {
  const c = await db.connect();
  const applied: number[] = [];
  try {
    await c.query("select pg_advisory_lock($1)", [MIGRATION_LOCK]);
    await c.query("create table if not exists egress_migrations (version int primary key, applied_at timestamptz not null default now())");
    // expand-only: rows recorded before the column existed get their sha256 filled in by the first build that knows it
    await c.query("alter table egress_migrations add column if not exists sha256 text");
    const { rows } = await c.query<{ version: number; sha256: string | null }>("select version, sha256 from egress_migrations order by version");
    const have = new Map(rows.map((r) => [r.version, r.sha256]));
    const newest = rows.at(-1)?.version ?? 0;
    for (const [v, sha] of have) {
      if (v > migrations.length) continue;
      const mine = migrationSha(migrations[v - 1]!);
      if (sha === null) await c.query("update egress_migrations set sha256 = $2 where version = $1", [v, mine]);
      else if (sha !== mine) throw new Error(`egress migration ${v} was edited: the database recorded sha256 ${sha}, this build has ${mine}`);
    }
    if (newest > migrations.length) log("egress database is at a newer migration than this build knows; serving (expand-only)", { database: newest, build: migrations.length });
    for (let v = 1; v <= migrations.length; v++) {
      if (have.has(v)) continue;
      await c.query("begin");
      try {
        await c.query(migrations[v - 1]!);
        await c.query("insert into egress_migrations (version, sha256) values ($1, $2)", [v, migrationSha(migrations[v - 1]!)]);
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

/** The key-set store of tokens.ts in table token_keys (one row). */
export function keyStore(db: Db): { load(): Promise<string | null>; save(body: string): Promise<void> } {
  return {
    async load() {
      const { rows } = await db.query<{ body: string }>("select body from token_keys where id = 1");
      return rows[0]?.body ?? null;
    },
    async save(body) {
      await db.query("insert into token_keys (id, body, fetched_at) values (1, $1, now()) on conflict (id) do update set body = excluded.body, fetched_at = excluded.fetched_at", [body]);
    },
  };
}
