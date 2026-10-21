// Postgres access for every role: one pool, migrations under an advisory lock and
// advisory-lock leadership for singleton loops (the janitor). Every replica of every role shares one database.
import pg from "pg";
import { MIGRATIONS } from "./schema.ts";

export type Db = pg.Pool;
export type Tx = pg.PoolClient;

const MIGRATION_LOCK = 4_242_001;

/** Names this process's connections in pg_stat_activity (connections per role, lock holders). */
export const appName = (): string => `jarvis-${process.env.JARVIS_ROLE || "chat"}:${process.env.HOSTNAME || "local"}`.slice(0, 63);

export function connect(url = process.env.DATABASE_URL): Db {
  if (!url) throw new Error("DATABASE_URL is not set");
  const pool = new pg.Pool({ connectionString: url, application_name: appName(), max: Number(process.env.PG_POOL_MAX || 10), idleTimeoutMillis: 30_000 });
  pool.on("error", (e) => console.error(JSON.stringify({ at: "pg pool", error: e.message })));
  return pool;
}

export async function migrate(db: Db): Promise<void> {
  const c = await db.connect();
  try {
    await c.query("select pg_advisory_lock($1)", [MIGRATION_LOCK]);
    await c.query("create table if not exists schema_migrations (version int primary key, applied_at timestamptz not null default now())");
    const { rows } = await c.query<{ v: number | null }>("select max(version) as v from schema_migrations");
    for (let v = (rows[0]?.v ?? 0) + 1; v <= MIGRATIONS.length; v++) {
      await c.query("begin");
      try {
        await c.query(MIGRATIONS[v - 1]!);
        await c.query("insert into schema_migrations (version) values ($1)", [v]);
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

export async function tx<T>(db: Db, fn: (c: Tx) => Promise<T>): Promise<T> {
  const c = await db.connect();
  try {
    await c.query("begin");
    const r = await fn(c);
    await c.query("commit");
    return r;
  } catch (e) {
    await c.query("rollback").catch(() => {});
    throw e;
  } finally {
    c.release();
  }
}

/**
 * Leadership through a session-level advisory lock on a dedicated connection: `run` is called while this process
 * holds the lock; another replica takes over within `retryMs` when this one exits or its connection drops.
 */
export function lead(url: string | undefined, key: number, run: () => Promise<void>, intervalMs: number, retryMs = 5000): { stop(): Promise<void> } {
  let stopped = false;
  let client: pg.Client | null = null;
  let timer: NodeJS.Timeout | null = null;
  const loop = async () => {
    if (stopped) return;
    try {
      if (!client) {
        const c = new pg.Client({ connectionString: url, application_name: appName() });
        c.on("error", () => {
          if (client === c) client = null;
        });
        await c.connect();
        const { rows } = await c.query<{ ok: boolean }>("select pg_try_advisory_lock($1) as ok", [key]);
        if (!rows[0]?.ok) {
          await c.end();
          timer = setTimeout(loop, retryMs);
          return;
        }
        client = c;
      }
      await run();
    } catch (e) {
      console.error(JSON.stringify({ at: "leader loop", key, error: (e as Error).message }));
    }
    if (!stopped) timer = setTimeout(loop, client ? intervalMs : retryMs);
  };
  timer = setTimeout(loop, 0);
  return {
    async stop() {
      stopped = true;
      if (timer) clearTimeout(timer);
      await client?.end().catch(() => {});
    },
  };
}
