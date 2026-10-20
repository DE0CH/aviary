import assert from "node:assert/strict";
import { test } from "node:test";
import { expandEnv, parseConfig } from "../src/common/config.ts";
import { lead, Listener, migrate, channel } from "../src/common/db.ts";
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

test("db: migrations are idempotent, notify reaches a listener, one leader at a time", async () => {
  const t = await freshDb("common");
  try {
    await migrate(t.db);
    await migrate(t.db);
    const l = new Listener(t.url);
    const got: (string | null)[] = [];
    const ch = channel("conv", "1234-ABCD");
    assert.equal(ch, "conv_1234abcd");
    await l.listen(ch, (p) => got.push(p));
    await t.db.query("select pg_notify($1, $2)", [ch, "7"]);
    for (let i = 0; i < 50 && !got.length; i++) await new Promise((r) => setTimeout(r, 20));
    assert.deepEqual(got, ["7"]);
    await l.close();

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
