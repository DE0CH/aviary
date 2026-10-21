// Cheap operational records: `ops_events` rows for things the other tables don't keep (sandbox start latency and
// failures; provider vm keeps its own in vm_events), read with SQL (docs/RUNBOOK.md). Never put a secret, a token or a
// staff message into one.
import type { Db } from "./db.ts";
import { logError } from "./log.ts";

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
