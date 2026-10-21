// The egress role: the proxy on 3128, /healthz and /metrics on 9090. On SIGTERM it stops being ready, stops
// accepting, lets open requests and tunnels finish (EGRESS_DRAIN_SECONDS, default 120), flushes the log and exits.
//
// Env: DATABASE_URL, JARVIS_CONFIG, JARVIS_CACHE_DIR (default /var/cache/jarvis-egress), EGRESS_SELF_HOSTS (comma
// list; default egress, egress.<POD_NAMESPACE>.svc, egress.<POD_NAMESPACE>.svc.cluster.local), EGRESS_BLOCK_CIDRS
// (extra refused ranges, e.g. the node's own /64), and every variable a route's inject names (OPENROUTER_API_KEY).
import { createServer } from "node:http";
import { loadConfig } from "../common/config.ts";
import { connect } from "../common/db.ts";
import { log } from "../common/log.ts";
import { startPresence } from "../common/ops.ts";
import { AddressGuard } from "./netguard.ts";
import { Egress } from "./proxy.ts";

export async function main(): Promise<void> {
  const config = loadConfig();
  const db = connect();
  const ns = process.env.POD_NAMESPACE || "jarvis";
  const selfHosts = (process.env.EGRESS_SELF_HOSTS || `egress,egress.${ns},egress.${ns}.svc,egress.${ns}.svc.cluster.local`).split(",").map((s) => s.trim()).filter(Boolean);
  // provider "vm": sandboxes reach this proxy (and its mirror) at the egress VIP, which the hub translates to a pod
  const vip = config.sandbox.vm?.egressVip;
  if (vip && !selfHosts.includes(vip)) selfHosts.push(vip);
  const egress = new Egress({
    db,
    defaultAllowedHosts: config.egress.defaultAllowedHosts,
    routes: config.egress.routes,
    cacheDir: process.env.JARVIS_CACHE_DIR || "/var/cache/jarvis-egress",
    cacheBytes: Math.round(config.egress.cacheGiB * 1024 ** 3),
    selfHosts,
    guard: new AddressGuard((process.env.EGRESS_BLOCK_CIDRS || "").split(",").map((s) => s.trim())),
  });
  const port = await egress.listen(Number(process.env.PORT || 3128));

  const health = createServer((req, res) => {
    if (req.url === "/healthz") {
      res.writeHead(egress.draining ? 503 : 200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ ok: !egress.draining }));
    }
    if (req.url === "/metrics") {
      const c = egress.counters;
      res.writeHead(200, { "content-type": "text/plain; version=0.0.4" });
      return void res.end(
        [
          `egress_requests_total ${c.requests}`,
          `egress_denied_total ${c.denied}`,
          `egress_tunnels_total ${c.tunnels}`,
          `egress_bytes_up_total ${c.bytesUp}`,
          `egress_bytes_down_total ${c.bytesDown}`,
          `egress_open_sockets ${egress.openSockets}`,
          `egress_cache_bytes ${egress.cache.totalBytes}`,
          `egress_cache_hits_total ${egress.cache.hits}`,
          `egress_cache_misses_total ${egress.cache.misses}`,
          `egress_log_rows_total ${egress.log.written}`,
          "",
        ].join("\n"),
      );
    }
    res.writeHead(404).end();
  });
  health.listen(Number(process.env.HEALTH_PORT || 9090), "::");
  log("egress listening", { port, selfHosts });
  const stopPresence = startPresence(db, "egress", process.env.HOSTNAME || "egress", () => ({
    openSockets: egress.openSockets,
    draining: egress.draining,
    cacheBytes: egress.cache.totalBytes,
    cacheHits: egress.cache.hits,
    cacheMisses: egress.cache.misses,
  }));
  // provider "vm": this pod's address, for the hub's translation of the egress VIP (sandbox-vm.ts reads kv egress_pod:*)
  const podIp = process.env.POD_IP;
  const podKey = `egress_pod:${(process.env.HOSTNAME || "egress").replace(/[^a-z0-9-]/gi, "")}`;
  let announcing = true;
  const announce = () =>
    !announcing || !podIp
      ? Promise.resolve()
      : db
          .query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [podKey, JSON.stringify({ ip: podIp, at: new Date().toISOString() })])
          .then(() => {}, () => {});
  if (config.sandbox.vm && podIp) {
    void announce();
    setInterval(() => void announce(), 10_000).unref();
  }

  let stopping = false;
  process.on("SIGTERM", async () => {
    if (stopping) return;
    stopping = true;
    log("egress draining", { open: egress.openSockets });
    // new sandbox connections go to the other pods from the hub's next update on (open ones finish here)
    announcing = false;
    if (config.sandbox.vm) await db.query("delete from kv where key = $1", [podKey]).catch(() => {});
    await egress.drain(Number(process.env.EGRESS_DRAIN_SECONDS || 120) * 1000);
    await stopPresence();
    health.close();
    await db.end().catch(() => {});
    log("egress stopped");
    process.exit(0);
  });
}
