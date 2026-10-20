import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { after, before, test } from "node:test";
import { packBuffers, unpack } from "../sandbox/tar.mjs";

const TOKEN = "exec-token-test";
let work: string;
let outside: string;
let agent: ChildProcess;
let base: string;

async function freePort(): Promise<number> {
  const s = createServer();
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  const p = (s.address() as { port: number }).port;
  await new Promise((r) => s.close(r));
  return p;
}

before(async () => {
  work = realpathSync(mkdtempSync(join(tmpdir(), "sbx-work-")));
  outside = realpathSync(mkdtempSync(join(tmpdir(), "sbx-outside-")));
  const port = await freePort();
  agent = spawn(process.execPath, [join(import.meta.dirname, "../sandbox/agent.mjs")], {
    env: { ...process.env, JARVIS_EXEC_TOKEN: TOKEN, JARVIS_WORK_DIR: work, PORT: String(port) },
    stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise<void>((resolve) => agent.stdout!.once("data", () => resolve()));
  base = `http://127.0.0.1:${port}`;
});

after(() => {
  agent.kill("SIGTERM");
  rmSync(work, { recursive: true, force: true });
  rmSync(outside, { recursive: true, force: true });
});

const auth = { authorization: `Bearer ${TOKEN}` };

async function exec(command: string, extra: Record<string, unknown> = {}) {
  const r = await fetch(`${base}/exec`, { method: "POST", headers: { ...auth, "content-type": "application/json" }, body: JSON.stringify({ command, ...extra }) });
  assert.equal(r.status, 200);
  return (await r.json()) as { stdout: string; stderr: string; exit_code: number; interrupted: boolean; truncated: boolean; background_id?: string; log?: string };
}

async function put(files: { name: string; data: string | Buffer; mode?: number }[], del: string[] = []) {
  const r = await fetch(`${base}/files`, { method: "PUT", headers: { ...auth, "x-delete": JSON.stringify(del) }, body: packBuffers(files) });
  return { status: r.status, body: (await r.json()) as { ok: boolean; written: number; deleted: number; error?: string } };
}

async function changes(since = "") {
  const r = await fetch(`${base}/changes?since=${encodeURIComponent(since)}`, { headers: auth });
  assert.equal(r.status, 200);
  const files: Record<string, string> = {};
  await unpack(Readable.fromWeb(r.body as any), async (h, body) => {
    const chunks: Buffer[] = [];
    for await (const c of body) chunks.push(c as Buffer);
    files[h.name] = Buffer.concat(chunks).toString("utf8");
  });
  return {
    cursor: r.headers.get("x-cursor")!,
    full: r.headers.get("x-full") === "1",
    deleted: JSON.parse(r.headers.get("x-deleted") || "[]") as string[],
    skipped: JSON.parse(r.headers.get("x-skipped") || "[]") as string[],
    files,
  };
}

test("tar: round trip with a long and a non-ASCII name", async () => {
  const long = `${"d".repeat(120)}/報告 2026.docx`;
  const tar = packBuffers([{ name: "a.txt", data: "A" }, { name: long, data: Buffer.alloc(1500, 7) }, { name: "empty", data: "" }]);
  const got: Record<string, number> = {};
  await unpack(Readable.from([tar.subarray(0, 700), tar.subarray(700)]), async (h, body) => {
    let n = 0;
    for await (const c of body) n += (c as Buffer).length;
    got[h.name] = n;
  });
  assert.deepEqual(got, { "a.txt": 1, [long]: 1500, empty: 0 });
});

test("healthz is open, everything else needs the exec token", async () => {
  const h = await fetch(`${base}/healthz`);
  assert.equal(h.status, 200);
  assert.match(((await h.json()) as { boot_id: string }).boot_id, /^[0-9a-f]{16}$/);
  assert.equal((await fetch(`${base}/changes`)).status, 401);
  assert.equal((await fetch(`${base}/exec`, { method: "POST", headers: { authorization: "Bearer nope" }, body: "{}" })).status, 401);
});

test("PUT /files writes, deletes and refuses paths outside /work", async () => {
  const r = await put([{ name: "docs/a.txt", data: "hello" }, { name: "run.sh", data: "#!/bin/sh\necho ran\n", mode: 0o755 }]);
  assert.deepEqual(r.body, { ok: true, written: 2, deleted: 0 });
  assert.equal(readFileSync(join(work, "docs/a.txt"), "utf8"), "hello");
  assert.equal((await exec("./run.sh")).stdout, "ran\n");
  assert.equal((await put([], ["docs"])).body.deleted, 1);
  assert.ok(!existsSync(join(work, "docs")));
  assert.equal((await put([{ name: "../escape.txt", data: "x" }])).status, 400);
  assert.equal((await put([], ["/etc/passwd"])).status, 400);
  symlinkSync(outside, join(work, "link"));
  const viaLink = await put([{ name: "link/x.txt", data: "x" }]);
  assert.equal(viaLink.status, 400);
  assert.match(viaLink.body.error!, /escapes/);
  assert.ok(!existsSync(join(outside, "x.txt")));
  rmSync(join(work, "link"));
});

test("exec: output, exit codes, cwd, no token in the environment", async () => {
  const r = await exec("echo out; echo err >&2; pwd; echo token=${JARVIS_EXEC_TOKEN:-none}; exit 3");
  assert.equal(r.exit_code, 3);
  assert.equal(r.stdout, `out\n${work}\ntoken=none\n`);
  assert.equal(r.stderr, "err\n");
  assert.equal(r.interrupted, false);
});

test("exec: timeout kills the whole process group", async () => {
  const t0 = Date.now();
  const r = await exec("sleep 30 & sleep 30; echo never", { timeout_ms: 1000 });
  assert.ok(r.interrupted);
  assert.ok(Date.now() - t0 < 5000);
  assert.notEqual(r.exit_code, 0);
  assert.equal(r.stdout, "");
});

test("exec: output over 1 MB keeps the tail", async () => {
  const r = await exec("head -c 1500000 /dev/zero | tr '\\0' a; echo END");
  assert.ok(r.truncated);
  assert.ok(r.stdout.endsWith("aEND\n"));
  assert.ok(r.stdout.length <= 1024 * 1024);
});

test("exec: a command that leaves a process running returns; background jobs keep running between calls", async () => {
  const t0 = Date.now();
  const r = await exec("sleep 20 > /dev/null 2>&1 & echo started");
  assert.equal(r.stdout, "started\n");
  assert.ok(Date.now() - t0 < 3000);
  const bg = await exec("for i in 1 2 3; do echo tick $i; sleep 0.2; done; sleep 20", { background: true });
  assert.match(bg.background_id!, /^[0-9a-f]{12}$/);
  await new Promise((res) => setTimeout(res, 900));
  assert.match(readFileSync(join(work, bg.log!), "utf8"), /tick 3/);
  const ps = await exec("pgrep -f 'sleep 20' | wc -l");
  assert.ok(Number(ps.stdout.trim()) >= 2, ps.stdout);
  await exec("pkill -f 'sleep 20' || true");
});

test("changes: full first, then only what changed; pushes are not echoed back; excludes and big files", async () => {
  rmSync(join(work, ".jobs"), { recursive: true, force: true });
  const first = await changes();
  assert.ok(first.full);
  assert.ok("run.sh" in first.files);
  const c0 = first.cursor;

  await exec("mkdir -p out node_modules/x && echo 'r1' > out/report.md && echo junk > node_modules/x/i.js && truncate -s 51M big.bin");
  const second = await changes(c0);
  assert.equal(second.full, false);
  assert.deepEqual(Object.keys(second.files).sort(), ["out/report.md"]);
  assert.equal(second.files["out/report.md"], "r1\n");
  assert.deepEqual(second.skipped, ["big.bin"]);
  assert.deepEqual(second.deleted, []);

  await put([{ name: "uploads/brief.txt", data: "from the server" }]);
  await exec("rm out/report.md");
  const third = await changes(second.cursor);
  assert.deepEqual(Object.keys(third.files), [], "the pushed file is not sent back");
  assert.deepEqual(third.deleted, ["out/report.md"]);

  const unknown = await changes("deadbeef:1");
  assert.ok(unknown.full);
  assert.ok("uploads/brief.txt" in unknown.files);
});

test("changes: very long deletion lists move into a meta entry", async () => {
  mkdirSync(join(work, "many"), { recursive: true });
  for (let i = 0; i < 400; i++) writeFileSync(join(work, "many", `file-number-${i}-with-a-long-name.txt`), "x");
  const c = (await changes()).cursor;
  rmSync(join(work, "many"), { recursive: true });
  const r = await fetch(`${base}/changes?since=${c}`, { headers: auth });
  assert.equal(r.headers.get("x-meta-entry"), ".jarvis-meta/changes.json");
  let meta: { deleted: string[] } | null = null;
  await unpack(Readable.fromWeb(r.body as any), async (h, body) => {
    const chunks: Buffer[] = [];
    for await (const x of body) chunks.push(x as Buffer);
    if (h.name === ".jarvis-meta/changes.json") meta = JSON.parse(Buffer.concat(chunks).toString());
  });
  assert.equal(meta!.deleted.length, 400);
});
