// Every request through egress becomes one egress_log row. Rows are buffered and written in batches (every second
// or every 200 rows); flush() on shutdown writes the rest.
import type { Db } from "../common/db.ts";
import { logError } from "../common/log.ts";

export interface EgressRow {
  conversation_id?: string | null;
  sandbox_id?: string | null;
  kind: "connect" | "http" | "mirror" | "model" | "inject";
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
}

const COLS = ["conversation_id", "sandbox_id", "kind", "method", "host", "port", "path", "status", "decision", "bytes_up", "bytes_down", "ms", "cost_usd"] as const;

export class EgressLog {
  readonly #db: Db;
  #buf: EgressRow[] = [];
  #timer: NodeJS.Timeout | null = null;
  #flushing: Promise<void> = Promise.resolve();
  written = 0;
  constructor(db: Db) {
    this.#db = db;
  }
  add(row: EgressRow): void {
    this.#buf.push(row);
    if (this.#buf.length >= 200) void this.flush();
    else this.#timer ??= setTimeout(() => void this.flush(), 1000);
  }
  flush(): Promise<void> {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    const rows = this.#buf;
    this.#buf = [];
    if (!rows.length) return this.#flushing;
    this.#flushing = this.#flushing.then(async () => {
      const vals: unknown[] = [];
      const tuples = rows.map((r, i) => {
        for (const c of COLS) vals.push(c === "path" && r.path ? r.path.slice(0, 2000) : (r[c] ?? null));
        return `(${COLS.map((_, j) => `$${i * COLS.length + j + 1}`).join(",")})`;
      });
      try {
        await this.#db.query(`insert into egress_log (${COLS.join(",")}) values ${tuples.join(",")}`, vals);
        this.written += rows.length;
      } catch (e) {
        logError("egress log insert", e, { rows: rows.length });
      }
    });
    return this.#flushing;
  }
}
