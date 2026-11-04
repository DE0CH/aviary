// `node src/main.ts egress import-jarvis [--delta]` (aviary cutover spec 8.2, platform spec 17.2 item 6): copies the
// old stack's `jarvis.egress_log` into egress's own `egress_log`, every column, `source_id` = the old id, so the new
// egress's budget counts the old stack's spend like its own. `person` is the old row's sandbox owner
// (`jarvis.sandboxes.owner` by `sandbox_id`), else the owner of its conversation (`jarvis.conversations.owner` by
// `conversation_id`, the legacy UI's calls), else null, lowercased (one person key however the old row spelled it,
// as hermit's import does). CronJob `egress-import` (every minute from A2 to A6,
// `--delta` after the first run; A6's last run is a full pass, which also checks that every source row is there); deleted
// in A6. Environment: EGRESS_IMPORT_JARVIS_URL (read-only, the old
// database), EGRESS_DATABASE_URL (egress's own).
import { logError } from "../common/log.ts";
import { hasTable, parseArgs, run, type CopySpec } from "../common/importjarvis.ts";
import { connect, migrate } from "./db.ts";

const COLUMNS = ["at", "conversation_id", "sandbox_id", "turn_id", "kind", "method", "host", "port", "path", "status", "decision", "bytes_up", "bytes_down", "ms", "cost_usd", "model", "owner", "person"] as const;

export const EGRESS_LOG: CopySpec = {
  target: "egress_log",
  columns: COLUMNS,
  async sourceSql(src) {
    const sbx = await hasTable(src, "sandboxes");
    const conv = await hasTable(src, "conversations");
    const person = `lower(coalesce(${sbx ? "s.owner" : "null"}, ${conv ? "c.owner" : "null"}))`;
    return `select e.at, e.conversation_id, e.sandbox_id, e.turn_id, e.kind, e.method, e.host, e.port, e.path, e.status,
        e.decision, e.bytes_up, e.bytes_down, e.ms, e.cost_usd, e.model, e.owner, ${person} as person, e.id as source_id
      from egress_log e
      ${sbx ? "left join sandboxes s on s.id = e.sandbox_id" : ""}
      ${conv ? "left join conversations c on c.id = e.conversation_id" : ""}
      where e.id > $1 order by e.id limit $2`;
  },
};

/** The subcommand; exit 0 after a pass, 1 when it failed (the next run retries), 2 on bad arguments or env. */
export async function importJarvis(args: readonly string[]): Promise<void> {
  let delta: boolean, source: string, target: string;
  try {
    ({ delta } = parseArgs(args));
    source = need("EGRESS_IMPORT_JARVIS_URL");
    target = need("EGRESS_DATABASE_URL");
  } catch (e) {
    logError("egress import-jarvis", e);
    process.exit(2);
  }
  const db = connect(target);
  try {
    await migrate(db);
    const results = await run("egress", source, db, [EGRESS_LOG], delta);
    if (results.some((r) => (r.missing ?? 0) > 0)) throw new Error("the source holds rows egress_log does not (see the log line)");
  } catch (e) {
    logError("egress import-jarvis", e);
    process.exitCode = 1;
  } finally {
    await db.end().catch(() => {});
  }
}

function need(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`environment variable ${name} is not set`);
  return v;
}
