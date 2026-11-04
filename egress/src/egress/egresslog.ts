// Every request through egress except magpie and hermit calls becomes one egress_log row. Rows are buffered and
// written in order, in batches (every second, at 200 rows, and at once for a model row: its cost counts toward the
// budget). A database outage never stops a request (aviary platform spec 17.2 item 8): unwritten rows stay in memory,
// at most MAX_BUFFERED, the oldest non-model row dropped first when full (counted) and never a model row, and are
// written in order once the database is back. A row Postgres refuses as data (SQLSTATE class 22 or 23) is a bad row,
// not an outage: it is found by writing its batch row by row, dropped and counted, and the rows after it go on. Rows
// are cleaned in add() first (port 1-65535 or null, no NUL in text, lengths capped), so a sandbox's odd request (a
// CONNECT to port 99999999999, a model name with \u0000) never reaches that path in practice. Writes are idempotent:
// each row carries a row_id made once in add() (unique index, insert ... on conflict do nothing), so a batch retried
// after an insert whose outcome is unknown (the client's query_timeout, a connection lost after COMMIT) is never
// written twice and never counts its model cost twice toward the budget.
import { randomUUID } from "node:crypto";
import type { Db } from "./db.ts";
import { logError, log } from "../common/log.ts";

/** `route`: a sandbox route (platform 3.11.8, egress.json's routes beside model and tools) */
export type RowKind = "connect" | "http" | "mirror" | "model" | "tools" | "route";

export interface EgressRow {
  /** when it happened (set by add() when absent; kept through an outage) */
  at?: Date;
  conversation_id?: string | null;
  sandbox_id?: string | null;
  kind: RowKind;
  method?: string | null;
  host: string;
  port?: number | null;
  path?: string | null;
  status?: number | null;
  decision: string;
  bytes_up?: number | null;
  bytes_down?: number | null;
  ms?: number | null;
  cost_usd?: number | null;
  /** kind model: the model the request asked for */
  model?: string | null;
  /** the sandbox's owner (email) from the verified token */
  person?: string | null;
  /** the Aviary-Request id */
  request?: string | null;
  /** the row's own id, made once in add() and kept through retries (the insert's idempotency key) */
  row_id?: string;
}

const COLS = ["at", "conversation_id", "sandbox_id", "kind", "method", "host", "port", "path", "status", "decision", "bytes_up", "bytes_down", "ms", "cost_usd", "model", "person", "request", "row_id"] as const;
/** About 15 MB of rows. */
export const MAX_BUFFERED = 50_000;
const BATCH = 500;

/** Text columns and their caps (characters). */
const TEXT_CAPS = { method: 32, host: 300, path: 2000, decision: 64, model: 200, person: 320, request: 64 } as const;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const INT4_MAX = 2_147_483_647;

const text = (v: string | null | undefined, cap: number): string | null => (typeof v === "string" ? v.replace(/\u0000/g, "").slice(0, cap) : null);
const int = (v: number | null | undefined, max = INT4_MAX): number | null => (typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max ? v : null);
const big = (v: number | null | undefined): number | null => (typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? v : null);

/** A row Postgres accepts whatever a sandbox sent: an out-of-range number is null, text has no NUL and is capped. */
export function cleanRow(r: EgressRow): EgressRow {
  const at = r.at instanceof Date && !Number.isNaN(r.at.getTime()) ? r.at : new Date();
  return {
    at,
    conversation_id: typeof r.conversation_id === "string" && UUID.test(r.conversation_id) ? r.conversation_id : null,
    sandbox_id: typeof r.sandbox_id === "string" && UUID.test(r.sandbox_id) ? r.sandbox_id : null,
    kind: r.kind,
    method: text(r.method, TEXT_CAPS.method),
    host: text(r.host, TEXT_CAPS.host) || "-",
    port: typeof r.port === "number" && Number.isInteger(r.port) && r.port >= 1 && r.port <= 65535 ? r.port : null,
    path: text(r.path, TEXT_CAPS.path),
    status: int(r.status, 999),
    decision: text(r.decision, TEXT_CAPS.decision) || "-",
    bytes_up: big(r.bytes_up),
    bytes_down: big(r.bytes_down),
    ms: int(r.ms),
    cost_usd: typeof r.cost_usd === "number" && Number.isFinite(r.cost_usd) ? r.cost_usd : null,
    model: text(r.model, TEXT_CAPS.model),
    person: text(r.person, TEXT_CAPS.person),
    request: text(r.request, TEXT_CAPS.request),
    row_id: typeof r.row_id === "string" && UUID.test(r.row_id) ? r.row_id : randomUUID(),
  };
}

/** Postgres refused the data itself (data exception 22, integrity 23): retrying the same row can never succeed. */
export function isDataError(e: unknown): boolean {
  const code = (e as { code?: unknown })?.code;
  return typeof code === "string" && /^2[23][0-9A-Z]{3}$/.test(code);
}

export class EgressLog {
  readonly #db: Db;
  readonly #max: number;
  /** rows not yet handed to an insert, oldest first */
  #buf: EgressRow[] = [];
  /** the batch an insert is writing (put back in front when it fails) */
  #inflight: EgressRow[] = [];
  #timer: NodeJS.Timeout | null = null;
  #running: Promise<void> | null = null;
  #failing = false;
  written = 0;
  /** rows Postgres refused as data (also counted in `dropped` by kind) */
  rejected = 0;
  readonly dropped: Record<string, number> = {};

  constructor(db: Db, max = MAX_BUFFERED) {
    this.#db = db;
    this.#max = max;
  }

  /** rows in memory, not yet written */
  get buffered(): number {
    return this.#buf.length + this.#inflight.length;
  }

  add(raw: EgressRow): void {
    const row = cleanRow(raw);
    this.#buf.push(row);
    if (this.buffered > this.#max) {
      const i = this.#buf.findIndex((r) => r.kind !== "model");
      if (i >= 0) {
        const [gone] = this.#buf.splice(i, 1);
        this.dropped[gone!.kind] = (this.dropped[gone!.kind] ?? 0) + 1;
      }
    }
    if (row.kind === "model" || this.#buf.length >= 200) void this.flush();
    else this.#schedule(1000);
  }

  /** The cost of model rows not yet written whose time is before `before` (ms). */
  unwrittenModelCost(before: number): number {
    let s = 0;
    for (const r of this.#inflight) if (r.kind === "model" && r.cost_usd && r.at!.getTime() < before) s += r.cost_usd;
    for (const r of this.#buf) if (r.kind === "model" && r.cost_usd && r.at!.getTime() < before) s += r.cost_usd;
    return s;
  }

  #schedule(ms: number): void {
    this.#timer ??= setTimeout(() => {
      this.#timer = null;
      void this.flush();
    }, ms);
    this.#timer.unref?.();
  }

  /** Writes what is buffered, in order; resolves when the buffer is empty or an insert failed (retried in 1 s). */
  flush(): Promise<void> {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    // one writer at a time keeps the order; a flush asked for while one runs follows it (one queued at most)
    if (this.#queued) return this.#queued;
    const prev = this.#running ?? Promise.resolve();
    const next = prev.then(() => {
      this.#queued = null;
      return this.#drainBuffer();
    });
    if (this.#running) this.#queued = next;
    this.#running = next.finally(() => {
      if (this.#running === tail) this.#running = null;
    });
    const tail = this.#running;
    return next;
  }

  #queued: Promise<void> | null = null;

  async #insert(rows: EgressRow[]): Promise<void> {
    const vals: unknown[] = [];
    const tuples = rows.map((r, i) => {
      for (const c of COLS) vals.push(r[c] ?? null);
      return `(${COLS.map((_, j) => `$${i * COLS.length + j + 1}`).join(",")})`;
    });
    await this.#db.query(`insert into egress_log (${COLS.join(",")}) values ${tuples.join(",")} on conflict (row_id) do nothing`, vals);
  }

  #writing(): void {
    if (this.#failing) {
      this.#failing = false;
      log("egress log writing again", { buffered: this.buffered });
    }
  }

  /** The database is away: `rows` go back in front, in order, and the next try is in 1 s. */
  #outage(rows: EgressRow[], e: unknown): void {
    this.#buf = [...rows, ...this.#buf];
    this.#inflight = [];
    if (!this.#failing) logError("egress log insert", e, { buffered: this.buffered });
    this.#failing = true;
    this.#schedule(1000);
  }

  async #drainBuffer(): Promise<void> {
    while (this.#buf.length) {
      this.#inflight = this.#buf.splice(0, BATCH);
      const rows = this.#inflight;
      try {
        await this.#insert(rows);
        this.written += rows.length;
        this.#inflight = [];
        this.#writing();
        continue;
      } catch (e) {
        if (!isDataError(e)) return this.#outage(rows, e);
      }
      // a bad row in the batch: write it row by row, drop what Postgres refuses as data. #inflight holds only the
      // rows not yet written or dropped, so unwrittenModelCost never counts a row that is already in the table.
      for (let i = 0; i < rows.length; i++) {
        const r = rows[i]!;
        try {
          await this.#insert([r]);
          this.written++;
        } catch (e) {
          if (!isDataError(e)) return this.#outage(rows.slice(i), e);
          this.dropped[r.kind] = (this.dropped[r.kind] ?? 0) + 1;
          this.rejected++;
          logError("egress log row refused", e, { kind: r.kind, decision: r.decision, status: r.status, request: r.request });
        }
        this.#inflight = rows.slice(i + 1);
      }
      this.#inflight = [];
      this.#writing();
    }
  }

  /** Flushes, retrying every second while the database fails, giving up after `ms` (the drain's last step: the
   * database may be down). What is still unwritten then is lost: logged with its count and the model cost it held. */
  async close(ms: number): Promise<void> {
    const deadline = Date.now() + ms;
    const sleep = (n: number) => new Promise<void>((r) => setTimeout(r, Math.max(0, Math.min(n, deadline - Date.now()))));
    while (this.buffered && Date.now() < deadline) {
      let t: NodeJS.Timeout | undefined;
      await Promise.race([this.flush(), new Promise<void>((r) => (t = setTimeout(r, Math.max(0, deadline - Date.now()))))]);
      clearTimeout(t);
      if (this.buffered) await sleep(1000);
    }
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    if (this.buffered) {
      const rows = [...this.#inflight, ...this.#buf];
      const model = rows.filter((r) => r.kind === "model");
      logError("egress log lost at exit", "database unavailable", { rows: rows.length, modelRows: model.length, modelCostUsd: model.reduce((s, r) => s + (r.cost_usd ?? 0), 0) });
    }
  }
}
