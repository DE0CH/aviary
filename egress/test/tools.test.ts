// The tools role end to end on the real local Postgres, with every outside service faked (test/fakes/tools-upstreams.ts):
// auth by a live sandbox's egress token (what Claude Code sends through egress), the MCP methods, each built-in tool,
// upstream proxying with its read-only allowlist, and tool_calls logging.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import type { AddressInfo } from "node:net";
import { after, before, test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import { newToken, sha256hex } from "../src/common/tokens.ts";
import { isPrivateAddress, htmlToText } from "../src/tools/netguard.ts";
import { iso } from "../src/tools/providers.ts";
import { createToolsServer, toolList } from "../src/tools/server.ts";
import type { Deps } from "../src/tools/types.ts";
import { pickResponse, readOnlyAllowed, Upstream } from "../src/tools/upstream.ts";
import { startFakes, type Fakes } from "./fakes/tools-upstreams.ts";
import { freshDb } from "./pg.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let fakes: Fakes;
let deps: Deps;
let url = "";
let close: () => Promise<void>;
const owner = "staff@yyc.hk";
let conv = "";
let token = "";

/** A sandbox row as the web role makes one (status, owner); returns its egress token. */
async function newSandbox(status = "ready", who: string | null = owner, conversation = conv): Promise<string> {
  const id = randomUUID();
  const tok = newToken();
  await t.db.query(
    "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner, user_key) values ($1,$2,$3,$4,'x',$5,now() + interval '1 hour',$6,'u1')",
    [id, conversation, `sbx-${id.slice(0, 8)}`, sha256hex(tok), status, who],
  );
  return tok;
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
  conv = randomUUID();
  token = await newSandbox();
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
      capture: { url: `${fakes.url}/capture`, tokenEnv: "CAP", name: "YYC" },
      upstreams: [{ prefix: "yyc", url: `${fakes.url}/mcp-up`, headers: { Authorization: "Bearer ${UP_TOKEN}", "X-YYC-User": "${USER_EMAIL}" }, tools: ["posts_search", "post_delete"] }],
    },
  });
  deps = {
    config,
    db: t.db,
    env: { EXA_KEY: "exa-key", TIK_KEY: "tik-key", W_ID: "cid", W_SECRET: "csecret", UP_TOKEN: "up", CAP: "cap-token" },
    fetch: globalThis.fetch,
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
  await fakes.close();
  await t.drop();
});

test("auth: a live sandbox's egress token only", async () => {
  assert.equal((await rpc("tools/list", {}, "")).status, 401);
  assert.equal((await rpc("tools/list", {}, "nope")).status, 401);
  assert.equal((await rpc("tools/list", {}, await newSandbox("gone"))).status, 401, "a released sandbox");
  assert.equal((await rpc("tools/list", {}, await newSandbox("ready", null, randomUUID()))).status, 401, "a row from before Claude Code (no owner)");
  assert.equal((await rpc("tools/list")).status, 200);
  await rpc("tools/call", { name: "web_fetch", arguments: { url: "http://127.0.0.1:1/" } });
  const { rows } = await t.db.query("select conversation_id, user_email, turn_id from tool_calls order by id desc limit 1");
  assert.deepEqual(rows[0], { conversation_id: conv, user_email: owner, turn_id: null });
});

test("MCP: initialize, ping, list, unknown method, notifications, batch", async () => {
  const init = (await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claude-code" } })).body;
  assert.equal(init.result.protocolVersion, "2025-06-18");
  assert.ok(init.result.capabilities.tools);
  assert.equal((await rpc("initialize", { protocolVersion: "1999-01-01" })).body.result.protocolVersion, "2025-06-18");
  assert.deepEqual((await rpc("ping")).body.result, {});
  assert.equal((await rpc("nope/nope")).body.error.code, -32601);
  const names = (await rpc("tools/list")).body.result.tools.map((x: any) => x.name);
  assert.deepEqual(names, ["web_search", "web_fetch", "news_search", "social_search", "yyc_posts_search"]);
  for (const n of names) assert.match(n, /^[a-z0-9_]+$/, "lowercase names");
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
  assert.equal(rows[0].conversation_id, conv);
  const bad = await call("web_search", { query: "x", since: "yesterday" });
  assert.equal(bad.isError, true);
  assert.match(textOf(bad), /YYYY-MM-DD/);
});

test("web_fetch: the instance's own copy or capture first, labelled; original: true, none and a failure read the page", async () => {
  const cached = await call("web_fetch", { url: "https://news.example/known-article" });
  assert.match(textOf(cached), /^Source: YYC's stored copy \(captured 2026-10-27 10:00\)/);
  assert.match(textOf(cached), /存檔全文/);
  assert.match(textOf(cached), /YYC's page for it: https:\/\/yyc\.example\/item\/7/);
  assert.equal((cached as any).structuredContent.via, "own-cache");
  const captured = await call("web_fetch", { url: "https://news.example/known-site/x" });
  assert.match(textOf(captured), /^Source: YYC's capture, read just now/);
  assert.equal((captured as any).structuredContent.via, "own-capture");
  // asked for as it is: the page itself (Exa here), no label
  const orig = await call("web_fetch", { url: "https://news.example/known-article", original: true });
  assert.match(textOf(orig), /全文內容/);
  assert.doesNotMatch(textOf(orig), /^Source:/);
  // not ours, or the capture down: read as before
  assert.match(textOf(await call("web_fetch", { url: "https://other.example/a" })), /全文內容/);
  assert.match(textOf(await call("web_fetch", { url: "https://news.example/capture-down" })), /全文內容/);
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

test("upstream MCP: listed read-only tools are proxied with the staff member's identity; everything else is hidden", async () => {
  const r = await call("yyc_posts_search", { query: "x" });
  assert.equal(textOf(r), `called posts_search for ${owner}`);
  const up = fakes.seen.filter((s) => s.path === "/mcp-up" && s.body?.method === "tools/call").at(-1)!;
  assert.equal(up.headers.authorization, "Bearer up");

  // not listed (Send-Mail, a new read-only tool) or listed but marked as writing (post_delete): never offered, never run
  const before = fakes.seen.filter((s) => s.body?.method === "tools/call").length;
  for (const name of ["yyc_send_mail", "yyc_stream_items", "yyc_post_delete"]) {
    const res = await rpc("tools/call", { name, arguments: {} });
    assert.equal(res.body.error.code, -32602, name);
    assert.match(res.body.error.message, /unknown tool/);
  }
  assert.equal(fakes.seen.filter((s) => s.body?.method === "tools/call").length, before, "never forwarded");

  // default deny: an upstream without a `tools` list offers nothing
  const bare = { prefix: "z", url: "http://x", headers: {} };
  assert.equal(readOnlyAllowed(bare, { name: "posts_search", annotations: { readOnlyHint: true } }), false);
  const listed = { ...bare, tools: ["a"] };
  assert.equal(readOnlyAllowed(listed, { name: "a" }), true);
  assert.equal(readOnlyAllowed(listed, { name: "a", annotations: { readOnlyHint: false } }), false);
  assert.equal(readOnlyAllowed(listed, { name: "a", annotations: { destructiveHint: true } }), false);
  assert.equal(readOnlyAllowed(listed, { name: "b", annotations: { readOnlyHint: true } }), false);
});

test("helpers: private addresses, hostnames, dates, SSE answers, HTML text", () => {
  for (const a of ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "100.64.0.1", "169.254.169.254", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "fd42::5"])
    assert.equal(isPrivateAddress(a), true, a);
  for (const a of ["8.8.8.8", "1.1.1.1", "2a01:4f8::1", "::ffff:8.8.8.8"]) assert.equal(isPrivateAddress(a), false, a);
  assert.equal(iso(1792000000), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("1792000000000"), "2026-10-14T17:46:40.000Z");
  assert.equal(iso("Wed Oct 15 10:00:00 +0800 2026"), "2026-10-15T02:00:00.000Z");
  assert.equal(iso("garbage"), null);
  assert.equal(pickResponse('event: message\ndata: {"jsonrpc":"2.0","id":4,"result":{"a":1}}\n\n', "text/event-stream", 4).result.a, 1);
  assert.deepEqual(htmlToText("<html><title>T &amp; U</title><script>x()</script><p>一</p><p>二&nbsp;三</p></html>"), { title: "T & U", text: "T & U 一\n二 三" });
  assert.deepEqual(toolList({ ...deps, config: { ...deps.config, tools: { upstreams: [] } } }, []).map((d) => d.name), ["web_fetch"]);
});
