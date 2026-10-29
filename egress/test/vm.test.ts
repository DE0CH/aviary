// Sandbox provider "vm" without hosts: placement (hub first, then the fullest AWS host), the scaler (an AWS host when
// the hub is full, drain-back by live migration when it has room again, termination of the empty host), a migration
// that would not converge leaving the sandbox where it was, a lost host, and the pure parts of the EC2 client.
// sandboxd itself (Rust, chat/sandboxd/daemon) has its own tests: nft rules, OCI bundle, spec checks (cargo test).
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { randomUUID } from "node:crypto";
import { parseConfig } from "../src/common/config.ts";
import type { Db } from "../src/common/db.ts";
import { freeAddress, hubPeers, parseBytes, VmController, VmProvider, type HostRow } from "../src/web/sandbox-vm.ts";
import { DEMAND_WINDOW_S, noteSandboxDemand, type SandboxRow } from "../src/web/sandbox.ts";
import { parseInstances, SandboxdError, wgKeyPair, type Ec2Api, type Ec2Instance, type HostInfo, type SandboxdApi } from "../src/web/vm-clients.ts";
import { freshDb } from "./pg.ts";

const GiB = 1024 ** 3;
const HUB = "https://10.201.0.1:7443";

function config(extra: Record<string, unknown> = {}) {
  return parseConfig({
    product: { name: "Test" }, publicUrl: "https://x.example.org", access: { teamDomain: "t.example.org", audience: "a" },
    users: ["a@example.org"], admins: [], models: [{ id: "m", label: "M" }], defaultModel: "m", titleModel: "m",
    systemPromptFile: "/dev/null",
    sandbox: {
      provider: "vm", image: `ghcr.io/x/sandbox@sha256:${"a".repeat(64)}`, memory: "2Gi", cpu: "1",
      vm: {
        hub: { name: "hub", url: HUB, endpoint: "203.0.113.1:51820" }, cpuFeatures: "fpu,sse2", spareSlots: 1, minHostLifetimeSeconds: 900,
        aws: { region: "eu-central-1", launchTemplate: "lt", maxHosts: 3, emptyMinutes: 0, instance: "test" },
        ...extra,
      },
    },
    egress: { defaultAllowedHosts: [], routes: [] }, tools: {}, s3: { endpoint: "http://s3", region: "r", bucket: "b", prefix: "" },
  });
}

/** sandboxd on every host, in memory. `refuseSend`: the next send answers 409 (didn't converge). */
class FakeSandboxd implements SandboxdApi {
  hosts = new Map<string, { info: HostInfo; specs: Map<string, any>; receiving: Map<string, any> }>();
  calls: string[] = [];
  refuseSend = false;
  down = new Set<string>();
  addHost(url: string, name: string, memGiB: number, role: "hub" | "burst"): void {
    const keys = wgKeyPair();
    this.hosts.set(url, {
      info: {
        name, role, wgAddress: new URL(url).hostname, wgPublicKey: keys.publicKey, peers: [], egressTargets: [], memTotal: memGiB * GiB, memAvailable: memGiB * GiB,
        reserveMemory: GiB, cpus: 4, cpuFeatures: "fpu,sse2,avx2", runsc: "runsc", gvisorRelease: "x", images: [], bootId: "b", startedAt: "", sandboxes: [],
      },
      specs: new Map(),
      receiving: new Map(),
    });
  }
  #h(url: string) {
    const h = this.hosts.get(new URL(url).origin);
    if (!h || this.down.has(new URL(url).origin)) throw new Error("connect ECONNREFUSED");
    return h;
  }
  async host(url: string): Promise<HostInfo> {
    const h = this.#h(url);
    h.info.sandboxes = [...h.specs.values()].map((s) => ({ name: s.name, ip: s.ip, state: "running", createdAt: new Date().toISOString(), cpu: 1, memory: s.memory, cpuUsage: 0, memoryCurrent: 0 }));
    return structuredClone(h.info);
  }
  async call(url: string, method: string, path: string, body?: any): Promise<any> {
    const h = this.#h(url);
    this.calls.push(`${h.info.name} ${method} ${path}`);
    const name = path.split("/")[3] ?? "";
    if (path === "/v1/peers") return { peers: body.peers };
    if (path === "/v1/egress") return { targets: body.targets };
    if (path === "/v1/images") return { digest: body.digest ?? "a".repeat(64) };
    if (path === "/v1/sandboxes" && method === "GET") return { sandboxes: (await this.host(url)).sandboxes };
    if (path === "/v1/sandboxes" && method === "POST") {
      h.specs.set(body.name, body);
      return {};
    }
    if (method === "DELETE") {
      h.specs.delete(name);
      h.receiving.delete(name);
      return {};
    }
    if (path.endsWith("/spec")) return h.specs.get(name);
    if (path.endsWith("/receive") && method === "POST") {
      h.receiving.set(name, body);
      return { port: 7100, address: h.info.wgAddress };
    }
    if (path.endsWith("/send")) {
      if (this.refuseSend) {
        this.refuseSend = false;
        throw new SandboxdError(409, "not converging; the sandbox still runs here");
      }
      const target = [...this.hosts.values()].find((x) => x.info.wgAddress === body.to.split(":")[0])!;
      const spec = h.specs.get(name);
      h.specs.delete(name);
      target.receiving.delete(name);
      target.specs.set(name, spec);
      return { ms: 5 };
    }
    if (path.includes("/receive?")) return h.specs.has(name) ? { state: "running", stats: { final_ms: 1 } } : { state: "failed" };
    throw new Error(`fake sandboxd: ${method} ${path}`);
  }
}

class FakeEc2 implements Ec2Api {
  launched: { userData: any; id: string }[] = [];
  terminated: string[] = [];
  readonly sbx: FakeSandboxd;
  constructor(sbx: FakeSandboxd) {
    this.sbx = sbx;
  }
  async runInstance(o: { userData: string; tags: Record<string, string> }): Promise<Ec2Instance> {
    const ud = JSON.parse(o.userData);
    const id = `i-${this.launched.length + 1}`;
    this.launched.push({ userData: ud, id });
    this.sbx.addHost(`https://${ud.host.wgAddress}:7443`, ud.host.name, 16, "burst");
    return { id, state: "pending", privateIp: `172.31.0.${this.launched.length + 10}`, launchTime: new Date().toISOString(), tags: o.tags };
  }
  async terminate(ids: string[]): Promise<void> {
    this.terminated.push(...ids);
  }
  async describe(): Promise<Ec2Instance[]> {
    return this.launched.filter((l) => !this.terminated.includes(l.id)).map((l) => ({ id: l.id, state: "running", privateIp: "172.31.0.9", launchTime: new Date().toISOString(), tags: {} }));
  }
}

let db: Db;
let drop: () => Promise<void>;
before(async () => {
  const t = await freshDb("vm");
  db = t.db;
  drop = t.drop;
});
after(async () => drop());

async function newRow(conv = randomUUID()): Promise<SandboxRow> {
  const id = randomUUID();
  const { rows } = await db.query<SandboxRow>(
    `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, created_at)
     values ($1, $2, $3, $4, 'x', 'starting', now() + interval '1 hour', now() - interval '1 minute') returning *`,
    [id, conv, `sbx-${id.replace(/-/g, "").slice(0, 16)}`, randomUUID()],
  );
  return rows[0]!;
}
const ready = (id: string) => db.query("update sandboxes set status = 'ready' where id = $1", [id]);
const hostOf = async (id: string) => (await db.query<{ host: string }>("select host from sandboxes where id = $1", [id])).rows[0]!.host;
const hostRow = async (name: string) => (await db.query<HostRow>("select * from vm_hosts where name = $1", [name])).rows[0]!;

test("pure helpers: addresses, quantities, EC2 answers", () => {
  assert.equal(freeAddress("10.200.0.0/16", new Set()), "10.200.0.10");
  assert.equal(freeAddress("10.200.0.0/16", new Set(["10.200.0.10", "10.200.0.11"])), "10.200.0.12");
  assert.equal(freeAddress("10.201.0.0/24", new Set(), 10, 12), "10.201.0.10");
  assert.equal(freeAddress("10.201.0.0/24", new Set(["10.201.0.10", "10.201.0.11"]), 10, 12), null);
  assert.equal(parseBytes("2Gi"), 2 * GiB);
  assert.equal(parseBytes("1536Mi"), 1536 * 1024 * 1024);
  const xml = `<RunInstancesResponse><instancesSet><item><instanceId>i-0abc</instanceId><imageId>ami-1</imageId>
    <instanceState><code>0</code><name>pending</name></instanceState><privateIpAddress>172.31.5.6</privateIpAddress>
    <launchTime>2026-10-21T01:00:00.000Z</launchTime><groupSet><item><groupId>sg-1</groupId></item></groupSet>
    <tagSet><item><key>app</key><value>yyc-ai-sandbox</value></item></tagSet></item></instancesSet></RunInstancesResponse>`;
  assert.deepEqual(parseInstances(xml), [{ id: "i-0abc", state: "pending", privateIp: "172.31.5.6", launchTime: "2026-10-21T01:00:00.000Z", tags: { app: "yyc-ai-sandbox" } }]);
  const k = wgKeyPair();
  assert.match(k.publicKey, /^[A-Za-z0-9+/]{43}=$/);
  assert.match(k.privateKey, /^[A-Za-z0-9+/]{43}=$/);
});

test("placement, burst to AWS, drain-back by live migration, termination", async () => {
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub"); // (8 - 1) GiB / 2 GiB = 3 slots
  const ec2 = new FakeEc2(sbx);
  const cfg = config();
  const p = new VmProvider({ db, config: cfg, sandboxd: sbx, ec2, hostTls: { ca: "CA", cert: "C", key: "K" } });
  const ctl = p.controller;
  await ctl.pass();
  const hub = await hostRow("hub");
  assert.equal(hub.status, "active");
  assert.ok(hub.wg_public_key);

  // the hub first
  const a = await newRow();
  await p.create(a, "tok-a");
  assert.equal(await hostOf(a.id), "hub");
  const spec = [...sbx.hosts.get(HUB)!.specs.values()][0];
  assert.equal(spec.env.HTTPS_PROXY, "http://sbx:tok-a@10.201.1.1:3128");
  assert.equal(spec.env.JARVIS_EXEC_TOKEN, "x");
  assert.equal(spec.cpuFeatures, "fpu,sse2");
  for (const k of Object.keys(spec.env)) assert.ok(!/KEY|SECRET|DATABASE|PASSWORD/.test(k), `no secrets in the sandbox env (${k})`);
  await ready(a.id);

  // the hub held to 1 sandbox (the operator's cap): free 0 < spare 1, so the scaler starts an AWS host
  await db.query("update vm_hosts set max_sandboxes = 1 where name = 'hub'");
  await ctl.pass();
  assert.equal(ec2.launched.length, 1);
  const ud = ec2.launched[0]!.userData;
  assert.equal(ud.host.role, "burst");
  assert.equal(ud.host.hub.publicKey, hub.wg_public_key);
  assert.equal(ud.host.hub.endpoint, "203.0.113.1:51820");
  assert.deepEqual(ud.tls, { ca: "CA", cert: "C", key: "K" });
  const awsName = ud.host.name as string;
  assert.equal((await hostRow(awsName)).status, "booting");
  // no second host while one boots
  await ctl.pass();
  assert.equal(ec2.launched.length, 1);
  assert.equal((await hostRow(awsName)).status, "active");
  // the hub's peers carry the AWS host
  assert.ok(sbx.calls.some((c) => c === "hub PUT /v1/peers"));

  // a new sandbox goes to AWS; its /32 is routed there
  const b = await newRow();
  await p.create(b, "tok-b");
  assert.equal(await hostOf(b.id), awsName);
  await ready(b.id);

  // the hub has room again and takes a new sandbox: packing moves the AWS host's one there (fewest hosts; a move that
  // wouldn't converge leaves it where it was first)
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
  const c = await newRow();
  await p.create(c, "tok-c");
  assert.equal(await hostOf(c.id), "hub");
  await ready(c.id);
  // hosts on different gVisor releases: no move (runsc restore refuses another version's checkpoint)
  const awsFake = [...sbx.hosts.values()].find((h) => h.info.name === awsName)!;
  awsFake.info.gvisorRelease = "y";
  await ctl.pass();
  assert.equal(await hostOf(b.id), awsName);
  assert.ok(!sbx.hosts.get(HUB)!.receiving.size, "nothing was sent");
  awsFake.info.gvisorRelease = "x";
  sbx.refuseSend = true;
  await ctl.pass();
  assert.equal(await hostOf(b.id), awsName);
  assert.ok(!sbx.hosts.get(HUB)!.receiving.size, "the target's half-received sandbox is removed");
  await ctl.pass();
  assert.equal(await hostOf(b.id), "hub");
  assert.ok(sbx.hosts.get(HUB)!.specs.has(b.pod));
  const { rows: pack } = await db.query("select data from vm_events where kind = 'pack' and sandbox_id = $1", [b.id]);
  assert.ok(pack.length >= 1, "the packing decision is recorded");
  // empty but within its minimum lifetime: kept
  await ctl.pass();
  assert.equal((await hostRow(awsName)).status, "active");
  await db.query("update vm_hosts set created_at = now() - interval '16 minutes' where name = $1", [awsName]);
  // empty and past its minimum lifetime: terminated
  await ctl.pass();
  assert.equal((await hostRow(awsName)).status, "gone");
  assert.deepEqual(ec2.terminated, [ec2.launched[0]!.id]);

  // list() and destroy() follow the sandbox to where it is now
  assert.deepEqual((await p.list()).map((x) => x.name).sort(), [a.pod, b.pod, c.pod].sort());
  await p.destroy(b.pod);
  assert.ok(!sbx.hosts.get(HUB)!.specs.has(b.pod));
});

test("never more than maxHosts AWS hosts; a full pool is reported, not waited out", async () => {
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  await db.query("update sandboxes set status = 'gone'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  const ec2 = new FakeEc2(sbx);
  const p = new VmProvider({ db, config: config({ aws: { region: "eu-central-1", launchTemplate: "lt", maxHosts: 1, emptyMinutes: 60, instance: "test" } }), sandboxd: sbx, ec2, hostTls: { ca: "", cert: "", key: "" } });
  await db.query("update vm_hosts set max_sandboxes = 0 where name = 'hub'");
  await p.controller.pass(); // launches the one allowed
  await p.controller.pass(); // active
  for (let i = 0; i < 7; i++) await p.create(await newRow(), "t"); // 16-1 GiB / 2 GiB = 7 slots
  await assert.rejects(p.create(await newRow(), "t"), /full/);
  assert.equal(ec2.launched.length, 1);
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
});

test("typing starts an AWS host only when no slot is free; an empty host goes once past its minimum lifetime", async () => {
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  await db.query("update sandboxes set status = 'gone'");
  await db.query("delete from kv where key like 'sandbox_demand:%'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub"); // 3 slots
  const ec2 = new FakeEc2(sbx);
  const p = new VmProvider({ db, config: config({ spareSlots: 0, minHostLifetimeSeconds: 900 }), sandboxd: sbx, ec2, hostTls: { ca: "", cert: "", key: "" } });
  const ctl = p.controller;
  await ctl.pass();
  // no warm host: nobody typing, nothing launched
  assert.equal(ec2.launched.length, 0);
  // someone types while the hub has room: still nothing
  await noteSandboxDemand(db, "a@example.org");
  await ctl.pass();
  assert.equal(ec2.launched.length, 0);
  // the hub is full: typing starts an AWS host at once
  for (let i = 0; i < 3; i++) await p.create(await newRow(), "t");
  await ctl.pass();
  assert.equal(ec2.launched.length, 1);
  await ctl.pass(); // active
  const aws = ec2.launched[0]!.userData.host.name as string;
  assert.equal((await hostRow(aws)).status, "active");
  // empty but younger than its minimum lifetime: kept, even with nobody typing any more
  await db.query("delete from kv where key like 'sandbox_demand:%'");
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "active");
  // a sandbox on it keeps it past the minimum lifetime
  await db.query("update vm_hosts set max_sandboxes = 3 where name = 'hub'");
  const s = await newRow();
  await p.create(s, "t");
  assert.equal(await hostOf(s.id), aws);
  await db.query("update vm_hosts set created_at = now() - interval '16 minutes' where name = $1", [aws]);
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "active");
  // no sandbox and past 15 minutes: terminated
  await p.destroy(s.pod);
  await db.query("update sandboxes set status = 'gone' where id = $1", [s.id]);
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "gone");
  assert.deepEqual(ec2.terminated, [ec2.launched[0]!.id]);
  // a typing signal counts for DEMAND_WINDOW_S only
  await noteSandboxDemand(db, "b@example.org");
  await db.query("update kv set value = $2 where key = $1", ["sandbox_demand:b@example.org", JSON.stringify({ at: new Date(Date.now() - (DEMAND_WINDOW_S + 5) * 1000).toISOString() })]);
  assert.equal(await ctl.demand(), 0);
  await ctl.pass();
  assert.equal(ec2.launched.length, 1);
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
});

test("a hub renamed in config (the same overlay address) replaces the old row instead of failing", async () => {
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  await db.query("update vm_hosts set status = 'gone' where name = 'hub'");
  await db.query("insert into vm_hosts (name, kind, url, wg_ip, status) values ('old-hub', 'hub', $1, '10.201.0.1', 'active')", [HUB]);
  const p = new VmProvider({ db, config: config(), sandboxd: sbx, ec2: null, hostTls: null });
  await p.controller.ensureHub();
  assert.equal((await hostRow("old-hub")).status, "gone");
  assert.equal((await hostRow("hub")).status, "active", "the configured hub is live again");
});

test("hub peers: each AWS host's /32s, and its IPv6 endpoint when it reports one (the gateway can dial it)", () => {
  const h = (name: string, info: any) => ({ name, wg_public_key: `k-${name}`, wg_ip: name === "a" ? "10.201.0.10" : "10.201.0.11", info }) as unknown as HostRow;
  const peers = hubPeers([h("a", { publicIpv6: "2a05:d014::5", wgPort: 51820 }), h("b", null)], new Map([["a", ["10.200.0.10/32"]]]));
  assert.deepEqual(peers, [
    { publicKey: "k-a", endpoint: "[2a05:d014::5]:51820", allowedIps: ["10.201.0.10/32", "10.200.0.10/32"] },
    { publicKey: "k-b", allowedIps: ["10.201.0.11/32"] },
  ]);
});

test("capacity planning: two people typing at once -> one host (pending slots count); a burst -> enough hosts at once", async () => {
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  await db.query("update sandboxes set status = 'gone'");
  await db.query("delete from kv where key like 'sandbox_demand:%'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  const ec2 = new FakeEc2(sbx);
  const mk = () => new VmProvider({ db, config: config({ spareSlots: 0 }), sandboxd: sbx, ec2, hostTls: { ca: "", cert: "", key: "" } });
  const [p1, p2] = [mk(), mk()]; // two web replicas
  await db.query("update vm_hosts set max_sandboxes = 0 where name = 'hub'");
  await Promise.all([noteSandboxDemand(db, "a@example.org"), noteSandboxDemand(db, "b@example.org")]);
  await Promise.all([p1.controller.pass(), p2.controller.pass(), p1.controller.launch("race"), p2.controller.launch("race")]);
  assert.equal(ec2.launched.length, 1, "the second signal fits into the booting host's pending slots");
  const { rows } = await db.query("select status from vm_hosts where kind = 'aws' and status <> 'gone'");
  assert.deepEqual(rows.map((r) => r.status), ["booting"]);
  const plan = await p1.controller.plan();
  assert.equal(plan.available, 0);
  assert.equal(plan.pending, plan.perHost);
  // nothing is placed on a booting host
  await assert.rejects(p1.controller.place(await newRow()).then((r) => r ?? Promise.reject(new Error("no room"))), /no room/);

  // a burst bigger than one host's slots: enough hosts at once
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  const n = plan.perHost + 1;
  await Promise.all(Array.from({ length: n }, (_, i) => noteSandboxDemand(db, `u${i}@example.org`)));
  const before = ec2.launched.length;
  await p1.controller.launch("burst");
  assert.equal(ec2.launched.length - before, 2, `${n} people typing, ${plan.perHost} slots per host: two hosts`);
  await p2.controller.launch("burst again");
  assert.equal(ec2.launched.length - before, 2, "already planned");
  await db.query("delete from kv where key like 'sandbox_demand:%'");
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
});

test("slots: a sandbox made with a bigger limit takes more of them; a host short of memory takes no new one", async () => {
  const ctl = new VmProvider({ db, config: config({ spareSlots: 0 }), sandboxd: new FakeSandboxd(), ec2: null as any, hostTls: null as any }).controller;
  const base = { mem_total: 8 * GiB, mem_reserve: GiB, cpus: 4, max_sandboxes: null } as unknown as HostRow;
  assert.equal(ctl.slots(base), 3); // (8 - 1) GiB / 2 GiB
  const old = { ...base, info: { sandboxes: [{ name: "sbx-old", memory: 4 * GiB }, { name: "sbx-new", memory: 2 * GiB }] } } as unknown as HostRow;
  assert.equal(ctl.units(old, 2), 3, "a 4 GiB sandbox from before a smaller sandbox.memory counts as two");
  assert.equal(ctl.underPressure({ ...base, info: { memAvailable: 100 * 1024 ** 2 } } as unknown as HostRow), true);
  assert.equal(ctl.underPressure({ ...base, info: { memAvailable: 2 * GiB } } as unknown as HostRow), false);
  assert.equal(VmController.pick([{ host: { ...base, name: "hub", kind: "hub" } as HostRow, used: 2, free: 1 }], undefined, 2), null, "a 2-slot sandbox needs 2 free");
});

test("a host that stops answering loses its sandboxes and is terminated", async () => {
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  await db.query("update sandboxes set status = 'gone'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  const ec2 = new FakeEc2(sbx);
  const p = new VmProvider({ db, config: config(), sandboxd: sbx, ec2, hostTls: { ca: "", cert: "", key: "" } });
  await db.query("update vm_hosts set max_sandboxes = 0 where name = 'hub'");
  await p.controller.pass();
  await p.controller.pass();
  const s = await newRow();
  await p.create(s, "t");
  await ready(s.id);
  const aws = await hostOf(s.id);
  const inst = ec2.launched.at(-1)!;
  sbx.down.add(`https://${inst.userData.host.wgAddress}:7443`);
  await db.query("update vm_hosts set last_seen_at = now() - interval '1 minute' where name = $1", [aws]);
  // the hub just came back (say, a new gateway): the host first gets its peers, it is not judged yet
  p.controller.hubUpSince = Date.now();
  await p.controller.pass();
  assert.equal((await hostRow(aws)).status, "active", "not lost while the hub has only just come back");
  p.controller.hubUpSince = Date.now() - 60_000;
  await p.controller.pass();
  const { rows } = await db.query<{ status: string; release_reason: string }>("select status, release_reason from sandboxes where id = $1", [s.id]);
  assert.deepEqual(rows[0], { status: "gone", release_reason: "host lost" });
  assert.equal((await hostRow(aws)).status, "gone");
  assert.ok(ec2.terminated.includes(inst.id));
  // while the hub itself is unreachable, AWS hosts are never declared lost (they are reached through it)
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
});

test("a new AWS host stays booting until its JuiceFS mount is there; an active host without it takes no sandbox", async () => {
  await db.query("update vm_hosts set status = 'gone' where kind = 'aws'");
  await db.query("update sandboxes set status = 'gone'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  sbx.hosts.get(HUB)!.info.jfsMounted = null; // the gateway: no JuiceFS, counts as having it
  const ec2 = new FakeEc2(sbx);
  const p = new VmProvider({ db, config: config(), sandboxd: sbx, ec2, hostTls: { ca: "", cert: "", key: "" } });
  const ctl = p.controller;
  await db.query("update vm_hosts set max_sandboxes = 0 where name = 'hub'");
  await ctl.pass();
  assert.equal(ec2.launched.length, 1);
  const aws = ec2.launched[0]!.userData.host.name as string;
  const fake = [...sbx.hosts.values()].find((h) => h.info.name === aws)!;
  // sandboxd answers (wg0 up) but sbx-jfs has not mounted yet: still booting, not placeable, no second host
  fake.info.jfsMounted = false;
  const activeEvents = async () => Number((await db.query<{ n: string }>("select count(*) as n from vm_events where kind = 'active' and host = $1", [aws])).rows[0]!.n);
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "booting");
  assert.equal((await hostRow(aws)).info?.jfsMounted, false);
  assert.equal(ctl.placeable(await hostRow(aws)), false);
  assert.equal(ec2.launched.length, 1, "its slots still count as pending");
  assert.equal(await activeEvents(), 0);
  // mounted: active, the transition recorded once
  fake.info.jfsMounted = true;
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "active");
  assert.equal(ctl.placeable(await hostRow(aws)), true);
  await ctl.pass();
  assert.equal(await activeEvents(), 1);
  const s = await newRow();
  await p.create(s, "t");
  assert.equal(await hostOf(s.id), aws);
  // juicefs crashed (sbx-jfs remounts it): the host stays active, keeps its sandbox, takes no new one
  fake.info.jfsMounted = false;
  await ctl.pass();
  const row = await hostRow(aws);
  assert.equal(row.status, "active");
  assert.equal(ctl.placeable(row), false);
  assert.equal(await hostOf(s.id), aws);
  // a booting host that never mounts is given up after the boot timeout
  fake.info.jfsMounted = true;
  await ctl.pass();
  await db.query("update vm_hosts set status = 'booting', created_at = now() - interval '9 minutes' where name = $1", [aws]);
  fake.info.jfsMounted = false;
  await ctl.pass();
  assert.equal((await hostRow(aws)).status, "gone");
  assert.ok(ec2.terminated.includes(ec2.launched[0]!.id));
  await db.query("update vm_hosts set max_sandboxes = null where name = 'hub'");
});

test("placeable: active, answering, and not reporting its JuiceFS mount missing (older sandboxd: as before)", () => {
  const ctl = new VmProvider({ db, config: config(), sandboxd: new FakeSandboxd(), ec2: null, hostTls: null }).controller;
  const h = (status: HostRow["status"], info: HostRow["info"], seen = new Date()) => ({ name: "a", kind: "aws", status, info, last_seen_at: seen }) as unknown as HostRow;
  assert.equal(ctl.placeable(h("active", { jfsMounted: true })), true);
  assert.equal(ctl.placeable(h("active", { jfsMounted: null })), true, "no JuiceFS on the host (the gateway)");
  assert.equal(ctl.placeable(h("active", {})), true, "a sandboxd that doesn't report it");
  assert.equal(ctl.placeable(h("active", null)), true);
  assert.equal(ctl.placeable(h("active", { jfsMounted: false })), false);
  assert.equal(ctl.placeable(h("booting", { jfsMounted: true })), false);
  assert.equal(ctl.placeable(h("draining", { jfsMounted: true })), false);
  assert.equal(ctl.placeable(h("active", { jfsMounted: true }, new Date(Date.now() - 3600_000))), false, "not answering");
});

test("VmController.pick: the hub first, then the fullest AWS host", () => {
  const h = (name: string, kind: "hub" | "aws") => ({ name, kind }) as HostRow;
  const pick = (s: { host: HostRow; used: number; free: number }[]) => VmController.pick(s)?.host.name;
  assert.equal(pick([{ host: h("hub", "hub"), used: 0, free: 1 }, { host: h("a", "aws"), used: 0, free: 7 }]), "hub");
  assert.equal(pick([{ host: h("hub", "hub"), used: 3, free: 0 }, { host: h("a", "aws"), used: 1, free: 6 }, { host: h("b", "aws"), used: 5, free: 2 }]), "b");
  assert.equal(pick([{ host: h("hub", "hub"), used: 3, free: 0 }]), undefined);
});

test("switching from kubernetes: the previous provider's sandboxes stay listed and are removed through it", async () => {
  await db.query("update sandboxes set status = 'gone'");
  const sbx = new FakeSandboxd();
  sbx.addHost(HUB, "hub", 8, "hub");
  const removed: string[] = [];
  const legacy = {
    kind: "kubernetes" as const,
    create: async () => ({ name: "" }),
    waitReady: async () => ({ url: "", headers: {} }),
    keepAlive: async () => {},
    destroy: async (n: string) => void removed.push(n),
    list: async () => [{ name: "sbx-oldpod", createdAt: Date.now() - 3600_000 }],
  };
  const p = new VmProvider({ db, config: config(), sandboxd: sbx, ec2: null, hostTls: null }, legacy);
  await p.controller.pass();
  const old = await newRow();
  await db.query("update sandboxes set pod = 'sbx-oldpod', status = 'ready' where id = $1", [old.id]); // no host: kubernetes
  const fresh = await newRow();
  await p.create(fresh, "t");
  assert.deepEqual((await p.list()).map((x) => x.name).sort(), ["sbx-oldpod", fresh.pod].sort());
  await p.destroy("sbx-oldpod");
  assert.deepEqual(removed, ["sbx-oldpod"]);
  await p.destroy(fresh.pod);
  assert.deepEqual(removed, ["sbx-oldpod"]);
  assert.ok(!sbx.hosts.get(HUB)!.specs.has(fresh.pod));
});

test("vm_events: the scaler's decisions and migrations are recorded", async () => {
  // the earlier tests launched, packed, migrated and terminated AWS hosts
  const kinds = new Set((await db.query<{ kind: string }>("select distinct kind from vm_events")).rows.map((r) => r.kind));
  for (const k of ["launch", "pack", "terminate", "migration-skipped"]) assert.ok(kinds.has(k), k);
  const m = (await db.query<{ data: any }>("select data from vm_events where kind = 'migration' order by id limit 1")).rows[0]!;
  assert.equal(m.data.to, "hub");
  assert.equal(m.data.pause_ms, 1);
});
