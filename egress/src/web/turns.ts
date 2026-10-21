// The turn runner (docs/CHAT.md, "Turn runner"): one claw process per turn on the replica that accepted the message.
// claw's NDJSON events become rows in `events` (text deltas batched), the conversation's files and claw's session
// file are restored before and stored after, and bash goes to the conversation's sandbox through jarvis-bash.
import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { BUDGET_ERROR_TYPE, budgetMessage, budgetStatus, type BudgetStatus } from "../common/budget.ts";
import { effectiveModel } from "../common/config.ts";
import { channel } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import type { WebDeps } from "./ctx.ts";
import { appendEvent } from "./events.ts";
import { dirs, restoreWorkdir, storeWorkdir } from "./files.ts";
import { PoolFullError, type LiveSandbox, type SandboxManager } from "./sandbox.ts";

const TEXT_BATCH_MS = 250;
const MAX_TOOL_OUTPUT = 20_000;
/** claw and jarvis-bash run in the workdir: relative paths become absolute (bare command names stay PATH lookups). */
const abs = (p: string) => (p.includes("/") ? resolve(p) : p);
/** How often a running turn marks its sandbox active. */
const KEEPALIVE_MS = 60_000;
export const RESET_NOTE =
  "[System note: the sandbox was reset: files in the workspace are restored; installed packages and background processes from before are gone.]";

interface Running {
  child: ChildProcess | null;
  cancelled: boolean;
  done: Promise<void>;
}

interface TurnInfo {
  id: string;
  conversation_id: string;
  model: string;
  owner: string;
  title: string;
}

export class TurnRunner {
  readonly #deps: WebDeps;
  readonly #sandboxes: SandboxManager;
  readonly #running = new Map<string, Running>();
  readonly #pendingCancel = new Set<string>();
  draining = false;

  constructor(deps: WebDeps, sandboxes: SandboxManager) {
    this.#deps = deps;
    this.#sandboxes = sandboxes;
  }

  get active(): number {
    return this.#running.size;
  }

  /** Cancels on this replica's channel come from POST /api/turns/:id/cancel on any replica. */
  async listen(): Promise<void> {
    await this.#deps.listener.listen(channel("cancel", this.#deps.replica), (turnId) => {
      if (turnId) this.cancel(turnId);
    });
  }

  start(turnId: string, token: string): void {
    let resolveDone!: () => void;
    const r: Running = { child: null, cancelled: this.#pendingCancel.delete(turnId), done: new Promise((res) => (resolveDone = res)) };
    this.#running.set(turnId, r);
    this.#run(turnId, token, r)
      .catch((e) => logError("turn", e, { turn: turnId }))
      .finally(() => {
        this.#running.delete(turnId);
        resolveDone();
      });
  }

  cancel(turnId: string): boolean {
    const r = this.#running.get(turnId);
    if (!r) return false;
    r.cancelled = true;
    const child = r.child;
    if (child && child.exitCode === null) {
      child.kill("SIGTERM");
      setTimeout(() => child.exitCode === null && child.kill("SIGKILL"), 5000).unref();
    }
    return true;
  }

  /** Waits for running turns (up to timeoutMs), then cancels what is left. */
  async drain(timeoutMs: number): Promise<void> {
    this.draining = true;
    const all = () => Promise.all([...this.#running.values()].map((r) => r.done));
    await Promise.race([all(), new Promise((r) => setTimeout(r, timeoutMs).unref())]);
    for (const id of this.#running.keys()) this.cancel(id);
    await Promise.race([all(), new Promise((r) => setTimeout(r, 10_000).unref())]);
  }

  async #run(turnId: string, token: string, r: Running): Promise<void> {
    const deps = this.#deps;
    const db = deps.db;
    const { rows } = await db.query<TurnInfo>(
      `update turns t set status = 'running', replica = $2 from conversations c
        where t.id = $1 and t.status = 'queued' and c.id = t.conversation_id
        returning t.id, t.conversation_id, t.model, c.owner, c.title`,
      [turnId, deps.replica],
    );
    const turn = rows[0];
    if (!turn) return;
    turn.model = effectiveModel(deps.config, turn.model);
    const conv = turn.conversation_id;
    const startedAt = new Date();
    const emit = (type: string, data: Record<string, unknown>) => appendEvent(db, conv, turnId, type, data);
    const d = dirs(deps, conv);
    let status: "done" | "error" | "cancelled" = "error";
    let error: string | null = null;
    let finalText = "";
    let usage: unknown = null;
    let sessionPath = "";
    let clawCost: number | null = null;
    let budgetHit: BudgetStatus | null = null;
    // the shared budget: used up = no turn (and so no model call); fails open when it can't be read
    const budget = await budgetStatus(db, deps.config.budget).catch((e) => (logError("budget", e, { turn: turnId }), null));
    if (budget?.exhausted) return await this.#refuseForBudget(turn, budget, startedAt);
    try {
      await emit("turn_started", { turnId, model: turn.model });
      const { rows: msgs } = await db.query<{ content: string; attachments: string[] }>(
        "select content, attachments from messages where turn_id = $1 and role = 'user' order by id limit 1",
        [turnId],
      );
      const msg = msgs[0];
      if (!msg) throw new Error("the turn has no message");
      await restoreWorkdir(deps, conv);
      await mkdir(d.sessions, { recursive: true });
      await mkdir(d.clawConfig, { recursive: true });

      // sandbox (warm already, usually); a turn without one still runs, its bash calls then fail with a reason
      let sandbox: LiveSandbox | null = null;
      let sandboxUnavailable = "";
      try {
        const { rows: live } = await db.query("select 1 from sandboxes where conversation_id = $1 and status = 'ready'", [conv]);
        if (!live.length) await emit("sandbox", { status: "starting" });
        sandbox = await this.#sandboxes.ensure(conv);
        if (!live.length) await emit("sandbox", { status: "ready" });
      } catch (e) {
        await emit("sandbox", { status: "unavailable", message: (e as Error).message });
        // what the bash tool tells the model (and so the staff member): plain words, no internals
        sandboxUnavailable = e instanceof PoolFullError ? e.message : "it could not be started";
      }
      let prompt = msg.content;
      const attachments = Array.isArray(msg.attachments) ? msg.attachments : [];
      if (attachments.length) prompt += `\n\n[Attached files, in the workspace: ${attachments.join(", ")}]`;
      if (sandbox) {
        const { rows: seen } = await db.query<{ value: string }>("select value from kv where key = $1", [`turn_sandbox:${conv}`]);
        if (seen[0] && seen[0].value !== sandbox.id) {
          prompt = `${RESET_NOTE}\n\n${prompt}`;
          await emit("sandbox", { status: "reset" });
        }
        await db.query(
          "insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value",
          [`turn_sandbox:${conv}`, JSON.stringify(sandbox.id)],
        );
      }

      // claw's config: the tools MCP server through the stdio bridge; nothing is read from the workdir
      const toolNames = await this.#toolNames(token);
      const settings: Record<string, unknown> = { permissions: { defaultMode: "danger-full-access" } };
      if (toolNames.length) {
        settings.mcpServers = {
          tools: { command: abs(deps.mcpBridgePath), args: [], env: { JARVIS_TOOLS_URL: deps.toolsUrl, JARVIS_TURN_TOKEN: token } },
        };
      }
      await writeFile(join(d.clawConfig, "settings.json"), JSON.stringify(settings, null, 2));
      const { rows: sess } = await db.query<{ content: string }>("select content from claw_sessions where conversation_id = $1", [conv]);
      const resumePath = join(d.sessions, `${conv}.jsonl`);
      if (sess[0]) await writeFile(resumePath, sess[0].content);

      const allowed = ["read", "write", "edit", "glob", "grep", "bash", ...toolNames.map((n) => `mcp__tools__${n.replace(/[^A-Za-z0-9_-]/g, "_")}`)];
      const args = ["--model", turn.model, "--permission-mode", "danger-full-access", "--allowedTools", allowed.join(","), "--output-format", "json", "prompt", prompt];
      const env: Record<string, string> = {
        PATH: process.env.PATH || "/usr/local/bin:/usr/bin:/bin",
        HOME: d.base,
        LANG: "C.UTF-8",
        CLAW_EVENT_STREAM: "1",
        CLAW_SYSTEM_PROMPT_FILE: abs(deps.config.systemPromptFile),
        CLAW_BASH_COMMAND: abs(deps.bashPath),
        CLAW_CONFIG_DIR: d.clawConfig,
        CLAW_CONFIG_HOME: d.clawConfig,
        CLAW_SESSIONS_DIR: d.sessions,
        CLAW_PROMPT_CACHE: "1",
        ANTHROPIC_BASE_URL: `${deps.egressBase}/x/model`,
        ANTHROPIC_AUTH_TOKEN: token,
        JARVIS_SYNC_STATE: d.syncState,
        ...(deps.clawEnv ?? {}),
      };
      if (sess[0]) env.CLAW_RESUME_SESSION = resumePath;
      if (!sandbox) env.JARVIS_SANDBOX_UNAVAILABLE = sandboxUnavailable || "it could not be started";
      if (sandbox) {
        Object.assign(env, { JARVIS_SANDBOX_URL: sandbox.url, JARVIS_SANDBOX_TOKEN: sandbox.execToken, JARVIS_SANDBOX_ID: sandbox.id });
        if (Object.keys(sandbox.headers).length) env.JARVIS_SANDBOX_HEADERS = JSON.stringify(sandbox.headers);
      }
      if (r.cancelled) throw new CancelledError();

      const child = spawn(abs(deps.clawPath), args, { cwd: d.work, env, stdio: ["pipe", "pipe", "pipe"] });
      r.child = child;
      let stderrTail = "";
      child.stderr!.on("data", (c: Buffer) => (stderrTail = (stderrTail + c.toString("utf8")).slice(-4000)));
      child.stdin!.on("error", () => {});
      const exited = new Promise<number | null>((res) => child.on("close", (code) => res(code)));
      // the agent's bash calls go straight to the sandbox: keep it (and a provider's own timeout) alive meanwhile
      const keep = setInterval(() => void this.#sandboxes.touch(conv).catch((e) => logError("sandbox touch", e, { turn: turnId })), KEEPALIVE_MS);
      void exited.finally(() => clearInterval(keep));
      const spawnError = new Promise<Error>((res) => child.on("error", res));
      void spawnError.then(() => clearInterval(keep));

      // events, with text and thinking deltas batched
      let pending: { type: string; text: string } | null = null;
      let flushTimer: NodeJS.Timeout | null = null;
      let chain = Promise.resolve();
      const queue = (f: () => Promise<unknown>) => (chain = chain.then(() => f().then(() => {})).catch((e) => logError("turn event", e, { turn: turnId })));
      const flush = () => {
        if (flushTimer) clearTimeout(flushTimer);
        flushTimer = null;
        if (!pending) return;
        const p = pending;
        pending = null;
        queue(() => emit(p.type, { text: p.text }));
      };
      let lastMessageText = "";
      let errorMessage = "";
      const rl = createInterface({ input: child.stdout! });
      const rlClosed = new Promise<void>((res) => rl.once("close", () => res()));
      rl.on("line", (line) => {
        let ev: Record<string, any>;
        try {
          ev = JSON.parse(line);
        } catch {
          return;
        }
        if (!ev || typeof ev.type !== "string") return;
        const { type, ...data } = ev;
        if (type === "text_delta" || type === "thinking_delta") {
          const text = String(data.text ?? "");
          if (type === "text_delta") lastMessageText += text;
          if (!text) return;
          if (pending && pending.type !== type) flush();
          pending = pending ? { type, text: pending.text + text } : { type, text };
          flushTimer ??= setTimeout(flush, TEXT_BATCH_MS);
          return;
        }
        flush();
        switch (type) {
          case "session":
            sessionPath = String(data.path || "");
            return;
          case "message_start":
            lastMessageText = "";
            break;
          case "permission_request":
            // danger-full-access asks nothing; anything that does ask is refused (outward actions confirm in tools)
            child.stdin!.write(JSON.stringify({ type: "permission_response", request_id: data.request_id, allow: false, reason: "not allowed here" }) + "\n");
            break;
          case "tool_result":
            if (typeof data.output === "string" && data.output.length > MAX_TOOL_OUTPUT) {
              data.output = data.output.slice(0, MAX_TOOL_OUTPUT);
              data.truncated = true;
            }
            break;
          case "result":
            finalText = typeof data.message === "string" ? data.message : "";
            usage = data.usage ?? null;
            clawCost = typeof data.estimated_cost === "number" ? data.estimated_cost : null;
            break;
          case "error":
            errorMessage = String(data.message ?? "error");
            // egress refused a model call: the budget ran out during this turn
            if (errorMessage.includes(BUDGET_ERROR_TYPE)) {
              const msg = /budget_exceeded\)?:\s*(.+)$/s.exec(errorMessage)?.[1]?.trim() || errorMessage;
              budgetHit = { ...(budget ?? emptyBudget()), exhausted: true };
              errorMessage = msg;
              for (const k of Object.keys(data)) delete data[k];
              Object.assign(data, budgetEventData(budgetHit, msg));
            }
            break;
        }
        queue(() => emit(type, data));
      });
      const code = await Promise.race([exited, spawnError.then((e) => Promise.reject(e))]);
      await rlClosed;
      flush();
      await chain;
      if (!finalText) finalText = lastMessageText;
      if (budgetHit) {
        // the reply so far is kept; the budget message ends it (code "budget": the UI shows it in its own locale)
        const fresh = await budgetStatus(db, deps.config.budget).catch(() => null);
        if (fresh) budgetHit = fresh;
        errorMessage = budgetMessage(deps.config.product.locale, budgetHit.limitUsd, budgetHit.ownerName);
      }
      if (r.cancelled) status = "cancelled";
      else if (code === 0 && !errorMessage) status = "done";
      else {
        status = "error";
        error = errorMessage || stderrTail.trim().split("\n").slice(-3).join("\n") || `claw exited with ${code}`;
      }
    } catch (e) {
      if (e instanceof CancelledError || r.cancelled) status = "cancelled";
      else {
        status = "error";
        error = (e as Error).message;
      }
    }

    // store what the turn produced, whatever its outcome
    try {
      if (status === "done") {
        const p = sessionPath || join(d.sessions, `${conv}.jsonl`);
        const content = await readFile(p, "utf8").catch(() => "");
        if (content) {
          await db.query(
            `insert into claw_sessions (conversation_id, content, updated_at) values ($1, $2, now())
             on conflict (conversation_id) do update set content = excluded.content, updated_at = now()`,
            [conv, content],
          );
        }
      }
      const changed = await storeWorkdir(deps, conv, startedAt);
      if (changed.length) await emit("files", { changed });
    } catch (e) {
      logError("turn store", e, { turn: turnId });
      if (status === "done") {
        status = "error";
        error = `saving the conversation's files failed: ${(e as Error).message}`;
      }
    }
    if (finalText || status === "done") {
      await db.query("insert into messages (conversation_id, role, content, turn_id) values ($1, 'assistant', $2, $3)", [conv, finalText, turnId]);
    }
    if (status === "done" && !turn.title) {
      const title = await this.#title(token, turn, finalText).catch((e) => (logError("title", e, { turn: turnId }), ""));
      if (title) {
        await db.query("update conversations set title = $2 where id = $1 and title = ''", [conv, title]);
        await emit("title", { title });
      }
    }
    const { rows: cost } = await db.query<{ c: string | null }>("select sum(cost_usd) as c from egress_log where turn_id = $1 and kind = 'model'", [turnId]);
    const costUsd = cost[0]?.c != null ? Number(cost[0].c) : clawCost;
    await db.query(
      "update turns set status = $2, finished_at = now(), error = $3, usage = $4, cost_usd = $5 where id = $1 and status in ('running','waiting')",
      [turnId, status, error, usage == null ? null : JSON.stringify(usage), costUsd],
    );
    await db.query("update conversations set updated_at = now() where id = $1", [conv]);
    await emit("turn_finished", { turnId, status, error, costUsd, ...(budgetHit && status === "error" ? budgetEventData(budgetHit, error ?? "") : {}) });
    await this.#sandboxes.touch(conv);
    log("turn", { turn: turnId, conversation: conv, status, ms: Date.now() - startedAt.getTime() });
  }

  /** The budget is used up: the turn ends at once with the budget message (no sandbox, no claw, no model call). */
  async #refuseForBudget(turn: TurnInfo, b: BudgetStatus, startedAt: Date): Promise<void> {
    const db = this.#deps.db;
    const conv = turn.conversation_id;
    const error = budgetMessage(this.#deps.config.product.locale, b.limitUsd, b.ownerName);
    await appendEvent(db, conv, turn.id, "turn_started", { turnId: turn.id, model: turn.model });
    await db.query("update turns set status = 'error', finished_at = now(), error = $2, cost_usd = 0 where id = $1 and status in ('running','waiting')", [turn.id, error]);
    await db.query("update conversations set updated_at = now() where id = $1", [conv]);
    await appendEvent(db, conv, turn.id, "turn_finished", { turnId: turn.id, status: "error", error, costUsd: 0, ...budgetEventData(b, error) });
    log("turn", { turn: turn.id, conversation: conv, status: "error", reason: "budget", ms: Date.now() - startedAt.getTime() });
  }

  /** The tools service's tool names (claw's --allowedTools must name MCP tools explicitly); [] when it can't answer. */
  async #toolNames(token: string): Promise<string[]> {
    try {
      const r = await fetch(this.#deps.toolsUrl, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json", accept: "application/json, text/event-stream" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
        signal: AbortSignal.timeout(10_000),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const text = await r.text();
      const body = text.trimStart().startsWith("{")
        ? JSON.parse(text)
        : JSON.parse(text.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5)).join("") || "{}");
      return (body?.result?.tools ?? []).map((t: { name: string }) => t.name).filter((n: unknown) => typeof n === "string");
    } catch (e) {
      logError("tools/list", e);
      return [];
    }
  }

  async #title(token: string, turn: TurnInfo, reply: string): Promise<string> {
    const { rows } = await this.#deps.db.query<{ content: string }>(
      "select content from messages where conversation_id = $1 and role = 'user' order by id limit 1",
      [turn.conversation_id],
    );
    const first = rows[0]?.content ?? "";
    const r = await fetch(`${this.#deps.egressBase}/x/model/v1/messages`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json", "anthropic-version": "2023-06-01" },
      body: JSON.stringify({
        model: this.#deps.config.titleModel,
        max_tokens: 40,
        system: "Write a short title (at most 6 words) for this conversation, in the language the user wrote in. Reply with the title only, no quotes.",
        messages: [{ role: "user", content: `User: ${first.slice(0, 2000)}\n\nAssistant: ${reply.slice(0, 1000)}` }],
      }),
      signal: AbortSignal.timeout(30_000),
    });
    if (!r.ok) throw new Error(`title: HTTP ${r.status}`);
    const body = (await r.json()) as { content?: { type: string; text?: string }[] };
    const text = (body.content ?? []).filter((c) => c.type === "text").map((c) => c.text ?? "").join("");
    return text.replace(/^["'「『]+|["'」』]+$/g, "").replace(/\s+/g, " ").trim().slice(0, 80);
  }
}

class CancelledError extends Error {}

/** What a budget refusal's events carry: the UI words it in its own locale from these (staff see the limit, not the spend). */
function budgetEventData(b: Pick<BudgetStatus, "limitUsd" | "ownerName">, message: string): Record<string, unknown> {
  return { code: "budget", message, limitUsd: b.limitUsd, ownerName: b.ownerName };
}

const emptyBudget = (): BudgetStatus => ({
  limitUsd: 0, since: new Date(0).toISOString(), ownerName: null, spentUsd: 0, remainingUsd: 0, exhausted: true, source: "config", override: null, config: null,
});
