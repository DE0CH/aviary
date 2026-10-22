// The operator page (docs/CHAT.md, "Operator page"): web's /internal/ops/* refuses anyone but config admins (with the
// UI pods' bearer token), and the snapshot reports sandboxes, hosts, the scaler, vm_events, spend and egress errors.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { after, before, test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { healthOf, OpsSnapshot, safeZone } from "../src/web/ops.ts";
import type { ClusterReader } from "../src/web/k8s.ts";
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

// the service's Deployments as the Kubernetes API reports them: web fine, egress rolling, tools with one pod down, no ui
const ready = (name: string, app: string, ok: boolean, restarts = 0) => ({
  metadata: { name, labels: { app }, creationTimestamp: new Date(Date.now() - 600_000).toISOString() },
  spec: { nodeName: "yyc-pool-small-worker1" },
  status: { phase: "Running", conditions: [{ type: "Ready", status: ok ? "True" : "False" }], containerStatuses: [{ restartCount: restarts, state: ok ? { running: {} } : { waiting: { reason: "CrashLoopBackOff" } } }] },
});
const cluster: ClusterReader = {
  async listDeployments(ns) {
    assert.equal(ns, "jarvis");
    return [
      { metadata: { name: "web", generation: 3 }, spec: { replicas: 2 }, status: { observedGeneration: 3, replicas: 2, readyReplicas: 2, updatedReplicas: 2 } },
      { metadata: { name: "egress", generation: 5 }, spec: { replicas: 2 }, status: { observedGeneration: 5, replicas: 3, readyReplicas: 2, updatedReplicas: 1 } },
      { metadata: { name: "tools", generation: 1 }, spec: { replicas: 2 }, status: { observedGeneration: 1, replicas: 2, readyReplicas: 1, updatedReplicas: 2 } },
      { metadata: { name: "cloudflared", generation: 1 }, spec: { replicas: 2 }, status: { observedGeneration: 1, replicas: 2, readyReplicas: 2, updatedReplicas: 2 } },
    ];
  },
  async listPods(ns, sel) {
    assert.match(sel, /^app in \(web,egress,tools,ui,cloudflared\)$/);
    return [ready("web-a-1", "web", true), ready("web-a-2", "web", true), ready("tools-b-1", "tools", true), ready("tools-b-2", "tools", false, 4)] as any;
  },
};

test("the snapshot: now (service, hosts, sandboxes, scaler, budget, recent errors, health) and history (hosts gone, events, starts and releases, spend per day/person/model, egress errors, conversations)", async () => {
  const db = pg.db;
  const config = parseConfig(raw(true));
  const provider = new VmProvider({ db, config, sandboxd: {} as any, ec2: null, hostTls: null });
  const ops = new OpsSnapshot({ config, db, dbUrl: pg.url, sandboxProvider: provider, namespace: "jarvis", drainSeconds: 5, cluster });
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
  await db.query(
    `insert into ops_events (kind, ok, ms, sandbox_id, conversation_id, host, detail) values
       ('sandbox_start', true, 4200, $1, $2, 'aws-1', '{}'), ('sandbox_start', false, 900, null, $3, null, '{"reason": "pool full"}')`,
    [sb1, conv1, conv3],
  );
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
  // a gone host is history only
  assert.equal(host("aws-0"), undefined);
  assert.equal(d.hostsGone.count24h, 1);
  assert.equal(d.hostsGone.list[0].name, "aws-0");
  assert.equal(d.hostsGone.list[0].goneReason, "empty");
  // scaler
  assert.equal(d.scaler.plan.demand, 1);
  assert.equal(d.scaler.hints[0].who, B);
  assert.equal(d.scaler.hints[0].counted, true);
  // events (newest first) with the sandbox's owner
  assert.equal(d.events[0].kind, "migration");
  assert.equal(d.events[0].owner, A);
  // service: per role, with pods; ui has no Deployment here
  const role = (n: string) => d.service.deployments.find((x: any) => x.name === n);
  assert.equal(d.service.available, true);
  assert.deepEqual(d.service.deployments.map((x: any) => x.name), ["web", "egress", "tools", "ui", "cloudflared"]);
  assert.equal(role("web").state, "ok");
  assert.equal(role("web").pods.length, 2);
  assert.equal(role("egress").state, "warming");
  assert.equal(role("tools").state, "problem");
  assert.equal(role("tools").pods.find((p: any) => !p.ready).phase, "CrashLoopBackOff");
  assert.equal(role("tools").pods.find((p: any) => !p.ready).restarts, 4);
  assert.equal(role("ui").missing, true);
  // sandbox starts and releases, newest first, with timings
  assert.equal(d.starts.summary.n, 2);
  assert.equal(d.starts.summary.failed, 1);
  const startOk = d.starts.list.find((x: any) => x.what === "start" && x.ok);
  assert.equal(startOk.ms, 4200);
  assert.equal(startOk.owner, A);
  const startBad = d.starts.list.find((x: any) => x.what === "start" && !x.ok);
  assert.equal(startBad.reason, "pool full");
  assert.equal(startBad.owner, B, "a failed start's person comes from its conversation");
  assert.equal(d.starts.list.filter((x: any) => x.what === "release").length, 2);
  for (let i = 1; i < d.starts.list.length; i++) assert.ok(d.starts.list[i - 1].at >= d.starts.list[i].at, "newest first");
  // errors of the last 15 minutes: the failed start and the 402
  assert.equal(d.recent.failedStarts, 1);
  assert.equal(d.recent.modelErrors, 1);
  assert.equal(d.recent.blocked, 1);
  assert.ok(d.recent.list.some((x: any) => x.what === "sandbox start failed"));
  // the verdict: problems first (tools, ui, the unreachable retiring host is warming, recent errors)
  assert.equal(d.health.state, "problem");
  const texts = d.health.findings.map((x: any) => `${x.level} ${x.text}`);
  assert.ok(texts.includes("problem tools: 1 of 2 pods ready"), texts.join(" | "));
  assert.ok(texts.includes("problem ui: no Deployment"));
  assert.ok(texts.some((x: string) => x.startsWith("warming egress: rolling out")));
  assert.ok(texts.some((x: string) => x.startsWith("warming Host aws-2 retiring")));
  assert.ok(texts.some((x: string) => x.startsWith("problem 1 sandbox start failed")));
  assert.equal(d.health.findings[0].level, "problem");
  // spend per day (Hong Kong days in the page; UTC here)
  assert.equal(d.budget.byDay.length >= 1, true);
  assert.equal(Math.round(d.budget.byDay.reduce((a: number, x: any) => a + x.usd, 0) * 100) / 100, 9.85);
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

test("health: all fine, warming, watch, problem", () => {
  const at = new Date().toISOString();
  const svc = (state: string) => ({ available: true, deployments: [{ name: "web", state, desired: 2, ready: 2, updated: 2 }] });
  const gw = { name: "gw", gateway: true, reachable: true, status: "active", launchedAt: at, lastSeenAt: at };
  assert.deepEqual(healthOf({ at, errors: {}, service: svc("ok"), hosts: { list: [gw] }, sandboxes: { list: [] } }), { state: "ok", findings: [] });
  const booting = { name: "aws-9", status: "booting", reachable: false, launchedAt: new Date(Date.now() - 30_000).toISOString() };
  assert.equal(healthOf({ at, errors: {}, service: svc("ok"), hosts: { list: [gw, booting] } }).state, "warming");
  const slow = { ...booting, launchedAt: new Date(Date.now() - 600_000).toISOString() };
  assert.equal(healthOf({ at, errors: {}, hosts: { list: [gw, slow] } }).state, "problem");
  assert.equal(healthOf({ at, errors: {}, budget: { status: { spentUsd: 85, limitUsd: 100, exhausted: false } } }).state, "watch");
  assert.equal(healthOf({ at, errors: {}, budget: { status: { spentUsd: 100, limitUsd: 100, exhausted: true } } }).state, "problem");
  assert.equal(healthOf({ at, errors: {}, hosts: { list: [{ ...gw, reachable: false }] } }).findings[0]?.about, "gateway");
  assert.equal(healthOf({ at, errors: { egress: "timeout" } }).state, "problem");
  assert.equal(healthOf({ at, errors: {}, sandboxes: { list: [{ status: "starting", ageS: 20, owner: "a@b" }] } }).state, "warming");
});

test("the page keeps now and history apart", async () => {
  const html = await (await get("/internal/ops/", as(A))).text();
  assert.match(html, /id="now"/);
  assert.match(html, /id="history"/);
  assert.match(html, /History \(log\)/);
  assert.match(html, /Asia\/Hong_Kong/);
});
