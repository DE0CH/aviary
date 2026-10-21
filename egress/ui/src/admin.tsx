// The admin monitor (docs/CHAT.md, "Admin monitor"), served at /admin/monitor to config admins only (the web role
// answers 404 to everyone else, for this page, its bundle and GET /api/admin/monitor). Polls the snapshot every 5 s
// while the tab is visible. Read-only.
import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import "./style.css";
import "./admin.css";

type Any = any;

// ---- formatting -------------------------------------------------------------------------------------------------

const n0 = (v: number | null | undefined) => (v == null ? "–" : Math.round(v).toLocaleString("en"));
const n1 = (v: number | null | undefined) => (v == null ? "–" : (Math.round(v * 10) / 10).toLocaleString("en"));
function bytes(v: number | null | undefined): string {
  if (v == null) return "–";
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  let x = v;
  while (x >= 1024 && i < u.length - 1) (x /= 1024), i++;
  return `${x >= 100 || i === 0 ? Math.round(x) : x.toFixed(1)} ${u[i]}`;
}
function dur(ms: number | null | undefined): string {
  if (ms == null) return "–";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)} s`;
  const m = s / 60;
  if (m < 60) return `${Math.floor(m)}m ${Math.round(s % 60)}s`;
  const h = m / 60;
  if (h < 48) return `${Math.floor(h)}h ${Math.round(m % 60)}m`;
  return `${Math.floor(h / 24)}d ${Math.round(h % 24)}h`;
}
const secs = (s: number | null | undefined) => (s == null ? "–" : dur(s * 1000));
const ago = (t: string | null | undefined) => (t ? `${dur(Date.now() - Date.parse(t))} ago` : "never");
const usd = (v: number | null | undefined) => (v == null ? "–" : v < 10 ? `$${v.toFixed(2)}` : `$${Math.round(v).toLocaleString("en")}`);
const pct = (v: number | null | undefined) => (v == null ? "–" : `${Math.round(v * 100)}%`);
const short = (s: string | null | undefined, n = 12) => (!s ? "–" : s.length > n ? s.slice(0, n) : s);
const tokens = (v: number | null | undefined) => (v == null ? "–" : v >= 1e6 ? `${(v / 1e6).toFixed(1)}M` : v >= 1e3 ? `${(v / 1e3).toFixed(1)}k` : String(Math.round(v)));
const hhmm = (t: string) => new Date(t).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });

// ---- small parts --------------------------------------------------------------------------------------------------

function Meter(p: { used: number | null | undefined; max: number | null | undefined; label?: string; fmt?: (v: number) => string }) {
  const f = p.fmt ?? ((v: number) => n1(v));
  if (p.used == null) return <span class="m-dim">–</span>;
  const r = p.max ? Math.min(1, p.used / p.max) : null;
  const tone = r == null ? "" : r >= 0.9 ? "is-bad" : r >= 0.7 ? "is-warn" : "";
  return (
    <span class={`meter ${tone}`} title={p.label}>
      <span class="meter-bar">{r != null && <span class="meter-fill" style={{ width: `${Math.max(2, r * 100)}%` }} />}</span>
      <span class="meter-text">
        {f(p.used)}
        {p.max ? <span class="m-dim"> / {f(p.max)}</span> : null}
      </span>
    </span>
  );
}

function Bars(p: { values: number[]; marks?: number[]; labels?: string[]; height?: number; title: string }) {
  const h = p.height ?? 44;
  const max = Math.max(1, ...p.values);
  const w = 100 / Math.max(1, p.values.length);
  return (
    <figure class="bars">
      <svg viewBox={`0 0 100 ${h}`} preserveAspectRatio="none" role="img" aria-label={p.title}>
        {p.values.map((v, i) => {
          const bh = (v / max) * (h - 2);
          const mh = ((p.marks?.[i] ?? 0) / max) * (h - 2);
          return (
            <g key={i}>
              <rect x={i * w + w * 0.12} y={h - bh} width={w * 0.76} height={bh} class="bar">
                <title>{`${p.labels?.[i] ?? i}: ${v}${p.marks ? ` (${p.marks[i]} flagged)` : ""}`}</title>
              </rect>
              {mh > 0 && <rect x={i * w + w * 0.12} y={h - mh} width={w * 0.76} height={mh} class="bar-mark" />}
            </g>
          );
        })}
      </svg>
      <figcaption>
        <span>{p.labels?.[0]}</span>
        <span>max {max}</span>
        <span>{p.labels?.[p.labels.length - 1]}</span>
      </figcaption>
    </figure>
  );
}

function Tile(p: { label: string; value: string; sub?: string; tone?: "bad" | "warn" | "" }) {
  return (
    <div class={`tile ${p.tone ? `is-${p.tone}` : ""}`}>
      <div class="tile-label">{p.label}</div>
      <div class="tile-value">{p.value}</div>
      {p.sub && <div class="tile-sub">{p.sub}</div>}
    </div>
  );
}

function Card(p: { title: string; id: string; aside?: Any; error?: string; children: Any }) {
  return (
    <section class="card" id={p.id} aria-labelledby={`${p.id}-h`}>
      <header class="card-head">
        <h2 id={`${p.id}-h`}>{p.title}</h2>
        {p.aside && <div class="card-aside">{p.aside}</div>}
      </header>
      {p.error && <p class="note is-bad">Could not read: {p.error}</p>}
      {p.children}
    </section>
  );
}

function Table(p: { head: (string | [string, "r"])[]; rows: Any[][]; empty?: string }) {
  if (!p.rows.length) return <p class="empty">{p.empty ?? "Nothing."}</p>;
  return (
    <div class="table-wrap">
      <table>
        <thead>
          <tr>{p.head.map((h, i) => (Array.isArray(h) ? <th key={i} class="r">{h[0]}</th> : <th key={i}>{h}</th>))}</tr>
        </thead>
        <tbody>
          {p.rows.map((r, i) => (
            <tr key={i}>
              {r.map((c, j) => (
                <td key={j} class={Array.isArray(p.head[j]) ? "r" : ""}>
                  {c}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const Dot = (p: { ok: boolean | null; label?: string }) => <span class={`dot ${p.ok == null ? "" : p.ok ? "is-ok" : "is-bad"}`} title={p.label} aria-label={p.label} />;

// ---- sections -----------------------------------------------------------------------------------------------------

function Sandboxes({ d }: { d: Any }) {
  const s = d.sandboxes;
  if (!s) return <Card title="Sandboxes" id="sandboxes" error={d.errors.sandboxes}>{null}</Card>;
  const cap = s.pool.capacity ?? s.pool.max;
  return (
    <Card
      title="Sandboxes"
      id="sandboxes"
      error={d.errors.sandboxes}
      aside={
        <span>
          {d.provider} · {s.pool.cpu} CPU / {s.pool.memory} each · idle release {s.pool.idleMinutes} min
        </span>
      }
    >
      <div class="kv-row">
        <span>
          Pool <Meter used={s.pool.live} max={cap} fmt={n0} />
        </span>
        <span>
          Starts 24 h <b>{s.starts24h.n}</b> ({s.starts24h.failed} failed)
        </span>
        <span>
          Allocation p50 <b>{dur(s.starts24h.p50Ms)}</b> · p95 <b>{dur(s.starts24h.p95Ms)}</b>
        </span>
      </div>
      {s.list.length ? (
        <ul class="sbx-list">
          {s.list.map((x: Any) => (
            <li key={x.id} class={`sbx is-${x.status}`}>
              <div class="sbx-head">
                <span class={`state state-${x.status}`}>{x.status}</span>
                {x.busy && <span class="state state-busy">turn</span>}
                <span class="sbx-title" title={x.title}>
                  {x.title || <i class="m-dim">untitled</i>}
                </span>
              </div>
              <div class="sbx-meta">
                <span>{x.owner}</span>
                <span class="mono" title={x.pod}>{x.pod}</span>
                <span>{x.provider?.host ?? x.host ?? "–"}</span>
                {x.provider && <span>{x.provider.state}</span>}
              </div>
              <div class="sbx-grid">
                <span class="lbl">CPU</span>
                <Meter used={x.provider?.cpuUsed} max={x.provider?.cpuLimit} fmt={(v) => v.toFixed(2)} />
                <span class="lbl">Memory</span>
                <Meter used={x.provider?.memUsed} max={x.provider?.memLimit} fmt={bytes} />
                <span class="lbl">Age</span>
                <span>{secs(x.ageS)}</span>
                <span class="lbl">Idle</span>
                <span class={x.idleS > s.pool.idleMinutes * 60 * 0.8 ? "is-warn-text" : ""}>{secs(x.idleS)}</span>
                <span class="lbl">Last bash</span>
                <span>{ago(x.lastExecAt)}</span>
                <span class="lbl">Resets</span>
                <span>
                  {x.resets}
                  {x.provider?.restarts ? ` · ${x.provider.restarts} restarts` : ""}
                </span>
                {d.provider === "vm" && (
                  <>
                    <span class="lbl">Migrations</span>
                    <span>
                      {x.migrations}
                      {x.lastPauseMs != null ? ` · last pause ${dur(x.lastPauseMs)}` : ""}
                    </span>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      ) : (
        <p class="empty">No live sandboxes.</p>
      )}
      {s.unowned?.length > 0 && <p class="note is-warn">At the provider without a live row (the janitor removes them): {s.unowned.map((u: Any) => u.name).join(", ")}</p>}
      <h3>Recent allocation failures</h3>
      <Table
        head={["When", "Reason", "Owner", "Detail"]}
        rows={s.failures.map((f: Any) => [ago(f.at), f.reason, f.owner ?? "–", <span class="wrap">{f.error ?? ""}</span>])}
        empty="None."
      />
      {s.releases24h.length > 0 && (
        <p class="note">
          Released in 24 h: {s.releases24h.map((r: Any) => `${r.reason} ${r.n}`).join(" · ")}
        </p>
      )}
    </Card>
  );
}

function Hosts({ d }: { d: Any }) {
  const h = d.hosts;
  if (!h) return <Card title="Hosts" id="hosts" error={d.errors.provider}>{null}</Card>;
  const x = h.extra;
  return (
    <Card title="Hosts" id="hosts" error={d.errors.provider} aside={x?.scaler ? <span>AWS 30 d {usd(x.awsCostLast30dUsd)}</span> : null}>
      {h.list.length ? (
        <ul class="host-list">
          {h.list.map((o: Any) => (
            <li key={o.name} class="host">
              <div class="sbx-head">
                <Dot ok={o.reachable} label={o.reachable ? "answering" : "not answering"} />
                <b class="mono">{o.name}</b>
                <span class="state">{o.kind}</span>
                <span class="m-dim">{o.status}</span>
                {o.costUsd != null && <span class="host-cost">{usd(o.costUsd)} so far</span>}
              </div>
              <div class="sbx-grid">
                <span class="lbl">CPU</span>
                <Meter used={o.cpuUsed} max={o.cpus} fmt={(v) => v.toFixed(1)} />
                <span class="lbl">Memory</span>
                <Meter used={o.memUsed} max={o.memTotal} fmt={bytes} />
                <span class="lbl">Load 1m</span>
                <span>{o.load1 == null ? "–" : o.load1.toFixed(2)}</span>
                <span class="lbl">Sandboxes</span>
                <Meter used={o.sandboxes} max={o.maxSandboxes} fmt={n0} />
                <span class="lbl">Up</span>
                <span>{o.startedAt ? dur(Date.now() - Date.parse(o.startedAt)) : "–"}</span>
                {o.detail?.kubelet && (
                  <>
                    <span class="lbl">Kubelet</span>
                    <span>{o.detail.kubelet}</span>
                  </>
                )}
                {o.detail?.instanceId && (
                  <>
                    <span class="lbl">Instance</span>
                    <span class="mono">{o.detail.instanceId}</span>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      ) : (
        <p class="empty">No hosts reported.</p>
      )}
      {h.notes.length > 0 && <p class="note">{h.notes.join(" · ")}</p>}
      {x?.scaler && (
        <>
          <h3>Scaler</h3>
          <div class="kv-row">
            <span>
              Free slots <b>{x.scaler.freeSlots}</b> (keeps {x.scaler.spareSlots})
            </span>
            <span>
              AWS hosts max <b>{x.scaler.maxAwsHosts}</b>
            </span>
            <span>Last heartbeat {ago(x.scaler.lastPassAt)}</span>
          </div>
          <Table
            head={["When", "Decision", "Host", "Detail"]}
            rows={(x.events ?? []).slice(0, 12).map((e: Any) => [ago(e.at), e.kind, e.host ?? "–", <span class="wrap">{Object.entries(e.data ?? {}).filter(([k]) => k !== "error").map(([k, v]) => `${k} ${typeof v === "object" ? JSON.stringify(v) : v}`).join(", ")}</span>])}
            empty="No decisions yet."
          />
          <h3>Migrations</h3>
          <Table
            head={["When", "From → to", ["Pause", "r"], ["Total", "r"], ["Rounds", "r"]]}
            rows={(x.migrations ?? []).slice(0, 10).map((m: Any) => [ago(m.at), `${m.from} → ${m.to}`, dur(m.pauseMs), dur(m.ms), m.rounds ?? "–"])}
            empty="None yet."
          />
        </>
      )}
    </Card>
  );
}

function Load({ d }: { d: Any }) {
  const l = d.load;
  if (!l) return <Card title="Load" id="load" error={d.errors.load}>{null}</Card>;
  return (
    <Card title="Load" id="load" error={d.errors.load}>
      <div class="kv-row">
        <span>
          In flight <b>{l.inFlight.running}</b> running · <b>{l.inFlight.waiting}</b> waiting · <b>{l.inFlight.queued}</b> queued
        </span>
        <span>
          claw <b>{l.clawProcesses}</b> · SSE <b>{l.sseStreams}</b>
        </span>
      </div>
      <div class="kv-row">
        <span>
          Wait p50 <b>{dur(l.waitP50Ms)}</b> · p95 <b>{dur(l.waitP95Ms)}</b>
        </span>
        <span>
          Turn p50 <b>{dur(l.durationP50Ms)}</b> · p95 <b>{dur(l.durationP95Ms)}</b>
        </span>
      </div>
      <h3>Turns per hour (24 h; errors darker)</h3>
      <Bars title="Turns per hour" values={l.perHour.map((h: Any) => h.turns)} marks={l.perHour.map((h: Any) => h.errors)} labels={l.perHour.map((h: Any) => hhmm(h.hour))} />
      <h3>Errors by kind (24 h)</h3>
      <Table head={["Kind", ["Count", "r"]]} rows={l.errors24h.map((e: Any) => [<span class="wrap">{e.kind}</span>, e.n])} empty="No errors." />
    </Card>
  );
}

function Models({ d }: { d: Any }) {
  const m = d.models;
  const t = d.tools;
  return (
    <Card title="Models & tools" id="models" error={d.errors.models || d.errors.tools}>
      {m && (
        <>
          <div class="kv-row">
            <span>
              Model spend today <b>{usd(m.egressUsdToday)}</b> · 7 d <b>{usd(m.egressUsd7d)}</b>
            </span>
            <span>
              Model calls today <b>{m.modelCallsToday}</b>
            </span>
          </div>
          <Table
            head={["Model", ["Turns today", "r"], ["In / out today", "r"], ["USD today", "r"], ["Turns 7 d", "r"], ["In / out 7 d", "r"], ["USD 7 d", "r"]]}
            rows={m.byModel.map((x: Any) => [
              <span class="mono">{String(x.model).replace(/^[^/]+\//, "")}</span>,
              x.turnsToday,
              `${tokens(x.inToday)} / ${tokens(x.outToday)}`,
              usd(x.usdToday),
              x.turns7d,
              `${tokens(x.in7d)} / ${tokens(x.out7d)}`,
              usd(x.usd7d),
            ])}
            empty="No turns in 7 days."
          />
        </>
      )}
      {t && (
        <>
          <h3>Tool calls (24 h)</h3>
          <Table
            head={["Tool", "Service", ["Calls", "r"], ["Errors", "r"], ["p50", "r"], ["p95", "r"], ["USD", "r"]]}
            rows={t.byTool.map((x: Any) => [
              <span class="mono">{x.tool}</span>,
              x.service,
              x.calls,
              <span class={x.errors / x.calls > 0.1 ? "is-bad-text" : ""}>
                {x.errors} ({pct(x.errors / x.calls)})
              </span>,
              dur(x.p50Ms),
              dur(x.p95Ms),
              usd(x.usd),
            ])}
            empty="No tool calls in 24 h."
          />
        </>
      )}
    </Card>
  );
}

function Egress({ d }: { d: Any }) {
  const e = d.egress;
  if (!e) return <Card title="Egress" id="egress" error={d.errors.egress}>{null}</Card>;
  return (
    <Card title="Egress" id="egress" error={d.errors.egress}>
      <div class="kv-row">
        <span>
          <b>{n1(e.reqPerMin)}</b> requests/min (5 min)
        </span>
        <span>
          Mirror hit rate <b>{pct(e.mirror.rate)}</b> ({e.mirror.hits}/{e.mirror.total}, {bytes(e.mirror.hitBytes)} served from cache, 24 h)
        </span>
      </div>
      <h3>Requests per minute (1 h; denied darker)</h3>
      <Bars title="Requests per minute" values={e.perMinute.map((r: Any) => r.n)} marks={e.perMinute.map((r: Any) => r.denied)} labels={e.perMinute.map((r: Any) => hhmm(r.minute))} />
      <h3>Denied hosts (24 h)</h3>
      <Table head={["Host", "Reason", ["Count", "r"], "Last"]} rows={e.deniedTop.map((x: Any) => [<span class="mono wrap">{x.host}</span>, x.why || "–", x.n, ago(x.lastAt)])} empty="Nothing denied." />
      <h3>Top talkers (1 h, by sandbox)</h3>
      <Table
        head={["Sandbox", "Owner", "Conversation", ["Requests", "r"], ["Denied", "r"], ["Bytes", "r"]]}
        rows={e.topTalkers.map((x: Any) => [<span class="mono">{short(x.pod, 20)}</span>, x.owner ?? "–", <span class="wrap">{x.title || "–"}</span>, x.n, x.denied, bytes(x.bytes)])}
        empty="No sandbox traffic in the last hour."
      />
      {e.cache.length > 0 && (
        <p class="note">
          Cache per pod: {e.cache.map((c: Any) => `${c.replica} ${bytes(c.bytes)} (${c.hits ?? 0} hits / ${c.misses ?? 0} misses since start, ${c.openSockets ?? 0} open)`).join(" · ")}
        </p>
      )}
    </Card>
  );
}

function Health({ d }: { d: Any }) {
  const h = d.health;
  if (!h) return <Card title="Health" id="health" error={d.errors.health}>{null}</Card>;
  return (
    <Card title="Health" id="health" error={d.errors.health}>
      <ul class="role-list">
        {h.roles.map((r: Any) => (
          <li key={r.role} class="role">
            <div class="sbx-head">
              <Dot ok={r.replicas.some((x: Any) => x.up)} />
              <b>{r.role}</b>
              {r.deployment ? (
                <span class="m-dim">
                  {r.deployment.ready}/{r.deployment.desired} ready · {r.deployment.updated} updated · <span class="mono">{r.deployment.image}</span>
                </span>
              ) : (
                <span class="m-dim">{r.replicas.filter((x: Any) => x.up).length} up</span>
              )}
            </div>
            <ul class="replicas">
              {r.replicas.map((x: Any) => (
                <li key={x.replica}>
                  <Dot ok={x.up} label={x.up ? "up" : "silent"} />
                  <span class="mono">{x.replica}</span>
                  <span class="mono m-dim">{short(x.commit, 12)}</span>
                  <span class="m-dim">up {x.startedAt ? dur(Date.now() - Date.parse(x.startedAt)) : "–"}</span>
                  {x.draining && <span class="state state-releasing">draining</span>}
                  {x.turns != null && <span class="m-dim">{x.turns} turns · {x.streams} SSE</span>}
                </li>
              ))}
            </ul>
          </li>
        ))}
      </ul>
      {h.deploymentsNote && <p class="note">Deployments: {h.deploymentsNote}</p>}
      <div class="kv-row">
        {h.leaders.map((l: Any) => (
          <span key={l.name}>
            {l.name} leader <b class="mono">{l.holder ? l.holder.replace(/^jarvis-[a-z]+:/, "") : "none"}</b>
          </span>
        ))}
      </div>
      <div class="kv-row">
        <span>
          <Dot ok={true} /> Postgres <b>{h.postgres.ms} ms</b> · <Meter used={h.postgres.total} max={h.postgres.max} fmt={n0} /> connections
        </span>
        <span>
          <Dot ok={h.s3.ok} /> S3 {h.s3.ok ? <b>{h.s3.ms} ms</b> : <b class="is-bad-text">{h.s3.error}</b>} <span class="m-dim">({h.s3.bucket})</span>
        </span>
      </div>
      <details>
        <summary>Connections by role</summary>
        <Table head={["Application", ["Open", "r"], ["Active", "r"]]} rows={h.postgres.connections.map((c: Any) => [<span class="mono">{c.app}</span>, c.n, c.active])} />
      </details>
      <h3>Recent errors (redacted)</h3>
      {h.errorLines.length ? (
        <ul class="log">
          {h.errorLines.map((e: Any, i: number) => (
            <li key={i}>
              <span class="m-dim">{hhmm(e.t)}</span> <b>{e.role}</b> <span class="mono">{e.at}</span>: {e.error}
            </li>
          ))}
        </ul>
      ) : (
        <p class="empty">No errors since the replicas started.</p>
      )}
    </Card>
  );
}

// ---- page ---------------------------------------------------------------------------------------------------------

const POLL_MS = 5000;

function Monitor() {
  const [d, setD] = useState<Any>(null);
  const [err, setErr] = useState<string | null>(null);
  const [status, setStatus] = useState<number>(0);
  const [, tick] = useState(0);
  useEffect(() => {
    let stop = false;
    let timer: number | undefined;
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    const load = async () => {
      if (stop) return;
      if (document.hidden) {
        timer = window.setTimeout(load, POLL_MS);
        return;
      }
      try {
        const r = await fetch(`/api/admin/monitor?tz=${encodeURIComponent(tz)}`, { credentials: "same-origin" });
        setStatus(r.status);
        if (!r.ok) throw new Error(r.status === 404 ? "not found" : `HTTP ${r.status}`);
        setD(await r.json());
        setErr(null);
      } catch (e) {
        setErr((e as Error).message);
      }
      timer = window.setTimeout(load, POLL_MS);
    };
    void load();
    const onVis = () => !document.hidden && (window.clearTimeout(timer), void load());
    document.addEventListener("visibilitychange", onVis);
    const t = window.setInterval(() => tick((x) => x + 1), 1000);
    return () => {
      stop = true;
      window.clearTimeout(timer);
      window.clearInterval(t);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, []);

  if (status === 404) return <main class="mon"><p class="empty">Not found.</p></main>;
  if (!d) return <main class="mon"><p class="empty">{err ? `Could not load: ${err}` : "Loading…"}</p></main>;
  const s = d.sandboxes;
  const l = d.load;
  const age = Date.now() - Date.parse(d.at);
  const errs24 = l ? l.errors24h.reduce((a: number, e: Any) => a + e.n, 0) : null;
  const cap = s ? s.pool.capacity ?? s.pool.max : null;
  return (
    <main class="mon">
      <header class="mon-head">
        <div>
          <h1>Jarvis 3 monitor</h1>
          <p class="m-dim">
            {d.product} · {d.provider} sandboxes · served by <span class="mono">{d.replica}</span> at <span class="mono">{short(d.commit, 12)}</span>
          </p>
        </div>
        <p class={`fresh ${err || age > 20_000 ? "is-stale" : ""}`} role="status">
          <span class="dot is-ok" /> {err ? `stale (${err})` : `updated ${dur(age)} ago`}
        </p>
      </header>
      <nav class="jump" aria-label="Sections">
        {[["sandboxes", "Sandboxes"], ["hosts", "Hosts"], ["load", "Load"], ["models", "Models"], ["egress", "Egress"], ["health", "Health"]].map(([id, t]) => (
          <a key={id} href={`#${id}`}>
            {t}
          </a>
        ))}
      </nav>
      <div class="tiles">
        <Tile label="Sandboxes" value={s ? `${s.pool.live} / ${cap}` : "–"} sub={s ? `alloc p50 ${dur(s.starts24h.p50Ms)}` : undefined} tone={s && cap && s.pool.live >= cap ? "warn" : ""} />
        <Tile label="Turns now" value={l ? String(l.inFlight.running + l.inFlight.waiting + l.inFlight.queued) : "–"} sub={l ? `${l.turnsToday} today` : undefined} />
        <Tile label="Users" value={l ? `${l.usersNow} now` : "–"} sub={l ? `${l.usersToday} today` : undefined} />
        <Tile label="Model spend" value={d.models ? usd(d.models.egressUsdToday) : "–"} sub={d.models ? `${usd(d.models.egressUsd7d)} 7 d` : undefined} />
        <Tile label="Egress" value={d.egress ? `${n1(d.egress.reqPerMin)}/min` : "–"} sub={d.egress ? `${d.egress.deniedTop.reduce((a: number, x: Any) => a + x.n, 0)} denied 24 h` : undefined} />
        <Tile label="Errors 24 h" value={errs24 == null ? "–" : String(errs24)} tone={errs24 ? "warn" : ""} sub={d.health ? `PG ${d.health.postgres.ms} ms` : undefined} />
      </div>
      <div class="cols">
        <Sandboxes d={d} />
        <Hosts d={d} />
        <Load d={d} />
        <Models d={d} />
        <Egress d={d} />
        <Health d={d} />
      </div>
    </main>
  );
}

render(<Monitor />, document.getElementById("app")!);
