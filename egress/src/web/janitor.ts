// Housekeeping, run by one replica at a time (advisory-lock leadership): release idle and expired sandboxes, delete
// pods nobody owns, end turns whose replica died, expire unanswered confirmations. Every replica also writes its own
// heartbeat, which is how a dead replica's turns are recognised.
import { channel, lead } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import type { WebDeps } from "./ctx.ts";
import { appendEvent } from "./events.ts";
import type { SandboxManager, SandboxRow } from "./sandbox.ts";

const JANITOR_LOCK = 4_242_003;
const HEARTBEAT_MS = 10_000;
const DEAD_AFTER_S = 60;
const CONFIRM_TTL_MIN = 15;

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
  for (const t of dead) await appendEvent(db, t.conversation_id, t.id, "turn_end", { status: "error", error: "interrupted: the server restarted", costUsd: null });

  // confirmations nobody answered
  const { rows: expired } = await db.query<{ id: string; conversation_id: string; turn_id: string }>(
    `update confirmations set status = 'expired', decided_at = now() where status = 'pending' and created_at < now() - make_interval(mins => $1)
     returning id, conversation_id, turn_id`,
    [CONFIRM_TTL_MIN],
  );
  for (const c of expired) {
    await db.query("select pg_notify($1, 'expired')", [channel("confirm", c.id)]);
    await appendEvent(db, c.conversation_id, c.turn_id, "confirmation_decided", { id: c.id, status: "expired" });
  }

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
    await deps.k8s.deletePod(cfg.namespace, s.pod).catch(() => {});
    await db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'stuck' where id = $1 and status <> 'gone'", [s.id]);
  }

  // pods without a live row, and live rows without a pod
  const pods = await deps.k8s.listPods(cfg.namespace, "app=jarvis-sandbox");
  const { rows: live } = await db.query<{ id: string; pod: string; status: string; created_at: Date }>("select id, pod, status, created_at from sandboxes where status <> 'gone'");
  const livePods = new Set(live.map((r) => r.pod));
  for (const p of pods) {
    const age = p.metadata.creationTimestamp ? Date.now() - Date.parse(p.metadata.creationTimestamp) : Infinity;
    if (!livePods.has(p.metadata.name) && age > 60_000) {
      log("janitor: orphan pod", { pod: p.metadata.name });
      await deps.k8s.deletePod(cfg.namespace, p.metadata.name).catch((e) => logError("janitor orphan", e));
    }
  }
  const podNames = new Set(pods.map((p) => p.metadata.name));
  for (const r of live) {
    if (r.status === "ready" && !podNames.has(r.pod)) {
      await db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'pod vanished' where id = $1 and status = 'ready'", [r.id]);
    }
  }
}

export function startJanitor(deps: WebDeps, sandboxes: SandboxManager, intervalMs = 30_000): { stop(): Promise<void> } {
  return lead(deps.dbUrl, JANITOR_LOCK, () => janitorPass(deps, sandboxes), intervalMs);
}
