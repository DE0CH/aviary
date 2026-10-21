// The operator page (docs/CHAT.md, "Operator page"): web's /internal/ops/* refuses anyone but config admins (with the
// UI pods' bearer token), and the snapshot reports sandboxes, hosts, the scaler, vm_events, spend and egress errors.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { after, before, test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { OpsSnapshot, safeZone } from "../src/web/ops.ts";
import { noteSandboxDemand } from "../src/web/sandbox.ts";
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
import { VmProvider } from "../src/web/sandbox-vm.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org"; // admin
const B = "bob@example.org";
const CC_TOKEN = "cc-token-ops";
const GiB = 1024 ** 3;

let pg: Awaited<ReturnType<typeof freshDb>>;
let web: WebServer;
let base: string;

const raw = (vm: boolean) => ({
  product: { name: "YYC AI", locale: "zh-HK" },
  publicUrl: "https://ai.example.org",
  access: { teamDomain: "t.cloudflareaccess.com", audience: "aud" },
  users: [A, B, "carol@example.org"],
  admins: [A],
  models: [{ id: "anthropic/claude-sonnet-5.5", label: "Sonnet" }],
  defaultModel: "anthropic/claude-sonnet-5.5",
  systemPromptFile: "/dev/null",
  budget: { limitUsd: 30, since: new Date(Date.now() - 3600_000).toISOString(), ownerName: "Deyao" },
  sandbox: vm
    ? {
        provider: "vm", image: "sandbox:test", memory: "1Gi", cpu: "1", maxActive: 10,
        vm: {
          hub: { name: "gw", url: "https://10.201.0.1:7443", endpoint: "203.0.113.1:51820" }, cpuFeatures: "fpu", spareSlots: 0,
          aws: { region: "eu-central-1", launchTemplate: "lt", maxHosts: 3, emptyMinutes: 5, instance: "test" },
        },
      }
    : { image: "sandbox:test", maxActive: 2 },
});

before(async () => {
  process.env.JARVIS_CC_TOKEN = CC_TOKEN;
  pg = await freshDb("ops");
  const config = parseConfig(raw(false));
  const k8s = new FakeK8s();
  const deps: WebDeps = { config, db: pg.db, dbUrl: pg.url, sandboxProvider: undefined as any, namespace: "jarvis", drainSeconds: 5 };
  deps.sandboxProvider = new KubernetesProvider({ k8s, config, egressNamespace: "jarvis", agentUrlFor: k8s.agentUrlFor });
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  await web.close();
  await pg.drop();
});

const get = (path: string, headers: Record<string, string> = {}) => fetch(base + path, { headers });
const as = (user: string | null, token = CC_TOKEN) => ({ authorization: `Bearer ${token}`, ...(user ? { "x-jarvis-user": user } : {}) });

test("only an admin with the UI pods' token gets the page and the snapshot", async () => {
  assert.equal((await get("/internal/ops/")).status, 401);
  assert.equal((await get("/internal/ops/", as(A, "wrong"))).status, 401);
  assert.equal((await get("/internal/ops/", as(null))).status, 403);
  assert.equal((await get("/internal/ops/", as(B))).status, 403, "a user who is not an admin");
  assert.equal((await get("/internal/ops/snapshot", as("mallory@example.org"))).status, 403);
  const page = await get("/internal/ops/", as(" Alice@Example.org "));
  assert.equal(page.status, 200);
  assert.match(page.headers.get("content-type") || "", /text\/html/);
  assert.match(page.headers.get("content-security-policy") || "", /frame-ancestors 'none'/);
  assert.match(await page.text(), /YYC AI Ops/);
  const snap = await get("/internal/ops/snapshot?tz=Asia/Hong_Kong", as(A));
  assert.equal(snap.status, 200);
  const d = (await snap.json()) as any;
  assert.equal(d.tz, "Asia/Hong_Kong");
  assert.deepEqual(d.errors, {});
  assert.equal((await fetch(base + "/internal/ops/snapshot", { method: "POST", headers: as(A) })).status, 405);
  assert.equal((await get("/internal/ops/other", as(A))).status, 404);
});

test("safeZone keeps zone names and refuses anything else", () => {
  assert.equal(safeZone("Europe/London"), "Europe/London");
  assert.equal(safeZone("America/Argentina/Buenos_Aires"), "America/Argentina/Buenos_Aires");
  assert.equal(safeZone("x'; drop table"), "Asia/Hong_Kong");
  assert.equal(safeZone(""), "Asia/Hong_Kong");
});

test("the snapshot: sandboxes, hosts with slots, scaler plan and hints, events, spend per person and model, egress errors, conversations", async () => {
  const db = pg.db;
  const config = parseConfig(raw(true));
  const provider = new VmProvider({ db, config, sandboxd: {} as any, ec2: null, hostTls: null });
  const ops = new OpsSnapshot({ config, db, dbUrl: pg.url, sandboxProvider: provider, namespace: "jarvis", drainSeconds: 5 });
  await db.query(
    `insert into vm_hosts (name, kind, url, wg_ip, status, cpus, mem_total, mem_reserve, instance_id, last_seen_at, info) values
       ('gw', 'hub', 'https://10.201.0.1:7443', '10.201.0.1', 'active', null, null, null, null, now(), '{"gateway": true}'),
       ('aws-1', 'aws', 'https://10.201.0.2:7443', '10.201.0.2', 'active', 4, $1, $2, 'i-1', now(), '{"memAvailable": 8000000000, "loadAvg": [0.5, 0.4, 0.3]}'),
       ('aws-2', 'aws', 'https://10.201.0.3:7443', '10.201.0.3', 'retiring', 4, $1, $2, 'i-2', now() - interval '5 minutes', null)`,
    [15.5 * GiB, GiB],
  );
  await db.query("insert into vm_hosts (name, kind, url, wg_ip, status, gone_at, gone_reason) values ('aws-0', 'aws', 'u', '10.201.0.9', 'gone', now() - interval '1 hour', 'empty')");
  const conv1 = randomUUID(), conv2 = randomUUID(), conv3 = randomUUID();
  const sb1 = randomUUID(), sb2 = randomUUID(), sb3 = randomUUID();
  const sbx = (id: string, conv: string, owner: string, status: string, host: string | null, reason: string | null) =>
    db.query(
      `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, host, owner, release_reason, released_at)
       values ($1, $2, $3, $4, 'x', $5, now() + interval '1 hour', $6, $7, $8, case when $5 = 'gone' then now() end)`,
      [id, conv, `sbx-${id.slice(0, 8)}`, `h-${id}`, status, host, owner, reason],
    );
  await sbx(sb1, conv1, A, "ready", "aws-1", null);
  await sbx(sb2, conv2, B, "gone", "aws-1", "deleted");
  await sbx(sb3, conv3, B, "gone", "aws-2", "idle");
  await db.query("insert into vm_events (kind, host, sandbox_id, data) values ('launch', 'aws-1', null, '{\"reason\": \"scaler\"}'), ('migration', 'aws-1', $1, '{\"pause_ms\": 120}')", [sb1]);
  await noteSandboxDemand(db, B);
  await db.query(
    `insert into egress_log (kind, host, status, decision, cost_usd, sandbox_id, conversation_id, model, at) values
       ('model', 'openrouter.ai', 200, 'allow', 0.5, $1, $3, 'anthropic/claude-sonnet-5.5', now()),
       ('model', 'openrouter.ai', 200, 'allow', 0.25, $2, $4, 'anthropic/claude-sonnet-5.5', now()),
       ('model', 'openrouter.ai', 200, 'allow', 0.1, $2, $4, null, now()),
       ('model', 'openrouter.ai', 402, 'deny:budget', null, $2, $4, 'anthropic/claude-sonnet-5.5', now()),
       ('connect', 'pastebin.com', 403, 'deny:denylist', null, $1, $3, null, now()),
       ('model', 'openrouter.ai', 200, 'allow', 9, $1, $3, 'anthropic/claude-sonnet-5.5', now() - interval '3 hours')`,
    [sb1, sb2, conv1, conv2],
  );

  const d = await ops.get("UTC");
  assert.deepEqual(d.errors, {});
  assert.equal(d.provider, "vm");
  // live sandboxes
  assert.equal(d.sandboxes.list.length, 1);
  assert.equal(d.sandboxes.list[0].owner, A);
  assert.equal(d.sandboxes.list[0].host, "aws-1");
  assert.equal(d.sandboxes.list[0].migrations, 1);
  // hosts: the gateway without slots, AWS with slots and sandbox counts, the retiring one flagged, the gone one listed
  const host = (n: string) => d.hosts.list.find((h: any) => h.name === n);
  assert.equal(host("gw").gateway, true);
  assert.equal(host("gw").slots, null);
  assert.equal(host("aws-1").sandboxes, 1);
  assert.equal(host("aws-1").slots, 12);
  assert.equal(host("aws-1").reachable, true);
  assert.equal(host("aws-2").retiring, true);
  assert.equal(host("aws-2").reachable, false);
  assert.equal(host("aws-0").status, "gone");
  assert.equal(host("aws-0").goneReason, "empty");
  // scaler
  assert.equal(d.scaler.plan.demand, 1);
  assert.equal(d.scaler.hints[0].who, B);
  assert.equal(d.scaler.hints[0].counted, true);
  // events (newest first) with the sandbox's owner
  assert.equal(d.events[0].kind, "migration");
  assert.equal(d.events[0].owner, A);
  // budget: the 3-hour-old call is before `since`
  assert.equal(d.budget.status.spentUsd, 0.85);
  assert.equal(d.budget.status.limitUsd, 30);
  const user = (e: string) => d.budget.byUser.find((u: any) => u.owner === e);
  assert.equal(user(B).sinceBudget, 0.35);
  assert.equal(user(A).sinceBudget, 0.5);
  const model = (m: string) => d.budget.byModel.find((u: any) => u.model === m);
  assert.ok(model("(not recorded)"));
  assert.equal(model("(not recorded)").sinceBudget, 0.1);
  // egress errors: the 402 and the blocked host
  assert.ok(d.egress.byStatus.some((x: any) => x.status === "402" && x.decision === "deny:budget"));
  assert.deepEqual(d.egress.deniedHosts.map((x: any) => [x.host, x.why]), [["pastebin.com", "denylist"]]);
  assert.equal(d.egress.recent.length, 2);
  // conversations per person: every config user, with none too
  const p = (e: string) => d.people.find((u: any) => u.owner === e);
  assert.equal(p(A).total, 1);
  assert.equal(p(A).live, 1);
  assert.equal(p(B).total, 2);
  assert.equal(p(B).deleted, 1);
  assert.equal(p("carol@example.org").total, 0);
});
