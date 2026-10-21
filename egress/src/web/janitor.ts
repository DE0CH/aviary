// Housekeeping, run by one replica at a time (advisory-lock leadership): release idle and expired sandboxes, clear
// stuck starts and releases, remove sandboxes the provider has without a live row, and mark rows whose sandbox
// vanished. A sandbox in use is touched every few seconds by the UI pod running Claude Code in it (/internal/cc/touch).
import { lead } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import type { WebDeps } from "./ctx.ts";
import type { SandboxManager, SandboxRow } from "./sandbox.ts";

const JANITOR_LOCK = 4_242_003;

export async function janitorPass(deps: WebDeps, sandboxes: SandboxManager): Promise<void> {
  const db = deps.db;
  const cfg = deps.config.sandbox;

  // idle and expired sandboxes
  const { rows: toRelease } = await db.query<SandboxRow & { why: string }>(
    `select s.*, case when s.expires_at < now() then 'max-lifetime' else 'idle' end as why from sandboxes s
      where s.status = 'ready' and (s.expires_at < now() or s.last_active_at < now() - make_interval(mins => $1))`,
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
  // a month of sandbox-start records (ops_events)
  await db.query("delete from ops_events where kind = 'sandbox_start' and at < now() - interval '30 days'");
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
