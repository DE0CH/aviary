// sandboxd: the host agent of sandbox provider "vm" (docs/SANDBOX-VM.md, "sandboxd"). One per host (the Hetzner hub
// and every AWS burst host). The controller (the web role) calls it over HTTPS with mutual TLS on the host's overlay
// address; nothing else may (the client certificate must be the controller's, CN sandbox-controller).
//
//   GET    /v1/host                        host facts: capacity, CPU features, images, WireGuard key/peers, sandboxes
//   PUT    /v1/peers {peers}               wg0's peers, exactly these (hub: AWS hosts and the /32s they hold)
//   PUT    /v1/egress {targets}            hub: egress pod addresses behind the egress VIP
//   POST   /v1/images {ref?, digest?}      make sure an image is unpacked here; returns its digest
//   GET    /v1/sandboxes                   the sandboxes here
//   POST   /v1/sandboxes {spec}            create and start one (idempotent for the same name and ip)
//   DELETE /v1/sandboxes/{name}            remove it (a missing one is fine)
//   POST   /v1/sandboxes/{name}/receive {spec}   migration target: prepare and listen; returns {port, address}
//   GET    /v1/sandboxes/{name}/receive?wait=ms  migration target: the receive's result
//   GET    /v1/sandboxes/{name}/spec       its spec (a migration target needs the same)
//   POST   /v1/sandboxes/{name}/send {to, downtimeMs?, maxRounds?, maxDowntimeMs?}   migration source: live-copy it
//                                          to `to`; 409 = not moved, still running here (incl. "won't converge")
//
// The hub also serves unpacked images to burst hosts (plain HTTP on its overlay address, overlay hosts only).
import { readFileSync } from "node:fs";
import { createServer as createHttpServer, type IncomingMessage, type ServerResponse } from "node:http";
import { createServer } from "node:https";
import type { TLSSocket } from "node:tls";
import { cpus } from "node:os";
import { loadHostConfig, validateSpec, type HostConfig, type PeerSpec } from "./config.ts";
import { ensureImage, haveImage, imageTarStream, listImages } from "./images.ts";
import { HostNet } from "./net.ts";
import { runsc } from "./runsc.ts";
import { HttpError, Sandboxes } from "./sandboxes.ts";
import { inCidr, log, logError } from "./util.ts";

const CONTROLLER_CN = "sandbox-controller";

function memInfo(): { total: number; available: number } {
  const m = readFileSync("/proc/meminfo", "utf8");
  const kb = (k: string) => Number(new RegExp(`^${k}:\\s+(\\d+)`, "m").exec(m)?.[1] ?? 0) * 1024;
  return { total: kb("MemTotal"), available: kb("MemAvailable") };
}

async function readBody(req: IncomingMessage, max = 1 << 20): Promise<any> {
  let size = 0;
  const chunks: Buffer[] = [];
  for await (const c of req) {
    size += (c as Buffer).length;
    if (size > max) throw new HttpError(413, "body too large");
    chunks.push(c as Buffer);
  }
  const s = Buffer.concat(chunks).toString("utf8");
  if (!s) return {};
  try {
    return JSON.parse(s);
  } catch {
    throw new HttpError(400, "body is not JSON");
  }
}

function json(res: ServerResponse, status: number, body: unknown): void {
  const s = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(s) });
  res.end(s);
}

export async function startSandboxd(cfg: HostConfig): Promise<void> {
  const net = new HostNet(cfg);
  await net.init();
  const sbx = new Sandboxes(cfg);
  await sbx.init();
  await sbx.sweep();
  const features = (await runsc(["cpu-features"])).stdout.trim();
  const version = (await runsc(["--version"])).stdout.split("\n")[0]?.trim() ?? "";
  const release = (() => {
    try {
      return readFileSync("/usr/local/bin/gvisor-bin/.release", "utf8").trim();
    } catch {
      return "";
    }
  })();
  const bootId = readFileSync("/proc/sys/kernel/random/boot_id", "utf8").trim();
  const startedAt = new Date().toISOString();

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const cert = (req.socket as TLSSocket).getPeerCertificate?.();
    if (!cert || cert.subject?.CN !== CONTROLLER_CN) return json(res, 403, { error: "not the controller" });
    const url = new URL(req.url ?? "/", "https://x");
    const p = url.pathname.split("/").filter(Boolean);
    const m = req.method ?? "GET";
    if (m === "GET" && url.pathname === "/v1/host") {
      const mem = memInfo();
      return json(res, 200, {
        name: cfg.name,
        role: cfg.role,
        wgAddress: cfg.wgAddress,
        wgPublicKey: net.publicKey,
        peers: await net.peers(),
        egressTargets: net.egressTargets,
        memTotal: mem.total,
        memAvailable: mem.available,
        reserveMemory: cfg.reserveMemory,
        cpus: cpus().length,
        cpuFeatures: features,
        runsc: version,
        gvisorRelease: release,
        images: listImages(),
        bootId,
        startedAt,
        sandboxes: sbx.list(),
      });
    }
    if (m === "PUT" && url.pathname === "/v1/peers") {
      const b = await readBody(req);
      if (!Array.isArray(b.peers)) throw new HttpError(400, "peers must be a list");
      const peers: PeerSpec[] = b.peers.map((x: any) => ({
        publicKey: String(x.publicKey),
        endpoint: x.endpoint ? String(x.endpoint) : undefined,
        allowedIps: (x.allowedIps ?? []).map(String).filter((a: string) => /^\d+\.\d+\.\d+\.\d+\/\d+$/.test(a)),
      }));
      await net.setPeers(peers);
      return json(res, 200, { peers: await net.peers() });
    }
    if (m === "PUT" && url.pathname === "/v1/egress") {
      if (cfg.role !== "hub") throw new HttpError(400, "only the hub translates the egress address");
      const b = await readBody(req);
      await net.setEgressTargets((b.targets ?? []).map(String));
      return json(res, 200, { targets: net.egressTargets });
    }
    if (m === "POST" && url.pathname === "/v1/images") {
      const b = await readBody(req);
      const digest = await ensureImage(cfg, b.digest ? String(b.digest) : null, b.ref ? String(b.ref) : undefined);
      return json(res, 200, { digest });
    }
    if (p[0] === "v1" && p[1] === "sandboxes") {
      if (p.length === 2 && m === "GET") return json(res, 200, { sandboxes: sbx.list() });
      if (p.length === 2 && m === "POST") {
        const spec = validateSpec(await readBody(req), cfg);
        return json(res, 200, await sbx.create(spec));
      }
      const name = p[2] ?? "";
      if (!/^sbx-[a-z0-9]{1,40}$/.test(name)) throw new HttpError(400, "bad sandbox name");
      if (p.length === 3 && m === "DELETE") {
        await sbx.delete(name);
        return json(res, 200, { deleted: name });
      }
      if (p.length === 4 && p[3] === "spec" && m === "GET") {
        const e = sbx.get(name);
        if (!e) throw new HttpError(404, `${name} is not here`);
        return json(res, 200, e.spec);
      }
      if (p.length === 4 && p[3] === "receive" && m === "POST") {
        const spec = validateSpec(await readBody(req), cfg);
        if (spec.name !== name) throw new HttpError(400, "name mismatch");
        return json(res, 200, await sbx.receive(spec));
      }
      if (p.length === 4 && p[3] === "receive" && m === "GET") {
        const wait = Math.min(Number(url.searchParams.get("wait") ?? 0) || 0, 120_000);
        return json(res, 200, await sbx.receiveResult(name, wait));
      }
      if (p.length === 4 && p[3] === "send" && m === "POST") {
        const b = await readBody(req);
        const to = String(b.to ?? "");
        const [host, port] = to.split(":");
        if (!host || !inCidr(host, cfg.hostCidr) || !/^\d+$/.test(port ?? "")) throw new HttpError(400, "to must be <overlay host address>:<port>");
        const downtime = Math.max(10, Math.min(Number(b.downtimeMs ?? 100), 5000));
        const rounds = Math.max(1, Math.min(Number(b.maxRounds ?? 20), 50));
        const maxDowntime = Math.max(0, Math.min(Number(b.maxDowntimeMs ?? 0), 60_000));
        return json(res, 200, await sbx.send(name, to, downtime, rounds, maxDowntime));
      }
    }
    return json(res, 404, { error: "not found" });
  }

  const server = createServer(
    {
      cert: readFileSync(cfg.tls.cert),
      key: readFileSync(cfg.tls.key),
      ca: readFileSync(cfg.tls.ca),
      requestCert: true,
      rejectUnauthorized: true,
      minVersion: "TLSv1.3",
    },
    (req, res) => {
      handle(req, res).catch((e) => {
        const status = e instanceof HttpError ? e.status : 500;
        if (status >= 500) logError("request", e, { method: req.method, url: req.url });
        if (!res.headersSent) json(res, status, { error: e instanceof Error ? e.message : String(e) });
        else res.destroy();
      });
    },
  );
  server.requestTimeout = 20 * 60_000;
  server.headersTimeout = 30_000;
  await new Promise<void>((r) => server.listen(cfg.apiPort, cfg.wgAddress, r));
  log("sandboxd listening", { name: cfg.name, role: cfg.role, address: `${cfg.wgAddress}:${cfg.apiPort}`, runsc: version, release, sandboxes: sbx.list().length });

  if (cfg.role === "hub") {
    const img = createHttpServer((req, res) => {
      const from = req.socket.remoteAddress?.replace(/^::ffff:/, "") ?? "";
      const mm = /^\/images\/([a-f0-9]{64})\.tar\.zst$/.exec(req.url ?? "");
      if (!inCidr(from, cfg.hostCidr)) return void res.writeHead(403).end();
      if (req.method !== "GET" || !mm || !haveImage(mm[1]!)) return void res.writeHead(404).end();
      res.writeHead(200, { "content-type": "application/zstd" });
      const t = imageTarStream(mm[1]!);
      t.stdout.pipe(res);
      req.on("close", () => t.kill());
    });
    await new Promise<void>((r) => img.listen(cfg.imagePort, cfg.wgAddress, r));
  }

  setInterval(() => void sbx.sweep().catch((e) => logError("sweep", e)), 5 * 60_000).unref();
  const stop = () => {
    // sandboxes keep running (KillMode=process); the next start finds them in their state files
    log("sandboxd stopping");
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}

if (process.argv[1]?.endsWith("sandboxd/src/main.ts")) {
  startSandboxd(loadHostConfig()).catch((e) => {
    logError("sandboxd failed to start", e);
    process.exit(1);
  });
}
