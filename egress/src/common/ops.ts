// Cheap operational records for the admin monitor (docs/CHAT.md, "Admin monitor"): `ops_events` rows for things the
// other tables don't keep (sandbox start latency and failures; provider vm keeps its own in vm_events), and a presence record
// per replica of every role (kv `presence:<role>:<replica>`: version, start time, live counters, recent errors).
// Never put a secret, a token or a staff message into either.
import type { Db } from "./db.ts";
import { logError, recentErrors } from "./log.ts";

export type OpsKind = "sandbox_start";

export interface OpsFields {
  ok?: boolean | null;
  ms?: number | null;
  sandboxId?: string | null;
  conversationId?: string | null;
  host?: string | null;
  detail?: Record<string, unknown>;
}

/** Records one event; never throws and never delays the caller's work. */
export function opsEvent(db: Db, kind: OpsKind, f: OpsFields = {}): Promise<void> {
  return db
    .query("insert into ops_events (kind, ok, ms, sandbox_id, conversation_id, host, detail) values ($1,$2,$3,$4,$5,$6,$7)", [
      kind,
      f.ok ?? null,
      f.ms == null ? null : Math.round(f.ms),
      f.sandboxId ?? null,
      f.conversationId ?? null,
      f.host ?? null,
      JSON.stringify(f.detail ?? {}),
    ])
    .then(
      () => {},
      (e) => logError("ops event", e, { kind }),
    );
}

/** The image's jarvis3 commit (Dockerfile build arg CHAT_COMMIT), or "dev". */
export const appCommit = (): string => process.env.JARVIS_COMMIT || "dev";

export const PRESENCE_MS = 10_000;

/**
 * Writes this replica's presence every 10 s until stopped: {role, replica, commit, startedAt, at, errors, ...extra()}.
 * The monitor counts a replica as up while its record is younger than 30 s.
 */
export function startPresence(db: Db, role: "web" | "egress" | "tools", replica: string, extra: () => Record<string, unknown> = () => ({})): () => Promise<void> {
  const startedAt = new Date().toISOString();
  const key = `presence:${role}:${replica}`;
  const write = (gone = false) =>
    db
      .query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [
        key,
        JSON.stringify({ role, replica, commit: appCommit(), startedAt, at: new Date().toISOString(), gone, errors: recentErrors(10), ...extra() }),
      ])
      .then(() => {}, () => {});
  void write();
  const t = setInterval(() => void write(), PRESENCE_MS);
  t.unref();
  return async () => {
    clearInterval(t);
    await write(true);
  };
}
