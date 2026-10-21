// The tools role (docs/CHAT.md, "tools"): the MCP server claw reaches through jarvis-mcp-bridge. Holds the tool keys
// (Exa, TikHub, the Wisers relay's Access service token, upstream MCP tokens); nothing else in the chat runtime does.
import { loadConfig } from "../common/config.ts";
import { connect } from "../common/db.ts";
import { log } from "../common/log.ts";
import { startPresence } from "../common/ops.ts";
import { createToolsServer } from "./server.ts";
import type { Deps } from "./types.ts";
import { startRefresh, Upstream } from "./upstream.ts";

export async function main(): Promise<void> {
  const config = loadConfig();
  const db = connect();
  const deps: Deps = {
    config,
    db,
    env: process.env,
    fetch: globalThis.fetch,
  };
  const upstreams = config.tools.upstreams.map((u) => new Upstream(u, deps));
  // listing identity for upstreams (their per-call identity is the staff member): the first admin
  const refresh = startRefresh(upstreams, config.admins[0] ?? config.users[0]!);
  await refresh.ready;
  const { server, drain } = createToolsServer(deps, upstreams);
  const port = Number(process.env.PORT || 8080);
  server.listen(port, () => log("tools listening", { port }));
  let draining = false;
  const stopPresence = startPresence(db, "tools", process.env.HOSTNAME || "tools", () => ({ draining }));
  let stopping = false;
  const stop = async () => {
    if (stopping) return;
    stopping = true;
    log("tools draining");
    draining = true;
    refresh.stop();
    // an upstream call may take up to 2 minutes; the pod's grace period bounds this
    await drain(Number(process.env.JARVIS_DRAIN_MS || 150_000));
    await stopPresence();
    await db.end();
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}
