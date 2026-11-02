// The shared budget (docs/CHAT.md, "Budget"): spend from egress's own egress_log, the gate's last known status and
// budget_state, the message staff see, and egress's model route refusing other models and calls once it is used up.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, request, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { HostPolicy } from "../src/egress/allowlist.ts";
import { BudgetGate, budgetMessage, spentSince } from "../src/egress/budget.ts";
import { allowedModels, parseEgressConfig, type BudgetConfig } from "../src/egress/config.ts";
import { EgressLog } from "../src/egress/egresslog.ts";
import { Egress } from "../src/egress/proxy.ts";
import { hermitGetter, SandboxTokens } from "../src/egress/tokens.ts";
import { freshEgressDb } from "./egress-db.ts";
import { FakeHermit } from "./fakes/hermit.ts";

let t: Awaited<ReturnType<typeof freshEgressDb>>;
let up: Server;
let hermit: FakeHermit;
let tokens: SandboxTokens;
let tmp: string;
let upCalls = 0;
let upPort = 0;
const SONNET = "anthropic/claude-sonnet-5.5";

const cfgWith = (budget?: BudgetConfig) =>
  parseEgressConfig({
    product: { name: "YYC AI", locale: "zh-HK" },
    models: [{ id: SONNET, label: "Sonnet 5.5" }],
    defaultModel: SONNET,
    contextTokens: 200000,
    routes: [
      { name: "model", upstream: `http://127.0.0.1:${upPort}/api`, model: true, inject: { Authorization: "Bearer k" } },
      { name: "tools", upstream: "http://127.0.0.1:9", inject: {} },
    ],
    ...(budget ? { budget } : {}),
  });

before(async () => {
  t = await freshEgressDb("budget");
  hermit = new FakeHermit("hb");
  tokens = new SandboxTokens(hermitGetter(await hermit.listen(), "hb"));
  await tokens.start();
  up = createServer((req, res) => {
    upCalls++;
    req.resume();
    req.on("end", () => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ content: [{ type: "text", text: "ok" }], usage: { cost: 0.5 } }));
    });
  });
  await new Promise<void>((r) => up.listen(0, "127.0.0.1", r));
  upPort = (up.address() as AddressInfo).port;
  tmp = mkdtempSync(join(tmpdir(), "budget-"));
});

after(async () => {
  tokens.stop();
  hermit.close();
  up.close();
  await t.drop();
  rmSync(tmp, { recursive: true, force: true });
});

const spend = (usd: number, at = "now()") => t.db.query(`insert into egress_log (at, kind, host, decision, cost_usd) values (${at}, 'model', 'openrouter.ai', 'allow', $1)`, [usd]);

test("the allowed models: the offered ones, the default, the small and the title model", () => {
  assert.deepEqual(allowedModels(cfgWith()), [SONNET]);
  const c = parseEgressConfig({ ...cfgWith(), routes: cfgWith().routes, smallModel: "a/small", titleModel: "a/title" });
  assert.deepEqual(allowedModels(c), [SONNET, "a/small", "a/title"]);
});

test("the budget message names the owner and the limit, in zh-HK or English", () => {
  assert.equal(budgetMessage("zh-HK", 30, "Deyao"), "Deyao 設定了 30 美元的使用上限，目前已用完。");
  assert.equal(budgetMessage("en", 30, "Deyao"), "Deyao has set a USD 30 budget limit and it has now been used up.");
  assert.equal(budgetMessage("en", 12.5, null), "The administrator has set a USD 12.50 budget limit and it has now been used up.");
});

test("spend: model cost from `since` on; the gate's status, budget_state, and a replica seeded from it", async () => {
  await t.db.query("delete from egress_log");
  const since = new Date(Date.now() - 60_000).toISOString();
  await spend(5, "now() - interval '1 day'");
  await spend(1.25);
  await spend(0.5);
  await t.db.query("insert into egress_log (kind, host, decision, cost_usd) values ('http', 'x', 'allow', 100)");
  assert.equal(await spentSince(t.db, since), 1.75);
  const gate = new BudgetGate(t.db, { limitUsd: 2, since, ownerName: "Deyao" }, 0);
  const s = (await gate.status())!;
  assert.equal(s.spentUsd, 1.75);
  assert.equal(s.remainingUsd, 0.25);
  assert.equal(s.exhausted, false);
  gate.noteCost(0.5);
  await new Promise((r) => setTimeout(r, 5));
  assert.equal((await gate.status())!.exhausted, false, "a cost already in the database is not counted twice once read");
  await spend(0.25);
  assert.equal((await gate.status())!.exhausted, true, "spent >= limit is used up");
  await new Promise((r) => setTimeout(r, 100));
  const st = (await t.db.query("select spent_usd from budget_state where since = $1", [since])).rows[0];
  assert.equal(Number(st.spent_usd), 1.75, "budget_state keeps a read status (at most every 10 s)");
  const fresh = new BudgetGate(t.db, { limitUsd: 2, since }, 60_000);
  await fresh.seed();
  assert.equal((await fresh.status())!.spentUsd, 1.75, "a new replica starts from budget_state");
  assert.equal(await new BudgetGate(t.db, undefined).status(), null, "no budget: no limit");
});

test("egress model route: only the configured models; refused with budget_exceeded once the budget is used up", async () => {
  await t.db.query("delete from egress_log");
  let budget: BudgetConfig | undefined = { limitUsd: 1, since: new Date(Date.now() - 1000).toISOString(), ownerName: "Deyao" };
  const make = () => {
    const c = cfgWith(budget);
    const log = new EgressLog(t.db);
    const gate = new BudgetGate(t.db, c.budget, 0);
    gate.attach(log);
    return new Egress({ config: c, log, tokens, policy: new HostPolicy({ allow: [] }), pairTokens: { magpie: "m", hermit: "h" }, cacheDir: join(tmp, `c${Math.random()}`), cacheBytes: 1024 * 1024, budget: gate });
  };
  let egress = make();
  let port = await egress.listen(0, "127.0.0.1");
  const sb = hermit.token();
  const model = (body: unknown): Promise<{ status: number; body: string; headers: IncomingMessage["headers"] }> =>
    new Promise((resolve, reject) => {
      const auth = "Basic " + Buffer.from(`sbx:${sb.token}`).toString("base64");
      const r = request({ host: "127.0.0.1", port, path: "http://model/v1/messages", method: "POST", headers: { "proxy-authorization": auth, "content-type": "application/json" } }, (res) => {
        let b = "";
        res.on("data", (c) => (b += c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, body: b, headers: res.headers }));
      });
      r.on("error", reject);
      r.end(JSON.stringify(body));
    });
  const lastDecision = async () => {
    await egress.log.flush();
    return (await t.db.query("select decision, status from egress_log where kind = 'model' order by id desc limit 1")).rows[0];
  };
  const before = upCalls;
  const opus = await model({ model: "anthropic/claude-opus-5.5", messages: [] });
  assert.equal(opus.status, 400);
  assert.match(JSON.parse(opus.body).error.message, /not offered/);
  assert.deepEqual(await lastDecision(), { decision: "deny:model", status: 400 });
  assert.equal(upCalls, before, "a refused model never reaches OpenRouter");
  assert.equal((await model({ model: "claude-sonnet-5.5", messages: [] })).status, 200, "the id without its provider prefix");
  assert.equal((await model({ model: "claude-opus-5.5", messages: [] })).status, 400);
  await spend(0.6); // the in-flight overshoot: this pushes spend (0.5 + 0.6) past the limit
  const refused = await model({ model: SONNET, messages: [] });
  assert.equal(refused.status, 402);
  const err = JSON.parse(refused.body);
  assert.equal(err.type, "error");
  assert.equal(err.error.type, "budget_exceeded");
  assert.equal(err.error.message, "Deyao 設定了 1 美元的使用上限，目前已用完。");
  assert.deepEqual(await lastDecision(), { decision: "deny:budget", status: 402 });
  assert.equal(upCalls, before + 1);
  await egress.drain();
  budget = undefined;
  egress = make();
  port = await egress.listen(0, "127.0.0.1");
  assert.equal((await model({ model: SONNET, messages: [] })).status, 200, "no budget: no limit");
  await egress.drain();
  await egress.log.close(1000);
});
