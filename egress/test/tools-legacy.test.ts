// The old stack's tools front door (src/tools/legacy.ts): a live sandbox's egress token → its conversation and owner,
// the REST API of the new stack's tools, every call a row in database jarvis's tool_calls.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import type { AddressInfo } from "node:net";
import { freshDb } from "./pg.ts";
import { sha256hex } from "../src/common/tokens.ts";
import { legacyServer } from "../src/tools/legacy.ts";
import type { Deps, ToolDef } from "../src/tools/types.ts";
import type { Upstream } from "../src/tools/upstream.ts";

let t: Awaited<ReturnType<typeof freshDb>>;
let url: string;
let srv: ReturnType<typeof legacyServer>;
const conv = randomUUID();
const TOKEN = "sandbox-egress-token";

const echo = {
  cfg: { prefix: "up" },
  tools: (): ToolDef[] => [
    { name: "up_echo", description: "echo", inputSchema: { type: "object" }, run: async (args, ctx) => ({ content: [], structuredContent: { args, person: ctx.caller.person } }) },
    { name: "up_fail", description: "fails", inputSchema: { type: "object" }, run: async () => ({ content: [{ type: "text", text: "nothing found" }], isError: true }) },
  ],
} as unknown as Upstream;

before(async () => {
  t = await freshDb("tools_legacy");
  await t.db.query(
    `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner) values ($1, $2, 'p', $3, 'x', 'ready', now() + interval '1 hour', 'Staff@Example.com')`,
    [randomUUID(), conv, sha256hex(TOKEN)],
  );
  const deps: Deps = { config: { upstreams: [] }, env: {}, fetch: globalThis.fetch };
  srv = legacyServer(deps, t.db, [echo]);
  await new Promise<void>((r) => srv.server.listen(0, "127.0.0.1", r));
  url = `http://127.0.0.1:${(srv.server.address() as AddressInfo).port}`;
});

after(async () => {
  await srv.drain(1000);
  await t.drop();
});

const auth = (tok = TOKEN) => ({ authorization: `Bearer ${tok}`, "content-type": "application/json" });

test("old-stack tools: the sandbox's egress token admits it; REST list, call, errors; tool_calls rows", async () => {
  assert.equal((await fetch(`${url}/healthz`)).status, 200);
  assert.equal((await fetch(`${url}/v1/tools`)).status, 401);
  assert.equal((await fetch(`${url}/v1/tools`, { headers: auth("nope") })).status, 401);
  const list = (await (await fetch(`${url}/v1/tools`, { headers: auth() })).json()) as { name: string }[];
  assert.deepEqual(list.map((x) => x.name), ["web_fetch", "up_echo", "up_fail"]);
  const r = await fetch(`${url}/v1/tools/up_echo`, { method: "POST", headers: auth(), body: JSON.stringify({ q: 1 }) });
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), { args: { q: 1 }, person: "staff@example.com" });
  const f = await fetch(`${url}/v1/tools/up_fail`, { method: "POST", headers: auth(), body: "{}" });
  assert.equal(f.status, 422);
  assert.deepEqual(await f.json(), { error: "nothing found" });
  assert.equal((await fetch(`${url}/v1/tools/nope`, { method: "POST", headers: auth(), body: "{}" })).status, 404);
  assert.equal((await fetch(`${url}/mcp`, { method: "POST", headers: auth(), body: "{}" })).status, 404);
  assert.equal(((await (await fetch(`${url}/v1/openapi.json`, { headers: auth() })).json()) as any).openapi, "3.1.0");
  const { rows } = await t.db.query("select conversation_id, user_email, tool, ok from tool_calls order by id");
  assert.deepEqual(rows, [
    { conversation_id: conv, user_email: "staff@example.com", tool: "up_echo", ok: true },
    { conversation_id: conv, user_email: "staff@example.com", tool: "up_fail", ok: false },
  ]);
  await t.db.query("update sandboxes set status = 'gone'");
  assert.equal((await fetch(`${url}/v1/tools`, { headers: auth() })).status, 401, "a released sandbox's token no longer works");
});
