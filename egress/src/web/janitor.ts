// Housekeeping, run by one replica at a time (advisory-lock leadership): release idle and expired sandboxes, remove
// sandboxes nobody owns, end turns whose replica died. Every replica also writes its own
// heartbeat, which is how a dead replica's turns are recognised.
import { lead } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import type { WebDeps } from "./ctx.ts";
import { appendEvent } from "./events.ts";
import type { SandboxManager, SandboxRow } from "./sandbox.ts";

const JANITOR_LOCK = 4_242_003;
const HEARTBEAT_MS = 10_000;
const DEAD_AFTER_S = 60;

export function startHeartbeat(deps: WebDeps): () => void {
  const beat = () =>
    deps.db
      .query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [
        `replica:${deps.replica}`,
        JSON.stringify({ at: new Date().toISOString() }),
      ])
      .catch((e) => logError("heartbeat", e));
  void beat();
  const t = setInterval(beat, HEARTBEAT_MS);
  return () => clearInterval(t);
}

export async function janitorPass(deps: WebDeps, sandboxes: SandboxManager): Promise<void> {
  const db = deps.db;
  const cfg = deps.config.sandbox;
  const busy = `exists (select 1 from turns t where t.conversation_id = s.conversation_id and t.status in ('queued','running','waiting'))`;

  // turns whose replica stopped beating (crashed or killed past its drain)
  const { rows: dead } = await db.query<{ id: string; conversation_id: string }>(
    `update turns t set status = 'error', finished_at = now(), error = 'interrupted: the server restarted'
      where (t.status in ('running','waiting') and not exists (
               select 1 from kv where kv.key = 'replica:' || t.replica and (kv.value->>'at')::timestamptz > now() - make_interval(secs => $1)))
         or (t.status = 'queued' and t.started_at < now() - interval '2 minutes')
      returning t.id, t.conversation_id`,
    [DEAD_AFTER_S],
  );
  for (const t of dead) await appendEvent(db, t.conversation_id, t.id, "turn_finished", { turnId: t.id, status: "error", error: "interrupted: the server restarted", costUsd: null });

  // idle and expired sandboxes (never while their conversation has a turn going)
  const { rows: toRelease } = await db.query<SandboxRow & { why: string }>(
    `select s.*, case when s.expires_at < now() then 'max-lifetime' else 'idle' end as why from sandboxes s
      where s.status = 'ready' and not ${busy}
        and (s.expires_at < now() or s.last_active_at < now() - make_interval(mins => $1))`,
    [cfg.idleMinutes],
  );
  for (const s of toRelease) await sandboxes.release(s, s.why).catch((e) => logError("janitor release", e, { sandbox: s.id }));

  // stuck starts and releases
  const { rows: stuck } = await db.query<SandboxRow>(
    "select * from sandboxes where (status = 'starting' and created_at < now() - interval '5 minutes') or (status = 'releasing' and last_active_at < now() - interval '30 minutes')",
  );
  for (const s of stuck) {
    await deps.sandboxProvider.destroy(s.pod).catch(() => {});
    await db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'stuck' where id = $1 and status <> 'gone'", [s.id]);
  }

  // sandboxes at the provider without a live row, and live rows whose sandbox is gone
  const pods = await deps.sandboxProvider.list();
  const { rows: all } = await db.query<{ id: string; pod: string; status: string; created_at: Date; host: string | null }>("select id, pod, status, created_at, host from sandboxes where status <> 'gone'");
  // only rows of the configured provider: during a switch to "vm" an older replica must not take the new provider's
  // sandboxes for vanished (and the vm provider lists the previous provider's own, sandbox.vm.legacyProvider)
  const live = deps.sandboxProvider.kind === "vm" ? all : all.filter((r) => !r.host);
  const livePods = new Set(live.map((r) => r.pod));
  for (const p of pods) {
    const age = Number.isNaN(p.createdAt) ? Infinity : Date.now() - p.createdAt;
    if (!livePods.has(p.name) && age > 60_000) {
      log("janitor: orphan sandbox", { pod: p.name });
      await deps.sandboxProvider.destroy(p.name).catch((e) => logError("janitor orphan", e));
    }
  }
  // the admin monitor's records: a month of sandbox starts, presence of replicas that are gone a day
  await db.query("delete from ops_events where kind = 'sandbox_start' and at < now() - interval '30 days'");
  await db.query("delete from kv where key like 'presence:%' and (value->>'at')::timestamptz < now() - interval '1 day'");
  const podNames = new Set(pods.map((p) => p.name));
  for (const r of live) {
    if (r.status === "ready" && !podNames.has(r.pod)) {
      await db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'sandbox vanished' where id = $1 and status = 'ready'", [r.id]);
    }
  }
}

export function startJanitor(deps: WebDeps, sandboxes: SandboxManager, intervalMs = 30_000): { stop(): Promise<void> } {
  return lead(deps.dbUrl, JANITOR_LOCK, () => janitorPass(deps, sandboxes), intervalMs);
}
