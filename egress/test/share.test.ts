// Sharing and continuing (docs/CHAT.md, "Sharing and continuing") end to end on fakes: real Postgres, the real
// jarvis-bash and sandbox agent, a fake claw, a fake S3 and a fake Kubernetes API.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { AccessVerifier } from "../src/common/access.ts";
import { parseConfig } from "../src/common/config.ts";
import { Listener } from "../src/common/db.ts";
import { S3BlobStore } from "../src/common/s3.ts";
import type { WebDeps } from "../src/web/ctx.ts";
import { appendEvent } from "../src/web/events.ts";
import { janitorPass } from "../src/web/janitor.ts";
import { startWeb, type WebServer } from "../src/web/main.ts";
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
import { continuedSession, shareDownloadable } from "../src/web/share.ts";
import { RESET_NOTE } from "../src/web/turns.ts";
import { AUD, fakeAccess, TEAM } from "./fakes/access.ts";
import { FakeK8s } from "./fakes/k8s.ts";
import { fakeS3 } from "./fakes/s3.ts";
import { freshDb } from "./pg.ts";

const A = "alice@example.org";
const B = "bob@example.org";
const STRANGER = "eve@example.org";
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

let pg: Awaited<ReturnType<typeof freshDb>>;
let s3: Awaited<ReturnType<typeof fakeS3>>;
let access: Awaited<ReturnType<typeof fakeAccess>>;
let k8s: FakeK8s;
let web: WebServer;
let deps: WebDeps;
let base: string;
let upstream: { close(): Promise<void>; port: number };

before(async () => {
  pg = await freshDb("share");
  s3 = await fakeS3();
  access = await fakeAccess();
  k8s = new FakeK8s();
  const server = createServer((req, res) => {
    if (req.url === "/mcp") {
      res.writeHead(200, { "content-type": "application/json" });
      return void res.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { tools: [] } }));
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ content: [{ type: "text", text: "Shared title" }] }));
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  upstream = { port: (server.address() as { port: number }).port, close: () => new Promise<void>((r) => server.close(() => r())) };
  const work = mkdtempSync(join(tmpdir(), "chat-share-"));
  const promptFile = join(work, "system-prompt.md");
  writeFileSync(promptFile, "You are a test assistant.");
  const config = parseConfig({
    product: { name: "YYC AI", locale: "zh-HK" },
    publicUrl: "https://ai.example.org",
    access: { teamDomain: TEAM, audience: AUD },
    users: [A, B],
    admins: [A],
    models: [{ id: "anthropic/claude-sonnet-5.5", label: "Sonnet" }],
    defaultModel: "anthropic/claude-sonnet-5.5",
    titleModel: "anthropic/claude-haiku-5.5",
    systemPromptFile: promptFile,
    sandbox: { image: "sandbox:test", maxActive: 3 },
    s3: { endpoint: s3.url, region: "fsn1", bucket: "b", prefix: "jarvis/" },
  });
  deps = {
    config,
    db: pg.db,
    dbUrl: pg.url,
    listener: new Listener(pg.url),
    blobs: new S3BlobStore({ ...config.s3, accessKeyId: "k", secretAccessKey: "s" }),
    sandboxProvider: undefined as any,
    verifier: new AccessVerifier({ teamDomain: TEAM, audience: AUD, jwks: access.jwks }),
    replica: "web_1",
    namespace: "jarvis",
    workRoot: join(work, "work"),
    clawPath: here("./fakes/claw.mjs"),
    bashPath: here("../src/bin/jarvis-bash"),
    mcpBridgePath: here("../src/bin/jarvis-mcp-bridge"),
    egressBase: `http://127.0.0.1:${upstream.port}`,
    toolsUrl: `http://127.0.0.1:${upstream.port}/mcp`,
    uiDir: join(work, "ui"),
    brandDir: join(work, "brand"),
    drainSeconds: 5,
  };
  deps.sandboxProvider = new KubernetesProvider({ k8s, config, egressNamespace: "jarvis", agentUrlFor: k8s.agentUrlFor });
  web = await startWeb(deps, 0, { janitorMs: 3_600_000 });
  base = `http://127.0.0.1:${web.port}`;
});

after(async () => {
  await web.close();
  await deps.listener.close();
  await k8s.closeAll();
  await upstream.close();
  await s3.close();
  await pg.drop();
});

async function call(email: string | null, method: string, path: string, body?: unknown) {
  const h: Record<string, string> = {};
  if (email) h["cf-access-jwt-assertion"] = await access.token(email);
  let b: any;
  if (typeof body === "string") b = body;
  else if (body !== undefined) {
    b = JSON.stringify(body);
    h["content-type"] = "application/json";
  }
  const r = await fetch(base + path, { method, headers: h, body: b });
  const text = await r.text();
  let json: any = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: r.status, json, text, headers: r.headers };
}

async function waitTurn(turnId: string, statuses = ["done", "error", "cancelled"]): Promise<void> {
  for (let i = 0; i < 300; i++) {
    const { rows } = await pg.db.query("select status from turns where id = $1", [turnId]);
    if (statuses.includes(rows[0]?.status)) {
      // the turn's own end (files stored, turn_finished) comes just after the status
      const { rows: fin } = await pg.db.query("select 1 from events where turn_id = $1 and type = 'turn_finished'", [turnId]);
      if (fin.length || statuses.includes("running")) return;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`turn ${turnId} did not reach ${statuses}`);
}

async function runTurn(email: string, conv: string, text: string, attachments: string[] = []): Promise<string> {
  const r = await call(email, "POST", `/api/conversations/${conv}/messages`, { text, attachments });
  assert.equal(r.status, 202, r.text);
  await waitTurn(r.json.turnId);
  return r.json.turnId;
}

const fileSha = async (conv: string, path: string): Promise<string> =>
  (await pg.db.query("select sha256 from files where conversation_id = $1 and path = $2", [conv, path])).rows[0].sha256;

let conv: string; // Alice's original
let turn1: string;
let shareId: string;
let continued: string; // Bob's continued conversation
const sha: Record<string, string> = {};

test("share: owner only, instance users only, a frozen snapshot of finished turns", async () => {
  conv = (await call(A, "POST", "/api/conversations", {})).json.id;
  assert.equal((await call(A, "POST", `/api/conversations/${conv}/files?name=in.csv`, "a,b\n1,2\n")).status, 201);
  turn1 = await runTurn(
    A,
    conv,
    "analyse BASH:mkdir -p scripts && echo 'print(1)' > scripts/tmp.py && echo r,1 > report.csv && printf PNGDATA > chart.png && echo ok",
    ["uploads/in.csv"],
  );
  for (const p of ["uploads/in.csv", "scripts/tmp.py", "report.csv", "chart.png"]) sha[p] = await fileSha(conv, p);
  // what present_files and image viewing record (files_presented; tool_result.images): report.csv shown, chart.png viewed
  const shown = [{ path: "report.csv", sha256: sha["report.csv"], size: 4 }];
  await pg.db.query("update turns set presented = $2 where id = $1", [turn1, JSON.stringify(shown)]);
  await appendEvent(pg.db, conv, turn1, "files_presented", { files: shown });
  await appendEvent(pg.db, conv, turn1, "tool_result", { id: "v1", name: "read_file", output: "image", images: [{ path: "chart.png", sha256: sha["chart.png"] }] });

  // nothing to share in a conversation without a finished turn
  const empty = (await call(A, "POST", "/api/conversations", {})).json.id;
  assert.equal((await call(A, "POST", `/api/conversations/${empty}/shares`)).status, 400);
  // only the owner shares; a non-user is refused before anything
  assert.equal((await call(B, "POST", `/api/conversations/${conv}/shares`)).status, 404);
  assert.equal((await call(STRANGER, "POST", `/api/conversations/${conv}/shares`)).status, 403);
  assert.equal((await call(null, "POST", `/api/conversations/${conv}/shares`)).status, 401);

  const made = await call(A, "POST", `/api/conversations/${conv}/shares`);
  assert.equal(made.status, 201, made.text);
  shareId = made.json.id;
  assert.match(shareId, /^[A-Za-z0-9_-]{22}$/);
  assert.equal(made.json.url, `https://ai.example.org/s/${shareId}`);
  assert.equal((await call(A, "GET", `/api/conversations/${conv}/shares`)).json[0].id, shareId);
  assert.equal((await call(B, "GET", `/api/conversations/${conv}/shares`)).status, 404);

  // the original goes on: a later turn and a turn still running are not in the snapshot
  await runTurn(A, conv, "more WRITE:later.txt=new");
  const running = await call(A, "POST", `/api/conversations/${conv}/messages`, { text: "slow SLEEP" });
  await waitTurn(running.json.turnId, ["running"]);
  const second = await call(A, "POST", `/api/conversations/${conv}/shares`);
  assert.equal(second.status, 201);
  assert.notEqual(second.json.id, shareId, "sharing again makes a new link");
  const s2 = await call(B, "GET", `/api/shares/${second.json.id}`);
  assert.equal(s2.json.turns.length, 2);
  assert.ok(!s2.json.turns.some((t: any) => t.id === running.json.turnId));
  assert.ok(!s2.json.messages.some((m: any) => m.content === "slow SLEEP"));
  await call(A, "POST", `/api/turns/${running.json.turnId}/cancel`);
  await waitTurn(running.json.turnId);

  // Bob (another user) sees the first snapshot, read-only; Eve (not a user) and a signed-out caller don't
  assert.equal((await call(STRANGER, "GET", `/api/shares/${shareId}`)).status, 403);
  assert.equal((await call(null, "GET", `/api/shares/${shareId}`)).status, 401);
  const view = await call(B, "GET", `/api/shares/${shareId}`);
  assert.equal(view.status, 200, view.text);
  assert.equal(view.json.title, "Shared title");
  assert.equal(view.json.ownerName, "alice");
  assert.equal(view.json.isOwner, false);
  assert.deepEqual(view.json.turns.map((t: any) => t.id), [turn1]);
  assert.deepEqual(view.json.messages.map((m: any) => m.role), ["user", "assistant"]);
  assert.deepEqual(view.json.messages[0].attachments, ["uploads/in.csv"]);
  assert.ok(view.json.turns[0].events.some((e: any) => e.type === "tool_use" && e.data.name === "bash"), "tool steps are shown");
  assert.equal(view.json.turns[0].costUsd, null);
  assert.deepEqual(view.json.turns[0].presented, [{ path: "report.csv", sha256: sha["report.csv"], size: 4 }]);
  // only uploads/attachments and presented files are listed; never scripts, intermediates or later files
  assert.deepEqual(view.json.files.map((f: any) => f.path).sort(), ["report.csv", "uploads/in.csv"]);
  // Bob can't write to it: messages, uploads, streams of the original stay Alice's
  assert.equal((await call(B, "POST", `/api/conversations/${conv}/messages`, { text: "hi" })).status, 404);
});

test("share page downloads: uploads, presented files and viewed images only", async () => {
  const dl = (p: string, q = "") => call(B, "GET", `/api/shares/${shareId}/files/${p.split("/").map(encodeURIComponent).join("/")}${q}`);
  assert.equal((await dl("uploads/in.csv")).text, "a,b\n1,2\n");
  const rep = await dl("report.csv");
  assert.equal(rep.text, "r,1\n");
  assert.match(rep.headers.get("content-disposition")!, /^attachment/);
  // the image a tool step showed, by its version, inline
  const img = await dl("chart.png", `?v=${sha["chart.png"]}&inline=1`);
  assert.equal(img.status, 200);
  assert.equal(img.text, "PNGDATA");
  assert.match(img.headers.get("content-disposition")!, /^inline/);
  // hidden workdir files: neither by path nor by their sha256; files after the snapshot neither
  assert.equal((await dl("scripts/tmp.py")).status, 404);
  assert.equal((await dl("scripts/tmp.py", `?v=${sha["scripts/tmp.py"]}`)).status, 404);
  assert.equal((await dl("chart.png")).status, 404);
  // a shown version only under the name it was shown with (no other file name, no other content type)
  assert.equal((await dl("chart.html", `?v=${sha["chart.png"]}`)).status, 404);
  assert.equal((await dl("later.txt")).status, 404);
  assert.equal((await call(STRANGER, "GET", `/api/shares/${shareId}/files/uploads/in.csv`)).status, 403);
  // the rule itself
  const allowed = shareDownloadable({
    messages: [{ attachments: ["data/x.csv"] }],
    turns: [{ presented: [{ path: "out.pdf", sha256: "a".repeat(64), size: 1 }] }],
    events: [],
    files: [
      { path: "data/x.csv", sha256: "b".repeat(64), size: 1, source: "agent" },
      { path: "out.pdf", sha256: "a".repeat(64), size: 1, source: "agent" },
      { path: "tmp.py", sha256: "c".repeat(64), size: 1, source: "agent" },
    ],
  });
  assert.deepEqual([...allowed.paths.keys()].sort(), ["data/x.csv", "out.pdf"]);
  assert.ok(!allowed.shas.has("c".repeat(64)));
});

test("continue: a new conversation of the viewer's with exactly the listed parts; no sandbox, no spend, no parent", async () => {
  const objects = s3.objects.size;
  const sandboxesBefore = (await pg.db.query("select count(*)::int as n from sandboxes")).rows[0].n;
  const r = await call(B, "POST", `/api/shares/${shareId}/continue`);
  assert.equal(r.status, 201, r.text);
  continued = r.json.id;
  assert.notEqual(continued, conv);
  assert.equal(s3.objects.size, objects, "no bytes copied");

  // in Bob's list, same title; not Alice's
  const list = (await call(B, "GET", "/api/conversations")).json;
  assert.equal(list[0].id, continued);
  assert.equal(list[0].title, "Shared title");
  assert.equal((await call(A, "GET", `/api/conversations/${continued}`)).status, 404);

  const got = (await call(B, "GET", `/api/conversations/${continued}`)).json;
  const orig = (await call(A, "GET", `/api/conversations/${conv}`)).json;
  assert.deepEqual(got.messages.map((m: any) => [m.role, m.content, m.attachments]), orig.messages.slice(0, 2).map((m: any) => [m.role, m.content, m.attachments]));
  assert.equal(got.turns.length, 1);
  assert.notEqual(got.turns[0].id, turn1);
  assert.equal(got.turns[0].status, "done");
  assert.equal(got.turns[0].costUsd, null, "spend stays with the original");
  assert.deepEqual(got.turns[0].presented, orig.turns[0].presented, "presented-file records");
  // the presented version downloads from the new conversation (bytes under the original's prefix, through the grant)
  assert.equal((await call(B, "GET", `/api/conversations/${continued}/files/report.csv?v=${sha["report.csv"]}`)).text, "r,1\n");
  assert.deepEqual(got.turns[0].events.map((e: any) => e.type), orig.turns[0].events.map((e: any) => e.type));
  assert.equal(got.messages[1].turnId, got.turns[0].id);
  // nothing names the original or the share: ids, workdir paths (the fake claw's debug cwd) and claw's session rewritten
  const text = JSON.stringify(got);
  assert.ok(!text.includes(conv) && !text.includes(turn1) && !text.includes(shareId), "no trace of the original");
  assert.ok(got.turns[0].events.find((e: any) => e.type === "debug").data.cwd.includes(continued));
  const row = (await pg.db.query("select to_jsonb(c) as c from conversations c where id = $1", [continued])).rows[0].c;
  assert.deepEqual(Object.keys(row).sort(), ["created_at", "deleted_at", "id", "model", "owner", "title", "updated_at"], "no parent column");
  const dump = await pg.db.query(
    `select (select string_agg(to_jsonb(x)::text, '') from turns x where conversation_id = $1) as t,
            (select string_agg(to_jsonb(x)::text, '') from events x where conversation_id = $1) as e,
            (select string_agg(to_jsonb(x)::text, '') from messages x where conversation_id = $1) as m,
            (select content from claw_sessions where conversation_id = $1) as s,
            (select string_agg(to_jsonb(x)::text, '') from kv x where key like '%' || $1 || '%') as k`,
    [continued],
  );
  for (const [k, v] of Object.entries(dump.rows[0])) {
    assert.ok(!String(v ?? "").includes(conv) && !String(v ?? "").includes(turn1) && !String(v ?? "").includes(shareId), `${k} names the original`);
  }
  assert.ok(dump.rows[0].s, "claw's session carried over");
  // every workdir file, hidden ones included, as new rows on the same objects; no sandbox, logs or spend
  const files = (await call(B, "GET", `/api/conversations/${continued}/files`)).json;
  assert.deepEqual(files.map((f: any) => f.path).sort(), ["chart.png", "report.csv", "scripts/tmp.py", "uploads/in.csv"]);
  assert.equal((await call(B, "GET", `/api/conversations/${continued}/files/scripts/tmp.py`)).text, "print(1)\n");
  assert.equal((await pg.db.query("select count(*)::int as n from sandboxes where conversation_id = $1", [continued])).rows[0].n, 0);
  assert.equal((await pg.db.query("select count(*)::int as n from sandboxes")).rows[0].n, sandboxesBefore);
  for (const t of ["egress_log", "tool_calls", "ops_events"]) {
    assert.equal((await pg.db.query(`select count(*)::int as n from ${t} where conversation_id = $1`, [continued])).rows[0].n, 0, t);
  }
  assert.equal((await pg.db.query("select count(*)::int as n from turns where conversation_id = $1 and (usage is not null or cost_usd is not null)", [continued])).rows[0].n, 0);

  // Bob keeps typing: claw resumes the carried session (turn 2), a new sandbox starts with the files restored and the
  // reset note on the first bash command
  const t2 = await call(B, "POST", `/api/conversations/${continued}/messages`, { text: "go on BASH:cat scripts/tmp.py; echo more > mine.txt" });
  assert.equal(t2.status, 202, t2.text);
  await waitTurn(t2.json.turnId);
  const ev = (await pg.db.query("select type, data from events where turn_id = $1 order by seq", [t2.json.turnId])).rows;
  assert.equal(ev.find((e) => e.type === "turn_finished").data.status, "done");
  assert.equal(ev.find((e) => e.type === "text_delta").data.text, "Hello turn 2");
  assert.ok(ev.some((e) => e.type === "sandbox" && e.data.status === "reset"), "the reset notice");
  const out = JSON.parse(ev.find((e) => e.type === "tool_result").data.output);
  assert.ok(out.stdout.startsWith(RESET_NOTE), out.stdout);
  assert.match(out.stdout, /print\(1\)/);

  // independent: Bob's new file is not Alice's, and Alice's later work is not Bob's
  assert.ok(!(await call(A, "GET", `/api/conversations/${conv}/files`)).json.some((f: any) => f.path === "mine.txt"));
  assert.ok(!(await call(B, "GET", `/api/conversations/${continued}/files`)).json.some((f: any) => f.path === "later.txt"));
});

test("revoke: the link stops working for everyone; only the owner revokes", async () => {
  const s = (await call(A, "POST", `/api/conversations/${conv}/shares`)).json.id;
  assert.equal((await call(B, "DELETE", `/api/shares/${s}`)).status, 404);
  assert.equal((await call(B, "GET", `/api/shares/${s}`)).status, 200);
  assert.equal((await call(A, "DELETE", `/api/shares/${s}`)).status, 204);
  assert.equal((await call(B, "GET", `/api/shares/${s}`)).status, 404);
  assert.equal((await call(A, "GET", `/api/shares/${s}`)).status, 404);
  assert.equal((await call(B, "GET", `/api/shares/${s}/files/uploads/in.csv`)).status, 404);
  assert.equal((await call(B, "POST", `/api/shares/${s}/continue`)).status, 404);
  assert.ok(!(await call(A, "GET", `/api/conversations/${conv}/shares`)).json.some((x: any) => x.id === s));
  assert.equal((await call(B, "GET", "/api/shares/not-a-share")).status, 404);
});

test("deleting the original: its links stop, the continued conversation keeps every file (nothing collects them)", async () => {
  assert.equal((await call(A, "DELETE", `/api/conversations/${conv}`)).status, 204);
  await janitorPass(deps, web.app.sandboxes);
  assert.equal((await call(B, "GET", `/api/shares/${shareId}`)).status, 404);
  for (const k of Object.values(sha)) assert.ok([...s3.objects.keys()].some((key) => key.endsWith(k)), "the bytes are still in the bucket");
  assert.equal((await call(B, "GET", `/api/conversations/${continued}/files/scripts/tmp.py`)).text, "print(1)\n");
  // a replica without the local workdir restores everything from the bucket
  rmSync(join(deps.workRoot, continued), { recursive: true, force: true });
  const t = await call(B, "POST", `/api/conversations/${continued}/messages`, { text: "check BASH:cat report.csv uploads/in.csv" });
  await waitTurn(t.json.turnId);
  const ev = (await pg.db.query("select type, data from events where turn_id = $1 order by seq", [t.json.turnId])).rows;
  assert.equal(ev.find((e) => e.type === "turn_finished").data.status, "done");
  assert.match(JSON.parse(ev.find((e) => e.type === "tool_result").data.output).stdout, /r,1\na,b\n1,2/);
  // a grant only opens the bytes it names: another conversation can't read them by sha256
  const other = (await call(B, "POST", "/api/conversations", {})).json.id;
  assert.equal(await deps.blobs.get(`jarvis/files/${other}/${sha["scripts/tmp.py"]}`), null);

  // a continued conversation shared and continued again still finds bytes stored under the deleted original
  const again = (await call(B, "POST", `/api/conversations/${continued}/shares`)).json.id;
  assert.equal((await call(A, "GET", `/api/shares/${again}/files/uploads/in.csv`)).text, "a,b\n1,2\n");
  const third = (await call(A, "POST", `/api/shares/${again}/continue`)).json.id;
  assert.equal((await call(A, "GET", `/api/conversations/${third}/files/scripts/tmp.py`)).text, "print(1)\n");
  assert.equal((await call(A, "GET", `/api/conversations/${third}/files/mine.txt`)).text, "more\n");
});

test("claw's session for a continued conversation: its own session id, rewritten paths", () => {
  const src = [
    JSON.stringify({ type: "session_meta", version: 3, session_id: "session-1-0", workspace_root: "/w/OLD/work", fork: { parent_session_id: "x" } }),
    JSON.stringify({ type: "message", message: { role: "user", blocks: [{ type: "text", text: "read /w/OLD/work/a.txt" }] } }),
  ].join("\n");
  const out = continuedSession(src, (s) => s.replaceAll("OLD", "NEW")).split("\n").map((l) => JSON.parse(l));
  assert.notEqual(out[0].session_id, "session-1-0");
  assert.equal(out[0].workspace_root, "/w/NEW/work");
  assert.equal(out[0].fork, undefined);
  assert.match(out[1].message.blocks[0].text, /\/w\/NEW\/work\/a\.txt/);
});
