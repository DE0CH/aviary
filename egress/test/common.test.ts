import assert from "node:assert/strict";
import { test } from "node:test";
import { expandEnv, isUser, parseConfig, reasoningPlan } from "../src/common/config.ts";
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

test("isUser: listed emails, and everyone at a \"@domain\" entry, only them", () => {
  const c = parseConfig({ ...base, users: ["A@Example.org", "@YYC.hk"] });
  assert.ok(isUser(c, "a@example.org"));
  assert.ok(isUser(c, "Staff@yyc.HK"));
  assert.ok(!isUser(c, "someone@notyyc.hk"));
  assert.ok(!isUser(c, "x@sub.yyc.hk"));
  assert.ok(!isUser(c, "yyc.hk@evil.test"));
  assert.ok(!isUser(c, "@yyc.hk"));
  assert.ok(!isUser(c, ""));
});

test("config: defaults, lower-cased users, validation", () => {
  const c = parseConfig(base);
  assert.equal(c.publicUrl, "https://ai.example.org");
  assert.deepEqual(c.users, ["a@example.org"]);
  assert.equal(c.sandbox.idleMinutes, 30);
  assert.equal(c.titleModel, "anthropic/claude-opus-5.5");
  assert.throws(() => parseConfig({ ...base, defaultModel: "x" }), /defaultModel/);
  assert.throws(() => parseConfig({ ...base, users: [] }), /users/);
});

test("config: thinking levels per model become egress's reasoning plan", () => {
  const ds = "deepseek/deepseek-v4.1-flash-1017";
  const c = parseConfig({ ...base, models: [{ id: ds, label: "DeepSeek", reasoning: "high" }], defaultModel: ds, titleModel: ds, titleReasoning: "none" });
  assert.deepEqual(reasoningPlan(c), { [ds]: { withTools: "high", withoutTools: "none" } });
  assert.deepEqual(reasoningPlan(parseConfig(base)), {});
  assert.throws(() => parseConfig({ ...base, models: [{ id: "anthropic/claude-opus-5.5", label: "Opus", reasoning: "lots" }] }), /reasoning/);
  assert.throws(() => parseConfig({ ...base, titleReasoning: "lots" }), /titleReasoning/);
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

test("sent-file snapshots: a sent-files route turns on the SendUserFile hook; the hook asks it and blocks on a failure", async () => {
  const off = await claudeCodeEnv(parseConfig({ ...base, systemPromptFile: "/dev/null" }), "http://10.201.1.1:3128", "tok");
  assert.equal(off.JARVIS_CLAUDE_SETTINGS, undefined, "no route: no hook");
  assert.equal(off.JARVIS_SENT_FILES_URL, undefined);
  const routes = [{ name: "sent-files", upstream: "http://ui:8080/__jarvis/sent-files", identity: true, inject: {} }];
  const on = await claudeCodeEnv(parseConfig({ ...base, systemPromptFile: "/dev/null", egress: { routes } }), "http://10.201.1.1:3128", "tok");
  assert.equal(on.JARVIS_SENT_FILES_URL, "http://10.201.1.1:3128/x/sent-files/snapshot");
  const settings = JSON.parse(Buffer.from(on.JARVIS_CLAUDE_SETTINGS!, "base64").toString());
  assert.deepEqual(settings.hooks.PreToolUse[0].matcher, "SendUserFile");
  assert.equal(settings.hooks.PreToolUse[0].hooks[0].command, "node /opt/agent/send-file-hook.mjs");

  // the hook itself, against a fake UI: relative paths resolved against cwd, the egress token sent, 200 = go on,
  // anything else = exit 2 with the reason on stderr
  const { createServer } = await import("node:http");
  const { spawn } = await import("node:child_process");
  const seen: any[] = [];
  let status = 200;
  const srv = createServer((req, res) => {
    let b = "";
    req.on("data", (c) => (b += c));
    req.on("end", () => {
      seen.push({ auth: req.headers.authorization, body: JSON.parse(b) });
      res.writeHead(status).end(status === 200 ? "{}" : "disk full");
    });
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", r));
  const port = (srv.address() as import("node:net").AddressInfo).port;
  const hook = (input: object) =>
    new Promise<{ status: number | null; stderr: string }>((resolve) => {
      const p = spawn(process.execPath, [new URL("../sandbox/send-file-hook.mjs", import.meta.url).pathname], {
        env: { PATH: process.env.PATH!, JARVIS_SENT_FILES_URL: `http://127.0.0.1:${port}/x/sent-files/snapshot`, ANTHROPIC_AUTH_TOKEN: "egress-tok" },
      });
      let stderr = "";
      p.stderr.on("data", (c) => (stderr += c));
      p.on("close", (code) => resolve({ status: code, stderr }));
      p.stdin.end(JSON.stringify(input));
    });
  try {
    const send = { tool_name: "SendUserFile", tool_use_id: "toolu_1", cwd: "/workspace", tool_input: { files: ["out/report.pdf", "/workspace/chart.png"] } };
    assert.equal((await hook(send)).status, 0);
    assert.deepEqual(seen[0], { auth: "Bearer egress-tok", body: { toolUseId: "toolu_1", files: ["/workspace/out/report.pdf", "/workspace/chart.png"] } });
    status = 507;
    const failed = await hook(send);
    assert.equal(failed.status, 2, "a failed snapshot blocks the send");
    assert.match(failed.stderr, /could not be saved for the user \(HTTP 507: disk full\)/);
    assert.equal((await hook({ tool_name: "Bash", tool_input: {} })).status, 0, "other tools pass untouched");
    assert.equal(seen.length, 2);
  } finally {
    srv.close();
  }
});
