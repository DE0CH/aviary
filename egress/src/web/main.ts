// The web role (docs/CHAT.md): UI, API, SSE, turn runner, sandbox manager and janitor. Zero-downtime: on SIGTERM
// the replica reports not ready, refuses new turns (the UI retries on another replica), ends its SSE streams so
// clients reconnect elsewhere, and finishes its running turns before it exits.
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { AccessVerifier } from "../common/access.ts";
import { expandEnv, loadConfig, requireEnv, type ChatConfig } from "../common/config.ts";
import { connect, Listener, migrate, type Db } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import { S3BlobStore } from "../common/s3.ts";
import { WebApp } from "./app.ts";
import { replicaName, type WebDeps } from "./ctx.ts";
import { startHeartbeat, startJanitor } from "./janitor.ts";
import { K8sClient } from "./k8s.ts";
import type { SandboxProvider } from "./provider.ts";
import { E2bProvider } from "./sandbox-e2b.ts";
import { KubernetesProvider } from "./sandbox-k8s.ts";
import { startVmScaler, VmProvider } from "./sandbox-vm.ts";
import { Ec2Client, SandboxdClient } from "./vm-clients.ts";
import { SandboxManager } from "./sandbox.ts";
import { TurnRunner } from "./turns.ts";

export interface WebServer {
  app: WebApp;
  close(): Promise<void>;
  port: number;
}

/** Wires a WebApp to an HTTP server, the cancel channel, the heartbeat and the janitor. */
export async function startWeb(deps: WebDeps, port: number, opts: { janitorMs?: number; scalerMs?: number } = {}): Promise<WebServer> {
  await migrate(deps.db);
  const sandboxes = new SandboxManager(deps);
  const runner = new TurnRunner(deps, sandboxes);
  await runner.listen();
  const app = new WebApp(deps, sandboxes, runner);
  const server = createServer((req, res) => void app.handle(req, res));
  server.keepAliveTimeout = 65_000;
  await new Promise<void>((r) => server.listen(port, r));
  const stopBeat = startHeartbeat(deps);
  const janitor = startJanitor(deps, sandboxes, opts.janitorMs);
  const scaler = deps.sandboxProvider instanceof VmProvider ? startVmScaler(deps.dbUrl, deps.sandboxProvider.controller, opts.scalerMs) : null;
  const addr = server.address();
  return {
    app,
    port: typeof addr === "object" && addr ? addr.port : port,
    async close() {
      await janitor.stop();
      await scaler?.stop();
      stopBeat();
      app.closeStreams();
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
}

function sandboxProvider(config: ChatConfig, namespace: string, db: Db): SandboxProvider {
  if (config.sandbox.provider === "vm") {
    const vm = config.sandbox.vm!;
    const pem = (name: string) => requireEnv(name).replace(/\\n/g, "\n");
    const sandboxd = new SandboxdClient({ ca: pem(vm.tls.caEnv), cert: pem(vm.tls.certEnv), key: pem(vm.tls.keyEnv) });
    const aws = vm.aws;
    const ec2 = aws ? new Ec2Client({ accessKeyId: requireEnv(aws.accessKeyIdEnv), secretAccessKey: requireEnv(aws.secretAccessKeyEnv), region: aws.region }) : null;
    const hostTls = aws ? { ca: pem(vm.tls.caEnv), cert: pem(vm.tls.hostCertEnv), key: pem(vm.tls.hostKeyEnv) } : null;
    return new VmProvider({ db, config, sandboxd, ec2, hostTls });
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
  const appDir = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
  const deps: WebDeps = {
    config,
    db: connect(dbUrl),
    dbUrl,
    listener: new Listener(dbUrl),
    blobs: new S3BlobStore({
      endpoint: config.s3.endpoint,
      region: config.s3.region,
      bucket: config.s3.bucket,
      accessKeyId: requireEnv("S3_ACCESS_KEY_ID"),
      secretAccessKey: requireEnv("S3_SECRET_ACCESS_KEY"),
    }),
    sandboxProvider: null as unknown as SandboxProvider,
    verifier: new AccessVerifier(config.access),
    replica: replicaName(process.env.HOSTNAME || "web"),
    namespace,
    workRoot: process.env.JARVIS_WORK_ROOT || "/var/lib/jarvis/work",
    clawPath: process.env.CLAW_PATH || "/usr/local/bin/claw",
    bashPath: process.env.JARVIS_BASH || join(appDir, "src/bin/jarvis-bash"),
    mcpBridgePath: process.env.JARVIS_MCP_BRIDGE || join(appDir, "src/bin/jarvis-mcp-bridge"),
    egressBase: process.env.JARVIS_EGRESS_URL || `http://egress.${namespace}.svc.cluster.local:3128`,
    toolsUrl: process.env.JARVIS_TOOLS_URL || `http://tools.${namespace}.svc.cluster.local:8080/mcp`,
    uiDir: process.env.JARVIS_UI_DIR || "/app/ui-dist",
    brandDir: process.env.JARVIS_BRAND_DIR || "/etc/jarvis/brand",
    drainSeconds: Number(process.env.JARVIS_DRAIN_SECONDS || 1770),
  };
  deps.sandboxProvider = sandboxProvider(config, namespace, deps.db);
  const web = await startWeb(deps, Number(process.env.PORT || 8080));
  log("web listening", { port: web.port, replica: deps.replica });
  let stopping = false;
  const stop = async (sig: string) => {
    if (stopping) return;
    stopping = true;
    log("draining", { signal: sig, turns: web.app.runner.active });
    web.app.runner.draining = true;
    // let the readiness probe see 503 and the Service drop this endpoint before streams are cut
    setTimeout(() => web.app.closeStreams(), 5000).unref();
    try {
      await web.app.runner.drain(deps.drainSeconds * 1000);
      await web.close();
      await deps.listener.close();
      await deps.db.end();
    } catch (e) {
      logError("shutdown", e);
    }
    process.exit(0);
  };
  process.on("SIGTERM", () => void stop("SIGTERM"));
  process.on("SIGINT", () => void stop("SIGINT"));
}
