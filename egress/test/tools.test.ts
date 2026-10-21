// The tools role end to end on the real local Postgres, with every outside service faked (test/fakes/tools-upstreams.ts):
// auth by turn token, the MCP methods, each built-in tool, upstream proxying with its read-only allowlist, tool_calls logging, and the
// stdio bridge claw uses.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import type { AddressInfo } from "node:net";
import { after, before, test } from "node:test";
import { copyFileSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
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
let turnId = "";
let token = "";
// sandbox_environment's build-time files: the real Dockerfile and a small package list
const envDir = mkdtempSync(join(tmpdir(), "sbxenv-"));
copyFileSync(fileURLToPath(new URL("../sandbox/Dockerfile", import.meta.url)), join(envDir, "Dockerfile"));
writeFileSync(join(envDir, "packages.txt"), "# apt (dpkg-query)\nffmpeg-is-not-here 0\njq 1.7.1-6\n# pip (/opt/py)\npandas==2.3.3\n# npm (global, /usr/local)\nnpm 10.9.4\n");

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
      upstreams: [{ prefix: "yyc", url: `${fakes.url}/mcp-up`, headers: { Authorization: "Bearer ${UP_TOKEN}", "X-YYC-User": "${USER_EMAIL}" }, tools: ["posts_search", "post_delete"] }],
    },
  });
  deps = {
    config,
    db: t.db,
    env: { EXA_KEY: "exa-key", TIK_KEY: "tik-key", W_ID: "cid", W_SECRET: "csecret", UP_TOKEN: "up" },
    fetch: globalThis.fetch,
    resolve: async () => ({ address: "127.0.0.1", family: 4 }),
    fetchPorts: "any",
    sandboxEnvDir: envDir,
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
  assert.deepEqual(names, ["web_search", "web_fetch", "news_search", "social_search", "sandbox_environment", "present_files", "yyc_posts_search"]);
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

test("sandbox_environment: summary from the config, the Dockerfile and the package list from the build, no secrets", async () => {
  const sum = await call("sandbox_environment", {});
  assert.notEqual(sum.isError, true);
  const t0 = textOf(sum);
  assert.match(t0, /Image: img/);
  assert.match(t0, /CPU 1 core\(s\), memory 1Gi/);
  assert.match(t0, /pip install/);
  assert.match(t0, /npm install -g/);
  assert.match(t0, /web_fetch/);
  // provider kubernetes (the default): no apt, no Docker
  assert.match(t0, /\(apt\) can't be installed/);
  assert.match(t0, /Docker\n- Not available/);
  assert.doesNotMatch(t0, /Dockerfile it is built|FROM node/);
  const vm = (await import("../src/tools/sandbox-env.ts")).summary({ ...deps.config, sandbox: { ...deps.config.sandbox, provider: "vm" } }, "abc");
  assert.match(vm, /sudo apt-get install -y x/);
  assert.match(vm, /`docker` works/);
  assert.match(vm, /capped at 5 GiB/);
  const df = textOf(await call("sandbox_environment", { section: "dockerfile" }));
  assert.match(df, /FROM node:22-trixie-slim/);
  assert.match(df, /sudoers/);
  const pk = textOf(await call("sandbox_environment", { section: "packages", match: "PANDAS" }));
  assert.match(pk, /pandas==2\.3\.3/);
  assert.doesNotMatch(pk, /jq 1/);
  assert.match(textOf(await call("sandbox_environment", { match: "nothing-like-it" })), /No preinstalled package matches/);
  const all = textOf(await call("sandbox_environment", { section: "all" }));
  for (const secret of ["exa-key", "tik-key", "csecret", token]) assert.ok(!all.includes(secret), "no secrets");
  assert.equal((await call("sandbox_environment", { section: "nope" })).isError, true);
  const { rows } = await t.db.query("select ok from tool_calls where tool = 'sandbox_environment' order by id desc limit 1");
  assert.equal(rows[0].ok, false);
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
  assert.deepEqual(toolList({ ...deps, config: { ...deps.config, tools: { upstreams: [] } } }, []).map((d) => d.name), ["web_fetch", "sandbox_environment", "present_files"]);
});


test("present_files: read-only, needs the bridge's workspace facts, all or nothing, merges calls on the turn (copies once, latest wins)", async () => {
  ({ id: turnId, token } = await newTurn());
  const listed = (await rpc("tools/list")).body.result.tools.find((x: any) => x.name === "present_files");
  assert.equal(listed.annotations.readOnlyHint, true);
  assert.match(listed.description, /never scripts, fonts or intermediate files/);
  const callMeta = async (args: unknown, meta?: unknown) =>
    (await rpc("tools/call", { name: "present_files", arguments: args, ...(meta ? { _meta: meta } : {}) })).body.result;
  const f = (path: string, s: string, size = 3) => ({ path, sha256: s.repeat(64), size });
  // no facts (not through the bridge): refused
  assert.equal((await callMeta({ files: [{ path: "a.pdf" }] })).isError, true);
  // bad input
  assert.match(textOf(await callMeta({ files: [] }, { "jarvis/files": {} })), /non-empty list/);
  assert.match(textOf(await callMeta({ files: Array.from({ length: 21 }, (_, i) => ({ path: `${i}` })) }, { "jarvis/files": {} })), /at most 20/);
  // one bad path: nothing is shown
  const bad = await callMeta({ files: [{ path: "a.pdf" }, { path: "nope.pdf" }] }, { "jarvis/files": { "a.pdf": f("a.pdf", "a"), "nope.pdf": { error: "no such file in the workspace" } } });
  assert.equal(bad.isError, true);
  assert.match(textOf(bad), /nope\.pdf: no such file/);
  assert.equal((await t.db.query("select presented from turns where id = $1", [turnId])).rows[0].presented, null);
  // two calls: the second adds, a copy (same contents) and a path given again are shown once, at the latest place
  const one = await callMeta({ files: [{ path: "/work/build/a.pdf", caption: "  Report\n v1 " }, { path: "c.png" }] }, { "jarvis/files": { "/work/build/a.pdf": f("build/a.pdf", "a"), "c.png": f("c.png", "c") } });
  assert.equal(one.isError, undefined, textOf(one));
  const two = await callMeta({ files: [{ path: "a.pdf", caption: "Report" }] }, { "jarvis/files": { "a.pdf": f("a.pdf", "a") } });
  assert.deepEqual(two.structuredContent.files, [f("c.png", "c"), { path: "a.pdf", caption: "Report", sha256: "a".repeat(64), size: 3 }]);
  const { rows } = await t.db.query("select presented from turns where id = $1", [turnId]);
  assert.deepEqual(rows[0].presented, two.structuredContent.files);
  const { rows: ev } = await t.db.query("select data from events where turn_id = $1 and type = 'files_presented' order by seq", [turnId]);
  assert.equal(ev.length, 2);
  assert.deepEqual(ev[1].data.files, two.structuredContent.files);
  assert.equal(ev[0].data.files[0].caption, "Report v1");
});
