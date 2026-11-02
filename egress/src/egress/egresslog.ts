// Every request through egress except magpie and hermit calls becomes one egress_log row. Rows are buffered and
// written in order, in batches (every second, at 200 rows, and at once for a model row: its cost counts toward the
// budget). A database outage never stops a request (aviary platform spec 17.2 item 8): unwritten rows stay in memory,
// at most MAX_BUFFERED, the oldest non-model row dropped first when full (counted) and never a model row, and are
// written in order once the database is back.
import type { Db } from "./db.ts";
import { logError, log } from "../common/log.ts";

export type RowKind = "connect" | "http" | "mirror" | "model" | "tools";

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
}

const COLS = ["at", "conversation_id", "sandbox_id", "kind", "method", "host", "port", "path", "status", "decision", "bytes_up", "bytes_down", "ms", "cost_usd", "model", "person", "request"] as const;
/** About 15 MB of rows. */
export const MAX_BUFFERED = 50_000;
const BATCH = 500;

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
  readonly dropped: Record<string, number> = {};

  constructor(db: Db, max = MAX_BUFFERED) {
    this.#db = db;
    this.#max = max;
  }

  /** rows in memory, not yet written */
  get buffered(): number {
    return this.#buf.length + this.#inflight.length;
  }

  add(row: EgressRow): void {
    row.at ??= new Date();
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

  async #drainBuffer(): Promise<void> {
    while (this.#buf.length) {
      this.#inflight = this.#buf.splice(0, BATCH);
      const rows = this.#inflight;
      const vals: unknown[] = [];
      const tuples = rows.map((r, i) => {
        for (const c of COLS) vals.push(c === "path" && r.path ? r.path.slice(0, 2000) : (r[c] ?? null));
        return `(${COLS.map((_, j) => `$${i * COLS.length + j + 1}`).join(",")})`;
      });
      try {
        await this.#db.query(`insert into egress_log (${COLS.join(",")}) values ${tuples.join(",")}`, vals);
        this.written += rows.length;
        this.#inflight = [];
        if (this.#failing) {
          this.#failing = false;
          log("egress log writing again", { buffered: this.buffered });
        }
      } catch (e) {
        this.#buf = [...rows, ...this.#buf];
        this.#inflight = [];
        if (!this.#failing) logError("egress log insert", e, { buffered: this.buffered });
        this.#failing = true;
        this.#schedule(1000);
        return;
      }
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
