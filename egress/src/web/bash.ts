// jarvis-bash: claw's bash tool (CLAW_BASH_COMMAND, docs/CHAT.md "jarvis-bash"). The command runs in the
// conversation's sandbox, never here: push what changed in the workdir since the last call, exec, pull what the
// command changed back into the workdir. The sync state (what the sandbox already has, the /changes cursor) is a JSON
// file outside the workdir, so the agent's file tools can't touch it.
import { request } from "node:http";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { tarPack } from "../common/tar.ts";
import { AgentClient, type ExecResult } from "./agent.ts";
import { manifestOf, readJson, removeFrom, sha256Buf, writeInto, writeJsonAtomic, type Manifest } from "./workspace.ts";

export interface BashInput {
  command?: string;
  timeout?: number;
  description?: string;
  run_in_background?: boolean;
}

export interface BashOutput {
  stdout: string;
  stderr: string;
  exit_code: number | null;
  interrupted: boolean;
  background_task_id?: string;
}

/** Told to the model (with the output of its first command there) when a new sandbox replaced the conversation's earlier one. */
export const RESET_NOTE =
  "[System note: the sandbox was reset: files in the workspace are restored; installed packages and background processes from before are gone.]";

/** The turn runner's answer to jarvis-bash's `POST /sandbox` (turns.ts): the sandbox, or why there is none. */
export type SandboxGrant =
  | { id: string; url: string; token: string; headers: Record<string, string>; reset: boolean; unavailable?: undefined }
  | { unavailable: string };

/**
 * Asks this replica's turn runner (unix socket JARVIS_SANDBOX_SOCKET) for the conversation's sandbox, made if needed;
 * with path "/sandbox/used", says a command ended (the sandbox's idle time starts again).
 */
export function requestSandbox(socketPath: string, turnToken: string, timeoutMs = 240_000, path = "/sandbox"): Promise<SandboxGrant> {
  return new Promise((resolve, reject) => {
    const req = request({ socketPath, path, method: "POST", headers: { authorization: `Bearer ${turnToken}`, "content-length": "0" }, timeout: timeoutMs }, (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (c: Buffer) => chunks.push(c));
      res.on("end", () => {
        try {
          const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          if (res.statusCode === 200 || body?.unavailable) resolve(body);
          else reject(new Error(`sandbox request: HTTP ${res.statusCode}`));
        } catch (e) {
          reject(e);
        }
      });
    });
    req.on("timeout", () => req.destroy(new Error("sandbox request timed out")));
    req.on("error", reject);
    req.end();
  });
}

export interface SyncState {
  sandboxId: string;
  cursor: string;
  /** What the sandbox holds of the workdir, as last pushed or pulled. */
  manifest: Manifest;
}

const DEFAULT_TIMEOUT_MS = 120_000;
const MAX_TIMEOUT_MS = 600_000;

export async function pushChanges(workdir: string, agent: AgentClient, state: SyncState): Promise<number> {
  const now = await manifestOf(workdir, state.manifest);
  const changed: string[] = [];
  for (const [rel, e] of Object.entries(now)) if (state.manifest[rel]?.sha256 !== e.sha256) changed.push(rel);
  const deleted = Object.keys(state.manifest).filter((rel) => !now[rel]);
  if (changed.length || deleted.length) {
    const entries = [];
    for (const rel of changed) entries.push({ path: rel, data: await readFile(join(workdir, rel)), mtimeMs: now[rel]!.mtimeMs });
    await agent.putFiles(tarPack(entries), deleted);
  }
  state.manifest = now;
  return changed.length + deleted.length;
}

export async function pullChanges(workdir: string, agent: AgentClient, state: SyncState): Promise<number> {
  const { entries, cursor, deleted, skipped, full } = await agent.changes(state.cursor);
  let n = 0;
  const present = new Set<string>();
  for (const e of entries) {
    present.add(e.path);
    if (state.manifest[e.path]?.sha256 === sha256Buf(e.data)) continue; // unchanged, or an echo of what was pushed
    await writeInto(workdir, e.path, e.data);
    n++;
  }
  // a full listing is the whole truth: what the sandbox no longer has (and didn't skip as too large) is gone
  const gone = full ? Object.keys(state.manifest).filter((rel) => !present.has(rel) && !skipped.includes(rel)) : deleted;
  for (const rel of gone) {
    if (state.manifest[rel]) {
      await removeFrom(workdir, rel);
      n++;
    }
  }
  if (cursor) state.cursor = cursor;
  // the workdir now matches the sandbox: re-read stats (only pulled files are re-hashed)
  state.manifest = await manifestOf(workdir, state.manifest);
  return n;
}

export async function runBash(
  input: BashInput,
  env: { workdir: string; sandboxUrl: string; sandboxToken: string; sandboxId: string; statePath: string; sandboxHeaders?: Record<string, string> },
): Promise<BashOutput> {
  const command = String(input.command ?? "").trim();
  if (!command) return { stdout: "", stderr: "No command given.", exit_code: 2, interrupted: false };
  const agent = new AgentClient(env.sandboxUrl, env.sandboxToken, env.sandboxHeaders);
  let state = await readJson<SyncState>(env.statePath, { sandboxId: "", cursor: "", manifest: {} });
  if (state.sandboxId !== env.sandboxId) state = { sandboxId: env.sandboxId, cursor: "", manifest: {} };
  await pushChanges(env.workdir, agent, state);
  await writeJsonAtomic(env.statePath, state);
  const timeoutMs = Math.min(Math.max(Number(input.timeout) || DEFAULT_TIMEOUT_MS, 1000), MAX_TIMEOUT_MS);
  let r: ExecResult;
  try {
    r = await agent.exec(command, timeoutMs, Boolean(input.run_in_background));
  } finally {
    await pullChanges(env.workdir, agent, state).catch(() => {});
    await writeJsonAtomic(env.statePath, state);
  }
  if (r.background_id) {
    return {
      stdout: `Started in the background (id ${r.background_id}, pid ${r.pid ?? "?"}). Its output goes to ${r.log || `.jobs/${r.background_id}.log`} in the workspace.`,
      stderr: "",
      exit_code: 0,
      interrupted: false,
      background_task_id: r.background_id,
    };
  }
  const note = r.truncated ? "\n[output truncated: only the end is shown]" : "";
  return { stdout: (r.stdout ?? "") + note, stderr: r.stderr ?? "", exit_code: r.exit_code, interrupted: Boolean(r.interrupted) };
}
