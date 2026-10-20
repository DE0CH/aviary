// The tools role end to end on the real local Postgres, with every outside service faked (test/fakes/tools-upstreams.ts):
// auth by turn token, the MCP methods, each built-in tool, upstream proxying, confirmations, tool_calls logging, and the
// stdio bridge claw uses.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import type { AddressInfo } from "node:net";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { parseConfig } from "../src/common/config.ts";
import { channel, Listener } from "../src/common/db.ts";
import { newToken, sha256hex } from "../src/common/tokens.ts";
import { normalHost } from "../src/tools/builtin.ts";
import { isPrivateAddress, htmlToText } from "../src/tools/netguard.ts";
import { iso } from "../src/tools/providers.ts";
import { createToolsServer, toolList } from "../src/tools/server.ts";
import { toolsLocale } from "../src/tools/strings.ts";
import type { Deps } from "../src/tools/types.ts";
import { pickResponse, Upstream } from "../src/tools/upstream.ts";
import { startFakes, type Fakes } from "./fakes/tools-upstreams.ts";
import { freshDb } from "./pg.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let fakes: Fakes;
let deps: Deps;
let url = "";
let close: () => Promise<void>;
const owner = "staff@yyc.hk";
let conv = "";
let turnId = "";
let token = "";

async function newTurn(status = "running"): Promise<{ id: string; token: string }> {
  const id = randomUUID();
  const tok = newToken();
  await t.db.query("update turns set status = 'done' where conversation_id = $1 and status in ('queued','running','waiting')", [conv]);
  await t.db.query("insert into turns (id, conversation_id, status, token_hash, model) values ($1,$2,$3,$4,'m')", [id, conv, status, sha256hex(tok)]);
  return { id, token: tok };
}

async function rpc(method: string, params: Record<string, unknown> = {}, tok = token): Promise<{ status: number; body: any }> {
  const r = await fetch(`${url}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${tok}`, "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const s = await r.text();
  return { status: r.status, body: s ? JSON.parse(s) : null };
}

const call = async (name: string, args: Record<string, unknown>) => (await rpc("tools/call", { name, arguments: args })).body.result;
const textOf = (r: any) => r.content.map((c: any) => c.text).join("");

before(async () => {
  t = await freshDb("tools");
  fakes = await startFakes();
  await t.db.query("insert into users (email) values ($1)", [owner]);
  conv = randomUUID();
  await t.db.query("insert into conversations (id, owner, model) values ($1,$2,'m')", [conv, owner]);
  ({ id: turnId, token } = await newTurn());
  const config = parseConfig({
    product: { name: "YYC AI" },
    publicUrl: "https://ai.example.org",
    access: { teamDomain: "t.cloudflareaccess.com", audience: "aud" },
    users: [owner],
    admins: ["admin@yyc.hk"],
    models: [{ id: "m", label: "M" }],
    defaultModel: "m",
    systemPromptFile: "/x",
    sandbox: { image: "img" },
    egress: { defaultAllowedHosts: ["pypi.org", ".githubusercontent.com"] },
    tools: {
      exa: { keyEnv: "EXA_KEY", baseUrl: `${fakes.url}/exa` },
      tikhub: { keyEnv: "TIK_KEY", baseUrl: `${fakes.url}/tikhub` },
      wisers: { url: `${fakes.url}/wisers`, accessClientIdEnv: "W_ID", accessClientSecretEnv: "W_SECRET" },
      upstreams: [{ prefix: "yyc", url: `${fakes.url}/mcp-up`, headers: { Authorization: "Bearer ${UP_TOKEN}", "X-YYC-User": "${USER_EMAIL}" } }],
      confirm: ["yyc_send_mail"],
    },
  });
  deps = {
    config,
    db: t.db,
    listener: new Listener(t.url),
    webUrl: `${fakes.url}/web`,
    env: { EXA_KEY: "exa-key", TIK_KEY: "tik-key", W_ID: "cid", W_SECRET: "csecret", UP_TOKEN: "up" },
    fetch: globalThis.fetch,
    confirmTimeoutMs: 2000,
    resolve: async () => ({ address: "127.0.0.1", family: 4 }),
    fetchPorts: "any",
  };
  const ups = config.tools.upstreams.map((u) => new Upstream(u, deps));
  for (const u of ups) await u.refresh("admin@yyc.hk");
  const s = createToolsServer(deps, ups);
  await new Promise<void>((r) => s.server.listen(0, "127.0.0.1", r));
  url = `http://127.0.0.1:${(s.server.address() as AddressInfo).port}`;
  close = () => s.drain(1000);
});

after(async () => {
  await close();
  await deps.listener.close();
  await fakes.close();
  await t.drop();
});

test("auth: a running turn's token only", async () => {
  assert.equal((await rpc("tools/list", {}, "")).status, 401);
  assert.equal((await rpc("tools/list", {}, "nope")).status, 401);
  const old = await newTurn("done");
  assert.equal((await rpc("tools/list", {}, old.token)).status, 401);
  ({ id: turnId, token } = await newTurn());
  assert.equal((await rpc("tools/list")).status, 200);
});

test("MCP: initialize, ping, list, unknown method, notifications, batch", async () => {
  const init = (await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claw" } })).body;
  assert.equal(init.result.protocolVersion, "2025-06-18");
  assert.ok(init.result.capabilities.tools);
  assert.equal((await rpc("initialize", { protocolVersion: "1999-01-01" })).body.result.protocolVersion, "2025-06-18");
  assert.deepEqual((await rpc("ping")).body.result, {});
  assert.equal((await rpc("nope/nope")).body.error.code, -32601);
  const names = (await rpc("tools/list")).body.result.tools.map((x: any) => x.name);
  assert.deepEqual(names, ["web_search", "web_fetch", "news_search", "social_search", "request_network_access", "yyc_posts_search", "yyc_send_mail"]);
  for (const n of names) assert.match(n, /^[a-z0-9_]+$/, "lowercase names (claw's ask rules compare lowercased)");
  const note = await fetch(`${url}/mcp`, { method: "POST", headers: { authorization: `Bearer ${token}` }, body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) });
  assert.equal(note.status, 202);
  const batch = await fetch(`${url}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
    body: JSON.stringify([{ jsonrpc: "2.0", id: 7, method: "ping" }, { jsonrpc: "2.0", method: "notifications/initialized" }]),
  });
  assert.deepEqual(await batch.json(), [{ jsonrpc: "2.0", id: 7, result: {} }]);
});

test("web_search: Exa results as text, cost logged in tool_calls", async () => {
  const r = await call("web_search", { query: "施政報告", since: "2026-10-01", include_domains: ["https://www.info.gov.hk/x"] });
  assert.match(textOf(r), /施政報告 2026[\s\S]*https:\/\/www\.info\.gov\.hk\/a[\s\S]*Published: 2026-10-15/);
  const req = fakes.seen.filter((s) => s.path === "/exa/search").at(-1)!;
  assert.equal(req.body.startPublishedDate, "2026-10-01T00:00:00.000Z");
  assert.deepEqual(req.body.includeDomains, ["www.info.gov.hk"]);
  const { rows } = await t.db.query("select * from tool_calls where tool = 'web_search' order by id desc limit 1");
  assert.equal(rows[0].user_email, owner);
  assert.equal(rows[0].ok, true);
  assert.equal(Number(rows[0].cost_usd), 0.007);
  assert.equal(rows[0].turn_id, turnId);
  const bad = await call("web_search", { query: "x", since: "yesterday" });
  assert.equal(bad.isError, true);
  assert.match(textOf(bad), /YYYY-MM-DD/);
});

test("web_fetch: Exa text first, then the page itself; private addresses refused", async () => {
  fakes.exaHasText = true;
  assert.match(textOf(await call("web_fetch", { url: "https://example.org/a" })), /全文內容/);
  fakes.exaHasText = false;
  // Exa has nothing: the fake server itself is fetched (deps.resolve sends every host to 127.0.0.1)
  const direct = await call("web_fetch", { url: `${fakes.url.replace("127.0.0.1", "page.example")}/nothing-here` });
  assert.match(textOf(direct), /HTTP 404/);
  fakes.exaHasText = true;
  const saved = deps.resolve;
  delete deps.resolve; // the real check
  try {
    const r = await call("web_fetch", { url: "http://127.0.0.1:9/x", raw_html: true });
    assert.equal(r.isError, true);
    assert.match(textOf(r), /non-public address/);
  } finally {
    deps.resolve = saved;
  }
  assert.equal((await call("web_fetch", { url: "file:///etc/passwd" })).isError, true);
});

test("news_search: through the relay with the service token, polling a running job", async () => {
  fakes.relayRunsFor = 2;
  const r = await call("news_search", { query: "施政", period: "30", reason: "test" });
  const s = textOf(r);
  assert.match(s, /45 articles/);
  assert.match(s, /明報 報道[\s\S]*明報 · 2026-10-16 08:00 · 記者[\s\S]*\(print; no web link\)[\s\S]*今日 \*\*施政\*\*/);
  assert.match(s, /https:\/\/hk01\.com\/x/);
  const asked = fakes.seen.filter((x) => x.path.startsWith("/wisers/"));
  assert.equal(asked[0]!.headers["x-yyc-user"], owner);
  assert.match(asked[0]!.path, /q=%E6%96%BD%E6%94%BF&period=30&reason=test/);
  assert.ok(asked.some((x) => x.path.startsWith("/wisers/job/J1?wait=")));
  const failed = await call("news_search", { query: "fail" });
  assert.equal(failed.isError, true);
  assert.match(textOf(failed), /Wisers 沒有回應.*quota may have been used: maybe/);
});

test("social_search: TikHub posts, fallback endpoint, platform check", async () => {
  const th = await call("social_search", { platform: "threads", query: "香港" });
  assert.match(textOf(th), /hkuser[\s\S]*Threads 帖子[\s\S]*threads\.com\/@hkuser\/post\/ABC[\s\S]*likes 12/);
  const wb = await call("social_search", { platform: "weibo", query: "香港" });
  assert.match(textOf(wb), /博主 · 2026-10-15 02:00 UTC[\s\S]*微博 內容[\s\S]*likes 12000/);
  assert.equal((await call("social_search", { platform: "myspace", query: "x" })).isError, true);
  const { rows } = await t.db.query("select cost_usd from tool_calls where tool = 'social_search' and ok order by id desc limit 1");
  assert.equal(Number(rows[0].cost_usd), 0.001);
});

test("upstream MCP: tools proxied with the staff member's identity; a confirm-listed tool waits for approval", async () => {
  const r = await call("yyc_posts_search", { query: "x" });
  assert.equal(textOf(r), `called posts_search for ${owner}`);
  const up = fakes.seen.filter((s) => s.path === "/mcp-up" && s.body?.method === "tools/call").at(-1)!;
  assert.equal(up.headers.authorization, "Bearer up");

  // approved
  fakes.onEvent = async (e) => {
    if (e.type !== "confirmation_request") return;
    await t.db.query("update confirmations set status = 'approved', decided_at = now(), decided_by = $2 where id = $1", [e.id, owner]);
    await t.db.query("select pg_notify($1, 'approved')", [channel("confirm", e.id)]);
  };
  const sent = await call("yyc_send_mail", { to: "x@y.com" });
  assert.equal(textOf(sent), `called Send-Mail for ${owner}`);
  const ev = fakes.events.filter((e) => e.type === "confirmation_request").at(-1);
  assert.match(ev.summary, /x@y\.com/);
  assert.equal(ev.summary, `yyc_send_mail: {"to":"x@y.com"}`);
  assert.equal(ev.detail.tool, "yyc_send_mail");

  // the card's summary follows the instance locale (product.locale)
  deps.config.product.locale = "zh-HK";
  try {
    await call("yyc_send_mail", { to: "z@y.com" });
  } finally {
    deps.config.product.locale = "en";
  }
  const zh = fakes.events.filter((e) => e.type === "confirmation_request").at(-1);
  assert.equal(zh.summary, '執行「yyc_send_mail」：{"to":"z@y.com"}');
  const row = await t.db.query("select summary from confirmations where id = $1", [zh.id]);
  assert.equal(row.rows[0].summary, zh.summary);

  // denied
  fakes.onEvent = async (e) => {
    if (e.type !== "confirmation_request") return;
    await t.db.query("update confirmations set status = 'denied' where id = $1", [e.id]);
    await t.db.query("select pg_notify($1, 'denied')", [channel("confirm", e.id)]);
  };
  const before = fakes.seen.filter((s) => s.body?.method === "tools/call").length;
  const no = await call("yyc_send_mail", { to: "x@y.com" });
  assert.equal(no.isError, true);
  assert.equal(textOf(no), "The user declined.");
  assert.equal(fakes.seen.filter((s) => s.body?.method === "tools/call").length, before, "never forwarded");
  fakes.onEvent = () => {};
});

test("request_network_access: approve adds the host; no answer expires; defaults need no ask", async () => {
  assert.match(textOf(await call("request_network_access", { host: "pypi.org", reason: "x" })), /already allowed/);
  assert.match(textOf(await call("request_network_access", { host: "raw.githubusercontent.com", reason: "x" })), /already allowed/);
  assert.equal((await call("request_network_access", { host: "10.0.0.1", reason: "x" })).isError, true);
  assert.equal((await call("request_network_access", { host: "web.jarvis.svc.cluster.local", reason: "x" })).isError, true);

  fakes.onEvent = async (e) => {
    if (e.type !== "confirmation_request") return;
    await t.db.query("update confirmations set status = 'approved' where id = $1", [e.id]);
    await t.db.query("select pg_notify($1, 'approved')", [channel("confirm", e.id)]);
  };
  const ok = await call("request_network_access", { host: "https://data.gov.hk/en/path", reason: "下載數據" });
  assert.match(textOf(ok), /allowed data\.gov\.hk/);
  assert.equal(fakes.events.filter((e) => e.type === "confirmation_request").at(-1).summary, "Allow the sandbox to reach data.gov.hk?");
  deps.config.product.locale = "zh-HK";
  try {
    assert.match(textOf(await call("request_network_access", { host: "data.example.hk", reason: "x" })), /allowed data\.example\.hk/);
  } finally {
    deps.config.product.locale = "en";
  }
  assert.equal(fakes.events.filter((e) => e.type === "confirmation_request").at(-1).summary, "允許沙盒連接 data.example.hk？");
  const { rows } = await t.db.query("select host, added_by from allowed_hosts where conversation_id = $1", [conv]);
  assert.deepEqual(rows.filter((r) => r.host === "data.gov.hk"), [{ host: "data.gov.hk", added_by: owner }]);
  assert.match(textOf(await call("request_network_access", { host: "data.gov.hk", reason: "x" })), /already allowed/);

  fakes.onEvent = () => {}; // nobody answers: expires after confirmTimeoutMs
  const t0 = Date.now();
  const exp = await call("request_network_access", { host: "example.com", reason: "x" });
  assert.equal(textOf(exp), "No answer from the user.");
  assert.ok(Date.now() - t0 >= 1900);
  const c = await t.db.query("select status from confirmations where detail->>'host' = 'example.com'");
  assert.equal(c.rows[0].status, "expired");
  assert.ok(fakes.events.some((e) => e.type === "confirmation_resolved" && e.status === "expired"));
});

test("jarvis-mcp-bridge: Content-Length framing (claw's) and NDJSON, both ways", async () => {
  const bridge = fileURLToPath(new URL("../src/bin/jarvis-mcp-bridge", import.meta.url));
  const run = (input: Buffer) =>
    new Promise<string>((ok, fail) => {
      const p = spawn(process.execPath, [bridge], { env: { ...process.env, JARVIS_TOOLS_URL: `${url}/mcp`, JARVIS_TURN_TOKEN: token } });
      let out = "";
      p.stdout.on("data", (c) => (out += c));
      p.stderr.on("data", (c) => process.stderr.write(c));
      p.on("exit", () => ok(out));
      p.on("error", fail);
      p.stdin.end(input);
    });
  const frame = (m: unknown) => {
    const b = Buffer.from(JSON.stringify(m));
    return Buffer.concat([Buffer.from(`Content-Length: ${b.length}\r\n\r\n`), b]);
  };
  const out = await run(
    Buffer.concat([
      frame({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claw" } } }),
      frame({ jsonrpc: "2.0", method: "notifications/initialized" }),
      frame({ jsonrpc: "2.0", id: 2, method: "tools/list" }),
      frame({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "web_search", arguments: { query: "香港" } } }),
    ]),
  );
  const msgs: any[] = [];
  let rest = out;
  while (rest) {
    const m = /^Content-Length: (\d+)\r\n\r\n/.exec(rest);
    assert.ok(m, `framed output: ${rest.slice(0, 80)}`);
    const len = Number(m[1]);
    const b = Buffer.from(rest.slice(m[0].length), "utf8");
    msgs.push(JSON.parse(b.subarray(0, len).toString("utf8")));
    rest = b.subarray(len).toString("utf8");
  }
  assert.deepEqual(msgs.map((m) => m.id).sort(), [1, 2, 3]);
  assert.ok(msgs.find((m) => m.id === 2).result.tools.length >= 5);
  assert.match(msgs.find((m) => m.id === 3).result.content[0].text, /施政報告 2026/);

  const lines = await run(Buffer.from(JSON.stringify({ jsonrpc: "2.0", id: 9, method: "ping" }) + "\n"));
  assert.deepEqual(JSON.parse(lines.trim()), { jsonrpc: "2.0", id: 9, result: {} });
});

test("helpers: private addresses, hostnames, dates, SSE answers, HTML text", () => {
  for (const a of ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "100.64.0.1", "169.254.169.254", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "fd42::5"])
    assert.equal(isPrivateAddress(a), true, a);
  for (const a of ["8.8.8.8", "1.1.1.1", "2a01:4f8::1", "::ffff:8.8.8.8"]) assert.equal(isPrivateAddress(a), false, a);
  assert.equal(normalHost("HTTPS://Data.Gov.HK:443/x?y"), "data.gov.hk");
  assert.equal(normalHost("*.gov.hk"), ".gov.hk");
  assert.equal(normalHost("localhost"), null);
  assert.equal(normalHost("1.2.3.4"), null);
  assert.equal(iso(1792000000), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("1792000000000"), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("Wed Oct 15 10:00:00 +0800 2026"), "2026-10-15T02:00:00.000Z");
  assert.equal(iso("garbage"), null);
  assert.equal(pickResponse('event: message\ndata: {"jsonrpc":"2.0","id":4,"result":{"a":1}}\n\n', "text/event-stream", 4).result.a, 1);
  assert.deepEqual(htmlToText("<html><title>T &amp; U</title><script>x()</script><p>一</p><p>二&nbsp;三</p></html>"), { title: "T & U", text: "T & U 一\n二 三" });
  assert.deepEqual(toolList({ ...deps, config: { ...deps.config, tools: { upstreams: [], confirm: [] } } }, []).map((d) => d.name), ["web_fetch", "request_network_access"]);
});

test("confirmation text locale: zh* → zh-HK, anything else → en", () => {
  assert.equal(toolsLocale("zh-HK"), "zh-HK");
  assert.equal(toolsLocale("zh-Hant-HK"), "zh-HK");
  assert.equal(toolsLocale("en"), "en");
  assert.equal(toolsLocale(undefined), "en");
});
