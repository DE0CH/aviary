// `node src/main.ts tools import-jarvis [--delta]` (aviary cutover spec 8.2, platform spec 17.2 tools item 5): copies
// the old stack's `jarvis.tool_calls` into tools' own `tool_calls`, every column, `source_id` = the old id. CronJob
// `tools-import` (A2 once, A6 once with `--delta`); deleted in A6. Environment: TOOLS_IMPORT_JARVIS_URL (read-only,
// the old database), TOOLS_DATABASE_URL (tools' own).
import { logError } from "../common/log.ts";
import { parseArgs, run, type CopySpec } from "../common/importjarvis.ts";
import { connect, migrate } from "./db.ts";

export const TOOL_CALLS: CopySpec = {
  target: "tool_calls",
  columns: ["at", "turn_id", "conversation_id", "user_email", "tool", "args", "ok", "ms", "error", "cost_usd"],
  async sourceSql() {
    return `select at, turn_id, conversation_id, user_email, tool, args, ok, ms, error, cost_usd, id as source_id
      from tool_calls where id > $1 order by id limit $2`;
  },
};

/** The subcommand; exit 0 after a pass, 1 when it failed (a re-run retries), 2 on bad arguments or env. */
export async function importJarvis(args: readonly string[]): Promise<void> {
  let delta: boolean, source: string, target: string;
  try {
    ({ delta } = parseArgs(args));
    source = need("TOOLS_IMPORT_JARVIS_URL");
    target = need("TOOLS_DATABASE_URL");
  } catch (e) {
    logError("tools import-jarvis", e);
    process.exit(2);
  }
  const db = connect(target);
  try {
    await migrate(db);
    await run("tools", source, db, [TOOL_CALLS], delta);
  } catch (e) {
    logError("tools import-jarvis", e);
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
