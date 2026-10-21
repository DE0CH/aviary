// The sandbox agent (chat/sandbox/agent.mjs) as a local process: its health check, the exec token, and processes over
// `GET /proc` (what the CloudCLI UI runs Claude Code through).
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { request } from "node:http";
import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

const TOKEN = "exec-token-test";
let workspace: string;
let agent: ChildProcess;
let base: string;

async function freePort(): Promise<number> {
  const s = createServer();
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  const p = (s.address() as { port: number }).port;
  await new Promise((r) => s.close(r));
  return p;
}

async function startAgent(env: Record<string, string>): Promise<{ child: ChildProcess; base: string }> {
  const port = await freePort();
  const child = spawn(process.execPath, [join(import.meta.dirname, "../sandbox/agent.mjs")], {
    env: { PATH: process.env.PATH!, JARVIS_WORKSPACE: workspace, PORT: String(port), ...env },
    stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise<void>((resolve) => child.stdout!.once("data", () => resolve()));
  return { child, base: `http://127.0.0.1:${port}` };
}

before(async () => {
  workspace = realpathSync(mkdtempSync(join(tmpdir(), "sbx-ws-")));
  ({ child: agent, base } = await startAgent({ JARVIS_EXEC_TOKEN: TOKEN }));
});

after(() => {
  agent.kill("SIGTERM");
  rmSync(workspace, { recursive: true, force: true });
});

const PROC_STDIN = 0, PROC_STDOUT = 1, PROC_STDERR = 2, PROC_EXIT = 3, PROC_STDIN_END = 4, PROC_SIGNAL = 5;
const frame = (type: number, payload: string) => {
  const b = Buffer.from(payload);
  const h = Buffer.alloc(5);
  h[0] = type;
  h.writeUInt32BE(b.length, 1);
  return Buffer.concat([h, b]);
};

interface Proc {
  socket: Socket;
  done: Promise<{ stdout: string; stderr: string; exit: { code: number | null; signal: string | null } }>;
}

/** Opens a /proc upgrade; resolves with the socket, or rejects with the refusal's status line. */
function openProc(spec: Record<string, unknown>, token = TOKEN, url = base): Promise<Proc> {
  return new Promise((resolve, reject) => {
    const u = new URL("/proc", url);
    const req = request({
      hostname: u.hostname,
      port: u.port,
      path: u.pathname,
      headers: { "x-jarvis-exec-token": token, connection: "Upgrade", upgrade: "jarvis-proc", "x-proc": Buffer.from(JSON.stringify(spec)).toString("base64") },
    });
    req.on("error", reject);
    req.on("response", (res) => reject(new Error(String(res.statusCode))));
    req.on("upgrade", (_res, socket) => {
      let stdout = "";
      let stderr = "";
      let buf = Buffer.alloc(0);
      const done = new Promise<Awaited<Proc["done"]>>((ok) => {
        socket.on("data", (c: Buffer) => {
          buf = Buffer.concat([buf, c]);
          while (buf.length >= 5 && buf.length >= 5 + buf.readUInt32BE(1)) {
            const n = buf.readUInt32BE(1);
            const t = buf[0]!;
            const p = buf.subarray(5, 5 + n);
            buf = buf.subarray(5 + n);
            if (t === PROC_STDOUT) stdout += p.toString("utf8");
            if (t === PROC_STDERR) stderr += p.toString("utf8");
            if (t === PROC_EXIT) ok({ stdout, stderr, exit: JSON.parse(p.toString("utf8")) });
          }
        });
      });
      resolve({ socket, done });
    });
    req.end();
  });
}

test("healthz is open; /proc needs the exec token; nothing else is served", async () => {
  const h = await fetch(`${base}/healthz`);
  assert.equal(h.status, 200);
  assert.match(((await h.json()) as { boot_id: string }).boot_id, /^[0-9a-f]{16}$/);
  await assert.rejects(openProc({ args: ["true"] }, "nope"), /401/);
  assert.equal((await fetch(`${base}/exec`, { method: "POST", headers: { authorization: `Bearer ${TOKEN}` }, body: "{}" })).status, 404);
  assert.equal((await fetch(`${base}/changes`, { headers: { authorization: `Bearer ${TOKEN}` } })).status, 404);
});

test("/proc: stdin in, stdout and stderr out, the exit status, no token in the environment", async () => {
  const p = await openProc({ args: ["sh", "-c", 'read x; echo "out:$x"; echo err >&2; echo "tok:${JARVIS_EXEC_TOKEN:-none} env:$EXTRA"; pwd; exit 3'], env: { EXTRA: "yes" } });
  p.socket.write(frame(PROC_STDIN, "hi\n"));
  p.socket.write(frame(PROC_STDIN_END, ""));
  const r = await p.done;
  assert.equal(r.stdout, `out:hi\ntok:none env:yes\n${workspace}\n`);
  assert.equal(r.stderr, "err\n");
  assert.deepEqual(r.exit, { code: 3, signal: null });
});

test("/proc: cwd only inside /workspace (made if missing); bad specs are refused", async () => {
  const p = await openProc({ args: ["pwd"], cwd: join(workspace, "sub/dir") });
  p.socket.write(frame(PROC_STDIN_END, ""));
  assert.equal((await p.done).stdout, `${join(workspace, "sub/dir")}\n`);
  await assert.rejects(openProc({ args: ["pwd"], cwd: "/etc" }), /400/);
  await assert.rejects(openProc({ args: [] }), /400/);
  await assert.rejects(openProc({ args: ["true"], env: { "BAD-NAME": "x" } }), /400/);
  const missing = await openProc({ args: ["no-such-program-here"] });
  assert.equal((await missing.done).exit.code, 127);
});

test("/proc: a signal reaches the process group; a dropped connection kills the process", async () => {
  const p = await openProc({ args: ["sh", "-c", "sleep 30 & wait"] });
  p.socket.write(frame(PROC_SIGNAL, "SIGTERM"));
  const r = await p.done;
  assert.ok(r.exit.signal === "SIGTERM" || r.exit.code !== 0, JSON.stringify(r.exit));

  const marker = join(workspace, "alive");
  mkdirSync(marker);
  const q = await openProc({ args: ["sh", "-c", `trap 'rmdir ${marker}; exit 0' TERM; while true; do sleep 0.1; done`] });
  await new Promise((r) => setTimeout(r, 300));
  q.socket.destroy();
  for (let i = 0; i < 50 && existsSync(marker); i++) await new Promise((r) => setTimeout(r, 100));
  assert.equal(existsSync(marker), false, "the process got SIGTERM when its connection dropped");
});

test("JARVIS_INIT=1: not healthy until POST /init gives the token and env; init only once", async () => {
  const { child, base: b } = await startAgent({ JARVIS_INIT: "1" });
  try {
    assert.equal((await fetch(`${b}/livez`)).status, 200);
    assert.equal((await fetch(`${b}/healthz`)).status, 503);
    const token = "init-token-0123456789";
    const r = await fetch(`${b}/init`, { method: "POST", body: JSON.stringify({ token, env: { FROM_INIT: "1" } }) });
    assert.equal(r.status, 200);
    assert.equal((await fetch(`${b}/healthz`)).status, 200);
    assert.equal((await fetch(`${b}/init`, { method: "POST", body: JSON.stringify({ token: "another-token-0123456" }) })).status, 409);
    const p = await openProc({ args: ["sh", "-c", "echo $FROM_INIT"] }, token, b);
    p.socket.write(frame(PROC_STDIN_END, ""));
    assert.equal((await p.done).stdout, "1\n");
  } finally {
    child.kill("SIGTERM");
  }
});
