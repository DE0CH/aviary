// The tools role (`node src/main.ts tools`, Kubernetes `args: [tools]`; aviary platform spec 17.2 tools; docs/CHAT.md
// "tools"): YYC's read-only MCP server, named `tools`, which Claude Code in a sandbox reaches as `http://tools/mcp`
// through the gateway (egress). Holds the tool keys (Exa, TikHub, AnyAPI, upstream MCP tokens). Ports are fixed: the
// API on 8080, health on 9090 (/healthz, /readyz, /metrics, /version).
//
// Configuration: /etc/tools/tools.json (config.ts) and the staff catalogue /etc/tools/catalogue/catalogue.json
// (catalogue.ts). Environment, all required: TOOLS_DATABASE_URL, TOOLS_ACCEPT_GATEWAY (the gateway's bearers),
// TOOLS_TOKEN_HERMIT (presented to hermit for liveness), and every variable the config names.
//
// SIGTERM (after Kubernetes' 10 s preStop): /readyz 503, no new connections, a request on a kept-alive connection is
// served with `Connection: close`, in-flight calls finish up to 150 s, what is left is cut (tools_drain_cut_total),
// the pending tool_calls rows are written, exit 0 with log `drained`. A second SIGTERM exits at once.
import { createServer } from "node:http";
import { log, logError } from "../common/log.ts";
import { hermitGetter } from "../egress/tokens.ts";
import { Catalogue } from "./catalogue.ts";
import { checkConfigEnv, loadToolsConfig } from "./config.ts";
import { CallLog, connect, migrate } from "./db.ts";
import { Liveness } from "./liveness.ts";
import { acceptList, createToolsServer, DRAIN_MS, type ToolsServer } from "./server.ts";
import type { Deps } from "./types.ts";
import { startRefresh, Upstream } from "./upstream.ts";

const API_PORT = 8080;
const HEALTH_PORT = 9090;
const COMMIT = process.env.CHAT_COMMIT || "unknown";
const BUILT_AT = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$/.test(process.env.CHAT_BUILT_AT ?? "") ? process.env.CHAT_BUILT_AT! : null;

function need(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`environment variable ${name} is not set`);
  return v;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function main(): Promise<void> {
  let config, dbUrl: string, hermitToken: string, accept: Buffer[];
  try {
    config = loadToolsConfig();
    dbUrl = need("TOOLS_DATABASE_URL");
    hermitToken = need("TOOLS_TOKEN_HERMIT");
    accept = acceptList(process.env);
    checkConfigEnv(config, process.env);
  } catch (e) {
    logError("tools start", e);
    process.exit(2);
  }

  // why /readyz is 503: emptied as each part of the start completes; "draining" from SIGTERM on
  const notReady = new Set(["migrations", "catalogue", "upstreams"]);
  const db = connect(dbUrl);
  const calls = new CallLog(db);
  const catalogue = new Catalogue();
  const liveness = new Liveness(hermitGetter("http://hermit", hermitToken));
  const deps: Deps = { config, env: process.env, fetch: globalThis.fetch };
  const upstreams = config.upstreams.map((u) => new Upstream(u, deps));
  let api: ToolsServer | null = null;

  const health = createServer((req, res) => {
    const path = (req.url ?? "").split("?")[0];
    const json = (status: number, body: unknown) => {
      const b = JSON.stringify(body);
      res.writeHead(status, { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" });
      res.end(b);
    };
    if (path === "/healthz") return json(200, { ok: true });
    if (path === "/readyz") return notReady.size ? json(503, { ok: false, why: [...notReady] }) : json(200, { ok: true });
    if (path === "/version") return json(200, { name: "tools", version: COMMIT.slice(0, 12), revision: COMMIT, builtAt: BUILT_AT });
    if (path === "/metrics") {
      const lines = api ? api.metrics() : [];
      lines.push(
        `tools_ready ${notReady.size ? 0 : 1}`,
        `tools_liveness_lookup_failures_total ${liveness.failures}`,
        `tools_liveness_stale_served_total ${liveness.staleServed}`,
        `tools_catalogue_refused_total ${catalogue.refused}`,
        `tools_tool_calls_dropped_total ${calls.dropped}`,
        "",
      );
      res.writeHead(200, { "content-type": "text/plain; version=0.0.4" });
      return void res.end(lines.join("\n"));
    }
    res.writeHead(404).end();
  });
  health.listen(HEALTH_PORT, "::");

  let stopping = false;
  let refresh: { stop(): void } | null = null;
  const onSignal = async () => {
    if (stopping) {
      log("tools exit at once (second SIGTERM)");
      process.exit(0);
    }
    stopping = true;
    notReady.add("draining");
    log("tools draining", { inFlight: api?.inFlight ?? 0 });
    const cut = api ? await api.drain(DRAIN_MS) : 0;
    refresh?.stop();
    catalogue.stop();
    liveness.stop();
    await calls.flush();
    health.close();
    await db.end().catch(() => {});
    log("drained", { cut });
    process.exit(0);
  };
  process.on("SIGTERM", onSignal);
  process.on("SIGINT", onSignal);

  // migrations: retried until the database answers (the pod stays unready meanwhile); an edited history is fatal
  void (async () => {
    for (let i = 0; !stopping; i++) {
      try {
        await migrate(db);
        notReady.delete("migrations");
        return;
      } catch (e) {
        if (/was edited/.test(String((e as Error).message))) {
          logError("tools start", e);
          process.exit(2);
        }
        if (i % 10 === 0) logError("tools migrations", e);
        await sleep(2000);
      }
    }
  })();

  // the catalogue: re-read every 30 s; ready once a good copy has been read
  catalogue.start();
  void (async () => {
    while (!stopping && !(await catalogue.reload())) await sleep(2000);
    notReady.delete("catalogue");
  })();
  liveness.start();

  // upstream tool lists: the first read (each failure logged, the upstream then offers nothing until the next refresh)
  const r = startRefresh(upstreams);
  refresh = r;
  void r.ready.then(() => notReady.delete("upstreams"));

  api = createToolsServer(deps, upstreams, { accept, liveness, catalogue, calls });
  api.server.listen(API_PORT, "::", () => log("tools listening", { port: API_PORT, upstreams: upstreams.map((u) => u.cfg.prefix) }));
}
