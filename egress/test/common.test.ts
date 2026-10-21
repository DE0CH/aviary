import assert from "node:assert/strict";
import { test } from "node:test";
import { expandEnv, parseConfig } from "../src/common/config.ts";
import { lead, migrate } from "../src/common/db.ts";
import { randomUUID } from "node:crypto";
import { MIGRATIONS } from "../src/common/schema.ts";
import { claudeCodeEnv } from "../src/web/claude-code.ts";
import { freshDb } from "./pg.ts";

const base = {
  product: { name: "YYC AI" },
  publicUrl: "https://ai.example.org/",
  access: { teamDomain: "t.cloudflareaccess.com", audience: "aud" },
  users: ["A@Example.org"],
  models: [{ id: "anthropic/claude-opus-5.5", label: "Opus" }],
  defaultModel: "anthropic/claude-opus-5.5",
  systemPromptFile: "/etc/jarvis/system-prompt.md",
  sandbox: { image: "img" },
};

test("config: defaults, lower-cased users, validation", () => {
  const c = parseConfig(base);
  assert.equal(c.publicUrl, "https://ai.example.org");
  assert.deepEqual(c.users, ["a@example.org"]);
  assert.equal(c.sandbox.idleMinutes, 30);
  assert.equal(c.titleModel, "anthropic/claude-opus-5.5");
  assert.throws(() => parseConfig({ ...base, defaultModel: "x" }), /defaultModel/);
  assert.throws(() => parseConfig({ ...base, users: [] }), /users/);
});

test("expandEnv refuses unset variables", () => {
  assert.equal(expandEnv("Bearer ${K}", { K: "v" }), "Bearer v");
  assert.throws(() => expandEnv("${NOPE}", {}), /NOPE/);
});

test("db: migrations are idempotent, one leader at a time", async () => {
  const t = await freshDb("common");
  try {
    await migrate(t.db);
    await migrate(t.db);

    let a = 0;
    let b = 0;
    const la = lead(t.url, 99, async () => void a++, 20);
    await new Promise((r) => setTimeout(r, 100));
    const lb = lead(t.url, 99, async () => void b++, 20, 20);
    await new Promise((r) => setTimeout(r, 150));
    assert.ok(a > 0 && b === 0, `a=${a} b=${b}`);
    await la.stop();
    await new Promise((r) => setTimeout(r, 200));
    assert.ok(b > 0, "the second replica takes over");
    await lb.stop();
  } finally {
    await t.drop();
  }
});

test("schema: the old chat UI's tables stay with their rows, marked unused", async () => {
  const t = await freshDb("oldui");
  try {
    const conv = randomUUID();
    await t.db.query("insert into users (email) values ('a@x.org')");
    await t.db.query("insert into conversations (id, owner, model) values ($1, 'a@x.org', 'm')", [conv]);
    await t.db.query(MIGRATIONS[9]!); // schema 10 (applied again: it only comments)
    assert.equal((await t.db.query("select count(*)::int as n from conversations")).rows[0].n, 1);
    const { rows } = await t.db.query("select obj_description('turns'::regclass) as c");
    assert.match(rows[0].c, /unused since schema 10/);
  } finally {
    await t.drop();
  }
});

test("Claude Code in a sandbox: egress for the model and tools, SendUserFile on, the managed CLAUDE.md", async () => {
  const env = await claudeCodeEnv(parseConfig({ ...base, systemPromptFile: "/dev/null" }), "http://10.201.1.1:3128", "tok");
  assert.equal(env.ANTHROPIC_BASE_URL, "http://10.201.1.1:3128/x/claude");
  assert.equal(env.CLAUDE_CODE_ATTACHMENTS_BY_PATH, "1");
  const mcp = JSON.parse(Buffer.from(env.JARVIS_CLAUDE_MCP!, "base64").toString());
  assert.equal(mcp.mcpServers.yyc.url, "http://10.201.1.1:3128/x/tools/mcp");
  assert.match(Buffer.from(env.JARVIS_CLAUDE_MD!, "base64").toString(), /# Your computer/);
});
