// sandboxd's host configuration (/etc/sbx/host.json, mode 600) and the sandbox spec the controller sends.
// docs/SANDBOX-VM.md, "sandboxd". The hub (the always-on Hetzner box) and burst hosts (AWS) run the same program;
// `role` switches the hub's extra jobs on: WireGuard hub, egress address translation, image source for burst hosts.
import { readFileSync } from "node:fs";
import { inCidr } from "./util.ts";

export interface PeerSpec {
  publicKey: string;
  /** host:port; omitted for peers that connect to us (the hub learns AWS hosts' endpoints from their handshakes) */
  endpoint?: string;
  allowedIps: string[];
}

export interface HostConfig {
  name: string;
  role: "hub" | "burst";
  /** this host's overlay address (wg0), e.g. 10.201.0.1 for the hub; the API and migrations listen there */
  wgAddress: string;
  /** the overlay's host range (wg0's prefix), e.g. 10.201.0.0/24 */
  hostCidr: string;
  wgPort: number;
  /** file with the WireGuard private key (created at setup) */
  wgKeyFile: string;
  apiPort: number;
  /** hub only: plain-HTTP image source for burst hosts, bound to wgAddress, answering only hostCidr */
  imagePort: number;
  tls: { cert: string; key: string; ca: string };
  /** where the controller (the web pods, masqueraded to node addresses) comes from: may reach sandboxes on :7070 */
  controlCidr: string;
  /** every sandbox's /32 is in here */
  sandboxCidr: string;
  /** the egress proxy's address as sandboxes see it (the hub translates it to an egress pod) */
  egressVip: string;
  /** hub: the cluster's pod range, routed through the private network (Hetzner network routes per node) */
  podCidr?: string;
  /** hub: the private network's gateway (10.0.0.1 on Hetzner) */
  privateGateway?: string;
  /** burst: the hub as a WireGuard peer */
  hub?: PeerSpec;
  /** bytes kept for the host itself (not offered to sandboxes) */
  reserveMemory: number;
  /** hub: DOCKER_CONFIG directory with the registry login for image pulls */
  registryConfigDir?: string;
  /** hub: the URL burst hosts fetch images from (default http://<hub wgAddress>:<imagePort>) */
  imageSource?: string;
  /** MTU of sandbox veths and wg0 */
  mtu: number;
}

export function loadHostConfig(path = process.env.SBX_CONFIG || "/etc/sbx/host.json"): HostConfig {
  const c = JSON.parse(readFileSync(path, "utf8"));
  const cfg: HostConfig = {
    wgPort: 51820,
    wgKeyFile: "/etc/sbx/wg.key",
    apiPort: 7443,
    imagePort: 7444,
    hostCidr: "10.201.0.0/24",
    controlCidr: "10.0.0.0/16",
    sandboxCidr: "10.200.0.0/16",
    egressVip: "10.201.1.1",
    reserveMemory: 1536 * 1024 * 1024,
    mtu: 1420,
    ...c,
  };
  for (const k of ["name", "role", "wgAddress"] as const) if (!cfg[k]) throw new Error(`host.json: ${k} is required`);
  if (cfg.role !== "hub" && cfg.role !== "burst") throw new Error("host.json: role must be hub or burst");
  if (cfg.role === "burst" && !cfg.hub) throw new Error("host.json: a burst host needs `hub`");
  if (!inCidr(cfg.wgAddress, cfg.hostCidr)) throw new Error("host.json: wgAddress must be in hostCidr");
  return cfg;
}

export interface SandboxSpec {
  /** sbx-<hex>: the runsc container id, netns name and the controller's name for it */
  name: string;
  /** its /32 in sandboxCidr; never changes, wherever the sandbox runs */
  ip: string;
  /** image digest (hex, without sha256:) */
  image: string;
  /** hub: the reference to pull the digest from when it isn't cached */
  imageRef?: string;
  env: Record<string, string>;
  /** CPU cores (cgroup quota) */
  cpu: number;
  /** memory limit, bytes */
  memory: number;
  /** comma-separated CPU features every sandbox of the pool sees (empty: the host's own; can't then move) */
  cpuFeatures: string;
}

const NAME = /^sbx-[a-z0-9]{1,40}$/;
const ENVKEY = /^[A-Za-z_][A-Za-z0-9_]*$/;

export function validateSpec(s: unknown, cfg: Pick<HostConfig, "sandboxCidr">): SandboxSpec {
  const o = (s ?? {}) as Record<string, any>;
  if (typeof o.name !== "string" || !NAME.test(o.name)) throw new Error("spec: bad name");
  if (typeof o.ip !== "string" || !inCidr(o.ip, cfg.sandboxCidr)) throw new Error("spec: ip must be in the sandbox range");
  if (typeof o.image !== "string" || !/^[a-f0-9]{64}$/.test(o.image)) throw new Error("spec: image must be a sha256 digest (hex)");
  if (o.imageRef !== undefined && (typeof o.imageRef !== "string" || !/^[a-z0-9./:@_-]+$/i.test(o.imageRef))) throw new Error("spec: bad imageRef");
  const env = o.env ?? {};
  if (typeof env !== "object" || Array.isArray(env)) throw new Error("spec: env must be an object");
  for (const [k, v] of Object.entries(env)) {
    if (!ENVKEY.test(k) || typeof v !== "string" || v.includes("\0")) throw new Error(`spec: bad env ${k}`);
  }
  const cpu = Number(o.cpu);
  const memory = Number(o.memory);
  if (!(cpu > 0 && cpu <= 64)) throw new Error("spec: cpu must be in (0, 64]");
  if (!(Number.isInteger(memory) && memory >= 128 * 1024 * 1024)) throw new Error("spec: memory must be bytes >= 128 MiB");
  const cpuFeatures = String(o.cpuFeatures ?? "");
  if (!/^[a-z0-9_.,-]*$/.test(cpuFeatures)) throw new Error("spec: bad cpuFeatures");
  return { name: o.name, ip: o.ip, image: o.image, imageRef: o.imageRef, env: env as Record<string, string>, cpu, memory, cpuFeatures };
}
