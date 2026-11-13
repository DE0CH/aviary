// What egress tells hermit's ops page and the admin tooling (aviary platform spec 15.3, 17.2 item 4; gateway-api
// ops.rs): `GET /v1/ops/summary` (model spend and the budget, refusals and errors) and `GET /v1/admin/egress-log`
// (one conversation's rows). Read only from egress's own database. Every `owner` is the person's lowercased email;
// rows with no person (none since the gateway contract; old copied rows may lack one) count in the totals only.
import type { BudgetGate } from "./budget.ts";
import type { BudgetConfig } from "./config.ts";
import type { Db } from "./db.ts";

type Row = Record<string, any>;
const iso = (v: unknown): string | null => (v == null ? null : new Date(v as string).toISOString());
const num = (v: unknown): number => Number(v ?? 0);

/** not an error: an MCP client's optional GET stream that the tools server turns down with 405 (MCP streamable HTTP),
 *  and Claude Code's token count that a model without one answers 404 (it counts locally then) */
const NOISE = (e = "") => `((${e}kind in ('inject', 'tools') and ${e}status = 405) or (${e}path like '%/count_tokens' and ${e}status = 404))`;
/** a glitch, not an error: the provider busy or its upstream down for a moment (429, 5xx, no answer at all); Claude
 *  Code retries it */
const GLITCH = (e = "") => `(${e}decision = 'allow' and (${e}status = 429 or ${e}status >= 500 or ${e}status is null))`;
/** the caller's mistake, not ours: a tools or route call that reached its service and was turned down for what the
 *  agent sent (bad arguments 400, an unknown tool or path 404, the wrong method 405, 409, too large 413, 415, the
 *  tool's own "change this" 422). As an API provider is not alerted when a client misuses an endpoint. Still ours:
 *  401 and 403 (the pair token, the catalogue), a call egress itself refused (deny…), every model call */
const CALLER = (e = "") => `(${e}kind in ('inject', 'tools') and ${e}decision = 'allow' and ${e}status in (400, 404, 405, 409, 413, 415, 422))`;
const QUIET = (e = "") => `(${NOISE(e)} or ${GLITCH(e)} or ${CALLER(e)})`;
/** model, tools and sandbox route calls (`inject` is the old stack's kind for its tools route, in copied rows) */
const CALLS = "('model', 'inject', 'tools', 'route')";
const OWNER = (e = "") => `lower(trim(coalesce(${e}person, ${e}owner)))`;

/** An IANA zone name Postgres and Intl both know, else UTC. */
export function zoneOr(tz: string | null | undefined, d = "UTC"): string {
  if (!tz || !/^[A-Za-z0-9_+\-/]{1,64}$/.test(tz)) return d;
  try {
    new Intl.DateTimeFormat("en", { timeZone: tz });
    return tz;
  } catch {
    return d;
  }
}

export async function opsSummary(db: Db, gate: BudgetGate | null, cfg: BudgetConfig | undefined, tzIn: string | null, recentMinutesIn: number | null): Promise<Row> {
  const tz = zoneOr(tzIn);
  const recentMinutes = Number.isInteger(recentMinutesIn) && recentMinutesIn! > 0 && recentMinutesIn! <= 1440 ? recentMinutesIn! : 15;
  const { rows: b } = await db.query<{ day: Date; month: Date }>(
    "select date_trunc('day', now() at time zone $1) at time zone $1 as day, date_trunc('month', now() at time zone $1) at time zone $1 as month",
    [tz],
  );
  const day = b[0]!.day;
  const month = b[0]!.month;
  const [budget, egress, recent] = await Promise.all([spend(db, gate, cfg, tz, day, month), refusals(db), lately(db, recentMinutes)]);
  return { budget, egress, recent };
}

async function spend(db: Db, gate: BudgetGate | null, cfg: BudgetConfig | undefined, tz: string, day: Date, month: Date): Promise<Row> {
  const since = cfg?.since ?? month.toISOString();
  const from = new Date(Math.min(month.getTime(), Date.parse(since)));
  const sums = `coalesce(sum(e.cost_usd) filter (where e.at >= $2), 0)::float as today,
                coalesce(sum(e.cost_usd) filter (where e.at >= $3), 0)::float as month,
                coalesce(sum(e.cost_usd) filter (where e.at >= $4), 0)::float as "sinceBudget",
                count(*) filter (where e.at >= $2)::int as "callsToday",
                count(*) filter (where e.at >= $3)::int as "callsMonth"`;
  const args = [from, day, month, since];
  const [status, { rows: tot }, { rows: byUser }, { rows: byModel }, { rows: byDay }] = await Promise.all([
    gate?.status() ?? Promise.resolve(null),
    db.query(`select ${sums} from egress_log e where e.kind = 'model' and e.at >= $1`, args),
    db.query(
      `select ${OWNER("e.")} as owner, ${sums} from egress_log e
        where e.kind = 'model' and e.at >= $1 and coalesce(e.person, e.owner) like '%@%'
        group by 1 order by month desc, "sinceBudget" desc`,
      args,
    ),
    db.query(
      `select coalesce(e.model, '(not recorded)') as model, ${sums}
         from egress_log e where e.kind = 'model' and e.at >= $1 group by 1 order by month desc, "sinceBudget" desc`,
      args,
    ),
    db.query(
      `select to_char(date_trunc('day', e.at at time zone $1), 'YYYY-MM-DD') as day, coalesce(sum(e.cost_usd), 0)::float as usd,
              count(*)::int as calls, count(distinct ${OWNER("e.")})::int as people
         from egress_log e where e.kind = 'model' and e.at > now() - interval '14 days' group by 1 order by 1 desc`,
      [tz],
    ),
  ]);
  return {
    status: status
      ? { spentUsd: status.spentUsd, limitUsd: status.limitUsd, since: status.since, exhausted: status.exhausted, remainingUsd: status.remainingUsd, ownerName: status.ownerName }
      : { spentUsd: num(tot[0]?.sinceBudget), limitUsd: cfg?.limitUsd ?? null, since: cfg?.since ?? null, exhausted: false },
    total: tot[0] ?? { today: 0, month: 0, sinceBudget: 0, callsToday: 0, callsMonth: 0 },
    byUser,
    byModel,
    byDay,
  };
}

/** Refusals and errors over 24 h: model and tools calls that failed (402 budget, 503, …) and blocked hosts. A
 *  caller's mistake (CALLER) is not among the errors by kind; in `recent` it is marked quiet. */
async function refusals(db: Db): Promise<Row> {
  const [{ rows: byStatus }, { rows: recent }, { rows: denied }] = await Promise.all([
    db.query(
      `select kind, status, decision, count(*)::int as n, max(at) as "lastAt"
         from egress_log where at > now() - interval '24 hours' and kind in ${CALLS}
          and (status >= 400 or status is null or decision like 'deny%') and not ${CALLER()}
        group by 1, 2, 3 order by n desc limit 20`,
    ),
    db.query(
      `select e.at, e.kind, e.host, e.path, e.status, e.decision, ${OWNER("e.")} as owner, e.conversation_id as "conversationId",
              ${QUIET("e.")} as quiet
         from egress_log e
        where e.at > now() - interval '24 hours' and (e.status >= 400 or e.decision like 'deny%')
        order by e.id desc limit 30`,
    ),
    db.query(
      `select host, regexp_replace(decision, '^deny:?', '') as why, count(*)::int as n, max(at) as "lastAt"
         from egress_log where decision like 'deny%' and kind not in ${CALLS} and at > now() - interval '24 hours'
        group by 1, 2 order by n desc limit 20`,
    ),
  ]);
  return {
    byStatus: byStatus.map((r) => ({ ...r, lastAt: iso(r.lastAt) })),
    recent: recent.map((r) => ({ ...r, at: iso(r.at) })),
    deniedHosts: denied.map((r) => ({ ...r, lastAt: iso(r.lastAt) })),
  };
}

/** The last `minutes`: failed model and tools calls (not the quiet ones), the callers' own mistakes as a separate
 *  count, and blocked internet requests. */
async function lately(db: Db, minutes: number): Promise<Row> {
  const win = `now() - make_interval(mins => ${minutes})`;
  const [{ rows: c }, { rows: list }] = await Promise.all([
    db.query(
      `select (select count(*) from egress_log where at > ${win} and kind in ${CALLS} and (status >= 400 or decision like 'deny%') and not ${QUIET()})::int as "modelErrors",
              (select count(*) from egress_log where at > ${win} and kind = 'model' and ${GLITCH()})::int as "glitches",
              (select count(*) from egress_log where at > ${win} and ${CALLER()})::int as "callerErrors",
              (select count(*) from egress_log where at > ${win} and kind not in ${CALLS} and decision like 'deny%')::int as "blocked"`,
    ),
    db.query(
      `select e.at, e.kind || ' ' || coalesce(e.status::text, '–') as what, e.decision || ' ' || coalesce(e.host, '') as detail, ${OWNER("e.")} as owner
         from egress_log e
        where e.at > ${win} and e.kind in ${CALLS} and (e.status >= 400 or e.decision like 'deny%') and not ${QUIET("e.")}
        order by e.at desc limit 20`,
    ),
  ]);
  return { ...(c[0] ?? { modelErrors: 0, glitches: 0, callerErrors: 0, blocked: 0 }), list: list.map((r) => ({ ...r, at: iso(r.at), detail: String(r.detail ?? "").slice(0, 300) })) };
}

/** `GET /v1/admin/egress-log?conversation=&since=&limit=`: newest first. */
export async function egressLogRows(db: Db, q: { conversation: string | null; since: string | null; limit: number | null }): Promise<Row[]> {
  const where: string[] = [];
  const args: unknown[] = [];
  if (q.conversation) {
    args.push(q.conversation);
    where.push(`conversation_id = $${args.length}`);
  }
  if (q.since) {
    args.push(q.since);
    where.push(`at >= $${args.length}`);
  }
  const limit = Number.isInteger(q.limit) && q.limit! > 0 ? Math.min(q.limit!, 1000) : 200;
  const { rows } = await db.query(
    `select at, kind, method, host, path, status, decision, bytes_up as "bytesUp", bytes_down as "bytesDown", ms,
            cost_usd::float as "costUsd", ${OWNER()} as owner, conversation_id as "conversationId", sandbox_id as "sandboxId"
       from egress_log ${where.length ? `where ${where.join(" and ")}` : ""} order by id desc limit ${limit}`,
    args,
  );
  return rows.map((r) => ({ ...r, at: iso(r.at), bytesUp: r.bytesUp == null ? null : Number(r.bytesUp), bytesDown: r.bytesDown == null ? null : Number(r.bytesDown) }));
}
