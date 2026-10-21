// The shared budget and the model rules (docs/CLAUDE-CODE.md, "Budget"): config parsing, spend from egress_log, and
// egress's model route refusing other models and calls once the budget is used up.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, request, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { BudgetGate, budgetMessage, budgetStatus } from "../src/common/budget.ts";
import { allowedModels, parseConfig, type BudgetConfig } from "../src/common/config.ts";
import { sha256hex } from "../src/common/tokens.ts";
import { Identities } from "../src/egress/identity.ts";
import { Egress } from "../src/egress/proxy.ts";
import { freshDb } from "./pg.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let up: Server;
let egress: Egress;
let port: number;
let tmp: string;
let upCalls = 0;
const conv = randomUUID();
const sandboxId = randomUUID();
const sandboxToken = "budget-sandbox-token";
const SONNET = "anthropic/claude-sonnet-5.5";
let budgetCfg: BudgetConfig | undefined;
let gate: BudgetGate;

const BASE = {
  product: { name: "YYC AI", locale: "zh-HK" },
  publicUrl: "https://ai.example.org",
  access: { teamDomain: "t", audience: "a" },
  users: ["a@x.org"],
  models: [{ id: SONNET, label: "Sonnet 5.5" }],
  defaultModel: SONNET,
  titleModel: SONNET,
  systemPromptFile: "/dev/null",
  sandbox: { image: "x" },
};

before(async () => {
  t = await freshDb("budget");
  await t.db.query(
    "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner, user_key) values ($1,$2,'sbx-budget',$3,'x','ready',now() + interval '1 hour','a@x.org','u1')",
    [sandboxId, conv, sha256hex(sandboxToken)],
  );
  up = createServer((req, res) => {
    upCalls++;
    req.resume();
    req.on("end", () => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ content: [{ type: "text", text: "ok" }], usage: { cost: 0.5 } }));
    });
  });
  await new Promise<void>((r) => up.listen(0, "127.0.0.1", r));
  const upPort = (up.address() as AddressInfo).port;
  tmp = mkdtempSync(join(tmpdir(), "budget-"));
  // ttl 0: every call re-reads the budget, so the test sees each change at once
  gate = new BudgetGate(t.db, undefined, 0);
  const opts: any = {
    db: t.db,
    defaultAllowedHosts: [],
    routes: [{ name: "claude", upstream: `http://127.0.0.1:${upPort}/api`, model: true, inject: { Authorization: "Bearer k" } }],
    cacheDir: join(tmp, "cache"),
    cacheBytes: 1024 * 1024,
    selfHosts: ["egress.test"],
    identities: new Identities(t.db, 0),
    models: allowedModels(parseConfig(BASE)),
    budget: () => budgetStatus(t.db, budgetCfg),
    locale: "zh-HK",
  };
  egress = new Egress(opts);
  port = await egress.listen(0, "127.0.0.1");
});

after(async () => {
  await egress.drain(100);
  up.close();
  await t.drop();
  rmSync(tmp, { recursive: true, force: true });
});

function model(body: unknown): Promise<{ status: number; body: string; headers: IncomingMessage["headers"] }> {
  return new Promise((resolve, reject) => {
    const r = request(
      { host: "127.0.0.1", port, path: "/x/claude/v1/messages", method: "POST", headers: { authorization: `Bearer ${sandboxToken}`, "content-type": "application/json" } },
      (res) => {
        let b = "";
        res.on("data", (c) => (b += c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, body: b, headers: res.headers }));
      },
    );
    r.on("error", reject);
    r.end(JSON.stringify(body));
  });
}

const spend = (usd: number, at = "now()") => t.db.query(`insert into egress_log (at, kind, host, decision, cost_usd, sandbox_id) values (${at}, 'model', 'openrouter.ai', 'allow', $1, $2)`, [usd, sandboxId]);
const lastDecision = async () => {
  await egress.log.flush();
  return (await t.db.query("select decision, status from egress_log where kind = 'model' order by id desc limit 1")).rows[0];
};

test("config: budget is optional and validated; the allowed models", () => {
  const c = parseConfig(BASE);
  assert.equal(c.budget, undefined);
  assert.deepEqual(allowedModels(c), [SONNET]);
  const b = parseConfig({ ...BASE, budget: { limitUsd: 30, since: "2026-10-21T10:00:00+08:00", ownerName: "Deyao" } });
  assert.deepEqual(b.budget, { limitUsd: 30, since: "2026-10-21T02:00:00.000Z", ownerName: "Deyao" });
  assert.throws(() => parseConfig({ ...BASE, budget: { limitUsd: -1, since: "2026-10-21T00:00:00Z" } }), /limitUsd/);
  assert.throws(() => parseConfig({ ...BASE, budget: { limitUsd: 30, since: "soon" } }), /since/);
  assert.throws(() => parseConfig({ ...BASE, budget: { limitUsd: "30", since: "2026-10-21T00:00:00Z" } }), /limitUsd/);
  assert.deepEqual(allowedModels(parseConfig({ ...BASE, titleModel: "anthropic/claude-haiku-5.5" })), [SONNET, "anthropic/claude-haiku-5.5"]);
});

test("the budget message names the owner and the limit, in zh-HK or English", () => {
  assert.equal(budgetMessage("zh-HK", 30, "Deyao"), "Deyao 設定了 30 美元的使用上限，目前已用完。");
  assert.equal(budgetMessage("en", 30, "Deyao"), "Deyao has set a USD 30 budget limit and it has now been used up.");
  assert.equal(budgetMessage("en", 12.5, null), "The administrator has set a USD 12.50 budget limit and it has now been used up.");
});

test("spend: model cost from `since` on (older calls and other kinds don't count)", async () => {
  await t.db.query("delete from egress_log");
  const since = new Date(Date.now() - 60_000).toISOString();
  await spend(5, "now() - interval '1 day'");
  await spend(1.25);
  await spend(0.5);
  await t.db.query("insert into egress_log (kind, host, decision, cost_usd) values ('http', 'x', 'allow', 100)");
  const s = (await budgetStatus(t.db, { limitUsd: 2, since, ownerName: "Deyao" }))!;
  assert.equal(s.spentUsd, 1.75);
  assert.equal(s.remainingUsd, 0.25);
  assert.equal(s.exhausted, false);
  assert.equal((await budgetStatus(t.db, { limitUsd: 1.75, since }))!.exhausted, true, "spent >= limit is used up");
  assert.equal(await budgetStatus(t.db, undefined), null, "no budget: no limit");
  assert.equal(await gate.status(), null);
});

test("egress model route: only the configured models; refused with budget_exceeded once the budget is used up", async () => {
  await t.db.query("delete from egress_log");
  budgetCfg = { limitUsd: 1, since: new Date(Date.now() - 1000).toISOString(), ownerName: "Deyao" };
  const before = upCalls;
  const opus = await model({ model: "anthropic/claude-opus-5.5", messages: [] });
  assert.equal(opus.status, 400);
  assert.match(JSON.parse(opus.body).error.message, /not offered/);
  assert.deepEqual(await lastDecision(), { decision: "deny:model", status: 400 });
  assert.equal(upCalls, before, "a refused model never reaches OpenRouter");

  // the id without the provider prefix (as Claude Code may send it)
  assert.equal((await model({ model: "claude-sonnet-5.5", messages: [] })).status, 200);
  assert.equal((await model({ model: "claude-opus-5.5", messages: [] })).status, 400);
  const ok = await model({ model: SONNET, messages: [] });
  assert.equal(ok.status, 200);
  assert.equal(upCalls, before + 2);
  await egress.log.flush();
  await t.db.query("delete from egress_log where kind = 'model' and decision = 'allow' and cost_usd = 0.5 and id <> (select min(id) from egress_log where cost_usd = 0.5)");
  await spend(0.6); // the in-flight overshoot: this pushes spend (0.5 + 0.6) past the limit
  const refused = await model({ model: SONNET, messages: [] });
  assert.equal(refused.status, 402);
  const err = JSON.parse(refused.body);
  assert.equal(err.type, "error");
  assert.equal(err.error.type, "budget_exceeded");
  assert.equal(err.error.message, "Deyao 設定了 1 美元的使用上限，目前已用完。");
  assert.deepEqual(await lastDecision(), { decision: "deny:budget", status: 402 });
  assert.equal(upCalls, before + 2);
  budgetCfg = undefined;
  assert.equal((await model({ model: SONNET, messages: [] })).status, 200, "no budget: no limit");
});
