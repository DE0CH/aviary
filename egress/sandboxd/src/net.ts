// Host networking for sandboxd (docs/SANDBOX-VM.md, "Networking per sandbox", "Cross-cloud network").
//
// Per sandbox: a network namespace (named like the sandbox) with a veth pair; the sandbox side has <ip>/32, default
// route via 169.254.1.1, answered by proxy ARP on the host side (proxy_delay 0); the host side has the same MAC on
// every host and the sandbox side a MAC derived from its IP, so ARP caches stay valid across a move. MTU = the
// overlay's on every host. One stateless nftables table per sandbox: from the sandbox only <ip> -> egress VIP:3128
// and replies from :7070 to the controller range; to the sandbox only the VIP's replies and the controller's
// connections to :7070. Stateless, because a connection arriving mid-stream on a new host has no conntrack entry.
// The filter runs at prerouting (before the hub's DNAT) and postrouting (after the hub's reverse NAT), so the
// sandbox always sees the VIP. Nothing from a sandbox reaches the host itself (it is dropped at prerouting).
//
// Per host: wg0 (the overlay), the hub's translation of the egress VIP to egress pods (DNAT only, never SNAT: egress
// sees the sandbox's own address and the connection survives a move because the hub, which holds the translation,
// is on every path), and forwarding only between the overlay, the private network and sandboxes.
import { createHash, randomBytes } from "node:crypto";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import type { HostConfig, PeerSpec } from "./config.ts";
import { run, sh } from "./util.ts";

export const GW_MAC = "ee:ee:ee:ee:ee:ee";

/** nft -f with a file (Ubuntu 24.04's nft refuses a pipe on stdin: "Not a regular file") */
export async function nftApply(rules: string): Promise<void> {
  mkdirSync("/run/sbx", { recursive: true });
  const f = `/run/sbx/.nft-${randomBytes(6).toString("hex")}`;
  writeFileSync(f, rules, { mode: 0o600 });
  try {
    await run("nft", ["-f", f], { check: true });
  } finally {
    rmSync(f, { force: true });
  }
}

const h12 = (name: string) => createHash("sha256").update(name).digest("hex").slice(0, 12);
/** host side of the veth (<= 15 chars) */
export const vethOf = (name: string) => `v${h12(name)}`;
export const tableOf = (name: string) => `sbx_${h12(name)}`;
export function macOf(ip: string): string {
  return "02:5b:" + ip.split(".").map((x) => Number(x).toString(16).padStart(2, "0")).join(":");
}

/** The per-sandbox nftables table (pure: tested). */
export function sandboxRules(name: string, ip: string, cfg: Pick<HostConfig, "egressVip" | "controlCidr">): string {
  const v = vethOf(name);
  const t = tableOf(name);
  return `table inet ${t}
delete table inet ${t}
table inet ${t} {
  chain from_sandbox {
    type filter hook prerouting priority -300; policy accept;
    iifname "${v}" ip saddr ${ip} ip daddr ${cfg.egressVip} tcp dport 3128 accept
    iifname "${v}" ip saddr ${ip} ip daddr ${cfg.controlCidr} tcp sport 7070 accept
    iifname "${v}" counter drop
  }
  chain to_sandbox {
    type filter hook postrouting priority 300; policy accept;
    oifname "${v}" ip saddr ${cfg.egressVip} tcp sport 3128 ip daddr ${ip} accept
    oifname "${v}" ip saddr ${cfg.controlCidr} tcp dport 7070 ip daddr ${ip} accept
    oifname "${v}" counter drop
  }
}
`;
}

/** netns + veth + nft for a sandbox; the /32 route only with `route` (a migration target adds it after restore). */
export async function addSandboxNet(cfg: HostConfig, name: string, ip: string, route: boolean): Promise<void> {
  await removeSandboxNet(name, ip);
  const v = vethOf(name);
  await sh("ip", "netns", "add", name);
  await sh("ip", "link", "add", v, "type", "veth", "peer", "name", "eth0", "netns", name);
  await sh("ip", "link", "set", v, "mtu", String(cfg.mtu), "address", GW_MAC, "up");
  await sh("ip", "-n", name, "link", "set", "eth0", "mtu", String(cfg.mtu), "address", macOf(ip), "up");
  await sh("ip", "-n", name, "link", "set", "lo", "up");
  await sh("ip", "-n", name, "addr", "add", `${ip}/32`, "dev", "eth0");
  await sh("ip", "-n", name, "route", "add", "169.254.1.1", "dev", "eth0", "scope", "link");
  await sh("ip", "-n", name, "route", "add", "default", "via", "169.254.1.1", "dev", "eth0");
  await sh("sysctl", "-qw", `net.ipv4.conf.${v}.proxy_arp=1`, `net.ipv4.conf.${v}.rp_filter=0`, `net.ipv4.neigh.${v}.proxy_delay=0`, `net.ipv6.conf.${v}.disable_ipv6=1`);
  await nftApply(sandboxRules(name, ip, cfg));
  if (route) await routeSandbox(name, ip);
}

export async function routeSandbox(name: string, ip: string): Promise<void> {
  await sh("ip", "route", "replace", `${ip}/32`, "dev", vethOf(name));
}

export async function removeSandboxNet(name: string, ip?: string): Promise<void> {
  const v = vethOf(name);
  if (ip) await run("ip", ["route", "del", `${ip}/32`, "dev", v]);
  await run("nft", ["delete", "table", "inet", tableOf(name)]);
  await run("ip", ["link", "del", v]);
  await run("ip", ["netns", "del", name]);
}

// ---- host level ----------------------------------------------------------------------------------------------

async function defaultIface(): Promise<string> {
  const out = await sh("ip", "-o", "route", "show", "default");
  const m = / dev (\S+)/.exec(out);
  if (!m) throw new Error("no default route");
  return m[1]!;
}

async function ifaceFor(addr: string): Promise<string> {
  const out = await sh("ip", "-o", "route", "get", addr);
  const m = / dev (\S+)/.exec(out);
  if (!m) throw new Error(`no route to ${addr}`);
  return m[1]!;
}

/** Host rules: forwarding only between wg0, the private network (hub) and sandbox veths; the hub's egress table. */
export function hostRules(cfg: HostConfig, publicIf: string, privateIf: string | null, egressTargets: string[]): string {
  const nat =
    cfg.role === "hub"
      ? `
  chain egress_dnat {
    type nat hook prerouting priority dstnat; policy accept;
${egressTargets.length ? `    ip daddr ${cfg.egressVip} tcp dport 3128 dnat to numgen random mod ${egressTargets.length} map { ${egressTargets.map((t, i) => `${i} : ${t}`).join(", ")} }` : ""}
  }`
      : "";
  return `table ip sbxhost
delete table ip sbxhost
table ip sbxhost {${nat}
  chain forward {
    type filter hook forward priority 0; policy accept;
    ip daddr ${cfg.egressVip} drop
    iifname "${publicIf}" drop
    oifname "${publicIf}" drop
${privateIf ? "" : `    ip daddr ${cfg.controlCidr} oifname != "wg0" drop\n`}  }
  chain input {
    type filter hook input priority 0; policy accept;
    tcp dport { ${cfg.apiPort}, ${cfg.imagePort} } iifname "${publicIf}" drop
  }
}
`;
}

export class HostNet {
  readonly cfg: HostConfig;
  publicIf = "";
  privateIf: string | null = null;
  egressTargets: string[] = [];
  publicKey = "";

  constructor(cfg: HostConfig) {
    this.cfg = cfg;
  }

  async init(): Promise<void> {
    const c = this.cfg;
    await sh("sysctl", "-qw", "net.ipv4.ip_forward=1", "net.ipv4.conf.all.rp_filter=0", "net.ipv4.conf.default.rp_filter=0", "vm.max_map_count=262144");
    this.publicIf = await defaultIface();
    // wg0: the overlay
    if ((await run("ip", ["link", "show", "wg0"])).code !== 0) await sh("ip", "link", "add", "wg0", "type", "wireguard");
    await sh("wg", "set", "wg0", "private-key", c.wgKeyFile, "listen-port", String(c.wgPort));
    const prefix = c.hostCidr.split("/")[1];
    await run("ip", ["addr", "replace", `${c.wgAddress}/${prefix}`, "dev", "wg0"], { check: true });
    await sh("ip", "link", "set", "wg0", "mtu", String(c.mtu), "up");
    this.publicKey = (await sh("wg", "show", "wg0", "public-key")).trim();
    if (c.role === "hub") {
      this.privateIf = await ifaceFor(c.privateGateway ?? "10.0.0.1");
      // sandboxes elsewhere are behind WireGuard (the peer whose AllowedIPs hold the /32); local ones have /32 routes
      await sh("ip", "route", "replace", c.sandboxCidr, "dev", "wg0");
      if (c.podCidr) await sh("ip", "route", "replace", c.podCidr, "via", c.privateGateway ?? "10.0.0.1", "dev", this.privateIf);
    } else {
      await this.setPeers([c.hub!]);
      for (const dst of [c.controlCidr, `${c.egressVip}/32`]) await sh("ip", "route", "replace", dst, "dev", "wg0");
    }
    await this.applyHostRules();
  }

  async applyHostRules(): Promise<void> {
    await nftApply(hostRules(this.cfg, this.publicIf, this.privateIf, this.egressTargets));
  }

  /** hub: the egress pods' addresses (from the controller); new connections to the VIP go to one of them */
  async setEgressTargets(targets: string[]): Promise<void> {
    const t = [...new Set(targets)].filter((x) => /^\d+\.\d+\.\d+\.\d+$/.test(x)).sort();
    if (t.join() === this.egressTargets.join()) return;
    this.egressTargets = t;
    await this.applyHostRules();
  }

  /** Makes wg0's peers exactly `peers` (burst hosts always keep the hub). */
  async setPeers(peers: PeerSpec[]): Promise<void> {
    if (this.cfg.role === "burst" && !peers.some((p) => p.publicKey === this.cfg.hub!.publicKey)) peers = [this.cfg.hub!, ...peers];
    const dump = (await sh("wg", "show", "wg0", "dump")).trim().split("\n").slice(1);
    const have = new Map(dump.filter(Boolean).map((l) => {
      const f = l.split("\t");
      return [f[0]!, { endpoint: f[2]!, allowed: f[3] === "(none)" ? "" : f[3]! }];
    }));
    const want = new Set(peers.map((p) => p.publicKey));
    for (const k of have.keys()) if (!want.has(k)) await sh("wg", "set", "wg0", "peer", k, "remove");
    for (const p of peers) {
      if (!/^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$/.test(p.publicKey)) throw new Error("bad WireGuard public key");
      const allowed = [...p.allowedIps].sort().join(",");
      const cur = have.get(p.publicKey);
      if (cur && cur.allowed.split(",").sort().join(",") === allowed && (!p.endpoint || cur.endpoint === p.endpoint)) continue;
      const args = ["set", "wg0", "peer", p.publicKey, "allowed-ips", allowed, "persistent-keepalive", "25"];
      if (p.endpoint) args.push("endpoint", p.endpoint);
      await sh("wg", ...args);
    }
  }

  async peers(): Promise<{ publicKey: string; endpoint: string; allowedIps: string[]; latestHandshake: number }[]> {
    const dump = (await sh("wg", "show", "wg0", "dump")).trim().split("\n").slice(1);
    return dump.filter(Boolean).map((l) => {
      const f = l.split("\t");
      return { publicKey: f[0]!, endpoint: f[2]!, allowedIps: f[3] === "(none)" ? [] : f[3]!.split(","), latestHandshake: Number(f[4]) };
    });
  }
}
