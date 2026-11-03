// `egress import-jarvis` and `tools import-jarvis` (aviary cutover spec 8.2, platform spec 17.2 egress item 6 and tools
// item 5) on the real local Postgres: an old-stack `jarvis` database built by the chat schema's own migrations, and
// egress's and tools' own databases. Covers every column copied with its exact value, `person` backfilled by sandbox,
// then conversation, then null, idempotence by `source_id` (a second full pass and overlapping passes copy nothing
// twice), `--delta` (only what is new, plus the lookback), the budget counting copied model rows, a missing legacy
// table, and the subcommand end to end in a child process (exit codes, env).
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { after, before, test } from "node:test";
import pg from "pg";
import { connect as connectOld, migrate as migrateOld } from "../src/common/db.ts";
import { BATCH, copy, LOOKBACK, parseArgs, run, sourceClient } from "../src/common/importjarvis.ts";
import { spentSince } from "../src/egress/budget.ts";
import { connect as connectEgress, migrate as migrateEgress } from "../src/egress/db.ts";
import { EGRESS_LOG } from "../src/egress/importjarvis.ts";
import { connect as connectTools, migrate as migrateTools } from "../src/tools/db.ts";
import { TOOL_CALLS } from "../src/tools/importjarvis.ts";

const ADMIN = process.env.TEST_DATABASE_URL || "postgres://claude:dev@127.0.0.1:5432/postgres";

async function createDb(name: string): Promise<string> {
  const c = new pg.Client({ connectionString: ADMIN });
  await c.connect();
  await c.query(`drop database if exists ${name} with (force)`);
  await c.query(`create database ${name}`);
  await c.end();
  const u = new URL(ADMIN);
  u.pathname = `/${name}`;
  return u.toString();
}

async function dropDb(name: string): Promise<void> {
  const c = new pg.Client({ connectionString: ADMIN });
  await c.connect();
  await c.query(`drop database if exists ${name} with (force)`);
  await c.end();
}

const NAMES = { old: `ij_jarvis_${process.pid}`, egress: `ij_egress_${process.pid}`, tools: `ij_tools_${process.pid}` };
let oldUrl = "", egressUrl = "", toolsUrl = "";
let old: pg.Pool, egress: pg.Pool, tools: pg.Pool;

const ALICE = "alice@example.com";
const BOB = "bob@example.com";
const conv = randomUUID(); // Bob's conversation (the legacy UI's), no sandbox
const sbxConv = randomUUID();
const sbx = randomUUID(); // Alice's sandbox

before(async () => {
  oldUrl = await createDb(NAMES.old);
  egressUrl = await createDb(NAMES.egress);
  toolsUrl = await createDb(NAMES.tools);
  old = connectOld(oldUrl);
  await migrateOld(old);
  egress = connectEgress(egressUrl);
  await migrateEgress(egress);
  tools = connectTools(toolsUrl);
  await migrateTools(tools);
  await old.query("insert into users (email) values ($1), ($2)", [ALICE, BOB]);
  await old.query("insert into conversations (id, owner, model) values ($1, $2, 'm'), ($3, $4, 'm')", [conv, BOB, sbxConv, ALICE]);
  await old.query(
    `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner)
     values ($1, $2, 'sbx-a', 'h', 'x', 'gone', now(), $3)`,
    [sbx, sbxConv, ALICE],
  );
});

after(async () => {
  await Promise.all([old?.end(), egress?.end(), tools?.end()]);
  await Promise.all(Object.values(NAMES).map(dropDb));
});

/** Adds one old egress_log row; returns its id. */
async function oldEgress(r: { sandbox?: string | null; conversation?: string | null; kind?: string; cost?: string | null; at?: string; owner?: string | null }): Promise<number> {
  const { rows } = await old.query<{ id: string }>(
    `insert into egress_log (at, conversation_id, sandbox_id, turn_id, kind, method, host, port, path, status, decision,
       bytes_up, bytes_down, ms, cost_usd, model, owner)
     values ($1, $2, $3, $4, $5, 'POST', 'openrouter.ai', 443, '/api/v1/messages', 200, 'allow', 1234, 56789, 321, $6, 'deepseek/x', $7)
     returning id`,
    [r.at ?? new Date().toISOString(), r.conversation ?? null, r.sandbox ?? null, randomUUID(), r.kind ?? "model", r.cost ?? null, r.owner ?? null],
  );
  return Number(rows[0]!.id);
}

async function pass(spec: typeof EGRESS_LOG, dst: pg.Pool, delta: boolean) {
  const [r] = await run(spec.target, oldUrl, dst, [spec], delta);
  return r!;
}

const COLS = "at::text, conversation_id, sandbox_id, turn_id, kind, method, host, port, path, status, decision, bytes_up::text, bytes_down::text, ms, cost_usd::text, model, owner";

test("egress: every column copied exactly, person by sandbox then conversation then null, source_id the old id", async () => {
  const a = await oldEgress({ sandbox: sbx, conversation: sbxConv, cost: "0.000123456789", at: "2026-10-21 02:05:52.123456+00" });
  const b = await oldEgress({ conversation: conv, cost: "1.5" });
  const c = await oldEgress({ kind: "connect", owner: "relay@example.com" });
  const d = await oldEgress({ sandbox: randomUUID(), conversation: randomUUID() }); // neither known
  const r = await pass(EGRESS_LOG, egress, false);
  assert.equal(r.read, 4);
  assert.equal(r.copied, 4);
  assert.equal(r.last, d);
  const src = await old.query(`select id, ${COLS} from egress_log order by id`);
  const dst = await egress.query(`select source_id::int as id, ${COLS}, person, row_id, request from egress_log order by source_id`);
  assert.equal(dst.rows.length, 4);
  for (let i = 0; i < 4; i++) {
    const { person, row_id, request, ...rest } = dst.rows[i];
    assert.deepEqual(rest, { ...src.rows[i], id: Number(src.rows[i].id) });
    assert.equal(row_id, null);
    assert.equal(request, null);
  }
  assert.equal(dst.rows[0].at, "2026-10-21 02:05:52.123456+00");
  assert.equal(dst.rows[0].cost_usd, "0.000123456789");
  assert.deepEqual(dst.rows.map((x) => [x.id, x.person]), [[a, ALICE], [b, BOB], [c, null], [d, null]]);
  assert.equal(dst.rows[2].owner, "relay@example.com");
  assert.equal(r.missing, 0, "a full pass checks that every row read is there");
});

test("egress: a mixed-case legacy owner is one person key", async () => {
  const s = randomUUID();
  await old.query(
    `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner)
     values ($1, $2, $3, $4, 'x', 'gone', now(), 'Alice@Example.COM')`,
    [s, randomUUID(), `sbx-${s}`, `h-${s}`],
  );
  const id = await oldEgress({ sandbox: s, cost: "0.5", owner: "Alice@Example.COM" });
  await pass(EGRESS_LOG, egress, true);
  const { rows } = await egress.query("select person, owner from egress_log where source_id = $1", [id]);
  assert.deepEqual(rows, [{ person: ALICE, owner: "Alice@Example.COM" }]);
});

test("egress: a second full pass and overlapping passes copy nothing twice; --delta reads only new rows and the lookback", async () => {
  const before = Number((await egress.query("select count(*) from egress_log")).rows[0].count);
  const again = await pass(EGRESS_LOG, egress, false);
  assert.equal(again.copied, 0);
  assert.equal(again.read, before);
  assert.equal(again.missing, 0);
  const both = await Promise.all([pass(EGRESS_LOG, egress, false), pass(EGRESS_LOG, egress, true)]);
  assert.deepEqual(both.map((x) => x.copied), [0, 0]);
  const n = await oldEgress({ sandbox: sbx, cost: "0.25" });
  const d = await pass(EGRESS_LOG, egress, true);
  assert.equal(d.copied, 1);
  assert.equal(d.after, Math.max(0, n - 1 - LOOKBACK));
  assert.equal(Number((await egress.query("select count(*) from egress_log")).rows[0].count), before + 1);
  // a row whose id was handed out before a neighbour's but committed after it is still found by --delta (the lookback)
  const highest = Number((await egress.query("select max(source_id) as m from egress_log")).rows[0].m);
  await old.query("select setval('egress_log_id_seq', $1)", [highest + 5]);
  const late = await oldEgress({ sandbox: sbx, cost: "0.01" });
  await pass(EGRESS_LOG, egress, true);
  await old.query("insert into egress_log (id, kind, host, decision, cost_usd, sandbox_id) values ($1, 'model', 'h', 'allow', 0.02, $2)", [highest + 2, sbx]);
  const r = await pass(EGRESS_LOG, egress, true);
  assert.equal(r.copied, 1);
  const got = await egress.query("select source_id::int as s, person from egress_log where source_id in ($1, $2) order by 1", [highest + 2, late]);
  assert.deepEqual(got.rows, [{ s: highest + 2, person: ALICE }, { s: late, person: ALICE }]);
});

test("egress: the budget counts the copied model rows", async () => {
  const since = new Date(Date.now() - 365 * 86400_000).toISOString();
  const spent = await spentSince(egress, since);
  const want = Number((await old.query("select sum(cost_usd)::text as s from egress_log where kind = 'model'")).rows[0].s);
  assert.equal(spent.toFixed(9), want.toFixed(9));
});

test("egress: a pass longer than one batch copies every row, in order", async () => {
  const vals: string[] = [];
  for (let i = 0; i < BATCH + 7; i++) vals.push(`('connect', 'h${i}', 'allow')`);
  await old.query(`insert into egress_log (kind, host, decision) values ${vals.join(",")}`);
  const r = await pass(EGRESS_LOG, egress, true);
  assert.equal(r.copied, BATCH + 7);
  const [a, b] = await Promise.all([
    old.query("select count(*)::int as n, max(id)::int as m from egress_log"),
    egress.query("select count(*)::int as n, max(source_id)::int as m from egress_log"),
  ]);
  assert.deepEqual(b.rows[0], a.rows[0]);
});

test("egress: without the legacy conversations table, person comes from the sandbox or is null", async () => {
  const name = `ij_old2_${process.pid}`;
  const url = await createDb(name);
  const o = connectOld(url);
  try {
    await o.query(`create table sandboxes (id uuid primary key, owner text);
      create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
        sandbox_id uuid, turn_id uuid, kind text not null, method text, host text not null, port int, path text,
        status int, decision text not null, bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric, model text, owner text)`);
    const s = randomUUID();
    await o.query("insert into sandboxes values ($1, $2)", [s, ALICE]);
    await o.query("insert into egress_log (sandbox_id, conversation_id, kind, host, decision) values ($1, $2, 'http', 'h', 'allow'), (null, $2, 'http', 'h', 'allow')", [s, conv]);
    const dstName = `ij_egress2_${process.pid}`;
    const dstUrl = await createDb(dstName);
    const d = connectEgress(dstUrl);
    try {
      await migrateEgress(d);
      const src = sourceClient(url);
      await src.connect();
      try {
        await src.query("begin transaction isolation level repeatable read read only");
        const r = await copy(src, d, EGRESS_LOG, false);
        assert.equal(r.copied, 2);
      } finally {
        await src.end();
      }
      const { rows } = await d.query("select person from egress_log order by source_id");
      assert.deepEqual(rows.map((x) => x.person), [ALICE, null]);
      // a row the target lost (here a trigger drops it) is counted missing by a full pass
      await d.query(`create function drop_lost() returns trigger language plpgsql as $$ begin
          if new.host = 'lost' then return null; end if; return new; end $$;
        create trigger drop_lost before insert on egress_log for each row execute function drop_lost()`);
      await o.query("insert into egress_log (kind, host, decision) values ('http', 'lost', 'allow')");
      const src2 = sourceClient(url);
      await src2.connect();
      try {
        await src2.query("begin transaction isolation level repeatable read read only");
        const r = await copy(src2, d, EGRESS_LOG, false);
        assert.deepEqual([r.read, r.copied, r.missing], [3, 0, 1]);
        const dr = await copy(src2, d, EGRESS_LOG, true);
        assert.equal(dr.missing, undefined, "a delta pass does not count");
      } finally {
        await src2.end();
      }
    } finally {
      await d.end();
      await dropDb(dstName);
    }
  } finally {
    await o.end();
    await dropDb(name);
  }
});

test("tools: every column copied exactly, idempotent, --delta", async () => {
  const ins = (tool: string, args: unknown, ok: boolean | null, cost: string | null) =>
    old.query(
      `insert into tool_calls (at, turn_id, conversation_id, user_email, tool, args, ok, ms, error, cost_usd)
       values ('2026-10-30 01:02:03.456789+00', $1, $2, $3, $4, $5, $6, 42, $7, $8)`,
      [randomUUID(), sbxConv, ALICE, tool, JSON.stringify(args), ok, ok === false ? "boom" : null, cost],
    );
  await ins("web_search", { q: "港股", n: 3, nested: { a: [1, 2] } }, true, "0.005");
  await ins("tikhub", null, false, null);
  await ins("fetch", "a string", null, "0");
  const r = await pass(TOOL_CALLS, tools, false);
  assert.equal(r.copied, 3);
  const cols = "at::text, turn_id, conversation_id, user_email, tool, args::text, ok, ms, error, cost_usd::text";
  const src = await old.query(`select id::int, ${cols} from tool_calls order by id`);
  const dst = await tools.query(`select source_id::int as id, ${cols}, sandbox_id, request from tool_calls order by source_id`);
  assert.deepEqual(dst.rows.map(({ sandbox_id, request, ...x }) => (assert.equal(sandbox_id, null), assert.equal(request, null), x)), src.rows);
  assert.equal((await pass(TOOL_CALLS, tools, false)).copied, 0);
  assert.equal((await pass(TOOL_CALLS, tools, true)).copied, 0);
  await ins("exa", {}, true, null);
  assert.equal((await pass(TOOL_CALLS, tools, true)).copied, 1);
  assert.equal(Number((await tools.query("select count(*) from tool_calls")).rows[0].count), 4);
});

test("arguments: only --delta", () => {
  assert.deepEqual(parseArgs([]), { delta: false });
  assert.deepEqual(parseArgs(["--delta"]), { delta: true });
  assert.throws(() => parseArgs(["--all"]), /unknown argument/);
});

/** Runs `node src/main.ts <args>` with `env`; resolves with the exit code and output (killed after 60 s). */
function child(args: string[], env: Record<string, string>): Promise<{ code: number | null; out: string; err: string }> {
  return new Promise((resolve) => {
    const p = spawn(process.execPath, ["--disable-warning=ExperimentalWarning", "src/main.ts", ...args], { env: { PATH: process.env.PATH ?? "", ...env } });
    let out = "", err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    const t = setTimeout(() => p.kill("SIGKILL"), 60_000);
    p.on("close", (code) => (clearTimeout(t), resolve({ code, out, err })));
  });
}

test("the subcommands end to end: exit 0 after a pass, 2 on a missing env or a bad argument, 1 on an unreachable source", async () => {
  await oldEgress({ sandbox: sbx, cost: "0.125" });
  const e = await child(["egress", "import-jarvis", "--delta"], { EGRESS_IMPORT_JARVIS_URL: oldUrl, EGRESS_DATABASE_URL: egressUrl });
  assert.equal(e.code, 0, e.err);
  const line = JSON.parse(e.out.trim().split("\n").at(-1)!);
  assert.equal(line.at, "import-jarvis");
  assert.equal(line.table, "egress_log");
  assert.equal(line.copied, 1);
  const t = await child(["tools", "import-jarvis"], { TOOLS_IMPORT_JARVIS_URL: oldUrl, TOOLS_DATABASE_URL: toolsUrl });
  assert.equal(t.code, 0, t.err);
  assert.equal(JSON.parse(t.out.trim().split("\n").at(-1)!).copied, 0);
  assert.equal((await child(["egress", "import-jarvis"], { EGRESS_DATABASE_URL: egressUrl })).code, 2);
  assert.equal((await child(["tools", "import-jarvis", "--everything"], { TOOLS_IMPORT_JARVIS_URL: oldUrl, TOOLS_DATABASE_URL: toolsUrl })).code, 2);
  const bad = new URL(oldUrl);
  bad.port = "1";
  const u = await child(["egress", "import-jarvis"], { EGRESS_IMPORT_JARVIS_URL: bad.toString(), EGRESS_DATABASE_URL: egressUrl });
  assert.equal(u.code, 1);
  assert.match(u.err, /import-jarvis/);
});
