// Postgres access for every role: one pool, migrations under an advisory lock, LISTEN/NOTIFY helpers and
// advisory-lock leadership for singleton loops (the janitor). Every replica of every role shares one database.
import pg from "pg";
import { MIGRATIONS } from "./schema.ts";

export type Db = pg.Pool;
export type Tx = pg.PoolClient;

const MIGRATION_LOCK = 4_242_001;

export function connect(url = process.env.DATABASE_URL): Db {
  if (!url) throw new Error("DATABASE_URL is not set");
  const pool = new pg.Pool({ connectionString: url, max: Number(process.env.PG_POOL_MAX || 10), idleTimeoutMillis: 30_000 });
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

/** Channel names may only hold [a-z0-9_]; uuids lose their dashes. */
export const channel = (prefix: string, id: string): string => `${prefix}_${id.replace(/-/g, "").toLowerCase()}`;

/**
 * One dedicated connection that LISTENs on many channels; reconnects (and re-LISTENs) after a drop and then calls
 * every handler with payload null, so subscribers re-read what they may have missed.
 */
export class Listener {
  #client: pg.Client | null = null;
  #handlers = new Map<string, Set<(payload: string | null) => void>>();
  #closed = false;
  #connecting: Promise<void> | null = null;
  readonly url: string | undefined;
  constructor(url = process.env.DATABASE_URL) {
    this.url = url;
  }

  async #connect(): Promise<void> {
    if (this.#closed) return;
    const c = new pg.Client({ connectionString: this.url });
    c.on("notification", (n) => {
      for (const h of this.#handlers.get(n.channel) ?? []) h(n.payload ?? "");
    });
    const reconnect = () => {
      if (this.#client !== c || this.#closed) return;
      this.#client = null;
      setTimeout(() => {
        this.#connecting = this.#connect().then(() => {
          for (const hs of this.#handlers.values()) for (const h of hs) h(null);
        }).catch(() => reconnect());
      }, 1000);
    };
    c.on("error", reconnect);
    c.on("end", reconnect);
    await c.connect();
    this.#client = c;
    for (const ch of this.#handlers.keys()) await c.query(`listen ${ch}`);
  }

  async listen(ch: string, h: (payload: string | null) => void): Promise<() => Promise<void>> {
    if (!/^[a-z0-9_]+$/.test(ch)) throw new Error(`bad channel ${ch}`);
    if (!this.#client) await (this.#connecting ??= this.#connect().finally(() => (this.#connecting = null)));
    let hs = this.#handlers.get(ch);
    if (!hs) {
      hs = new Set();
      this.#handlers.set(ch, hs);
      await this.#client?.query(`listen ${ch}`);
    }
    hs.add(h);
    return async () => {
      const set = this.#handlers.get(ch);
      if (!set) return;
      set.delete(h);
      if (!set.size) {
        this.#handlers.delete(ch);
        await this.#client?.query(`unlisten ${ch}`).catch(() => {});
      }
    };
  }

  async close(): Promise<void> {
    this.#closed = true;
    const c = this.#client;
    this.#client = null;
    await c?.end().catch(() => {});
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
        const c = new pg.Client({ connectionString: url });
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
