// egress's own database `egress` (aviary platform spec 17.2 item 5, DESIGN 5.4): `egress_log` and `budget_state`, with
// migrations of these tables only, under egress's own advisory lock. Never edit a shipped migration: add the next one.
import pg from "pg";

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

/** Applies the missing migrations (two replicas starting at once apply each exactly once) and verifies the schema
 * is this build's: a database migrated by a newer build is a start failure. */
export async function migrate(db: Db): Promise<void> {
  const c = await db.connect();
  try {
    await c.query("select pg_advisory_lock($1)", [MIGRATION_LOCK]);
    await c.query("create table if not exists egress_migrations (version int primary key, applied_at timestamptz not null default now())");
    const { rows } = await c.query<{ v: number | null }>("select max(version) as v from egress_migrations");
    const have = rows[0]?.v ?? 0;
    if (have > MIGRATIONS.length) throw new Error(`egress database is at migration ${have}, this build knows ${MIGRATIONS.length}`);
    for (let v = have + 1; v <= MIGRATIONS.length; v++) {
      await c.query("begin");
      try {
        await c.query(MIGRATIONS[v - 1]!);
        await c.query("insert into egress_migrations (version) values ($1)", [v]);
        await c.query("commit");
      } catch (e) {
        await c.query("rollback");
        throw e;
      }
    }
  } finally {
    await c.query("select pg_advisory_unlock($1)", [MIGRATION_LOCK]).catch(() => {});
    c.release();
  }
}
