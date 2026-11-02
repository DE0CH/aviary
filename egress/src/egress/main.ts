// The egress role (`node src/main.ts egress`): YYC's sandbox gateway (aviary platform spec 3.11, 17.2; docs/CHAT.md
// "egress"). Ports are fixed: the sandbox proxy on 3128 (the only port a sandbox reaches), the in-cluster API on 8082
// (`model` for pigeon, the ops summary for hermit, admin reads), health on 9090.
//
// Configuration: /etc/egress/egress.json (config.ts). Environment, all required: EGRESS_DATABASE_URL; the pair tokens
// EGRESS_TOKEN_MAGPIE and EGRESS_TOKEN_HERMIT (presented to magpie and hermit), EGRESS_ACCEPT_PIGEON,
// EGRESS_ACCEPT_HERMIT and EGRESS_ACCEPT_ADMIN (accepted on 8082); every variable a route's inject names
// (OPENROUTER_API_KEY, EGRESS_TOKEN_TOOLS).
//
// SIGTERM (after Kubernetes' 10 s preStop): /readyz 503, no new connections, Connection: close on kept-alive ones,
// in-flight work finishes within its bound (model streams and tunnels 600 s, magpie/hermit 60 s, tools 180 s), what is
// left is cut and counted in egress_drain_cut_total{kind}, the log is flushed, exit 0 with log `drained`. A second
// SIGTERM exits at once.
import { createServer } from "node:http";
import { log, logError } from "../common/log.ts";
import { acceptLists, EgressApi } from "./api.ts";
import { HostPolicy } from "./allowlist.ts";
import { BudgetGate } from "./budget.ts";
import { checkInjectEnv, loadEgressConfig } from "./config.ts";
import { connect, keyStore, migrate } from "./db.ts";
import { EgressLog } from "./egresslog.ts";
import { AddressGuard } from "./netguard.ts";
import { Egress } from "./proxy.ts";
import { hermitGetter, SandboxTokens } from "./tokens.ts";

const PROXY_PORT = 3128;
const API_PORT = 8082;
const HEALTH_PORT = 9090;
const CACHE_DIR = "/var/cache/jarvis-egress";
const COMMIT = process.env.CHAT_COMMIT || "unknown";
/** RFC 3339 build time (image build arg CHAT_BUILT_AT), null when not built by the workflow. */
const BUILT_AT = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)$/.test(process.env.CHAT_BUILT_AT ?? "") ? process.env.CHAT_BUILT_AT! : null;
/** SIGTERM comes after the 10 s preStop; the grace period is 625 s (platform 9.3), so the kill comes 615 s after it. */
const KILL_AFTER_SIGTERM_MS = 615_000;

function need(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`environment variable ${name} is not set`);
  return v;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function main(): Promise<void> {
  let config, dbUrl: string, pair: { magpie: string; hermit: string }, accept;
  try {
    config = loadEgressConfig();
    dbUrl = need("EGRESS_DATABASE_URL");
    pair = { magpie: need("EGRESS_TOKEN_MAGPIE"), hermit: need("EGRESS_TOKEN_HERMIT") };
    accept = acceptLists(process.env);
    checkInjectEnv(config);
  } catch (e) {
    logError("egress start", e);
    process.exit(2);
  }

  // why /readyz is 503: emptied as each part of the start completes; "draining" from SIGTERM on
  const notReady = new Set(["migrations", "allowlist", "token-keys"]);
  let egress: Egress | null = null;
  let api: EgressApi | null = null;
  const db = connect(dbUrl);
  const tokens = new SandboxTokens(hermitGetter("http://hermit", pair.hermit), { store: keyStore(db) });
  const budget = new BudgetGate(db, config.budget);
  const egressLog = new EgressLog(db);
  budget.attach(egressLog);
  const health = createServer((req, res) => {
    const path = (req.url ?? "").split("?")[0];
    const json = (status: number, body: unknown) => {
      const b = JSON.stringify(body);
      res.writeHead(status, { "content-type": "application/json; charset=utf-8", "content-length": String(Buffer.byteLength(b)), "cache-control": "no-store" });
      res.end(b);
    };
    if (path === "/healthz") return json(200, { ok: true });
    if (path === "/readyz") return notReady.size ? json(503, { ok: false, why: [...notReady] }) : json(200, { ok: true });
    if (path === "/version") return json(200, { name: "egress", version: COMMIT.slice(0, 12), revision: COMMIT, builtAt: BUILT_AT, provides: { gateway: 1 }, accepts: { gateway: [1] } });
    if (path === "/metrics") {
      const lines = egress ? egress.metrics() : [];
      lines.push(`egress_ready ${notReady.size ? 0 : 1}`, `egress_liveness_lookup_failures_total ${tokens.lookupFailures}`, `egress_token_keys_from_store ${tokens.usable && !tokens.loaded ? 1 : 0}`, `egress_budget_read_failures_total ${budget.failures}`, `egress_budget_unknown_served_total ${budget.unknownServed}`, "");
      res.writeHead(200, { "content-type": "text/plain; version=0.0.4" });
      return void res.end(lines.join("\n"));
    }
    res.writeHead(404).end();
  });
  health.listen(HEALTH_PORT, "::");

  let stopping = false;
  process.on("SIGTERM", async () => {
    if (stopping) {
      log("egress exit at once (second SIGTERM)");
      process.exit(0);
    }
    stopping = true;
    const termAt = Date.now();
    notReady.add("draining");
    log("egress draining", { open: egress?.openSockets ?? 0, inFlight: egress?.inFlight() ?? null });
    const [cut] = await Promise.all([egress ? egress.drain() : Promise.resolve(null), api ? api.drain() : Promise.resolve()]);
    tokens.stop();
    // the log gets what the grace period has left after the drain (at least 10 s): a database outage during a rollout
    // loses only the rows still unwritten at the kill (logged with their model cost; docs/CHAT.md "Known gaps")
    await egressLog.close(Math.max(10_000, KILL_AFTER_SIGTERM_MS - (Date.now() - termAt) - 5_000));
    health.close();
    await db.end().catch(() => {});
    log("drained", { cut });
    process.exit(0);
  });

  // migrations: retried until the database answers (the pod stays unready meanwhile)
  for (let i = 0; ; i++) {
    try {
      await migrate(db);
      notReady.delete("migrations");
      break;
    } catch (e) {
      if (/was edited/.test(String((e as Error).message))) {
        logError("egress start", e);
        process.exit(2);
      }
      if (i % 10 === 0) logError("egress migrations", e);
      await sleep(2000);
    }
  }
  await budget.seed().catch((e) => logError("budget seed", e));

  const policy = HostPolicy.load({ defaultAllowedHosts: config.defaultAllowedHosts, denyHosts: config.denyHosts, generated: config.generatedAllowlist });
  log("egress allowlist", { version: policy.version, allow: policy.allow.size, deny: policy.deny.size, configDeny: policy.configDeny.size });
  notReady.delete("allowlist");

  egress = new Egress({
    config,
    log: egressLog,
    tokens,
    policy,
    pairTokens: pair,
    cacheDir: CACHE_DIR,
    cacheBytes: Math.round(config.cacheGiB * 1024 ** 3),
    guard: new AddressGuard(config.blockCidrs),
    budget,
  });
  api = new EgressApi({ db, config, budget: config.budget ? budget : null, accept });
  await egress.listen(PROXY_PORT);
  await api.listen(API_PORT);
  log("egress listening", { proxy: PROXY_PORT, api: API_PORT, selfHosts: config.selfHosts });

  // hermit's key set: fetched until it has been once (then every 5 min and on an unknown kid). Ready once tokens can
  // be verified: hermit's set, or, while hermit is away at this start, the last set it published (token_keys), so a
  // restart of both replicas during a hermit outage still serves. With neither (hermit away and a database that has
  // no set, or is away too) the pod stays unready: the recorded consequence in docs/CHAT.md "Known gaps".
  void (async () => {
    let ok = await tokens.start();
    while (!ok && !stopping) {
      if (await tokens.loadStored()) notReady.delete("token-keys");
      await sleep(2000);
      ok = await tokens.refreshKeys(false);
    }
    notReady.delete("token-keys");
  })();
}
