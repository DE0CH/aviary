// The tools role (docs/CHAT.md, "tools"): the MCP server claw reaches through jarvis-mcp-bridge. Holds the tool keys
// (Exa, TikHub, the Wisers relay's Access service token, upstream MCP tokens); nothing else in the chat runtime does.
import { loadConfig } from "../common/config.ts";
import { connect, Listener } from "../common/db.ts";
import { log } from "../common/log.ts";
import { createToolsServer } from "./server.ts";
import type { Deps } from "./types.ts";
import { startRefresh, Upstream } from "./upstream.ts";

export async function main(): Promise<void> {
  const config = loadConfig();
  const db = connect();
  const listener = new Listener();
  const ns = process.env.POD_NAMESPACE || "jarvis";
  const deps: Deps = {
    config,
    db,
    listener,
    webUrl: (process.env.JARVIS_WEB_URL || `http://web.${ns}.svc.cluster.local:8080`).replace(/\/$/, ""),
    env: process.env,
    fetch: globalThis.fetch,
    confirmTimeoutMs: Number(process.env.JARVIS_CONFIRM_TIMEOUT_MS || 15 * 60_000),
  };
  const upstreams = config.tools.upstreams.map((u) => new Upstream(u, deps));
  // listing identity for upstreams (their per-call identity is the staff member): the first admin
  const refresh = startRefresh(upstreams, config.admins[0] ?? config.users[0]!);
  await refresh.ready;
  const { server, drain } = createToolsServer(deps, upstreams);
  const port = Number(process.env.PORT || 8080);
  server.listen(port, () => log("tools listening", { port }));
  let stopping = false;
  const stop = async () => {
    if (stopping) return;
    stopping = true;
    log("tools draining");
    refresh.stop();
    // a confirmation can hold a call for 15 minutes; the pod's grace period bounds this
    await drain(Number(process.env.JARVIS_DRAIN_MS || 16 * 60_000));
    await listener.close();
    await db.end();
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}
