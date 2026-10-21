// The web role (docs/CHAT.md, "Control plane"): the sandbox manager behind /internal/cc/*, its janitor and,
// with provider "vm", the scaler. Zero-downtime: on SIGTERM the replica reports not ready (the Service stops sending it
// calls), answers the calls it already has (a sandbox start takes up to 3 minutes), then exits; the janitor and the
// scaler run on one replica at a time (advisory locks), so the other replica takes them over.
import { createServer } from "node:http";
import { expandEnv, loadConfig, requireEnv, type ChatConfig } from "../common/config.ts";
import { connect, migrate, type Db } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import { WebApp } from "./app.ts";
import type { WebDeps } from "./ctx.ts";
import { startJanitor } from "./janitor.ts";
import { K8sClient } from "./k8s.ts";
import type { SandboxProvider } from "./provider.ts";
import { E2bProvider } from "./sandbox-e2b.ts";
import { KubernetesProvider } from "./sandbox-k8s.ts";
import { startVmScaler, VmProvider } from "./sandbox-vm.ts";
import { Ec2Client, SandboxdClient } from "./vm-clients.ts";
import { SandboxManager } from "./sandbox.ts";

export interface WebServer {
  app: WebApp;
  close(): Promise<void>;
  port: number;
}

/** Wires a WebApp to an HTTP server, the janitor and (provider vm) the scaler. */
export async function startWeb(deps: WebDeps, port: number, opts: { janitorMs?: number; scalerMs?: number } = {}): Promise<WebServer> {
  await migrate(deps.db);
  const sandboxes = new SandboxManager(deps);
  const app = new WebApp(deps, sandboxes);
  const server = createServer((req, res) => void app.handle(req, res));
  server.keepAliveTimeout = 65_000;
  // a sandbox start may take up to 3 minutes
  server.requestTimeout = 300_000;
  await new Promise<void>((r) => server.listen(port, r));
  const janitor = startJanitor(deps, sandboxes, opts.janitorMs);
  const scaler = deps.sandboxProvider instanceof VmProvider ? startVmScaler(deps.dbUrl, deps.sandboxProvider.controller, opts.scalerMs) : null;
  const addr = server.address();
  return {
    app,
    port: typeof addr === "object" && addr ? addr.port : port,
    async close() {
      await janitor.stop();
      await scaler?.stop();
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
}

export function sandboxProvider(config: ChatConfig, namespace: string, db: Db): SandboxProvider {
  if (config.sandbox.provider === "vm") {
    const vm = config.sandbox.vm!;
    const pem = (name: string) => requireEnv(name).replace(/\\n/g, "\n");
    const sandboxd = new SandboxdClient({ ca: pem(vm.tls.caEnv), cert: pem(vm.tls.certEnv), key: pem(vm.tls.keyEnv) });
    const aws = vm.aws;
    const ec2 = aws ? new Ec2Client({ accessKeyId: requireEnv(aws.accessKeyIdEnv), secretAccessKey: requireEnv(aws.secretAccessKeyEnv), region: aws.region }) : null;
    const hostTls = aws ? { ca: pem(vm.tls.caEnv), cert: pem(vm.tls.hostCertEnv), key: pem(vm.tls.hostKeyEnv) } : null;
    const legacy = vm.legacyProvider === "kubernetes" ? new KubernetesProvider({ k8s: K8sClient.inCluster(), config, egressNamespace: namespace }) : null;
    return new VmProvider({ db, config, sandboxd, ec2, hostTls }, legacy);
  }
  if (config.sandbox.provider === "e2b") {
    const e = config.sandbox.e2b!;
    const apiKey = requireEnv(e.apiKeyEnv);
    // ${VAR}s in the wire headers come from this pod's env; checked now so a missing one stops the rollout
    const egressHeaders = Object.fromEntries(Object.entries(e.egressHeaders).map(([k, v]) => [k, expandEnv(v)]));
    // CPU and memory are the template's (set where the chat workflow builds it), not per sandbox
    return new E2bProvider({ ...e, apiKey, egressHeaders });
  }
  return new KubernetesProvider({ k8s: K8sClient.inCluster(), config, egressNamespace: namespace });
}

export async function main(): Promise<void> {
  const config = loadConfig();
  const namespace = process.env.JARVIS_NAMESPACE || "jarvis";
  const dbUrl = requireEnv("DATABASE_URL");
  const db = connect(dbUrl);
  const deps: WebDeps = {
    config,
    db,
    dbUrl,
    sandboxProvider: sandboxProvider(config, namespace, db),
    namespace,
    drainSeconds: Number(process.env.JARVIS_DRAIN_SECONDS || 300),
  };
  const web = await startWeb(deps, Number(process.env.PORT || 8080));
  log("web listening", { port: web.port });
  let stopping = false;
  const stop = async (sig: string) => {
    if (stopping) return;
    stopping = true;
    log("draining", { signal: sig, active: web.app.active });
    web.app.draining = true;
    try {
      // let the readiness probe see 503 and the Service drop this endpoint, then answer what is in flight
      await new Promise((r) => setTimeout(r, 5000));
      await web.app.drain(deps.drainSeconds * 1000);
      await web.close();
      await deps.db.end();
    } catch (e) {
      logError("shutdown", e);
    }
    process.exit(0);
  };
  process.on("SIGTERM", () => void stop("SIGTERM"));
  process.on("SIGINT", () => void stop("SIGINT"));
}
