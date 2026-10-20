// jarvis-bash: claw's bash tool (CLAW_BASH_COMMAND, docs/CHAT.md "jarvis-bash"). The command runs in the
// conversation's sandbox, never here: push what changed in the workdir since the last call, exec, pull what the
// command changed back into the workdir. The sync state (what the sandbox already has, the /changes cursor) is a JSON
// file outside the workdir, so the agent's file tools can't touch it.
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { tarPack, tarUnpack } from "../common/tar.ts";
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
  const { tar, cursor, deleted } = await agent.changes(state.cursor);
  let n = 0;
  for (const e of tarUnpack(tar)) {
    const sha = sha256Buf(e.data);
    if (state.manifest[e.path]?.sha256 === sha) continue; // an echo of what was pushed
    await writeInto(workdir, e.path, e.data);
    n++;
  }
  for (const rel of deleted) {
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
  env: { workdir: string; sandboxUrl: string; sandboxToken: string; sandboxId: string; statePath: string },
): Promise<BashOutput> {
  const command = String(input.command ?? "").trim();
  if (!command) return { stdout: "", stderr: "No command given.", exit_code: 2, interrupted: false };
  const agent = new AgentClient(env.sandboxUrl, env.sandboxToken);
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
      stdout: `Started in the background (id ${r.background_id}, pid ${r.pid ?? "?"}). Its output goes to .jobs/${r.background_id}.log in the workspace.`,
      stderr: "",
      exit_code: 0,
      interrupted: false,
      background_task_id: r.background_id,
    };
  }
  const note = r.truncated ? "\n[output truncated: only the end is shown]" : "";
  return { stdout: (r.stdout ?? "") + note, stderr: r.stderr ?? "", exit_code: r.exit_code, interrupted: Boolean(r.interrupted) };
}
