// The admin monitor (docs/CHAT.md, "Admin monitor"): one read-only snapshot of the instance for config `admins`,
// served at GET /api/admin/monitor and rendered by the UI's /admin/monitor page (polled every 5 s). Sources: Postgres
// (sandboxes, turns, events, egress_log, tool_calls, ops_events, vm_events, kv presence), the sandbox
// provider's stats() (pods/nodes or vm hosts), this cluster's Deployments (when the web role may read them), S3.
// Never returns message contents of staff conversations: titles, owners, counts and timings only; error lines are
// redacted where they are recorded (common/log.ts).
import { appCommit } from "../common/ops.ts";
import type { WebDeps } from "./ctx.ts";
import type { HostStats, ProviderStats, SandboxStats } from "./provider.ts";
import type { VmStats } from "./sandbox-vm.ts";
import type { TurnRunner } from "./turns.ts";

const JANITOR_LOCK = 4_242_003;
const SCALER_LOCK = 4_242_012;
const CACHE_MS = 4000;
const SECTION_TIMEOUT_MS = 8000;
const PRESENCE_FRESH_MS = 30_000;

export interface MonitorSources {
  deps: WebDeps;
  runner: TurnRunner;
  streamCount: () => number;
}

type Row = Record<string, any>;
const num = (v: unknown): number | null => (v == null ? null : Number(v));
const iso = (v: unknown): string | null => (v == null ? null : new Date(v as string).toISOString());

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  let t: NodeJS.Timeout;
  return Promise.race([p, new Promise<T>((_, rej) => (t = setTimeout(() => rej(new Error(`${what}: no answer in ${ms} ms`)), ms)))]).finally(() => clearTimeout(t));
}

/** Which service a tool belongs to (the page groups Exa, TikHub, Wisers, yyc-mcp, web fetch). */
export function toolService(tool: string, upstreamPrefixes: string[]): string {
  const t = tool.replace(/^mcp__tools__/, "");
  if (t === "web_search") return "Exa";
  if (t === "web_fetch") return "web fetch";
  if (t === "news_search") return "Wisers";
  if (t === "social_search") return "TikHub";
  if (t === "sandbox_environment") return "sandbox";
  const p = upstreamPrefixes.find((x) => t.startsWith(`${x}_`));
  return p ? `${p}-mcp` : "other";
}

/** Maps a provider's stats (the vm provider's own shape included) onto hosts + sandboxes. */
export function normalizeStats(s: ProviderStats | VmStats): ProviderStats {
  if (!("scaler" in s)) return s;
  const reachable = (at: string | null) => !!at && Date.now() - Date.parse(at) < 30_000;
  // the gateway (WireGuard hub on a k3s node, no sandboxes) is not a sandbox host: shown with the scaler instead
  const sandboxHosts = s.hosts.filter((h) => !h.gateway);
  const hosts: HostStats[] = sandboxHosts.map((h) => ({
    name: h.name,
    kind: h.kind,
    status: h.status,
    reachable: reachable(h.lastSeenAt),
    cpus: h.cpus,
    cpuUsed: h.sandboxes.length ? h.sandboxes.reduce((a, x) => a + (x.cpuUsage || 0), 0) : null,
    memTotal: h.memTotal,
    memUsed: h.memTotal != null && h.memAvailable != null ? h.memTotal - h.memAvailable : null,
    load1: h.loadAvg?.[0] ?? null,
    sandboxes: h.used,
    maxSandboxes: h.slots,
    startedAt: h.upSince,
    costUsd: h.costUsd,
    detail: { instanceId: h.instanceId, gvisor: h.gvisorRelease, upHours: h.upHours, lastSeenAt: h.lastSeenAt },
  }));
  const sandboxes: SandboxStats[] = sandboxHosts.flatMap((h) =>
    h.sandboxes.map((x) => ({ name: x.name, host: h.name, state: x.state, cpuUsed: x.cpuUsage, cpuLimit: s.scaler.cpuPerSandbox, memUsed: x.memoryCurrent, memLimit: x.memoryLimit, restarts: null })),
  );
  return {
    provider: "vm",
    hosts,
    sandboxes,
    capacity: sandboxHosts.filter((h) => h.status === "active").reduce((a, h) => a + h.slots, 0),
    notes: sandboxHosts.some((h) => h.loadAvg == null) ? ["vm: a host's sandboxd doesn't report its load yet"] : [],
    extra: { scaler: s.scaler, events: s.events.slice(0, 30), migrations: s.migrations, awsCostLast30dUsd: s.awsCostLast30dUsd },
  };
}

export class Monitor {
  readonly #src: MonitorSources;
  #cache = new Map<string, { at: number; value: Promise<Row> }>();
  #s3: { at: number; value: Row } | null = null;

  constructor(src: MonitorSources) {
    this.#src = src;
  }

  /** The snapshot for a page in time zone `tz` ("today" starts at its midnight); cached for 4 s per zone. */
  snapshot(tz: string): Promise<Row> {
    const zone = /^[A-Za-z]+(?:[/_+-][A-Za-z0-9]+)*$/.test(tz) && tz.length < 64 ? tz : "UTC";
    const hit = this.#cache.get(zone);
    if (hit && Date.now() - hit.at < CACHE_MS) return hit.value;
    const value = this.#build(zone);
    this.#cache.set(zone, { at: Date.now(), value });
    value.catch(() => this.#cache.delete(zone));
    return value;
  }

  async #build(tz: string): Promise<Row> {
    const { deps } = this.#src;
    const db = deps.db;
    const errors: Record<string, string> = {};
    const section = async <T>(name: string, fn: () => Promise<T>): Promise<T | null> => {
      try {
        return await withTimeout(fn(), SECTION_TIMEOUT_MS, name);
      } catch (e) {
        errors[name] = (e as Error).message.slice(0, 300);
        return null;
      }
    };
    let today: Date;
    try {
      const { rows } = await db.query<{ d: Date }>("select (date_trunc('day', now() at time zone $1) at time zone $1) as d", [tz]);
      today = rows[0]!.d;
    } catch {
      tz = "UTC";
      const { rows } = await db.query<{ d: Date }>("select date_trunc('day', now() at time zone 'UTC') at time zone 'UTC' as d");
      today = rows[0]!.d;
    }
    const providerStats = section("provider", async () => {
      const p = deps.sandboxProvider;
      if (!p.stats) return { provider: p.kind, hosts: [], sandboxes: [], capacity: null, notes: [`${p.kind}: no stats`] } as ProviderStats;
      return normalizeStats(await p.stats());
    });
    const [sandboxes, load, models, tools, egress, health, stats] = await Promise.all([
      section("sandboxes", () => this.#sandboxes()),
      section("load", () => this.#load(today)),
      section("models", () => this.#models(today)),
      section("tools", () => this.#tools()),
      section("egress", () => this.#egress()),
      section("health", () => this.#health()),
      providerStats,
    ]);
    // sandboxes joined with what the provider sees of them
    if (sandboxes && stats) {
      const byName = new Map(stats.sandboxes.map((s) => [s.name, s]));
      for (const s of sandboxes.list as Row[]) {
        const p = byName.get(s.pod);
        s.provider = p ?? null;
        byName.delete(s.pod);
      }
      // at the provider without a live row (the janitor removes these after a minute)
      sandboxes.unowned = [...byName.values()];
      sandboxes.pool.capacity = stats.capacity;
    }
    // new sections (e.g. the shared model budget) go into Promise.all above and here; the page renders each card
    // from its own key and shows errors[<name>] when it failed
    return {
      at: new Date().toISOString(),
      tz,
      today: today.toISOString(),
      replica: deps.replica,
      commit: appCommit(),
      product: deps.config.product.name,
      provider: deps.sandboxProvider.kind,
      sandboxes,
      hosts: stats ? { list: stats.hosts, notes: stats.notes, extra: stats.extra ?? null } : null,
      load,
      models,
      tools,
      egress,
      health,
      errors,
    };
  }

  async #sandboxes(): Promise<Row> {
    const { deps } = this.#src;
    const db = deps.db;
    const cfg = deps.config.sandbox;
    const { rows } = await db.query(
      `select s.id, s.pod, s.conversation_id as "conversationId", s.status, s.host, s.created_at as "createdAt",
              s.last_active_at as "lastActiveAt", s.expires_at as "expiresAt", c.title, c.owner,
              (select count(*) from turns t where t.conversation_id = s.conversation_id)::int as turns,
              (c.id is null or c.deleted_at is not null) as "conversationGone",
              exists (select 1 from turns t where t.conversation_id = s.conversation_id and t.status in ('queued','running','waiting')) as busy,
              (select count(*) from sandboxes p where p.conversation_id = s.conversation_id and p.id <> s.id)::int as resets
         from sandboxes s left join conversations c on c.id = s.conversation_id
        where s.status <> 'gone' order by s.created_at`,
    );
    const convs = rows.map((r) => r.conversationId);
    const ids = rows.map((r) => r.id);
    const [{ rows: exec }, { rows: mig }, { rows: starts }, { rows: fails }, { rows: releases }] = await Promise.all([
      db.query(
        `select conversation_id as c, max(created_at) as at from events
          where conversation_id = any($1::uuid[]) and type = 'tool_use' and data->>'name' = 'bash' group by 1`,
        [convs],
      ),
      db.query(
        `select sandbox_id as id, count(*)::int as n, (array_agg(coalesce((data->>'pause_ms')::numeric, (data->>'final_ms')::numeric) order by at desc))[1] as "lastPauseMs"
           from vm_events where kind = 'migration' and sandbox_id = any($1::uuid[]) group by 1`,
        [ids],
      ),
      db.query(
        `select count(*)::int as n, count(*) filter (where ok)::int as ok, count(*) filter (where not ok)::int as failed,
                percentile_cont(0.5) within group (order by ms) filter (where ok) as p50,
                percentile_cont(0.95) within group (order by ms) filter (where ok) as p95,
                percentile_cont(0.5) within group (order by (detail->>'startMs')::numeric) filter (where ok) as "startP50"
           from ops_events where kind = 'sandbox_start' and at > now() - interval '24 hours'`,
      ),
      db.query(
        `select o.at, o.detail->>'reason' as reason, o.detail->>'error' as error, o.ms, c.owner, c.title
           from ops_events o left join conversations c on c.id = o.conversation_id
          where o.kind = 'sandbox_start' and not o.ok order by o.id desc limit 15`,
      ),
      db.query(
        `select coalesce(regexp_replace(release_reason, ':.*', ''), '?') as reason, count(*)::int as n from sandboxes
          where released_at > now() - interval '24 hours' group by 1 order by 2 desc`,
      ),
    ]);
    const lastExec = new Map(exec.map((r) => [r.c, r.at]));
    const migs = new Map(mig.map((r) => [r.id, r]));
    const now = Date.now();
    const s = starts[0] ?? {};
    return {
      pool: { live: rows.length, max: cfg.maxActive, capacity: null as number | null, idleMinutes: cfg.idleMinutes, maxLifetimeHours: cfg.maxLifetimeHours, cpu: cfg.cpu, memory: cfg.memory },
      list: rows.map((r) => ({
        ...r,
        createdAt: iso(r.createdAt),
        lastActiveAt: iso(r.lastActiveAt),
        expiresAt: iso(r.expiresAt),
        ageS: Math.round((now - new Date(r.createdAt).getTime()) / 1000),
        idleS: Math.round((now - new Date(r.lastActiveAt).getTime()) / 1000),
        lastExecAt: iso(lastExec.get(r.conversationId)),
        migrations: migs.get(r.id)?.n ?? 0,
        lastPauseMs: num(migs.get(r.id)?.lastPauseMs),
      })),
      starts24h: { n: s.n ?? 0, ok: s.ok ?? 0, failed: s.failed ?? 0, p50Ms: num(s.p50), p95Ms: num(s.p95), startP50Ms: num(s.startP50) },
      failures: fails.map((f) => ({ ...f, at: iso(f.at) })),
      releases24h: releases,
    };
  }

  async #load(today: Date): Promise<Row> {
    const { deps, runner, streamCount } = this.#src;
    const db = deps.db;
    const [{ rows: flight }, { rows: hours }, { rows: timing }, { rows: errs }, { rows: users }, { rows: pres }] = await Promise.all([
      db.query(`select status, count(*)::int as n from turns where status in ('queued','running','waiting') group by 1`),
      db.query(
        `select h as hour, (select count(*) from turns t where t.started_at >= h and t.started_at < h + interval '1 hour')::int as turns,
                (select count(*) from turns t where t.started_at >= h and t.started_at < h + interval '1 hour' and t.status = 'error')::int as errors
           from generate_series(date_trunc('hour', now()) - interval '23 hours', date_trunc('hour', now()), interval '1 hour') h order by h`,
      ),
      db.query(
        `select percentile_cont(0.5) within group (order by extract(epoch from (t.finished_at - t.started_at)) * 1000) filter (where t.status = 'done') as "durP50",
                percentile_cont(0.95) within group (order by extract(epoch from (t.finished_at - t.started_at)) * 1000) filter (where t.status = 'done') as "durP95",
                percentile_cont(0.5) within group (order by extract(epoch from (e.created_at - t.started_at)) * 1000) as "waitP50",
                percentile_cont(0.95) within group (order by extract(epoch from (e.created_at - t.started_at)) * 1000) as "waitP95",
                count(*)::int as n, count(*) filter (where t.started_at >= $1)::int as today
           from turns t left join events e on e.turn_id = t.id and e.type = 'turn_started'
          where t.started_at > now() - interval '24 hours'`,
        [today],
      ),
      db.query(
        `select kind, n from (
           select 'turn: ' || regexp_replace(left(coalesce(error, 'unknown'), 80), '[0-9a-f]{8}-[0-9a-f-]{27}|[0-9]+', '#', 'g') as kind, count(*)::int as n
             from turns where status = 'error' and started_at > now() - interval '24 hours' group by 1
           union all
           select 'tool: ' || tool, count(*)::int from tool_calls where ok is false and at > now() - interval '24 hours' group by tool
           union all
           select 'model: HTTP ' || coalesce(status::text, '?'), count(*)::int from egress_log
            where kind = 'model' and (status >= 400 or status is null) and at > now() - interval '24 hours' group by status
           union all
           select 'sandbox: ' || coalesce(detail->>'reason', '?'), count(*)::int from ops_events
            where kind = 'sandbox_start' and not ok and at > now() - interval '24 hours' group by detail->>'reason'
         ) x order by n desc limit 20`,
      ),
      db.query(
        `select count(*) filter (where last_seen_at > now() - interval '5 minutes')::int as now,
                count(*) filter (where last_seen_at >= $1)::int as today from users`,
        [today],
      ),
      db.query(`select value from kv where key like 'presence:web:%'`),
    ]);
    const web = pres.map((p) => p.value).filter((v) => !v.gone && Date.now() - Date.parse(v.at) < PRESENCE_FRESH_MS);
    const t = timing[0] ?? {};
    const inFlight = Object.fromEntries(["queued", "running", "waiting"].map((s) => [s, flight.find((f) => f.status === s)?.n ?? 0]));
    return {
      inFlight,
      // other replicas from their presence (10 s old at most), this one live
      clawProcesses: web.filter((v) => v.replica !== deps.replica).reduce((a, v) => a + (v.turns ?? 0), 0) + runner.active,
      sseStreams: web.filter((v) => v.replica !== deps.replica).reduce((a, v) => a + (v.streams ?? 0), 0) + streamCount(),
      perHour: hours.map((h) => ({ hour: iso(h.hour), turns: h.turns, errors: h.errors })),
      turns24h: t.n ?? 0,
      turnsToday: t.today ?? 0,
      waitP50Ms: num(t.waitP50),
      waitP95Ms: num(t.waitP95),
      durationP50Ms: num(t.durP50),
      durationP95Ms: num(t.durP95),
      errors24h: errs,
      usersNow: users[0]?.now ?? 0,
      usersToday: users[0]?.today ?? 0,
    };
  }

  async #models(today: Date): Promise<Row> {
    const db = this.#src.deps.db;
    const { rows } = await db.query(
      `select t.model,
              count(*) filter (where t.started_at >= $1)::int as "turnsToday",
              coalesce(sum((t.usage->>'input_tokens')::numeric) filter (where t.started_at >= $1), 0)::float as "inToday",
              coalesce(sum((t.usage->>'output_tokens')::numeric) filter (where t.started_at >= $1), 0)::float as "outToday",
              coalesce(sum((t.usage->>'cache_read_input_tokens')::numeric) filter (where t.started_at >= $1), 0)::float as "cacheToday",
              coalesce(sum(t.cost_usd) filter (where t.started_at >= $1), 0)::float as "usdToday",
              count(*)::int as "turns7d",
              coalesce(sum((t.usage->>'input_tokens')::numeric), 0)::float as "in7d",
              coalesce(sum((t.usage->>'output_tokens')::numeric), 0)::float as "out7d",
              coalesce(sum((t.usage->>'cache_read_input_tokens')::numeric), 0)::float as "cache7d",
              coalesce(sum(t.cost_usd), 0)::float as "usd7d"
         from turns t where t.started_at > now() - interval '7 days' group by t.model order by "usd7d" desc`,
      [today],
    );
    const { rows: tot } = await db.query(
      `select coalesce(sum(cost_usd) filter (where at >= $1), 0)::float as today, coalesce(sum(cost_usd), 0)::float as "d7",
              count(*) filter (where at >= $1)::int as "callsToday"
         from egress_log where kind = 'model' and at > now() - interval '7 days'`,
      [today],
    );
    return { byModel: rows, egressUsdToday: tot[0]?.today ?? 0, egressUsd7d: tot[0]?.d7 ?? 0, modelCallsToday: tot[0]?.callsToday ?? 0 };
  }

  async #tools(): Promise<Row> {
    const { deps } = this.#src;
    const db = deps.db;
    const prefixes = deps.config.tools.upstreams.map((u) => u.prefix);
    const { rows } = await db.query(
        `select tool, count(*)::int as calls, count(*) filter (where ok is false)::int as errors,
                percentile_cont(0.5) within group (order by ms) as p50, percentile_cont(0.95) within group (order by ms) as p95,
                coalesce(sum(cost_usd), 0)::float as usd, max(at) as "lastAt"
           from tool_calls where at > now() - interval '24 hours' group by tool order by calls desc`,
    );
    return {
      byTool: rows.map((r) => ({ ...r, service: toolService(r.tool, prefixes), p50Ms: num(r.p50), p95Ms: num(r.p95), lastAt: iso(r.lastAt), p50: undefined, p95: undefined })),
    };
  }

  async #egress(): Promise<Row> {
    const { deps } = this.#src;
    const db = deps.db;
    const [{ rows: perMin }, { rows: denied }, { rows: mirror }, { rows: talkers }, { rows: pres }] = await Promise.all([
      db.query(
        `select m as minute, (select count(*) from egress_log e where e.at >= m and e.at < m + interval '1 minute')::int as n,
                (select count(*) from egress_log e where e.at >= m and e.at < m + interval '1 minute' and e.decision like 'deny%')::int as denied
           from generate_series(date_trunc('minute', now()) - interval '59 minutes', date_trunc('minute', now()), interval '1 minute') m order by m`,
      ),
      db.query(
        // the deny reason as egress logs it (deny:<why>: not on the allowlist, threat intel, private address, denyHosts…)
        `select host, regexp_replace(decision, '^deny:?', '') as why, count(*)::int as n, max(at) as "lastAt"
           from egress_log where decision like 'deny%' and at > now() - interval '24 hours' group by 1, 2 order by n desc limit 20`,
      ),
      db.query(
        `select count(*) filter (where decision = 'allow:cache')::int as hits, count(*) filter (where decision like 'allow%')::int as total,
                coalesce(sum(bytes_down) filter (where decision = 'allow:cache'), 0)::float as "hitBytes"
           from egress_log where kind = 'mirror' and at > now() - interval '24 hours'`,
      ),
      db.query(
        `select e.sandbox_id as "sandboxId", s.pod, c.owner, c.title, count(*)::int as n,
                count(*) filter (where e.decision like 'deny%')::int as denied,
                coalesce(sum(coalesce(e.bytes_up, 0) + coalesce(e.bytes_down, 0)), 0)::float as bytes
           from egress_log e left join sandboxes s on s.id = e.sandbox_id left join conversations c on c.id = e.conversation_id
          where e.sandbox_id is not null and e.at > now() - interval '1 hour'
          group by 1, 2, 3, 4 order by bytes desc limit 10`,
      ),
      db.query(`select value from kv where key like 'presence:egress:%'`),
    ]);
    const last5 = perMin.slice(-6, -1);
    const m = mirror[0] ?? { hits: 0, total: 0, hitBytes: 0 };
    const pods = pres.map((p) => p.value).filter((v) => !v.gone && Date.now() - Date.parse(v.at) < PRESENCE_FRESH_MS);
    return {
      perMinute: perMin.map((r) => ({ minute: iso(r.minute), n: r.n, denied: r.denied })),
      reqPerMin: last5.length ? last5.reduce((a, r) => a + r.n, 0) / last5.length : 0,
      deniedTop: denied.map((d) => ({ ...d, lastAt: iso(d.lastAt) })),
      mirror: { hits: m.hits, total: m.total, rate: m.total ? m.hits / m.total : null, hitBytes: m.hitBytes },
      cache: pods.map((p) => ({ replica: p.replica, bytes: p.cacheBytes ?? null, hits: p.cacheHits ?? null, misses: p.cacheMisses ?? null, openSockets: p.openSockets ?? null })),
      topTalkers: talkers,
    };
  }

  async #health(): Promise<Row> {
    const { deps } = this.#src;
    const db = deps.db;
    // Postgres round trip (median of 3), connections by role (application_name = jarvis-<role>:<pod>)
    const rtts: number[] = [];
    for (let i = 0; i < 3; i++) {
      const t0 = performance.now();
      await db.query("select 1");
      rtts.push(performance.now() - t0);
    }
    rtts.sort((a, b) => a - b);
    const [{ rows: conns }, { rows: maxc }, { rows: locks }, { rows: pres }, deployments, s3] = await Promise.all([
      db.query(
        `select coalesce(nullif(application_name, ''), '?') as app, count(*)::int as n,
                count(*) filter (where state = 'active')::int as active
           from pg_stat_activity where datname = current_database() group by 1 order by 2 desc`,
      ),
      db.query("select current_setting('max_connections')::int as n"),
      db.query(
        `select l.objid::bigint as key, a.application_name as holder, a.backend_start as since from pg_locks l
           left join pg_stat_activity a on a.pid = l.pid
          where l.locktype = 'advisory' and l.granted and l.classid = 0 and l.objsubid = 1 and l.objid = any($1::oid[])`,
        [[JANITOR_LOCK, SCALER_LOCK]],
      ),
      db.query(`select key, value from kv where key like 'presence:%'`),
      this.#deployments(),
      this.#s3Probe(),
    ]);
    const roles = ["web", "egress", "tools"].map((role) => ({
      role,
      replicas: pres
        .filter((p) => p.key.startsWith(`presence:${role}:`))
        .map((p) => p.value)
        .map((v) => ({ replica: v.replica, commit: v.commit, startedAt: v.startedAt, at: v.at, up: !v.gone && Date.now() - Date.parse(v.at) < PRESENCE_FRESH_MS, draining: !!v.draining, turns: v.turns ?? null, streams: v.streams ?? null }))
        .filter((v) => v.up || Date.now() - Date.parse(v.at) < 3_600_000)
        .sort((a, b) => Number(b.up) - Number(a.up) || a.replica.localeCompare(b.replica)),
      deployment: deployments.list?.find((d: Row) => d.name === role) ?? null,
    }));
    const errorLines = pres
      .flatMap((p) => ((p.value.errors ?? []) as Row[]).map((e): Row => ({ role: p.value.role, replica: p.value.replica, ...e })))
      .sort((a, b) => String(b.t).localeCompare(String(a.t)))
      .slice(0, 30);
    const lockName = (k: number) => (k === JANITOR_LOCK ? "janitor" : k === SCALER_LOCK ? "vm scaler" : String(k));
    return {
      roles,
      deploymentsNote: deployments.note,
      otherDeployments: deployments.list?.filter((d: Row) => !["web", "egress", "tools"].includes(d.name)) ?? [],
      leaders: [JANITOR_LOCK, ...(deps.sandboxProvider.kind === "vm" ? [SCALER_LOCK] : [])].map((k) => {
        const l = locks.find((x) => Number(x.key) === k);
        return { name: lockName(k), holder: l?.holder ?? null, since: iso(l?.since) };
      }),
      postgres: { ms: Math.round(rtts[1]! * 10) / 10, connections: conns, total: conns.reduce((a, c) => a + c.n, 0), max: maxc[0]?.n ?? null },
      s3,
      errorLines,
    };
  }

  async #deployments(): Promise<{ list: Row[] | null; note: string | null }> {
    const k8s = this.#src.deps.k8s;
    if (!k8s?.getJson) return { list: null, note: "not in a cluster" };
    try {
      const r = await withTimeout(k8s.getJson(`/apis/apps/v1/namespaces/${encodeURIComponent(this.#src.deps.namespace)}/deployments`), 5000, "deployments");
      const list = ((r?.items ?? []) as Row[]).map((d) => ({
        name: d.metadata?.name,
        desired: d.spec?.replicas ?? null,
        ready: d.status?.readyReplicas ?? 0,
        updated: d.status?.updatedReplicas ?? 0,
        available: d.status?.availableReplicas ?? 0,
        image: String(d.spec?.template?.spec?.containers?.[0]?.image ?? "").split("/").pop(),
      }));
      return { list, note: null };
    } catch (e) {
      return { list: null, note: `deployments: ${(e as Error).message}` };
    }
  }

  /** Whether the bucket answers (a HEAD of a key that doesn't exist), at most every 30 s. */
  async #s3Probe(): Promise<Row> {
    if (this.#s3 && Date.now() - this.#s3.at < 30_000) return this.#s3.value;
    const { deps } = this.#src;
    const t0 = performance.now();
    let value: Row;
    try {
      await withTimeout(deps.blobs.exists(`${deps.config.s3.prefix}monitor-probe`), 5000, "s3");
      value = { ok: true, ms: Math.round(performance.now() - t0), bucket: deps.config.s3.bucket, checkedAt: new Date().toISOString() };
    } catch (e) {
      value = { ok: false, ms: Math.round(performance.now() - t0), bucket: deps.config.s3.bucket, error: (e as Error).message.slice(0, 200), checkedAt: new Date().toISOString() };
    }
    this.#s3 = { at: Date.now(), value };
    return value;
  }
}
