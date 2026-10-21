// The operator page's data (docs/CHAT.md, "Operator page"): one read-only snapshot of the instance for config `admins`,
// served by web at GET /internal/ops/snapshot (the UI router forwards /ops/* there for admins only) and rendered by
// ops-page.ts (polled every 10 s). Sources: Postgres (sandboxes, vm_hosts, vm_events, kv demand hints, egress_log,
// ops_events) and, with provider vm, the scaler's own plan. Never returns message contents: owners, conversation ids,
// hosts, counts, costs and timings only.
import type { Db } from "../common/db.ts";
import { budgetStatus } from "../common/budget.ts";
import type { WebDeps } from "./ctx.ts";
import { DEMAND_WINDOW_S } from "./sandbox.ts";
import { VmProvider, type HostRow } from "./sandbox-vm.ts";

const CACHE_MS = 4000;
const SECTION_TIMEOUT_MS = 8000;
const HOST_FRESH_MS = 30_000;

type Row = Record<string, any>;
const num = (v: unknown): number | null => (v == null ? null : Number(v));
const iso = (v: unknown): string | null => (v == null ? null : new Date(v as string).toISOString());

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  let t: NodeJS.Timeout;
  return Promise.race([p, new Promise<T>((_, rej) => (t = setTimeout(() => rej(new Error(`${what}: no answer in ${ms} ms`)), ms)))]).finally(() => clearTimeout(t));
}

/** A time zone name the page may ask for (else the instance's default). */
export function safeZone(tz: string | null | undefined, fallback = "Asia/Hong_Kong"): string {
  return tz && tz.length < 64 && /^[A-Za-z]+(?:[/_+-][A-Za-z0-9]+)*$/.test(tz) ? tz : fallback;
}

export class OpsSnapshot {
  readonly #deps: WebDeps;
  #cache = new Map<string, { at: number; value: Promise<Row> }>();

  constructor(deps: WebDeps) {
    this.#deps = deps;
  }

  /** The snapshot for a page in time zone `tz` ("today" and "this month" start at its midnight); cached 4 s per zone. */
  get(tz: string): Promise<Row> {
    const zone = safeZone(tz);
    const hit = this.#cache.get(zone);
    if (hit && Date.now() - hit.at < CACHE_MS) return hit.value;
    const value = this.#build(zone);
    this.#cache.set(zone, { at: Date.now(), value });
    value.catch(() => this.#cache.delete(zone));
    return value;
  }

  async #build(tz: string): Promise<Row> {
    const db = this.#deps.db;
    const errors: Record<string, string> = {};
    const section = async <T>(name: string, fn: () => Promise<T>): Promise<T | null> => {
      try {
        return await withTimeout(fn(), SECTION_TIMEOUT_MS, name);
      } catch (e) {
        errors[name] = (e as Error).message.slice(0, 300);
        return null;
      }
    };
    let day: Date, month: Date;
    try {
      const { rows } = await db.query<{ d: Date; m: Date }>(
        "select (date_trunc('day', now() at time zone $1) at time zone $1) as d, (date_trunc('month', now() at time zone $1) at time zone $1) as m",
        [tz],
      );
      ({ d: day, m: month } = rows[0]!);
    } catch {
      tz = "UTC";
      const { rows } = await db.query<{ d: Date; m: Date }>("select date_trunc('day', now()) as d, date_trunc('month', now()) as m");
      ({ d: day, m: month } = rows[0]!);
    }
    const [sandboxes, hosts, scaler, events, budget, egress, people] = await Promise.all([
      section("sandboxes", () => this.#sandboxes()),
      section("hosts", () => this.#hosts()),
      section("scaler", () => this.#scaler()),
      section("events", () => this.#events()),
      section("budget", () => this.#budget(day, month)),
      section("egress", () => this.#egress()),
      section("people", () => this.#people(day)),
    ]);
    const cfg = this.#deps.config;
    return {
      at: new Date().toISOString(),
      tz,
      today: day.toISOString(),
      monthStart: month.toISOString(),
      product: cfg.product.name,
      provider: this.#deps.sandboxProvider.kind,
      sandboxImage: cfg.sandbox.image,
      sandboxes,
      hosts,
      scaler,
      events,
      budget,
      egress,
      people,
      errors,
    };
  }

  async #sandboxes(): Promise<Row> {
    const db = this.#deps.db;
    const cfg = this.#deps.config.sandbox;
    const [{ rows }, { rows: starts }, { rows: releases }] = await Promise.all([
      db.query(
        `select s.id, s.conversation_id as "conversationId", s.owner, s.host, s.status, s.created_at as "createdAt",
                s.last_active_at as "lastActiveAt", s.expires_at as "expiresAt",
                (select count(*) from vm_events v where v.sandbox_id = s.id and v.kind = 'migration')::int as migrations
           from sandboxes s where s.status <> 'gone' order by s.created_at`,
      ),
      db.query(
        `select count(*)::int as n, count(*) filter (where ok)::int as ok, count(*) filter (where not ok)::int as failed,
                percentile_cont(0.5) within group (order by ms) filter (where ok) as p50,
                percentile_cont(0.95) within group (order by ms) filter (where ok) as p95
           from ops_events where kind = 'sandbox_start' and at > now() - interval '24 hours'`,
      ),
      db.query(
        `select coalesce(regexp_replace(release_reason, ':.*', ''), '?') as reason, count(*)::int as n from sandboxes
          where released_at > now() - interval '24 hours' group by 1 order by 2 desc`,
      ),
    ]);
    const now = Date.now();
    const s = starts[0] ?? {};
    return {
      maxActive: cfg.maxActive,
      idleMinutes: cfg.idleMinutes,
      maxLifetimeHours: cfg.maxLifetimeHours,
      list: rows.map((r) => ({
        ...r,
        createdAt: iso(r.createdAt),
        lastActiveAt: iso(r.lastActiveAt),
        expiresAt: iso(r.expiresAt),
        ageS: Math.round((now - new Date(r.createdAt).getTime()) / 1000),
        idleS: Math.round((now - new Date(r.lastActiveAt).getTime()) / 1000),
      })),
      starts24h: { n: s.n ?? 0, ok: s.ok ?? 0, failed: s.failed ?? 0, p50Ms: num(s.p50), p95Ms: num(s.p95) },
      releases24h: releases,
    };
  }

  /** Sandbox hosts (AWS and the gateway): live ones, and those gone in the last 24 hours. */
  async #hosts(): Promise<Row> {
    const db = this.#deps.db;
    const p = this.#deps.sandboxProvider;
    const ctl = p instanceof VmProvider ? p.controller : null;
    const [{ rows }, { rows: used }] = await Promise.all([
      db.query<HostRow & { gone_at: Date | null; gone_reason: string | null }>(
        "select * from vm_hosts where status <> 'gone' or gone_at > now() - interval '24 hours' order by (status = 'gone'), kind desc, created_at",
      ),
      db.query<{ host: string; n: number }>("select host, count(*)::int as n from sandboxes where status <> 'gone' and host is not null group by host"),
    ]);
    const on = new Map(used.map((u) => [u.host, u.n]));
    const now = Date.now();
    return {
      maxHosts: this.#deps.config.sandbox.vm?.aws?.maxHosts ?? null,
      list: rows.map((h) => ({
        name: h.name,
        kind: h.kind,
        gateway: !!h.info?.gateway,
        status: h.status,
        instanceId: h.instance_id,
        launchedAt: iso(h.created_at),
        lastSeenAt: iso(h.last_seen_at),
        reachable: !!h.last_seen_at && now - new Date(h.last_seen_at).getTime() < HOST_FRESH_MS,
        emptySince: iso(h.empty_since),
        goneAt: iso(h.gone_at),
        goneReason: h.gone_reason,
        retiring: h.status === "retiring",
        sandboxes: on.get(h.name) ?? 0,
        slots: ctl && h.status !== "gone" && !h.info?.gateway ? ctl.slots(h) : null,
        maxSandboxes: h.max_sandboxes,
        cpus: h.cpus,
        memTotal: num(h.mem_total),
        memAvailable: num(h.info?.memAvailable),
        load1: h.info?.loadAvg?.[0] ?? null,
        gvisor: h.info?.gvisorRelease ?? null,
      })),
    };
  }

  /** What the scaler sees: its plan (provider vm) and the demand hints (people typing) behind it. */
  async #scaler(): Promise<Row> {
    const db = this.#deps.db;
    const p = this.#deps.sandboxProvider;
    const { rows } = await db.query<{ key: string; value: { at?: string } }>(
      "select key, value from kv where key like 'sandbox_demand:%' order by (value->>'at') desc nulls last limit 30",
    );
    const now = Date.now();
    const hints = rows.map((r) => {
      const at = r.value?.at ?? null;
      return { who: r.key.slice("sandbox_demand:".length), at, counted: !!at && now - Date.parse(at) < DEMAND_WINDOW_S * 1000 };
    });
    const plan = p instanceof VmProvider ? await p.controller.plan() : null;
    return { windowS: DEMAND_WINDOW_S, hints, plan };
  }

  async #events(): Promise<Row[]> {
    const { rows } = await this.#deps.db.query(
      `select v.at, v.kind, v.host, v.sandbox_id as "sandboxId", s.owner, s.conversation_id as "conversationId", v.data
         from vm_events v left join sandboxes s on s.id = v.sandbox_id order by v.id desc limit 60`,
    );
    return rows.map((r) => ({ ...r, at: iso(r.at) }));
  }

  /** Model spend (egress_log kind model): the shared budget, today, this month, per person and per model. */
  async #budget(day: Date, month: Date): Promise<Row> {
    const db: Db = this.#deps.db;
    const cfg = this.#deps.config.budget;
    const since = cfg?.since ?? month.toISOString();
    // everything from the earliest of the three windows, split in SQL
    const from = new Date(Math.min(month.getTime(), Date.parse(since)));
    const sums = `coalesce(sum(e.cost_usd) filter (where e.at >= $2), 0)::float as today,
                  coalesce(sum(e.cost_usd) filter (where e.at >= $3), 0)::float as month,
                  coalesce(sum(e.cost_usd) filter (where e.at >= $4), 0)::float as "sinceBudget",
                  count(*) filter (where e.at >= $2)::int as "callsToday",
                  count(*) filter (where e.at >= $3)::int as "callsMonth"`;
    const args = [from, day, month, since];
    const [status, { rows: tot }, { rows: byUser }, { rows: byModel }] = await Promise.all([
      budgetStatus(db, cfg),
      db.query(`select ${sums} from egress_log e where e.kind = 'model' and e.at >= $1`, args),
      db.query(
        `select coalesce(s.owner, '(unknown)') as owner, ${sums}
           from egress_log e left join sandboxes s on s.id = e.sandbox_id
          where e.kind = 'model' and e.at >= $1 group by 1 order by month desc, "sinceBudget" desc`,
        args,
      ),
      db.query(
        `select coalesce(e.model, '(not recorded)') as model, ${sums}
           from egress_log e where e.kind = 'model' and e.at >= $1 group by 1 order by month desc, "sinceBudget" desc`,
        args,
      ),
    ]);
    return { status, total: tot[0] ?? null, byUser, byModel };
  }

  /** Recent egress refusals and errors: model calls that failed (402 budget, 503, …) and blocked hosts. */
  async #egress(): Promise<Row> {
    const db = this.#deps.db;
    const [{ rows: byStatus }, { rows: recent }, { rows: denied }] = await Promise.all([
      db.query(
        `select kind, coalesce(status::text, 'none') as status, decision, count(*)::int as n, max(at) as "lastAt"
           from egress_log where at > now() - interval '24 hours' and kind in ('model', 'inject')
            and (status >= 400 or status is null or decision like 'deny%')
          group by 1, 2, 3 order by n desc limit 20`,
      ),
      db.query(
        `select e.at, e.kind, e.host, e.path, e.status, e.decision, s.owner, e.conversation_id as "conversationId"
           from egress_log e left join sandboxes s on s.id = e.sandbox_id
          where e.at > now() - interval '24 hours' and (e.status >= 400 or e.decision like 'deny%')
          order by e.id desc limit 30`,
      ),
      db.query(
        `select host, regexp_replace(decision, '^deny:?', '') as why, count(*)::int as n, max(at) as "lastAt"
           from egress_log where decision like 'deny%' and kind not in ('model', 'inject') and at > now() - interval '24 hours'
          group by 1, 2 order by n desc limit 20`,
      ),
    ]);
    return {
      byStatus: byStatus.map((r) => ({ ...r, lastAt: iso(r.lastAt) })),
      recent: recent.map((r) => ({ ...r, at: iso(r.at) })),
      deniedHosts: denied.map((r) => ({ ...r, lastAt: iso(r.lastAt) })),
    };
  }

  /**
   * Conversations per staff member, counted from their sandboxes (a conversation gets one at its first message, so a
   * conversation that never sent anything is not counted): total, started today, last 7 days, with a live sandbox,
   * deleted. Every config user is listed, also with none.
   */
  async #people(day: Date): Promise<Row[]> {
    const { rows } = await this.#deps.db.query(
      `select owner, count(distinct conversation_id)::int as total,
              (count(distinct conversation_id) filter (where first_at >= $1))::int as today,
              (count(distinct conversation_id) filter (where first_at > now() - interval '7 days'))::int as "last7d",
              (count(distinct conversation_id) filter (where live))::int as live,
              (count(distinct conversation_id) filter (where deleted))::int as deleted,
              max(last_at) as "lastActiveAt"
         from (select owner, conversation_id, min(created_at) over (partition by conversation_id) as first_at,
                      last_active_at as last_at, status <> 'gone' as live, release_reason = 'deleted' as deleted
                 from sandboxes where owner is not null) x
        group by owner`,
      [day],
    );
    const by = new Map(rows.map((r) => [r.owner, r]));
    const users = this.#deps.config.users;
    const out = users.map((u) => by.get(u) ?? { owner: u, total: 0, today: 0, last7d: 0, live: 0, deleted: 0, lastActiveAt: null });
    for (const r of rows) if (!users.includes(r.owner)) out.push({ ...r, former: true });
    return out.map((r) => ({ ...r, lastActiveAt: iso(r.lastActiveAt) }));
  }
}
