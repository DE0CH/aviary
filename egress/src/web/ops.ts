// The operator page's data (docs/CHAT.md, "Operator page"): one read-only snapshot of the instance for config `admins`,
// served by web at GET /internal/ops/snapshot (the UI router forwards /ops/* there for admins only) and rendered by
// ops-page.ts (polled every 10 s). Two halves the page keeps apart:
// - now: the service (Deployments and pods, read from the Kubernetes API with Role web-ops-read), the gateway and
//   the hosts that exist, live sandboxes, the scaler's plan and warm hints, the budget, errors of the last 15 minutes,
//   and `health`, a verdict over all of it (ok / warming / watch / problem, with the reasons);
// - history: hosts gone, vm_events, sandbox starts and releases with timings, egress errors over 24 h, spend per
//   day/person/model, conversations per staff member.
// Sources: Postgres (sandboxes, vm_hosts, vm_events, kv demand hints, egress_log, ops_events) and, with provider vm,
// the scaler's own plan. Never returns message contents: owners, conversation ids, hosts, counts, costs and timings.
import type { Db } from "../common/db.ts";
import { isUser } from "../common/config.ts";
import { budgetStatus } from "../common/budget.ts";
import type { WebDeps } from "./ctx.ts";
import type { Pod } from "./k8s.ts";
import { DEMAND_WINDOW_S } from "./sandbox.ts";
import { VmProvider, type HostRow } from "./sandbox-vm.ts";

const CACHE_MS = 4000;
const SECTION_TIMEOUT_MS = 8000;
const HOST_FRESH_MS = 30_000;
/** hosts gone in the last 24 hours that are listed (the latest); the rest are only counted */
const GONE_HOSTS_SHOWN = 30;
/** "current" errors: this many minutes back */
export const RECENT_MIN = 15;
/** the service's Deployments, in the order the page shows them */
export const SERVICE_ROLES = ["web", "egress", "tools", "ui", "cloudflared"] as const;
/** a host still booting after this long, or a sandbox still starting, is a problem rather than warming up */
const HOST_BOOT_SLOW_S = 300;
const SANDBOX_START_SLOW_S = 180;
/** the budget is worth a look from this share on */
const BUDGET_WATCH = 0.8;
/** not an error: an MCP client's optional GET stream that the tools server turns down with 405 (MCP streamable HTTP) */
const NOT_NOISE = "not (kind = 'inject' and status = 405)";

type Row = Record<string, any>;
export type Level = "ok" | "warming" | "watch" | "problem";
export interface Finding {
  level: Exclude<Level, "ok">;
  /** what it is about: service, gateway, host, sandbox, budget, errors, page */
  about: string;
  text: string;
}
const num = (v: unknown): number | null => (v == null ? null : Number(v));
const iso = (v: unknown): string | null => (v == null ? null : new Date(v as string).toISOString());
const RANK: Record<Level, number> = { ok: 0, warming: 1, watch: 2, problem: 3 };

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  let t: NodeJS.Timeout;
  return Promise.race([p, new Promise<T>((_, rej) => (t = setTimeout(() => rej(new Error(`${what}: no answer in ${ms} ms`)), ms)))]).finally(() => clearTimeout(t));
}

/** A time zone name the page may ask for (else the instance's default). */
export function safeZone(tz: string | null | undefined, fallback = "Asia/Hong_Kong"): string {
  return tz && tz.length < 64 && /^[A-Za-z]+(?:[/_+-][A-Za-z0-9]+)*$/.test(tz) ? tz : fallback;
}

function secs(s: number): string {
  s = Math.max(0, Math.round(s));
  if (s < 90) return `${s} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

/** The verdict over the "now" half: every reason it isn't simply fine, worst first. */
export function healthOf(d: Row): { state: Level; findings: Finding[] } {
  const out: Finding[] = [];
  const add = (level: Finding["level"], about: string, text: string) => out.push({ level, about, text });
  for (const [name, msg] of Object.entries((d.errors ?? {}) as Record<string, string>)) add("problem", "page", `Could not read ${name}: ${msg}`);
  const svc = d.service;
  if (svc?.available) {
    for (const x of svc.deployments as Row[]) {
      if (x.missing) add("problem", "service", `${x.name}: no Deployment`);
      else if (x.state === "problem") add("problem", "service", `${x.name}: ${x.ready} of ${x.desired} pods ready`);
      else if (x.state === "warming") add("warming", "service", `${x.name}: rolling out (${x.updated} of ${x.desired} updated, ${x.ready} ready)`);
    }
  }
  const now = Date.parse(d.at);
  for (const h of (d.hosts?.list ?? []) as Row[]) {
    const age = h.launchedAt ? (now - Date.parse(h.launchedAt)) / 1000 : 0;
    if (h.gateway) {
      if (!h.reachable) add("problem", "gateway", `Gateway ${h.name} not answering (last seen ${h.lastSeenAt ? secs((now - Date.parse(h.lastSeenAt)) / 1000) + " ago" : "never"})`);
    } else if (h.status === "booting") {
      if (age > HOST_BOOT_SLOW_S) add("problem", "host", `Host ${h.name} still booting after ${secs(age)}`);
      else add("warming", "host", `Host ${h.name} booting (${secs(age)})`);
    } else if (h.status === "retiring" || h.status === "draining") {
      add("warming", "host", `Host ${h.name} ${h.status}: its sandboxes move off, then it is terminated`);
    } else if (!h.reachable) {
      add("problem", "host", `Host ${h.name} not answering`);
    }
  }
  if (d.gatewayMissing) add("problem", "gateway", "No gateway host is registered");
  for (const s of (d.sandboxes?.list ?? []) as Row[]) {
    if (s.status === "starting") {
      if (s.ageS > SANDBOX_START_SLOW_S) add("problem", "sandbox", `Sandbox of ${s.owner ?? "?"} still starting after ${secs(s.ageS)}`);
      else add("warming", "sandbox", `Sandbox of ${s.owner ?? "?"} starting (${secs(s.ageS)})`);
    }
  }
  const st = d.budget?.status;
  if (st) {
    if (st.exhausted) add("problem", "budget", `Budget used up: USD ${Number(st.spentUsd).toFixed(2)} of ${st.limitUsd}; model calls are refused`);
    else if (st.limitUsd > 0 && st.spentUsd / st.limitUsd >= BUDGET_WATCH)
      add("watch", "budget", `Budget ${Math.round((st.spentUsd / st.limitUsd) * 100)}% used (USD ${Number(st.spentUsd).toFixed(2)} of ${st.limitUsd})`);
  }
  const r = d.recent;
  if (r) {
    if (r.failedStarts) add("problem", "errors", `${r.failedStarts} sandbox start${r.failedStarts > 1 ? "s" : ""} failed in the last ${RECENT_MIN} min`);
    if (r.modelErrors) add("problem", "errors", `${r.modelErrors} model or route call${r.modelErrors > 1 ? "s" : ""} failed in the last ${RECENT_MIN} min`);
    if (r.hostErrors) add("problem", "errors", `${r.hostErrors} host or move error${r.hostErrors > 1 ? "s" : ""} in the last ${RECENT_MIN} min`);
  }
  out.sort((a, b) => RANK[b.level] - RANK[a.level]);
  return { state: out[0]?.level ?? "ok", findings: out };
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
    const [service, sandboxes, hosts, scaler, recent, budget, hostsGone, events, starts, egress, people] = await Promise.all([
      section("service", () => this.#service()),
      section("sandboxes", () => this.#sandboxes()),
      section("hosts", () => this.#hosts()),
      section("scaler", () => this.#scaler()),
      section("recent errors", () => this.#recent()),
      section("budget", () => this.#budget(tz, day, month)),
      section("hosts gone", () => this.#hostsGone()),
      section("events", () => this.#events()),
      section("sandbox starts", () => this.#starts()),
      section("egress", () => this.#egress()),
      section("people", () => this.#people(day)),
    ]);
    const cfg = this.#deps.config;
    const d: Row = {
      at: new Date().toISOString(),
      tz,
      today: day.toISOString(),
      monthStart: month.toISOString(),
      product: cfg.product.name,
      provider: this.#deps.sandboxProvider.kind,
      sandboxImage: cfg.sandbox.image,
      recentMinutes: RECENT_MIN,
      // now
      service,
      hosts,
      gatewayMissing: this.#deps.sandboxProvider.kind === "vm" && !!hosts && !hosts.list.some((h: Row) => h.gateway),
      sandboxes,
      scaler,
      recent,
      budget,
      // history
      hostsGone,
      events,
      starts,
      egress,
      people,
      errors,
    };
    d.health = healthOf(d);
    return d;
  }

  /** The service's Deployments and their pods (Kubernetes API; null outside a cluster). */
  async #service(): Promise<Row> {
    const c = this.#deps.cluster;
    if (!c) return { available: false, deployments: [] };
    const ns = this.#deps.namespace;
    const [deps, pods] = await Promise.all([c.listDeployments(ns), c.listPods(ns, `app in (${SERVICE_ROLES.join(",")})`)]);
    const now = Date.now();
    const pod = (p: Pod) => {
      const ready = !!p.status?.conditions?.some((x) => x.type === "Ready" && x.status === "True");
      const waiting = p.status?.containerStatuses?.map((c) => c.state?.waiting?.reason).find(Boolean) ?? null;
      return {
        name: p.metadata.name,
        ready,
        phase: p.metadata.deletionTimestamp ? "Terminating" : (waiting ?? p.status?.phase ?? "?"),
        restarts: (p.status?.containerStatuses ?? []).reduce((a, c) => a + (c.restartCount ?? 0), 0),
        ageS: p.metadata.creationTimestamp ? Math.round((now - Date.parse(p.metadata.creationTimestamp)) / 1000) : null,
        node: ((p.spec as { nodeName?: string } | undefined)?.nodeName ?? null) as string | null,
      };
    };
    return {
      available: true,
      deployments: SERVICE_ROLES.map((name) => {
        const d = deps.find((x) => x.metadata.name === name);
        const mine = pods.filter((p) => p.metadata.labels?.app === name).map(pod).sort((a, b) => (a.ageS ?? 0) - (b.ageS ?? 0));
        if (!d) return { name, missing: true, state: "problem", desired: 0, ready: 0, updated: 0, pods: mine };
        const st = d.status ?? {};
        const desired = d.spec?.replicas ?? 0;
        const ready = st.readyReplicas ?? 0;
        const updated = st.updatedReplicas ?? 0;
        const rolling = updated < desired || (st.replicas ?? 0) > desired || (st.observedGeneration ?? 0) < (d.metadata.generation ?? 0);
        const state: Level = ready >= desired && desired > 0 ? (rolling ? "warming" : "ok") : rolling && ready > 0 ? "warming" : "problem";
        return { name, desired, ready, updated, total: st.replicas ?? 0, rolling, state, pods: mine };
      }),
    };
  }

  /** Live sandboxes (not gone) with their owners. */
  async #sandboxes(): Promise<Row> {
    const cfg = this.#deps.config.sandbox;
    const { rows } = await this.#deps.db.query(
      `select s.id, s.conversation_id as "conversationId", s.owner, s.host, s.status, s.created_at as "createdAt",
              s.last_active_at as "lastActiveAt", s.expires_at as "expiresAt",
              (select count(*) from vm_events v where v.sandbox_id = s.id and v.kind = 'migration')::int as migrations
         from sandboxes s where s.status <> 'gone' order by s.created_at`,
    );
    const now = Date.now();
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
    };
  }

  /** Sandbox hosts that exist now (AWS and the gateway; not gone). */
  async #hosts(): Promise<Row> {
    const db = this.#deps.db;
    const p = this.#deps.sandboxProvider;
    const ctl = p instanceof VmProvider ? p.controller : null;
    const [{ rows }, { rows: used }] = await Promise.all([
      db.query<HostRow>("select * from vm_hosts where status <> 'gone' order by kind desc, created_at"),
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
        retiring: h.status === "retiring",
        sandboxes: on.get(h.name) ?? 0,
        slots: ctl && !h.info?.gateway ? ctl.slots(h) : null,
        maxSandboxes: h.max_sandboxes,
        cpus: h.cpus,
        memTotal: num(h.mem_total),
        memAvailable: num(h.info?.memAvailable),
        load1: h.info?.loadAvg?.[0] ?? null,
        gvisor: h.info?.gvisorRelease ?? null,
      })),
    };
  }

  /** History: hosts gone in the last 24 hours, newest first, with how long each lived. */
  async #hostsGone(): Promise<Row> {
    const db = this.#deps.db;
    const [{ rows }, { rows: n }] = await Promise.all([
      db.query<HostRow & { gone_at: Date; gone_reason: string | null }>(
        "select * from vm_hosts where status = 'gone' and gone_at > now() - interval '24 hours' order by gone_at desc limit $1",
        [GONE_HOSTS_SHOWN],
      ),
      db.query<{ n: number }>("select count(*)::int as n from vm_hosts where status = 'gone' and gone_at > now() - interval '24 hours'"),
    ]);
    return {
      count24h: n[0]?.n ?? 0,
      list: rows.map((h) => ({
        name: h.name,
        kind: h.kind,
        instanceId: h.instance_id,
        launchedAt: iso(h.created_at),
        goneAt: iso(h.gone_at),
        goneReason: h.gone_reason,
        livedS: h.created_at && h.gone_at ? Math.round((new Date(h.gone_at).getTime() - new Date(h.created_at).getTime()) / 1000) : null,
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
    return { windowS: DEMAND_WINDOW_S, hints: hints.filter((h) => h.counted), plan };
  }

  /** Errors of the last RECENT_MIN minutes (the "now" half; the 24-hour log is in history). */
  async #recent(): Promise<Row> {
    const db = this.#deps.db;
    const win = `now() - interval '${RECENT_MIN} minutes'`;
    const [{ rows: c }, { rows: list }] = await Promise.all([
      db.query(
        `select (select count(*) from ops_events where kind = 'sandbox_start' and not ok and at > ${win})::int as "failedStarts",
                (select count(*) from egress_log where at > ${win} and kind in ('model', 'inject') and (status >= 400 or decision like 'deny%') and ${NOT_NOISE})::int as "modelErrors",
                (select count(*) from vm_events where at > ${win} and (kind like '%fail%' or kind like '%lost%' or data ? 'error'))::int as "hostErrors",
                (select count(*) from egress_log where at > ${win} and kind not in ('model', 'inject') and decision like 'deny%')::int as "blocked"`,
      ),
      db.query(
        `(select o.at, 'sandbox start failed' as what, coalesce(o.detail->>'reason', '') || coalesce(': ' || (o.detail->>'error'), '') as detail,
                 (select x.owner from sandboxes x where x.conversation_id = o.conversation_id order by x.created_at desc limit 1) as owner
            from ops_events o where o.kind = 'sandbox_start' and not o.ok and o.at > ${win})
         union all
         (select e.at, e.kind || ' ' || coalesce(e.status::text, '–') as what, e.decision || ' ' || coalesce(e.host, '') as detail, s.owner
            from egress_log e left join sandboxes s on s.id = e.sandbox_id
           where e.at > ${win} and e.kind in ('model', 'inject') and (e.status >= 400 or e.decision like 'deny%') and ${NOT_NOISE.replace(/\b(kind|status)\b/g, "e.$1")})
         union all
         (select v.at, v.kind as what, coalesce(v.host, '') || coalesce(' ' || (v.data->>'error'), '') as detail, s.owner
            from vm_events v left join sandboxes s on s.id = v.sandbox_id
           where v.at > ${win} and (v.kind like '%fail%' or v.kind like '%lost%' or v.data ? 'error'))
         order by at desc limit 20`,
      ),
    ]);
    return { ...(c[0] ?? {}), list: list.map((r) => ({ ...r, at: iso(r.at), detail: String(r.detail ?? "").slice(0, 300) })) };
  }

  /** History: vm_events, newest first. */
  async #events(): Promise<Row[]> {
    const { rows } = await this.#deps.db.query(
      `select v.at, v.kind, v.host, v.sandbox_id as "sandboxId", s.owner, s.conversation_id as "conversationId", v.data
         from vm_events v left join sandboxes s on s.id = v.sandbox_id order by v.id desc limit 60`,
    );
    return rows.map((r) => ({ ...r, at: iso(r.at) }));
  }

  /** History: sandbox starts (with how long they took) and releases (with how long they lived) over 24 h, newest first. */
  async #starts(): Promise<Row> {
    const db = this.#deps.db;
    const [{ rows: sum }, { rows: releases }, { rows: list }] = await Promise.all([
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
      db.query(
        `(select o.at, 'start' as what, o.ok, o.ms::float as ms, o.host, o.conversation_id as "conversationId",
                 coalesce(s.owner, (select x.owner from sandboxes x where x.conversation_id = o.conversation_id order by x.created_at desc limit 1)) as owner,
                 case when o.ok then null else coalesce(o.detail->>'reason', 'failed') end as reason
            from ops_events o left join sandboxes s on s.id = o.sandbox_id
           where o.kind = 'sandbox_start' and o.at > now() - interval '24 hours')
         union all
         (select released_at, 'release', null, extract(epoch from released_at - created_at) * 1000, host, conversation_id, owner,
                 left(release_reason, 120)
            from sandboxes where released_at > now() - interval '24 hours')
         order by at desc limit 80`,
      ),
    ]);
    const s = sum[0] ?? {};
    return {
      summary: { n: s.n ?? 0, ok: s.ok ?? 0, failed: s.failed ?? 0, p50Ms: num(s.p50), p95Ms: num(s.p95) },
      releases: releases,
      list: list.map((r) => ({ ...r, at: iso(r.at), ms: num(r.ms) })),
    };
  }

  /** Model spend (egress_log kind model): the shared budget, today, this month, per day, per person and per model. */
  async #budget(tz: string, day: Date, month: Date): Promise<Row> {
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
    const [status, { rows: tot }, { rows: byUser }, { rows: byModel }, { rows: byDay }] = await Promise.all([
      budgetStatus(db, cfg),
      db.query(`select ${sums} from egress_log e where e.kind = 'model' and e.at >= $1`, args),
      db.query(
        // a model call's person: its sandbox's owner, or for calls of the old chat UI (before schema 9) its conversation's
        `select coalesce(s.owner, c.owner, '(unknown)') as owner, ${sums}
           from egress_log e left join sandboxes s on s.id = e.sandbox_id left join conversations c on c.id = e.conversation_id
          where e.kind = 'model' and e.at >= $1 group by 1 order by month desc, "sinceBudget" desc`,
        args,
      ),
      db.query(
        `select coalesce(e.model, '(not recorded)') as model, ${sums}
           from egress_log e where e.kind = 'model' and e.at >= $1 group by 1 order by month desc, "sinceBudget" desc`,
        args,
      ),
      db.query(
        `select to_char(date_trunc('day', e.at at time zone $1), 'YYYY-MM-DD') as day, coalesce(sum(e.cost_usd), 0)::float as usd,
                count(*)::int as calls, count(distinct coalesce(s.owner, ''))::int as people
           from egress_log e left join sandboxes s on s.id = e.sandbox_id
          where e.kind = 'model' and e.at > now() - interval '14 days' group by 1 order by 1 desc`,
        [tz],
      ),
    ]);
    return { status, total: tot[0] ?? null, byUser, byModel, byDay };
  }

  /** History: egress refusals and errors over 24 h: model calls that failed (402 budget, 503, …) and blocked hosts. */
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
    // the listed people (a "@domain" entry has no one to list until someone at it has a conversation), then anyone else
    // with conversations: someone at a listed domain, or a former user
    const users = this.#deps.config.users.filter((u) => !u.startsWith("@"));
    const out = users.map((u) => by.get(u) ?? { owner: u, total: 0, today: 0, last7d: 0, live: 0, deleted: 0, lastActiveAt: null });
    for (const r of rows) if (!users.includes(r.owner)) out.push(isUser(this.#deps.config, r.owner) ? r : { ...r, former: true });
    return out
      .map((r) => ({ ...r, lastActiveAt: iso(r.lastActiveAt) }))
      .sort((a, b) => (b.lastActiveAt ?? "").localeCompare(a.lastActiveAt ?? ""));
  }
}
