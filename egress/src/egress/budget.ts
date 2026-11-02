// The shared budget (docs/CHAT.md, "Budget"): one spending limit for everyone, model cost only. Spend = sum of
// egress_log.cost_usd (kind 'model') from config `budget.since`; the limit is `budget.limitUsd` (egress.json).
//
// Checked before every model call. The gate re-reads the database at most every 0.5 s (both replicas see each other's
// model costs that soon); a call waits for that read at most 250 ms after it began, and not at all while reads fail,
// so a database that refuses or hangs is answered from the last known status plus the model costs this replica has
// logged since that read (aviary platform spec 17.2 item 8): a running turn never stops and a replica does not
// overspend on stale data. A call already in flight when the limit is crossed completes, so the spend can end
// a little above the limit (about one model call's cost per running conversation).
import type { BudgetConfig } from "./config.ts";
import type { Db } from "./db.ts";
import type { EgressLog } from "./egresslog.ts";
import { logError } from "../common/log.ts";

export interface BudgetStatus {
  limitUsd: number;
  since: string;
  ownerName: string | null;
  spentUsd: number;
  remainingUsd: number;
  exhausted: boolean;
}

export async function spentSince(db: Db, since: string): Promise<number> {
  const { rows } = await db.query<{ s: string | null }>("select sum(cost_usd) as s from egress_log where kind = 'model' and at >= $1", [since]);
  return Number(rows[0]?.s ?? 0);
}

function status(cfg: BudgetConfig, spentUsd: number): BudgetStatus {
  return {
    limitUsd: cfg.limitUsd,
    since: cfg.since,
    ownerName: cfg.ownerName ?? null,
    spentUsd,
    remainingUsd: Math.max(0, cfg.limitUsd - spentUsd),
    exhausted: spentUsd >= cfg.limitUsd,
  };
}

/** How long after a read began a caller may wait for it (a read takes milliseconds while the database is well). */
const READ_WAIT_MS = 250;
/** budget_state is written at most this often. */
const STATE_EVERY_MS = 10_000;

export class BudgetGate {
  readonly #db: Db;
  readonly #cfg: BudgetConfig | undefined;
  readonly #ttlMs: number;
  #log: EgressLog | null = null;
  /** the last spend read from the database, and when that read started (ms) */
  #last: { spentUsd: number; readAt: number } | null = null;
  #reading: Promise<void> | null = null;
  #stateAt = 0;
  /** model costs this replica logged, with their time (ms), since the last read began */
  #local: Array<[number, number]> = [];
  failures = 0;
  /** answers given with no status ever known (seed and every read failed) */
  unknownServed = 0;
  #failing = false;
  #readStartedAt = 0;

  constructor(db: Db, cfg: BudgetConfig | undefined, ttlMs = 500) {
    this.#db = db;
    this.#cfg = cfg;
    this.#ttlMs = ttlMs;
  }

  /** The log whose unwritten model rows count while the database is away. */
  attach(log: EgressLog): void {
    this.#log = log;
  }

  /** A model call's cost, as it is logged. */
  noteCost(usd: number | null | undefined): void {
    if (this.#cfg && usd) this.#local.push([Date.now(), usd]);
  }

  /** Seeds the last known status from budget_state (at start, before this replica has read the log). */
  async seed(): Promise<void> {
    if (!this.#cfg || this.#last) return;
    const { rows } = await this.#db.query<{ spent_usd: string; computed_at: Date }>("select spent_usd, computed_at from budget_state where since = $1", [this.#cfg.since]);
    if (rows[0] && !this.#last) this.#last = { spentUsd: Number(rows[0].spent_usd), readAt: rows[0].computed_at.getTime() };
  }

  /** null: no budget configured. Never throws. A status older than the TTL starts a read and waits for it, but only
   * up to READ_WAIT_MS after that read began, and not at all while reads fail: a database that refuses or hangs
   * (a lost node, a pooled connection to a vanished primary) costs at most one READ_WAIT_MS at its start, then every
   * call is answered at once from the last known status plus this replica's costs since. With no status ever known
   * (no budget_state row seeded, every read failed) it counts from zero: this replica's own costs against the limit,
   * counted in `unknownServed` (/metrics) and logged. */
  async status(): Promise<BudgetStatus | null> {
    const cfg = this.#cfg;
    if (!cfg) return null;
    if (!this.#last || Date.now() - this.#last.readAt >= this.#ttlMs) {
      const r = this.#read();
      const left = this.#readStartedAt + READ_WAIT_MS - Date.now();
      if (!this.#failing && left > 0) {
        let t: NodeJS.Timeout | undefined;
        await Promise.race([r, new Promise<void>((ok) => (t = setTimeout(ok, left)))]);
        clearTimeout(t);
      }
    }
    if (!this.#last) {
      if (this.unknownServed++ % 100 === 0) logError("budget status", "no status known (database away since start)", { serving: "this replica's own costs from zero" });
      return status(cfg, this.#pending(0));
    }
    return status(cfg, this.#last.spentUsd + this.#pending(this.#last.readAt));
  }

  /** Costs not in the last read: logged since it began, or logged before it but not yet written. */
  #pending(readAt: number): number {
    let s = 0;
    for (const [at, usd] of this.#local) if (at >= readAt) s += usd;
    return s + (this.#log?.unwrittenModelCost(readAt) ?? 0);
  }

  #read(): Promise<void> {
    if (this.#reading) return this.#reading;
    const cfg = this.#cfg!;
    const readAt = Date.now();
    this.#readStartedAt = readAt;
    this.#reading = spentSince(this.#db, cfg.since)
      .then(
        (spentUsd) => {
          this.#failing = false;
          if (!this.#last || readAt >= this.#last.readAt) this.#last = { spentUsd, readAt };
          this.#local = this.#local.filter(([at]) => at >= readAt);
          if (Date.now() - this.#stateAt >= STATE_EVERY_MS) {
            this.#stateAt = Date.now();
            void this.#db
              .query(
                `insert into budget_state (since, limit_usd, spent_usd, computed_at) values ($1, $2, $3, $4)
                 on conflict (since) do update set limit_usd = excluded.limit_usd, spent_usd = excluded.spent_usd, computed_at = excluded.computed_at
                 where budget_state.computed_at <= excluded.computed_at`,
                [cfg.since, cfg.limitUsd, spentUsd, new Date(readAt)],
              )
              .catch(() => {});
          }
        },
        (e) => {
          this.#failing = true;
          if (this.failures++ === 0 || this.failures % 100 === 0) logError("budget read", e, { serving: this.#last ? "last known" : "none" });
        },
      )
      .finally(() => (this.#reading = null));
    return this.#reading;
  }
}

const amount = (usd: number) => (Number.isInteger(usd) ? String(usd) : usd.toFixed(2));

/** What staff see when the budget is used up, in the given locale (zh-HK or English). */
export function budgetMessage(locale: string, limitUsd: number, ownerName: string | null | undefined): string {
  if (locale.toLowerCase().startsWith("zh")) return `${ownerName || "管理員"} 設定了 ${amount(limitUsd)} 美元的使用上限，目前已用完。`;
  return `${ownerName || "The administrator"} has set a USD ${amount(limitUsd)} budget limit and it has now been used up.`;
}

/** The error type egress answers a model call with once the budget is used up (Claude Code shows the message). */
export const BUDGET_ERROR_TYPE = "budget_exceeded";
