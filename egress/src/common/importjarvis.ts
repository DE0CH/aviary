// The one-off copies of the old stack's history into egress's and tools' own databases (aviary cutover spec 8.2,
// platform spec 17.2 egress item 6 and tools item 5): `node src/main.ts egress import-jarvis [--delta]` and
// `node src/main.ts tools import-jarvis [--delta]`, run as CronJobs `egress-import` (every minute from A2 to A6) and
// `tools-import` (A2 once, A6 once) with a read-only connection to the old `jarvis` database. Deleted in A6 with the
// old stack.
//
// A pass reads the source in id order inside one read-only, repeatable-read transaction (a consistent snapshot) and
// writes each batch with `on conflict (source_id) do nothing`: every source row lands exactly once however often a
// pass runs, is cut short or overlaps another (`source_id` is unique in the target). Every value travels as Postgres
// text both ways, so timestamps keep their microseconds and numerics their digits.
//
// `--delta` starts after the highest `source_id` already copied, less LOOKBACK ids: an old-stack row whose id was
// handed out before a neighbour's but committed after it (two replicas inserting at once) is still read by the next
// pass; the overlap costs nothing (conflicts are skipped). A first run without rows copied is a full pass either way.
//
// A full pass (no `--delta`; A6's last runs, cutover 8.2) also checks the target against the source, independently of
// what the pass itself read: `missing` is how many more rows the source's snapshot holds (a separate count of the whole
// source query) than the target holds with a `source_id` up to the highest one read (a row the target lost or
// refused, or one a batch skipped; logged with the pass, and the subcommands exit 1 when any is missing). The lookback
// is the only guarantee a delta pass has, so the last pass reads everything and checks it.
//
// The source connection has a client-side query timeout and TCP keepalives on top of the server's statement_timeout:
// the old database is reached across clusters, and a network drop mid-query would otherwise leave the run waiting
// forever (and, with the CronJob's concurrencyPolicy Forbid, stop every later run).
import pg from "pg";
import { log, logError } from "./log.ts";

/** Rows per read and per insert (18 columns x 1 000 rows stays far under Postgres' 65 535 parameters). */
export const BATCH = 1000;
/** `--delta` re-reads this many ids below the highest copied one (see the header). */
export const LOOKBACK = 1000;

/** One copy: what to read from the old database and which target columns its values fill, in order. */
export interface CopySpec {
  /** The target table (in the service's own database); it has a unique `source_id`. */
  target: string;
  /** Target columns, in the order the source query returns them; `source_id` is appended. */
  columns: readonly string[];
  /** The source query: selects `columns` (in order) and then the source id as `source_id`, for ids above $1, in id
   * order, at most $2 rows. Built per pass, so it can depend on which old tables exist. */
  sourceSql(src: pg.ClientBase): Promise<string>;
}

export interface CopyResult {
  /** Source rows read. */
  read: number;
  /** Rows inserted (read minus the ones already there). */
  copied: number;
  /** The id the pass started after. */
  after: number;
  /** The highest source id read, or `after` when none. */
  last: number;
  /** A full pass only: source rows read that the target does not hold after it. */
  missing?: number;
}

/** Every value as Postgres text (the copy passes them back as text: no Date or float conversion on the way). */
const TEXT_TYPES = { getTypeParser: () => (v: string) => v } as unknown as pg.CustomTypesConfig;

/** The server's statement timeout on the source, and the client's own (longer, so the server's error comes first when
 * the network is up). */
export const STATEMENT_TIMEOUT_MS = 120_000;
export const QUERY_TIMEOUT_MS = 150_000;

/** A client of the old database whose values all come back as text. */
export function sourceClient(url: string): pg.Client {
  return new pg.Client({
    connectionString: url,
    types: TEXT_TYPES,
    application_name: "import-jarvis",
    statement_timeout: STATEMENT_TIMEOUT_MS,
    query_timeout: QUERY_TIMEOUT_MS,
    connectionTimeoutMillis: 10_000,
    keepAlive: true,
    keepAliveInitialDelayMillis: 10_000,
  });
}

/** Whether table `name` exists in the source's search path. */
export async function hasTable(src: pg.ClientBase, name: string): Promise<boolean> {
  const { rows } = await src.query<{ r: string | null }>("select to_regclass($1)::text as r", [name]);
  return rows[0]?.r != null;
}

/** One pass of `spec` from `src` (already inside its read-only transaction) into `dst`. */
export async function copy(src: pg.ClientBase, dst: pg.Pool, spec: CopySpec, delta: boolean): Promise<CopyResult> {
  let after = 0;
  if (delta) {
    const { rows } = await dst.query<{ m: string | null }>(`select max(source_id)::text as m from ${spec.target}`);
    after = Math.max(0, Number(rows[0]?.m ?? 0) - LOOKBACK);
  }
  const sql = await spec.sourceSql(src);
  const cols = [...spec.columns, "source_id"];
  let last = after;
  let read = 0;
  let copied = 0;
  for (;;) {
    const { rows } = await src.query<Record<string, string | null>>(sql, [last, BATCH]);
    if (!rows.length) break;
    const vals: (string | null)[] = [];
    const tuples = rows.map((r) => `(${cols.map((c) => (vals.push(r[c] ?? null), `$${vals.length}`)).join(",")})`);
    const res = await dst.query(`insert into ${spec.target} (${cols.join(",")}) values ${tuples.join(",")} on conflict (source_id) do nothing`, vals);
    copied += res.rowCount ?? 0;
    read += rows.length;
    last = Number(rows.at(-1)!.source_id);
  }
  if (delta) return { read, copied, after, last };
  const { rows: src_n } = await src.query<{ n: string }>(`select count(*)::text as n from (${sql}) s`, [0, "9223372036854775807"]);
  const { rows: held } = await dst.query<{ n: string }>(`select count(*)::text as n from ${spec.target} where source_id is not null and source_id <= $1`, [last]);
  return { read, copied, after, last, missing: Math.max(0, Number(src_n[0]?.n ?? 0) - Number(held[0]?.n ?? 0)) };
}

/** A whole `import-jarvis` run: connects to the old database (`sourceUrl`, read-only), opens one repeatable-read
 * read-only transaction, runs each copy in order, logs one line per copy. The caller owns `dst`. */
export async function run(service: string, sourceUrl: string, dst: pg.Pool, specs: readonly CopySpec[], delta: boolean): Promise<CopyResult[]> {
  const src = sourceClient(sourceUrl);
  src.on("error", (e) => logError("import-jarvis source", e, { service }));
  await src.connect();
  const out: CopyResult[] = [];
  try {
    await src.query("begin transaction isolation level repeatable read read only");
    for (const s of specs) {
      const r = await copy(src, dst, s, delta);
      log("import-jarvis", { service, table: s.target, delta, ...r });
      out.push(r);
    }
    await src.query("commit");
  } finally {
    await src.end().catch(() => {});
  }
  return out;
}

/** Parses `import-jarvis`'s arguments (after the subcommand): only `--delta`. */
export function parseArgs(args: readonly string[]): { delta: boolean } {
  let delta = false;
  for (const a of args) {
    if (a === "--delta") delta = true;
    else throw new Error(`import-jarvis: unknown argument ${JSON.stringify(a)} (only --delta)`);
  }
  return { delta };
}
