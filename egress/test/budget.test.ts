// The shared budget and the single-model rules (docs/CHAT.md, "Budget", "Models"): config parsing, spend from
// egress_log, admin overrides with audit rows, egress /x/model refusing other models and calls once the budget is used
// up, and the UI's small rules (model picker, budget event). The web role's side (turn refused before claw, a running
// turn stopped, the admin API) is in web.test.ts.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, request, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { BudgetGate, budgetAudit, budgetMessage, budgetStatus, changeBudget, effectiveBudget } from "../src/common/budget.ts";
import { allowedModels, effectiveModel, parseConfig, type BudgetConfig } from "../src/common/config.ts";
import { sha256hex } from "../src/common/tokens.ts";
import { Identities } from "../src/egress/identity.ts";
import { Egress } from "../src/egress/proxy.ts";
import { budgetOf, showModelPicker, usdAmount } from "../ui/src/rules.ts";
import { freshDb } from "./pg.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let up: Server;
let egress: Egress;
let port: number;
let tmp: string;
let upCalls = 0;
const conv = randomUUID();
const turnId = randomUUID();
const turnToken = "budget-turn-token";
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
  s3: { endpoint: "http://s3", region: "r", bucket: "b" },
};

before(async () => {
  t = await freshDb("budget");
  await t.db.query("insert into users (email) values ('a@x.org')");
  await t.db.query("insert into conversations (id, owner, model) values ($1, 'a@x.org', $2)", [conv, SONNET]);
  await t.db.query("insert into turns (id, conversation_id, status, token_hash, model) values ($1,$2,'running',$3,$4)", [turnId, conv, sha256hex(turnToken), SONNET]);
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
    routes: [{ name: "model", upstream: `http://127.0.0.1:${upPort}/api`, auth: "turn", inject: { Authorization: "Bearer k" } }],
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
      { host: "127.0.0.1", port, path: "/x/model/v1/messages", method: "POST", headers: { authorization: `Bearer ${turnToken}`, "content-type": "application/json" } },
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

const spend = (usd: number, at = "now()") => t.db.query(`insert into egress_log (at, kind, host, decision, cost_usd, turn_id) values (${at}, 'model', 'openrouter.ai', 'allow', $1, $2)`, [usd, turnId]);
const lastDecision = async () => {
  await egress.log.flush();
  return (await t.db.query("select decision, status from egress_log where kind = 'model' order by id desc limit 1")).rows[0];
};

test("config: budget is optional and validated; one model; a removed model continues on the default", () => {
  const c = parseConfig(BASE);
  assert.equal(c.budget, undefined);
  assert.deepEqual(allowedModels(c), [SONNET]);
  assert.equal(effectiveModel(c, "anthropic/claude-opus-5.5"), SONNET, "an Opus conversation continues on Sonnet");
  assert.equal(effectiveModel(c, SONNET), SONNET);
  assert.equal(effectiveModel(c, null), SONNET);
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
  assert.equal(s.source, "config");
  assert.equal((await budgetStatus(t.db, { limitUsd: 1.75, since }))!.exhausted, true, "spent >= limit is used up");
  assert.equal(await budgetStatus(t.db, undefined), null, "no budget, no override: no limit");
  assert.equal(await gate.status(), null);
});

test("admin overrides: reset, new limit, back to config; each an audit row; a config edit takes over again", async () => {
  await t.db.query("delete from egress_log; delete from kv; delete from ops_events");
  const cfg: BudgetConfig = { limitUsd: 1, since: new Date(Date.now() - 60_000).toISOString(), ownerName: "Deyao" };
  await spend(1.5);
  assert.equal((await budgetStatus(t.db, cfg))!.exhausted, true);
  // raise the limit: spend so far still counts
  let s = (await changeBudget(t.db, cfg, "deyao@x.org", { action: "set", limitUsd: 5 }))!;
  assert.deepEqual([s.limitUsd, s.spentUsd, s.exhausted, s.source, s.since], [5, 1.5, false, "override", cfg.since]);
  // reset: spend counts from now
  await new Promise((r) => setTimeout(r, 20));
  s = (await changeBudget(t.db, cfg, "deyao@x.org", { action: "reset" }))!;
  assert.deepEqual([s.limitUsd, s.spentUsd, s.exhausted], [5, 0, false]);
  assert.ok(Date.parse(s.since) > Date.parse(cfg.since));
  // config edited after the override (e.g. a new `since` in config.json): config wins
  const edited = { ...cfg, since: new Date().toISOString(), limitUsd: 30 };
  assert.equal((await effectiveBudget(t.db, edited))!.source, "config");
  // back to config
  s = (await changeBudget(t.db, cfg, "deyao@x.org", { action: "clear" }))!;
  assert.deepEqual([s.source, s.limitUsd, s.exhausted], ["config", 1, true]);
  await assert.rejects(changeBudget(t.db, cfg, "deyao@x.org", { action: "set", limitUsd: -3 }), RangeError);
  const audit = await budgetAudit(t.db);
  assert.deepEqual(audit.map((a) => a.action), ["clear", "reset", "set"]);
  assert.equal(audit[2]!.actor, "deyao@x.org");
  assert.deepEqual(audit[2]!.after, { limitUsd: 5, since: cfg.since });
  assert.equal((audit[2]!.before as any).spentUsd, 1.5);
  // an override with no config budget at all is a limit too
  await changeBudget(t.db, undefined, "deyao@x.org", { action: "set", limitUsd: 0 });
  assert.equal((await gate.status())!.exhausted, true);
  await t.db.query("delete from kv");
});

test("egress /x/model: only the configured models; refused with budget_exceeded once the budget is used up", async () => {
  await t.db.query("delete from egress_log; delete from kv");
  budgetCfg = { limitUsd: 1, since: new Date(Date.now() - 1000).toISOString(), ownerName: "Deyao" };
  const before = upCalls;
  const opus = await model({ model: "anthropic/claude-opus-5.5", messages: [] });
  assert.equal(opus.status, 400);
  assert.match(JSON.parse(opus.body).error.message, /not offered/);
  assert.deepEqual(await lastDecision(), { decision: "deny:model", status: 400 });
  assert.equal(upCalls, before, "a refused model never reaches OpenRouter");

  // claw's own form: the id without the provider prefix
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

test("UI rules: no model picker for one model; a budget event's details", () => {
  assert.equal(showModelPicker([{ id: SONNET }]), false);
  assert.equal(showModelPicker([{ id: SONNET }, { id: "x" }]), true);
  assert.deepEqual(budgetOf({ code: "budget", limitUsd: 30, ownerName: "Deyao" }), { limitUsd: 30, ownerName: "Deyao" });
  assert.equal(budgetOf({ message: "boom" }), undefined);
  assert.equal(usdAmount(30), "30");
  assert.equal(usdAmount(7.5), "7.50");
});
