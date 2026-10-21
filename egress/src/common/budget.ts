// The shared budget (docs/CHAT.md, "Budget"): one spending limit for everyone, model cost only. Spend = sum of
// egress_log.cost_usd (kind 'model': every model call Claude Code makes, its titles included) from config
// `budget.since`; the limit is config `budget.limitUsd` (change either in the instance's config.json).
//
// Checked by egress before every model call (its model rows are written at once and the budget re-read at most every
// 0.5 s). A call already in flight when the limit is crossed completes, so the spend can end a little above the limit
// (about one model call's cost per running conversation).
import type { BudgetConfig } from "./config.ts";
import type { Db } from "./db.ts";

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

/** null: no budget configured (no limit). */
export async function budgetStatus(db: Db, cfg: BudgetConfig | undefined): Promise<BudgetStatus | null> {
  if (!cfg) return null;
  const spentUsd = await spentSince(db, cfg.since);
  return {
    limitUsd: cfg.limitUsd,
    since: cfg.since,
    ownerName: cfg.ownerName ?? null,
    spentUsd,
    remainingUsd: Math.max(0, cfg.limitUsd - spentUsd),
    exhausted: spentUsd >= cfg.limitUsd,
  };
}

/** budgetStatus, remembered for ttlMs (egress asks before every model call). */
export class BudgetGate {
  readonly #db: Db;
  readonly #cfg: BudgetConfig | undefined;
  readonly #ttlMs: number;
  #hit: { at: number; v: Promise<BudgetStatus | null> } | null = null;
  constructor(db: Db, cfg: BudgetConfig | undefined, ttlMs = 2000) {
    this.#db = db;
    this.#cfg = cfg;
    this.#ttlMs = ttlMs;
  }
  status(): Promise<BudgetStatus | null> {
    if (this.#hit && Date.now() - this.#hit.at < this.#ttlMs) return this.#hit.v;
    const v = budgetStatus(this.#db, this.#cfg);
    this.#hit = { at: Date.now(), v };
    v.catch(() => (this.#hit = null));
    return v;
  }
  forget(): void {
    this.#hit = null;
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
