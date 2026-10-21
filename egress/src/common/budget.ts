// The shared budget (docs/CHAT.md, "Budget"): one spending limit for everyone, model cost only. Spend = sum of
// egress_log.cost_usd (kind 'model': every model call, titles included) from the budget's `since`. The limit comes from
// config `budget`, or from an admin's override (kv `budget:override`, set on the admin monitor) as long as config
// `budget` is what it was when the override was made: editing config.json takes over again. Every override change is
// an audit row (ops_events kind 'budget').
//
// Checked before a turn starts (web) and before every model call (egress /x/model; its model rows are written at once
// and the budget re-read at most every 0.5 s). A call already in flight when the limit is crossed completes, so the
// spend can end a little above the limit (about one model call's cost per running turn, rarely two when calls follow
// each other within half a second).
import type { BudgetConfig } from "./config.ts";
import { tx, type Db } from "./db.ts";

export const OVERRIDE_KEY = "budget:override";

export interface BudgetOverride {
  limitUsd: number;
  since: string;
  by: string;
  at: string;
  /** config `budget` when the override was made (null: none); a different one now means config was edited since */
  basis: { limitUsd: number; since: string } | null;
}

export interface BudgetStatus {
  limitUsd: number;
  since: string;
  ownerName: string | null;
  spentUsd: number;
  remainingUsd: number;
  exhausted: boolean;
  source: "config" | "override";
  override: BudgetOverride | null;
  config: BudgetConfig | null;
}

const sameBasis = (a: BudgetOverride["basis"], c: BudgetConfig | undefined) =>
  a == null ? c == null : c != null && a.limitUsd === c.limitUsd && Date.parse(a.since) === Date.parse(c.since);

/** The budget in force: an override made against the current config, else config `budget`; null = no limit. */
export async function effectiveBudget(db: Db, cfg: BudgetConfig | undefined): Promise<{ limitUsd: number; since: string; ownerName: string | null; source: "config" | "override"; override: BudgetOverride | null } | null> {
  const { rows } = await db.query<{ value: BudgetOverride }>("select value from kv where key = $1", [OVERRIDE_KEY]);
  const o = rows[0]?.value ?? null;
  const owner = cfg?.ownerName ?? null;
  if (o && sameBasis(o.basis, cfg)) return { limitUsd: o.limitUsd, since: o.since, ownerName: owner, source: "override", override: o };
  if (!cfg) return null;
  return { limitUsd: cfg.limitUsd, since: cfg.since, ownerName: owner, source: "config", override: null };
}

export async function spentSince(db: Db, since: string): Promise<number> {
  const { rows } = await db.query<{ s: string | null }>("select sum(cost_usd) as s from egress_log where kind = 'model' and at >= $1", [since]);
  return Number(rows[0]?.s ?? 0);
}

export async function budgetStatus(db: Db, cfg: BudgetConfig | undefined): Promise<BudgetStatus | null> {
  const b = await effectiveBudget(db, cfg);
  if (!b) return null;
  const spentUsd = await spentSince(db, b.since);
  return {
    limitUsd: b.limitUsd,
    since: b.since,
    ownerName: b.ownerName,
    spentUsd,
    remainingUsd: Math.max(0, b.limitUsd - spentUsd),
    exhausted: spentUsd >= b.limitUsd,
    source: b.source,
    override: b.override,
    config: cfg ?? null,
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

/** The error body egress answers a model call with once the budget is used up (claw shows "... (budget_exceeded): <message>"). */
export const BUDGET_ERROR_TYPE = "budget_exceeded";

/**
 * An admin's change: "reset" (spend counts from now, limit unchanged), "set" (a new limit, `since` unchanged), or
 * "clear" (back to config `budget`). Written with its audit row in one transaction.
 */
export async function changeBudget(
  db: Db,
  cfg: BudgetConfig | undefined,
  actor: string,
  change: { action: "reset" | "set" | "clear"; limitUsd?: number },
): Promise<BudgetStatus | null> {
  const before = await effectiveBudget(db, cfg);
  let next: BudgetOverride | null = null;
  if (change.action !== "clear") {
    const limitUsd = change.action === "set" ? change.limitUsd : before?.limitUsd;
    if (typeof limitUsd !== "number" || !Number.isFinite(limitUsd) || limitUsd < 0 || limitUsd > 100_000) throw new RangeError("limitUsd must be a number from 0 to 100000");
    next = {
      limitUsd: Math.round(limitUsd * 100) / 100,
      since: change.action === "reset" || !before ? new Date().toISOString() : before.since,
      by: actor,
      at: new Date().toISOString(),
      basis: cfg ? { limitUsd: cfg.limitUsd, since: cfg.since } : null,
    };
  }
  const spent = before ? await spentSince(db, before.since) : null;
  await tx(db, async (c) => {
    if (next) await c.query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [OVERRIDE_KEY, JSON.stringify(next)]);
    else await c.query("delete from kv where key = $1", [OVERRIDE_KEY]);
    await c.query("insert into ops_events (kind, ok, detail) values ('budget', true, $1)", [
      JSON.stringify({
        actor,
        action: change.action,
        before: before ? { limitUsd: before.limitUsd, since: before.since, source: before.source, spentUsd: spent } : null,
        after: next ? { limitUsd: next.limitUsd, since: next.since } : cfg ? { limitUsd: cfg.limitUsd, since: cfg.since, source: "config" } : null,
      }),
    ]);
  });
  return budgetStatus(db, cfg);
}

/** The last changes (audit rows), newest first. */
export async function budgetAudit(db: Db, limit = 20): Promise<{ at: string; actor: string; action: string; before: unknown; after: unknown }[]> {
  const { rows } = await db.query<{ at: Date; detail: any }>("select at, detail from ops_events where kind = 'budget' order by id desc limit $1", [limit]);
  return rows.map((r) => ({ at: new Date(r.at).toISOString(), actor: String(r.detail?.actor ?? ""), action: String(r.detail?.action ?? ""), before: r.detail?.before ?? null, after: r.detail?.after ?? null }));
}
