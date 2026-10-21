// Sandbox provider "vm" (docs/SANDBOX-VM.md): gVisor sandboxes on our own hosts, the always-on Hetzner hub plus AWS
// EC2 burst hosts, with pre-copy live migration between them. The controller (placement, migrations) and the scaler
// (AWS hosts up and down, drain-back, heartbeats, the hub's WireGuard peers and egress targets) run in the web role;
// the scaler behind a Postgres leader lock like the janitor. Hosts run sandboxd (chat/sandboxd), which this calls
// over HTTPS with mutual TLS on the WireGuard overlay.
//
// - Placement: the hub while it has room, then the fullest AWS host with room; none: the scaler starts an AWS host
//   (at most aws.maxHosts in steady state; one more only while a `retiring` host is being replaced). An AWS host is
//   also started as soon as someone types in a conversation without a sandbox while no host has a free slot (kv
//   `sandbox_demand:<email>`, from POST /api/typing), so it is up by the time a bash command asks for a sandbox.
// - Each sandbox gets a fixed /32 from sandboxCidr (sandboxes.ip) that follows it; sandboxes.host is where it runs.
// - Packing (packing.ts): each pass plans a placement on the fewest hosts and, when it uses strictly fewer than now,
//   live-migrates one sandbox toward it (during a turn only with migration.duringTurns). A move across clouds also needs the sandbox
//   to be idle (CPU) and is aborted, sandbox untouched, if pre-copy wouldn't converge within migration.maxDowntimeMs
//   (the gVisor fork's --migrate-max-downtime). A `retiring` host (the operator's) has its sandboxes moved off.
// - An AWS host is terminated as soon as no sandbox is on it and it is older than vm.minHostLifetimeSeconds (default
//   15 min, from launch); nothing else keeps or ends it.
// - A host that stops answering for 30 s (the hub: 120 s) loses its sandboxes: their rows go `gone` ("host lost"),
//   the next turn makes a new sandbox and gets the reset note. AWS hosts are then terminated.
import { randomBytes } from "node:crypto";
import type pg from "pg";
import type { ChatConfig, VmConfig } from "../common/config.ts";
import { lead, tx, type Db } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import { AgentClient } from "./agent.ts";
import type { AgentEndpoint, ListedSandbox, SandboxProvider } from "./provider.ts";
import { sandboxEnv } from "./sandbox-k8s.ts";
import { planPacking } from "./packing.ts";
import { DEMAND_WINDOW_S, PoolFullError, type SandboxRow } from "./sandbox.ts";
import { SandboxdError, wgKeyPair, type Ec2Api, type HostInfo, type HostSandbox, type SandboxdApi } from "./vm-clients.ts";

const PLACE_LOCK = 4_242_010;
const SBX_LOCK = 4_242_011;
export const SCALER_LOCK = 4_242_012;
const HOST_DEAD_MS = 30_000;
const HUB_DEAD_MS = 120_000;
const BOOT_TIMEOUT_MS = 8 * 60_000;
/** an EC2 instance tagged for us that the database doesn't know is terminated after this long */
const ORPHAN_INSTANCE_MS = 10 * 60_000;
const PLACE_WAIT_MS = 170_000;

export interface HostRow {
  name: string;
  kind: "hub" | "aws";
  url: string;
  wg_ip: string;
  wg_public_key: string | null;
  instance_id: string | null;
  private_ip: string | null;
  status: "booting" | "active" | "draining" | "retiring" | "gone";
  max_sandboxes: number | null;
  mem_total: string | number | null;
  mem_reserve: string | number | null;
  cpus: number | null;
  info: {
    sandboxes?: HostSandbox[];
    cpuFeatures?: string;
    images?: string[];
    bootId?: string;
    gvisorRelease?: string;
    memAvailable?: number;
    loadAvg?: number[] | null;
    uptimeS?: number | null;
    peers?: number;
    egressTargets?: string[];
  } | null;
  created_at: Date;
  last_seen_at: Date | null;
  empty_since: Date | null;
}

/** Kubernetes-style quantity: "2Gi", "1536Mi", "1G", "512M", bytes */
export function parseBytes(s: string): number {
  const m = /^(\d+(?:\.\d+)?)(Ki|Mi|Gi|Ti|K|M|G|T)?$/.exec(String(s).trim());
  if (!m) throw new Error(`bad memory quantity ${s}`);
  const mul: Record<string, number> = { "": 1, Ki: 1024, Mi: 1024 ** 2, Gi: 1024 ** 3, Ti: 1024 ** 4, K: 1e3, M: 1e6, G: 1e9, T: 1e12 };
  return Math.round(Number(m[1]) * mul[m[2] ?? ""]!);
}
/** "1", "0.5", "500m" */
export function parseCpu(s: string): number {
  const t = String(s).trim();
  return t.endsWith("m") ? Number(t.slice(0, -1)) / 1000 : Number(t);
}

function ipInt(ip: string): number {
  return ip.split(".").reduce((a, x) => a * 256 + Number(x), 0);
}
const intIp = (n: number) => [n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255].join(".");

/** The lowest free address in `cidr` from host part `first` on (skipping .0 and .255 of every /24). */
export function freeAddress(cidr: string, used: Set<string>, first = 10, last?: number): string | null {
  const [base, bits] = cidr.split("/");
  const size = 2 ** (32 - Number(bits));
  const start = ipInt(base!);
  for (let i = first; i < Math.min(size - 1, last ?? size - 1); i++) {
    const ip = intIp(start + i);
    const low = (start + i) & 255;
    if (low === 0 || low === 255) continue;
    if (!used.has(ip)) return ip;
  }
  return null;
}

export interface VmDeps {
  db: Db;
  config: ChatConfig;
  sandboxd: SandboxdApi;
  ec2: Ec2Api | null;
  /** PEMs handed to new AWS hosts (their server certificate, shared, and the CA that signs the controller) */
  hostTls: { ca: string; cert: string; key: string } | null;
}

interface Slot {
  host: HostRow;
  used: number;
  free: number;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export class VmController {
  readonly #d: VmDeps;
  readonly #digests = new Map<string, string>();
  /** the scaler's last view of each host (sandboxes with CPU use), by name */
  readonly #lastInfo = new Map<string, HostInfo>();

  constructor(d: VmDeps) {
    this.#d = d;
  }

  /** Records what the scaler/controller did (vm_events: the admin monitor's "recent decisions" and migrations). */
  async event(kind: string, host: string | null, sandboxId: string | null, data: Record<string, unknown> = {}): Promise<void> {
    await this.#d.db
      .query("insert into vm_events (kind, host, sandbox_id, data) values ($1, $2, $3, $4)", [kind, host, sandboxId, JSON.stringify(data)])
      .catch((e) => logError("vm: event", e));
  }

  get vm(): VmConfig {
    return this.#d.config.sandbox.vm!;
  }
  get memBytes(): number {
    return parseBytes(this.#d.config.sandbox.memory);
  }
  get cpuCores(): number {
    return parseCpu(this.#d.config.sandbox.cpu);
  }

  /** How many sandboxes a host can hold (memory by the sandbox limit, CPU overcommitted, the operator's cap). */
  slots(h: HostRow): number {
    const mem = Number(h.mem_total ?? 0) - Number(h.mem_reserve ?? 0);
    const byMem = Math.floor(mem / this.memBytes);
    const byCpu = Math.floor(((h.cpus ?? 0) * this.vm.cpuOvercommit) / this.cpuCores);
    return Math.max(0, Math.min(byMem, byCpu, h.max_sandboxes ?? Infinity));
  }

  #fresh(h: HostRow): boolean {
    return !!h.last_seen_at && Date.now() - new Date(h.last_seen_at).getTime() < HOST_DEAD_MS;
  }

  async ensureHub(): Promise<void> {
    const wg = new URL(this.vm.hub.url).hostname;
    await this.#d.db.query(
      `insert into vm_hosts (name, kind, url, wg_ip, status) values ($1, 'hub', $2, $3, 'active')
       on conflict (name) do update set url = excluded.url, wg_ip = excluded.wg_ip`,
      [this.vm.hub.name, this.vm.hub.url, wg],
    );
  }

  async hosts(c: Db | pg.PoolClient = this.#d.db): Promise<HostRow[]> {
    const { rows } = await c.query<HostRow>("select * from vm_hosts where status <> 'gone' order by kind desc, created_at");
    return rows;
  }

  /**
   * Slots a host's sandboxes take: one each, more for one made with a bigger memory limit than the configured one
   * (sandboxes keep the size they started with until released, so a smaller `sandbox.memory` never overcommits).
   */
  units(h: HostRow, rowsOnHost: number): number {
    const extra = (h.info?.sandboxes ?? []).reduce((a, s) => a + Math.max(0, Math.ceil(s.memory / this.memBytes) - 1), 0);
    return rowsOnHost + extra;
  }

  /** The host is short of memory: nothing new goes there (MemAvailable below sandbox.vm.minAvailableMemory). */
  underPressure(h: HostRow): boolean {
    const avail = h.info?.memAvailable;
    return typeof avail === "number" && avail < this.vm.minAvailableMemory;
  }

  /** Placeable hosts (active, answering) with their use. */
  async #slots(c: Db | pg.PoolClient): Promise<Slot[]> {
    const hosts = (await this.hosts(c)).filter((h) => h.status === "active" && this.#fresh(h));
    const { rows } = await c.query<{ host: string; n: string }>("select host, count(*) as n from sandboxes where status <> 'gone' and host is not null group by host");
    const used = new Map(rows.map((r) => [r.host, Number(r.n)]));
    return hosts.map((h) => {
      const u = this.units(h, used.get(h.name) ?? 0);
      return { host: h, used: u, free: this.underPressure(h) ? 0 : Math.max(0, this.slots(h) - u) };
    });
  }

  /** People who may need a new sandbox soon (typing, a turn started without one) within DEMAND_WINDOW_S. */
  async demand(): Promise<number> {
    const { rows } = await this.#d.db.query<{ n: string }>(
      "select count(*) as n from kv where key like 'sandbox_demand:%' and (value->>'at')::timestamptz > now() - make_interval(secs => $1)",
      [DEMAND_WINDOW_S],
    );
    return Number(rows[0]?.n ?? 0);
  }

  /** The hub while it has room, else the fullest AWS host with room (`need`: slots the sandbox takes). */
  static pick(slots: Slot[], exclude?: string, need = 1): Slot | null {
    const c = slots.filter((s) => s.free >= need && s.host.name !== exclude);
    return c.find((s) => s.host.kind === "hub") ?? c.filter((s) => s.host.kind === "aws").sort((a, b) => a.free - b.free || a.host.name.localeCompare(b.host.name))[0] ?? null;
  }

  /** Places a `starting` sandbox: a host and an address, recorded in its row. Null when no host has room. */
  async place(row: SandboxRow): Promise<{ host: HostRow; ip: string } | null> {
    return tx(this.#d.db, async (c) => {
      await c.query("select pg_advisory_xact_lock($1)", [PLACE_LOCK]);
      const s = VmController.pick(await this.#slots(c));
      if (!s) return null;
      const { rows } = await c.query<{ ip: string }>("select ip from sandboxes where status <> 'gone' and ip is not null");
      const ip = freeAddress(this.vm.sandboxCidr, new Set(rows.map((r) => r.ip)));
      if (!ip) throw new Error("no free sandbox address");
      await c.query("update sandboxes set host = $2, ip = $3 where id = $1", [row.id, s.host.name, ip]);
      return { host: s.host, ip };
    });
  }

  /** The configured image's digest (the hub resolves and pulls it). */
  async imageDigest(): Promise<string> {
    const ref = this.#d.config.sandbox.image;
    const pinned = /@sha256:([a-f0-9]{64})$/.exec(ref)?.[1];
    if (pinned) return pinned;
    let d = this.#digests.get(ref);
    if (!d) {
      const r = await this.#d.sandboxd.call<{ digest: string }>(this.vm.hub.url, "POST", "/v1/images", { ref }, 15 * 60_000);
      d = r.digest;
      this.#digests.set(ref, d);
    }
    return d;
  }

  async withSandboxLock<T>(id: string, fn: (c: pg.PoolClient) => Promise<T>): Promise<T> {
    return tx(this.#d.db, async (c) => {
      await c.query("select pg_advisory_xact_lock($1, hashtext($2))", [SBX_LOCK, id]);
      return fn(c);
    });
  }

  // ---- AWS hosts -----------------------------------------------------------------------------------------------

  /**
   * Starts an AWS host unless one is booting already or the cap is reached. `transient`: one more than maxHosts is
   * allowed while a `retiring` host is replaced. Returns what happened.
   */
  async launch(reason: string, transient = false): Promise<"launched" | "booting" | "full" | "off"> {
    const aws = this.vm.aws;
    if (!aws || !this.#d.ec2 || !this.#d.hostTls) return "off";
    return tx(this.#d.db, async (c) => {
      await c.query("select pg_advisory_xact_lock($1)", [PLACE_LOCK + 100]);
      const hosts = (await this.hosts(c)).filter((h) => h.kind === "aws");
      if (hosts.some((h) => h.status === "booting")) return "booting";
      const retiring = hosts.filter((h) => h.status === "retiring").length;
      const cap = transient && retiring ? aws.maxHosts + 1 : aws.maxHosts;
      const count = transient ? hosts.length : hosts.length - retiring;
      if (count >= cap) return "full";
      const hub = (await c.query<HostRow>("select * from vm_hosts where name = $1", [this.vm.hub.name])).rows[0];
      if (!hub?.wg_public_key) throw new Error("the hub's WireGuard key is not known yet");
      const { rows: ips } = await c.query<{ wg_ip: string }>("select wg_ip from vm_hosts where status <> 'gone'");
      const wgIp = freeAddress(this.vm.hostCidr, new Set(ips.map((r) => r.wg_ip)), 10, 250);
      if (!wgIp) throw new Error("no free host address");
      const keys = wgKeyPair();
      const name = `aws-${randomBytes(3).toString("hex")}`;
      const url = `https://${wgIp}:7443`;
      await c.query("insert into vm_hosts (name, kind, url, wg_ip, wg_public_key, status) values ($1, 'aws', $2, $3, $4, 'booting')", [name, url, wgIp, keys.publicKey]);
      const userData = JSON.stringify({
        host: {
          name,
          role: "burst",
          wgAddress: wgIp,
          hostCidr: this.vm.hostCidr,
          controlCidr: this.vm.controlCidr,
          sandboxCidr: this.vm.sandboxCidr,
          egressVip: this.vm.egressVip,
          reserveMemory: 1024 * 1024 * 1024,
          imageSource: `http://${new URL(this.vm.hub.url).hostname}:7444`,
          hub: { publicKey: hub.wg_public_key, endpoint: this.vm.hub.endpoint, allowedIps: [this.vm.controlCidr, `${hub.wg_ip}/32`, `${this.vm.egressVip}/32`] },
        },
        wgPrivateKey: keys.privateKey,
        tls: this.#d.hostTls,
      });
      try {
        const inst = await this.#d.ec2!.runInstance({
          launchTemplate: aws.launchTemplate,
          userData,
          tags: { app: "yyc-ai-sandbox", jarvis: aws.instance, Name: `yyc-ai-sandbox-${name}` },
        });
        await c.query("update vm_hosts set instance_id = $2, private_ip = $3 where name = $1", [name, inst.id, inst.privateIp || null]);
        log("vm: AWS host launched", { host: name, instance: inst.id, reason });
        void this.event("launch", name, null, { instance: inst.id, reason, transient });
        return "launched";
      } catch (e) {
        await c.query("update vm_hosts set status = 'gone', gone_at = now(), gone_reason = $2 where name = $1", [name, `launch failed: ${(e as Error).message}`.slice(0, 500)]);
        logError("vm: AWS launch failed", e, { host: name });
        void this.event("launch-failed", name, null, { reason, error: (e as Error).message.slice(0, 300) });
        return "full";
      }
    });
  }

  async #terminate(h: HostRow, reason: string): Promise<void> {
    if (h.kind !== "aws") return;
    if (h.instance_id && this.#d.ec2) await this.#d.ec2.terminate([h.instance_id]);
    await this.#d.db.query("update vm_hosts set status = 'gone', gone_at = now(), gone_reason = $2 where name = $1 and status <> 'gone'", [h.name, reason]);
    log("vm: host gone", { host: h.name, instance: h.instance_id, reason });
    await this.event("terminate", h.name, null, { instance: h.instance_id, reason, upMs: Date.now() - new Date(h.created_at).getTime() });
  }

  async #hostLost(h: HostRow, reason: string): Promise<void> {
    const { rows } = await this.#d.db.query<{ id: string }>(
      "update sandboxes set status = 'gone', released_at = now(), release_reason = 'host lost' where host = $1 and status <> 'gone' returning id",
      [h.name],
    );
    logError("vm: host lost", reason, { host: h.name, sandboxes: rows.length });
    await this.event("host-lost", h.name, null, { reason, sandboxes: rows.length });
    if (h.kind === "aws") await this.#terminate(h, `lost: ${reason}`);
  }

  // ---- network ---------------------------------------------------------------------------------------------------

  /** The hub's WireGuard peers (AWS hosts with the /32s they hold) and each AWS host's mesh peers. */
  async pushPeers(): Promise<void> {
    const hosts = await this.hosts();
    const aws = hosts.filter((h) => h.kind === "aws" && h.wg_public_key);
    const { rows } = await this.#d.db.query<{ host: string; ip: string }>("select host, ip from sandboxes where status <> 'gone' and ip is not null and host is not null");
    const byHost = new Map<string, string[]>();
    for (const r of rows) byHost.set(r.host, [...(byHost.get(r.host) ?? []), `${r.ip}/32`]);
    const hubPeers = aws.map((h) => ({ publicKey: h.wg_public_key!, allowedIps: [`${h.wg_ip}/32`, ...(byHost.get(h.name) ?? [])] }));
    await this.#d.sandboxd.call(this.vm.hub.url, "PUT", "/v1/peers", { peers: hubPeers }, 10_000);
    // AWS hosts reach each other directly (VPC addresses) for migrations; the hub peer they keep themselves
    for (const h of aws.filter((x) => x.status !== "booting" && this.#fresh(x))) {
      const others = aws.filter((o) => o.name !== h.name && o.private_ip).map((o) => ({ publicKey: o.wg_public_key!, endpoint: `${o.private_ip}:51820`, allowedIps: [`${o.wg_ip}/32`] }));
      await this.#d.sandboxd.call(h.url, "PUT", "/v1/peers", { peers: others }, 10_000).catch((e) => logError("vm: peers", e, { host: h.name }));
    }
  }

  /** The egress pods' addresses (each egress pod announces itself in kv) for the hub's VIP translation. */
  async pushEgress(): Promise<void> {
    const { rows } = await this.#d.db.query<{ value: { ip: string; at: string } }>("select value from kv where key like 'egress_pod:%'");
    const targets = rows.filter((r) => Date.now() - Date.parse(r.value.at) < 30_000).map((r) => r.value.ip);
    if (!targets.length) return; // keep the last known ones rather than none
    await this.#d.sandboxd.call(this.vm.hub.url, "PUT", "/v1/egress", { targets }, 10_000);
  }

  // ---- migration -------------------------------------------------------------------------------------------------

  /**
   * Moves a sandbox to another host while it runs. False (and nothing changed) when it can't now: its row changed,
   * or the copy failed or would not converge before the switch (it keeps running where it was).
   */
  async migrate(sandboxId: string, to: HostRow, reason: string): Promise<boolean> {
    return this.withSandboxLock(sandboxId, async (c) => {
      const { rows } = await c.query<{ pod: string; ip: string; host: string; status: string }>("select pod, ip, host, status from sandboxes where id = $1", [sandboxId]);
      const s = rows[0];
      if (!s || s.status !== "ready" || !s.host || s.host === to.name) return false;
      const from = (await c.query<HostRow>("select * from vm_hosts where name = $1", [s.host])).rows[0];
      if (!from) return false;
      const sd = this.#d.sandboxd;
      const crossCloud = from.kind !== to.kind;
      const t0 = Date.now();
      const spec = await sd.call(from.url, "GET", `/v1/sandboxes/${s.pod}/spec`);
      const rx = await sd.call<{ port: number; address: string }>(to.url, "POST", `/v1/sandboxes/${s.pod}/receive`, spec, 15 * 60_000);
      let sendError: unknown = null;
      try {
        await sd.call(from.url, "POST", `/v1/sandboxes/${s.pod}/send`, {
          to: `${rx.address}:${rx.port}`,
          downtimeMs: this.vm.migration.downtimeMs,
          maxRounds: this.vm.migration.maxRounds,
          maxDowntimeMs: crossCloud ? this.vm.migration.maxDowntimeMs : 0,
        }, 15 * 60_000);
      } catch (e) {
        sendError = e;
        if (e instanceof SandboxdError && e.status === 409) {
          // not moved: it still runs on the source
          await sd.call(to.url, "DELETE", `/v1/sandboxes/${s.pod}`).catch(() => {});
          log("vm: migration not done, sandbox unchanged", { sandbox: sandboxId, from: from.name, to: to.name, error: e.message });
          void this.event("migration-skipped", from.name, sandboxId, { to: to.name, reason, error: e.message.slice(0, 300), ms: Date.now() - t0 });
          return false;
        }
      }
      const res = await sd.call<{ state: string; error?: string; stats?: any }>(to.url, "GET", `/v1/sandboxes/${s.pod}/receive?wait=90000`, undefined, 100_000).catch((e) => ({ state: "unknown", error: (e as Error).message, stats: undefined }));
      if (res.state === "running") {
        await c.query("update sandboxes set host = $2 where id = $1", [sandboxId, to.name]);
        await this.pushPeersWith(c).catch((e) => logError("vm: peers after migration", e));
        const m = { from: from.name, to: to.name, reason, ms: Date.now() - t0, final_ms: res.stats?.final_ms, restore_ms: res.stats?.restore_ms, rounds: res.stats?.rounds, bytes: res.stats?.bytes,
          // the sandbox is stopped for the last copy and the restore (plus the route switch, a few ms)
          pause_ms: res.stats ? Math.round((Number(res.stats.final_ms) || 0) + (Number(res.stats.restore_ms) || 0)) : null };
        log("vm: sandbox migrated", { sandbox: sandboxId, ...m });
        void this.event("migration", to.name, sandboxId, m);
        return true;
      }
      // did the source keep it?
      const still = await sd.call<{ sandboxes: HostSandbox[] }>(from.url, "GET", "/v1/sandboxes").then((r) => r.sandboxes.some((x) => x.name === s.pod && x.state === "running"), () => false);
      await sd.call(to.url, "DELETE", `/v1/sandboxes/${s.pod}`).catch(() => {});
      if (still) {
        logError("vm: migration failed, sandbox still on the source", sendError ?? res.error, { sandbox: sandboxId });
        void this.event("migration-failed", from.name, sandboxId, { to: to.name, kept: true, error: String((sendError as Error)?.message ?? res.error).slice(0, 300) });
        return false;
      }
      await c.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'migration failed' where id = $1", [sandboxId]);
      logError("vm: migration lost the sandbox", sendError ?? res.error, { sandbox: sandboxId, from: from.name, to: to.name });
      void this.event("migration-failed", from.name, sandboxId, { to: to.name, kept: false, error: String((sendError as Error)?.message ?? res.error).slice(0, 300) });
      return false;
    });
  }

  /** pushPeers inside a migration's transaction (its host change isn't committed yet). */
  async pushPeersWith(c: pg.PoolClient): Promise<void> {
    const { rows: aws } = await c.query<HostRow>("select * from vm_hosts where status <> 'gone' and kind = 'aws' and wg_public_key is not null");
    const { rows } = await c.query<{ host: string; ip: string }>("select host, ip from sandboxes where status <> 'gone' and ip is not null and host is not null");
    const byHost = new Map<string, string[]>();
    for (const r of rows) byHost.set(r.host, [...(byHost.get(r.host) ?? []), `${r.ip}/32`]);
    const peers = aws.map((h) => ({ publicKey: h.wg_public_key!, allowedIps: [`${h.wg_ip}/32`, ...(byHost.get(h.name) ?? [])] }));
    await this.#d.sandboxd.call(this.vm.hub.url, "PUT", "/v1/peers", { peers }, 10_000);
  }

  /** May this sandbox move now? Never while its conversation has a turn going; across clouds only when idle. */
  async #movable(s: { id: string; conversation_id: string; pod: string; created_at: Date }, from: HostRow, to: HostRow): Promise<boolean> {
    if (Date.now() - new Date(s.created_at).getTime() < 30_000) return false;
    // moves during a turn (a bash command running) only once verified live (sandbox.vm.migration.duringTurns)
    if (!this.vm.migration.duringTurns) {
      const { rows } = await this.#d.db.query("select 1 from turns where conversation_id = $1 and status in ('queued','running','waiting') limit 1", [s.conversation_id]);
      if (rows.length) return false;
    }
    if (from.kind !== to.kind) {
      const usage = this.#lastInfo.get(from.name)?.sandboxes.find((x) => x.name === s.pod)?.cpuUsage ?? 1;
      if (usage > this.vm.migration.idleCpu) return false;
    }
    return true;
  }

  // ---- the scaler ------------------------------------------------------------------------------------------------

  /** One pass: heartbeats, network state, AWS hosts up/down, at most one migration. */
  async pass(): Promise<void> {
    await this.ensureHub();
    let hosts = await this.hosts();
    const hubName = this.vm.hub.name;

    // heartbeats
    let hubUp = false;
    const down = new Map<string, number>();
    await Promise.all(
      hosts.map(async (h) => {
        try {
          const info = await this.#d.sandboxd.host(h.url, 5000);
          this.#lastInfo.set(h.name, info);
          const missing = this.vm.cpuFeatures.split(",").filter((f) => f && !info.cpuFeatures.split(",").includes(f));
          const status = h.status === "booting" ? "active" : h.status;
          if (missing.length && h.status !== "retiring") {
            logError("vm: host lacks CPU features of the pool; not used", missing.join(","), { host: h.name });
          }
          await this.#d.db.query(
            `update vm_hosts set last_seen_at = now(), mem_total = $2, mem_reserve = $3, cpus = $4, wg_public_key = $5,
               info = $6, status = $7 where name = $1`,
            [
              h.name,
              info.memTotal,
              info.reserveMemory,
              info.cpus,
              info.wgPublicKey,
              JSON.stringify({ sandboxes: info.sandboxes, cpuFeatures: info.cpuFeatures, images: info.images, bootId: info.bootId, gvisorRelease: info.gvisorRelease,
                memAvailable: info.memAvailable, loadAvg: info.loadAvg ?? null, uptimeS: info.uptimeS ?? null, peers: info.peers.length, egressTargets: info.egressTargets }),
              missing.length ? "retiring" : status,
            ],
          );
          if (h.status === "booting") {
            log("vm: AWS host active", { host: h.name, bootMs: Date.now() - new Date(h.created_at).getTime() });
            void this.event("active", h.name, null, { bootMs: Date.now() - new Date(h.created_at).getTime() });
            // warm the current image there now rather than at the first sandbox
            void this.imageDigest().then((digest) => this.#d.sandboxd.call(h.url, "POST", "/v1/images", { digest }, 15 * 60_000)).catch((e) => logError("vm: image warm-up", e, { host: h.name }));
          }
          if (h.name === hubName) hubUp = true;
        } catch (e) {
          const seen = h.last_seen_at ? new Date(h.last_seen_at).getTime() : new Date(h.created_at).getTime();
          const age = Date.now() - seen;
          if (h.status === "booting") {
            if (Date.now() - new Date(h.created_at).getTime() > BOOT_TIMEOUT_MS) await this.#hostLost(h, "did not come up");
          } else if (h.kind === "hub") {
            if (age > HUB_DEAD_MS && h.last_seen_at) await this.#hostLost(h, (e as Error).message);
          }
          // AWS hosts are judged below, once the hub's state is known (they are reached through it)
          if (h.kind === "aws" && h.status !== "booting") down.set(h.name, age);
        }
      }),
    );
    for (const h of hosts) {
      const age = down.get(h.name);
      if (hubUp && age !== undefined && age > HOST_DEAD_MS) await this.#hostLost(h, "stopped answering");
    }
    if (!hubUp) return; // without the hub nothing else can be judged or changed

    await this.pushPeers().catch((e) => logError("vm: peers", e));
    await this.pushEgress().catch((e) => logError("vm: egress targets", e));
    await this.imageDigest().catch((e) => logError("vm: image", e));
    await this.#reconcileEc2().catch((e) => logError("vm: ec2 reconcile", e));

    hosts = await this.hosts();
    const slots = await this.#slots(this.#d.db);
    const free = slots.reduce((a, s) => a + s.free, 0);
    const used = new Map<string, number>();
    {
      const { rows } = await this.#d.db.query<{ host: string; n: string }>("select host, count(*) as n from sandboxes where status <> 'gone' and host is not null group by host");
      for (const r of rows) used.set(r.host, Number(r.n));
    }
    for (const h of hosts.filter((x) => x.kind === "aws")) {
      await this.#d.db.query("update vm_hosts set empty_since = case when $2::boolean then coalesce(empty_since, now()) else null end where name = $1", [h.name, (used.get(h.name) ?? 0) === 0]);
    }
    hosts = await this.hosts();
    const aws = hosts.filter((h) => h.kind === "aws");
    const draining = aws.filter((h) => h.status === "draining");
    const retiring = hosts.filter((h) => h.status === "retiring");

    // scale out: keep spare slots, plus one for each person typing in a conversation without a sandbox (an AWS host
    // boots in about a minute, so it starts while they type; unused, it goes by the rule below). A draining host
    // comes back first.
    await this.#d.db.query("delete from kv where key like 'sandbox_demand:%' and (value->>'at')::timestamptz < now() - interval '1 hour'");
    const demand = await this.demand();
    const want = this.vm.spareSlots + demand;
    if (free < want) {
      if (draining[0]) {
        await this.#d.db.query("update vm_hosts set status = 'active' where name = $1 and status = 'draining'", [draining[0].name]);
        log("vm: drain cancelled, capacity needed", { host: draining[0].name });
        await this.event("drain-cancelled", draining[0].name, null, { free, demand });
        return;
      }
      await this.launch(`free slots ${free} < ${want} (spare ${this.vm.spareSlots}, demand ${demand})`).catch((e) => logError("vm: launch", e));
    }
    // a retiring host with sandboxes and nowhere to put them: one transient host
    if (retiring.some((h) => (used.get(h.name) ?? 0) > 0) && free === 0) await this.launch("replacing a retiring host", true).catch((e) => logError("vm: launch", e));

    // an AWS host goes as soon as no sandbox is on it and it is past its minimum lifetime (from launch)
    for (const h of aws) {
      if ((used.get(h.name) ?? 0) > 0 || h.status === "booting") continue;
      if (!this.#paid(h)) {
        await this.#terminate(h, h.status === "active" ? "empty" : `${h.status}, empty`);
        return;
      }
    }

    // a retiring (or draining) host: its sandboxes move off, one per pass, longest-idle first
    for (const from of [...draining, ...retiring]) {
      const { rows } = await this.#d.db.query<{ id: string; conversation_id: string; pod: string; created_at: Date }>(
        "select id, conversation_id, pod, created_at from sandboxes where host = $1 and status = 'ready' order by last_active_at",
        [from.name],
      );
      for (const s of rows) {
        const size = from.info?.sandboxes?.find((x) => x.name === s.pod)?.memory ?? this.memBytes;
        const target = VmController.pick(slots, from.name, Math.max(1, Math.ceil(size / this.memBytes)));
        if (!target) break;
        if (!(await this.#movable(s, from, target.host))) continue;
        await this.migrate(s.id, target.host, from.status);
        return;
      }
    }

    // packing: all sandboxes on the fewest hosts (packing.ts); a host it empties goes by the release rule above
    const active = hosts.filter((h) => h.status === "active" && this.#fresh(h));
    const { rows: live } = await this.#d.db.query<{ id: string; conversation_id: string; pod: string; created_at: Date; host: string }>(
      "select id, conversation_id, pod, created_at, host from sandboxes where status = 'ready' and host = any($1::text[])",
      [active.map((h) => h.name)],
    );
    const sizeOf = (r: { pod: string; host: string }) => {
      const mem = active.find((h) => h.name === r.host)?.info?.sandboxes?.find((x) => x.name === r.pod)?.memory ?? this.memBytes;
      return Math.max(1, Math.ceil(mem / this.memBytes));
    };
    const { rows: moved } = await this.#d.db.query<{ id: string; at: Date }>(
      "select sandbox_id as id, max(at) as at from vm_events where kind = 'migration' and sandbox_id = any($1::uuid[]) group by 1",
      [live.map((r) => r.id)],
    );
    const movedAt = new Map(moved.map((m) => [m.id, new Date(m.at).getTime()]));
    const moves = planPacking(
      active.map((h) => ({ name: h.name, slots: this.slots(h), open: !this.underPressure(h) })),
      live.map((r) => ({ id: r.id, host: r.host, size: sizeOf(r), movedAt: movedAt.get(r.id) ?? 0 })),
    );
    for (const m of moves) {
      const r = live.find((x) => x.id === m.id)!;
      const from = active.find((h) => h.name === m.from)!;
      const to = active.find((h) => h.name === m.to)!;
      if (!(await this.#movable(r, from, to))) continue;
      void this.event("pack", from.name, r.id, { to: to.name, moves: moves.length, plan: moves.map((x) => `${x.from}->${x.to}`) });
      await this.migrate(r.id, to, "packing");
      return;
    }
  }

  /** Still within its minimum lifetime (from launch): not released yet even when empty (the only use of host age). */
  #paid(h: HostRow): boolean {
    return Date.now() - new Date(h.created_at).getTime() < this.vm.minHostLifetimeSeconds * 1000;
  }

  async #reconcileEc2(): Promise<void> {
    const aws = this.vm.aws;
    if (!aws || !this.#d.ec2) return;
    const insts = await this.#d.ec2.describe({ app: "yyc-ai-sandbox", jarvis: aws.instance });
    const { rows } = await this.#d.db.query<HostRow>("select * from vm_hosts where kind = 'aws'");
    const live = new Map(rows.filter((r) => r.status !== "gone" && r.instance_id).map((r) => [r.instance_id!, r]));
    const orphans = insts.filter((i) => !live.has(i.id) && i.state !== "shutting-down" && Date.now() - Date.parse(i.launchTime) > ORPHAN_INSTANCE_MS);
    if (orphans.length) {
      log("vm: terminating instances the database doesn't know", { instances: orphans.map((i) => i.id) });
      await this.#d.ec2.terminate(orphans.map((i) => i.id));
    }
    const running = new Map(insts.map((i) => [i.id, i]));
    for (const h of live.values()) {
      const i = running.get(h.instance_id!);
      if (!i && Date.now() - new Date(h.created_at).getTime() > 120_000) await this.#hostLost(h, "instance no longer exists");
      else if (i && !h.private_ip && i.privateIp) await this.#d.db.query("update vm_hosts set private_ip = $2 where name = $1", [h.name, i.privateIp]);
    }
  }
}

export class VmProvider implements SandboxProvider {
  readonly kind = "vm" as const;
  readonly controller: VmController;
  readonly #d: VmDeps;
  /**
   * The provider sandboxes came from before the switch to "vm" (sandbox.vm.legacyProvider): its sandboxes (rows
   * without a host) keep running until the janitor releases them (idle or max lifetime, with the final save of
   * their files), are listed for the janitor and removed through it. New sandboxes always come from the hosts.
   */
  readonly #legacy: SandboxProvider | null;

  constructor(d: VmDeps, legacy: SandboxProvider | null = null) {
    this.#d = d;
    this.controller = new VmController(d);
    this.#legacy = legacy;
  }

  async create(row: SandboxRow, egressToken: string): Promise<{ name: string }> {
    const ctl = this.controller;
    const vm = ctl.vm;
    const until = Date.now() + PLACE_WAIT_MS;
    let placed: { host: HostRow; ip: string } | null = null;
    for (;;) {
      placed = await ctl.place(row);
      if (placed) break;
      const r = await ctl.launch("no room for a new sandbox");
      if (r === "full" || r === "off") throw new PoolFullError("all sandbox hosts are full, try again in a few minutes");
      if (Date.now() > until) throw new Error("no sandbox host became available in time");
      await sleep(3000);
    }
    const digest = await ctl.imageDigest();
    const env = {
      ...sandboxEnv(`http://${vm.egressVip}:3128`, vm.egressVip, egressToken),
      JARVIS_EXEC_TOKEN: row.exec_token,
      HOME: "/home/sandbox",
    };
    await this.#d.sandboxd.call(
      placed.host.url,
      "POST",
      "/v1/sandboxes",
      { name: row.pod, ip: placed.ip, image: digest, imageRef: this.#d.config.sandbox.image, env, cpu: ctl.cpuCores, memory: ctl.memBytes, cpuFeatures: vm.cpuFeatures },
      15 * 60_000,
    );
    // AWS: the hub must route the new /32 to that host
    if (placed.host.kind === "aws") await ctl.pushPeers();
    log("vm: sandbox placed", { sandbox: row.id, host: placed.host.name, ip: placed.ip });
    return { name: row.pod };
  }

  async waitReady(row: SandboxRow, deadline: number): Promise<AgentEndpoint> {
    const { rows } = await this.#d.db.query<{ ip: string }>("select ip from sandboxes where id = $1", [row.id]);
    const ip = rows[0]?.ip;
    if (!ip) throw new Error("the sandbox has no address");
    const url = `http://${ip}:7070`;
    let last = "";
    while (Date.now() < deadline) {
      try {
        const h = await new AgentClient(url, row.exec_token).health(2000);
        if (h.ok) return { url, headers: {} };
      } catch (e) {
        last = (e as Error).message;
      }
      await sleep(300);
    }
    throw new Error(`the sandbox's agent did not answer in time (${last})`);
  }

  async keepAlive(): Promise<void> {}

  /** The admin monitor's view (hosts, their sandboxes, AWS cost so far, scaler decisions, migrations). */
  stats(): Promise<VmStats> {
    return vmStats(this.#d.db, this.controller);
  }

  async destroy(name: string): Promise<void> {
    const { rows } = await this.#d.db.query<{ id: string; host: string | null }>("select id, host from sandboxes where pod = $1", [name]);
    if (this.#legacy && (rows[0] ? !rows[0].host : !(await this.#onHosts(name)))) return this.#legacy.destroy(name);
    const del = async (host: string | null) => {
      const hosts = (await this.controller.hosts()).filter((h) => !host || h.name === host);
      await Promise.all(hosts.map((h) => this.#d.sandboxd.call(h.url, "DELETE", `/v1/sandboxes/${name}`, undefined, 60_000)));
    };
    if (!rows[0]) return del(null);
    // waits for a migration of this sandbox to finish, then deletes it where it is
    await this.controller.withSandboxLock(rows[0].id, async (c) => {
      const { rows: r } = await c.query<{ host: string | null }>("select host from sandboxes where id = $1", [rows[0]!.id]);
      await del(r[0]?.host ?? null);
    });
  }

  async #onHosts(name: string): Promise<boolean> {
    return (await this.list(false)).some((s) => s.name === name);
  }

  async list(withLegacy = true): Promise<ListedSandbox[]> {
    const hosts = await this.controller.hosts();
    const out: ListedSandbox[] = withLegacy && this.#legacy ? await this.#legacy.list() : [];
    await Promise.all(
      hosts.map(async (h) => {
        try {
          const r = await this.#d.sandboxd.call<{ sandboxes: HostSandbox[] }>(h.url, "GET", "/v1/sandboxes", undefined, 5000);
          for (const s of r.sandboxes) out.push({ name: s.name, createdAt: Date.parse(s.createdAt) });
        } catch {
          // unreachable: assume its sandboxes are there (the scaler decides when a host is lost)
          const { rows } = await this.#d.db.query<{ pod: string }>("select pod from sandboxes where host = $1 and status <> 'gone'", [h.name]);
          for (const r of rows) out.push({ name: r.pod, createdAt: NaN });
        }
      }),
    );
    return out;
  }
}

/** On-demand price of an AWS burst host (m7i.xlarge, eu-central-1) plus its public IPv4 and 40 GB gp3, USD/hour. */
export const AWS_HOST_USD_PER_HOUR = 0.2415 + 0.005 + (40 * 0.0952) / 730;

export interface VmStats {
  provider: "vm";
  at: string;
  scaler: { spareSlots: number; minHostLifetimeSeconds: number; maxAwsHosts: number; freeSlots: number; demand: number; memoryPerSandbox: number; cpuPerSandbox: number; lastPassAt: string | null };
  hosts: {
    name: string;
    kind: "hub" | "aws";
    status: HostRow["status"];
    instanceId: string | null;
    cpus: number | null;
    memTotal: number | null;
    memAvailable: number | null;
    loadAvg: number[] | null;
    slots: number;
    used: number;
    underPressure: boolean;
    maxSandboxes: number | null;
    lastSeenAt: string | null;
    upSince: string;
    upHours: number;
    costUsd: number | null;
    gvisorRelease: string | null;
    sandboxes: { name: string; id: string | null; conversationId: string | null; ip: string; state: string; cpuUsage: number; memoryCurrent: number; memoryLimit: number; busy: boolean }[];
  }[];
  awsCostLast30dUsd: number;
  events: { at: string; kind: string; host: string | null; sandboxId: string | null; data: Record<string, unknown> }[];
  migrations: { at: string; sandboxId: string | null; from: string; to: string; ms: number; pauseMs: number | null; finalMs: number | null; restoreMs: number | null; rounds: number | null; bytes: number | null }[];
}

/** Everything the admin monitor shows for provider vm (hosts, their sandboxes, AWS cost so far, scaler decisions). */
export async function vmStats(db: Db, ctl: VmController): Promise<VmStats> {
  const { rows: hosts } = await db.query<HostRow & { gone_at: Date | null }>(
    "select * from vm_hosts where status <> 'gone' or gone_at > now() - interval '30 days' order by kind desc, created_at",
  );
  const { rows: sbx } = await db.query<{ id: string; pod: string; conversation_id: string; host: string; busy: boolean }>(
    `select s.id, s.pod, s.conversation_id, s.host, exists (select 1 from turns t where t.conversation_id = s.conversation_id
       and t.status in ('queued','running','waiting')) as busy from sandboxes s where s.status <> 'gone' and s.host is not null`,
  );
  const byPod = new Map(sbx.map((r) => [r.pod, r]));
  const used = new Map<string, number>();
  for (const r of sbx) used.set(r.host, (used.get(r.host) ?? 0) + 1);
  const hours = (h: { created_at: Date; gone_at: Date | null }, since = 0) =>
    Math.max(0, ((h.gone_at ? new Date(h.gone_at).getTime() : Date.now()) - Math.max(new Date(h.created_at).getTime(), since)) / 3_600_000);
  const monthAgo = Date.now() - 30 * 86_400_000;
  const live = hosts.filter((h) => h.status !== "gone");
  const { rows: ev } = await db.query<{ at: Date; kind: string; host: string | null; sandbox_id: string | null; data: any }>(
    "select at, kind, host, sandbox_id, data from vm_events order by id desc limit 100",
  );
  const { rows: last } = await db.query<{ at: Date }>("select max(last_seen_at) as at from vm_hosts where status <> 'gone'");
  return {
    provider: "vm",
    at: new Date().toISOString(),
    scaler: {
      spareSlots: ctl.vm.spareSlots,
      minHostLifetimeSeconds: ctl.vm.minHostLifetimeSeconds,
      maxAwsHosts: ctl.vm.aws?.maxHosts ?? 0,
      freeSlots: live.filter((h) => h.status === "active" && !ctl.underPressure(h)).reduce((a, h) => a + Math.max(0, ctl.slots(h) - ctl.units(h, used.get(h.name) ?? 0)), 0),
      demand: await ctl.demand().catch(() => 0),
      memoryPerSandbox: ctl.memBytes,
      cpuPerSandbox: ctl.cpuCores,
      lastPassAt: last[0]?.at ? new Date(last[0].at).toISOString() : null,
    },
    hosts: live.map((h) => ({
      name: h.name,
      kind: h.kind,
      status: h.status,
      instanceId: h.instance_id,
      cpus: h.cpus,
      memTotal: h.mem_total == null ? null : Number(h.mem_total),
      memAvailable: h.info?.memAvailable ?? null,
      loadAvg: h.info?.loadAvg ?? null,
      slots: ctl.slots(h),
      used: ctl.units(h, used.get(h.name) ?? 0),
      underPressure: ctl.underPressure(h),
      maxSandboxes: h.max_sandboxes,
      lastSeenAt: h.last_seen_at ? new Date(h.last_seen_at).toISOString() : null,
      upSince: new Date(h.created_at).toISOString(),
      upHours: Math.round(hours(h) * 100) / 100,
      costUsd: h.kind === "aws" ? Math.round(hours(h) * AWS_HOST_USD_PER_HOUR * 100) / 100 : null,
      gvisorRelease: h.info?.gvisorRelease ?? null,
      sandboxes: (h.info?.sandboxes ?? []).map((s) => {
        const r = byPod.get(s.name);
        return { name: s.name, id: r?.id ?? null, conversationId: r?.conversation_id ?? null, ip: s.ip, state: s.state, cpuUsage: s.cpuUsage, memoryCurrent: s.memoryCurrent, memoryLimit: s.memory, busy: !!r?.busy };
      }),
    })),
    awsCostLast30dUsd: Math.round(hosts.filter((h) => h.kind === "aws" && h.instance_id).reduce((a, h) => a + hours(h, monthAgo), 0) * AWS_HOST_USD_PER_HOUR * 100) / 100,
    events: ev.map((e) => ({ at: new Date(e.at).toISOString(), kind: e.kind, host: e.host, sandboxId: e.sandbox_id, data: e.data })),
    migrations: ev
      .filter((e) => e.kind === "migration")
      .slice(0, 20)
      .map((e) => ({
        at: new Date(e.at).toISOString(),
        sandboxId: e.sandbox_id,
        from: e.data.from,
        to: e.data.to,
        ms: e.data.ms,
        pauseMs: e.data.pause_ms ?? null,
        finalMs: e.data.final_ms ?? null,
        restoreMs: e.data.restore_ms ?? null,
        rounds: e.data.rounds ?? null,
        bytes: e.data.bytes ?? null,
      })),
  };
}

export function startVmScaler(dbUrl: string, ctl: VmController, intervalMs = 5000): { stop(): Promise<void> } {
  return lead(dbUrl, SCALER_LOCK, () => ctl.pass(), intervalMs);
}
