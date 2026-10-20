// Live end-to-end check of the "e2b" sandbox provider against real E2B (docs/CHAT.md, "Sandbox providers"): one
// sandbox from the pinned template, configured like the web role does it, bash through the real jarvis-bash, a pip
// install through the mirror and egress, direct internet blocked, then killed. Egress must be running and reachable
// at EGRESS_WIRE (wss://…/wire) with DATABASE_URL's database: the script inserts one `sandboxes` row (a throwaway
// conversation id) so egress knows the sandbox's token, and marks it gone at the end.
//
//   DATABASE_URL=… E2B_API_KEY=… E2B_TEMPLATE=jarvis3-sandbox:<tag> EGRESS_WIRE=wss://<host>/wire \
//     node --disable-warning=ExperimentalWarning scripts/e2b-live.ts
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { connect } from "../src/common/db.ts";
import { newToken, sha256hex } from "../src/common/tokens.ts";
import type { SandboxRow } from "../src/web/sandbox.ts";
import { E2bProvider } from "../src/web/sandbox-e2b.ts";

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} is not set`);
  return v;
};
const BASH = fileURLToPath(new URL("../src/bin/jarvis-bash", import.meta.url));
const db = connect(env("DATABASE_URL"));
const provider = new E2bProvider({
  apiKey: env("E2B_API_KEY"),
  apiUrl: process.env.E2B_API_URL || "https://api.e2b.app",
  domain: "e2b.app",
  template: env("E2B_TEMPLATE"),
  egressUrl: env("EGRESS_WIRE"),
  egressHeaders: {},
  instance: "live-check",
});
const egressToken = newToken();
const row: SandboxRow = {
  id: randomUUID(),
  conversation_id: randomUUID(),
  pod: "",
  exec_token: newToken(),
  status: "starting",
  created_at: new Date(),
  last_active_at: new Date(),
  expires_at: new Date(Date.now() + 3600_000),
  agent: null,
};
const work = mkdtempSync(join(tmpdir(), "e2b-live-"));
const t = (label: string, t0: number) => console.log(`${label}: ${Date.now() - t0} ms`);

let name = "";
try {
  let t0 = Date.now();
  ({ name } = await provider.create(row, egressToken));
  row.pod = name;
  t("create", t0);
  await db.query(
    "insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at) values ($1, $2, $3, $4, 'live-check', 'ready', $5)",
    [row.id, row.conversation_id, name, sha256hex(egressToken), row.expires_at],
  );
  t0 = Date.now();
  const agent = await provider.waitReady(row, Date.now() + 120_000);
  t("init + healthz", t0);
  await provider.keepAlive(row, new Date(Date.now() + 600_000));
  const bash = (command: string, timeout = 300_000) => {
    const t1 = Date.now();
    const out = execFileSync(process.execPath, ["--disable-warning=ExperimentalWarning", BASH], {
      cwd: work,
      input: JSON.stringify({ command, timeout }),
      env: {
        PATH: process.env.PATH!,
        JARVIS_SANDBOX_URL: agent.url,
        JARVIS_SANDBOX_TOKEN: row.exec_token,
        JARVIS_SANDBOX_ID: row.id,
        JARVIS_SANDBOX_HEADERS: JSON.stringify(agent.headers),
        JARVIS_SYNC_STATE: join(work, "..", `sync-${row.id}.json`),
      },
    });
    const r = JSON.parse(out.toString());
    console.log(`\n$ ${command}\n[exit ${r.exit_code}, ${Date.now() - t1} ms]\n${r.stdout}${r.stderr ? `[stderr]\n${r.stderr}` : ""}`);
    return r;
  };
  bash("id; echo HOME=$HOME; uname -r; nproc; free -m | head -2; echo proxy=${HTTPS_PROXY%%:*}://sbx:…@${HTTPS_PROXY##*@}");
  bash("pip install --no-cache-dir cowsay 2>&1 | tail -2 && python3 -c 'import cowsay; cowsay.cow(\"through the egress mirror\")'");
  bash("curl -sS -o /dev/null -w 'via egress, allowed host: HTTP %{http_code}\\n' https://pypi.org/simple/cowsay/");
  bash("curl -sS -o /dev/null -w 'via egress, other host: HTTP %{http_code}\\n' https://example.com/; true");
  bash("curl -sS -m 10 --noproxy '*' -o /dev/null -w 'direct: HTTP %{http_code}\\n' https://example.com/ || echo 'direct internet: blocked'");
  bash("curl -sS -m 10 --noproxy '*' -o /dev/null -w 'direct by IP: HTTP %{http_code}\\n' http://1.1.1.1/ || echo 'direct by IP: blocked'");
  bash("echo saved > from-sandbox.txt");
  console.log("\nfile pulled back into the workdir:", execFileSync("cat", [join(work, "from-sandbox.txt")]).toString().trim());
  const rows = await db.query("select kind, method, host, decision, status from egress_log where sandbox_id = $1 order by id", [row.id]);
  console.log("\negress_log for this sandbox:");
  for (const r of rows.rows) console.log(" ", r.kind, r.method ?? "", r.host, r.decision, r.status ?? "");
} finally {
  if (name) {
    await provider.destroy(name);
    const left = await provider.list().catch(() => []);
    console.log(`\nkilled ${name}; still listed: ${left.some((s) => s.name === name)}`);
  }
  await db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = 'live check' where id = $1", [row.id]);
  await db.end();
  rmSync(work, { recursive: true, force: true });
}
