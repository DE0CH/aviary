// The turn runner (docs/CHAT.md, "Turn runner"): one claw process per turn on the replica that accepted the message.
// claw's NDJSON events become rows in `events` (text deltas batched), the conversation's files and claw's session
// file are restored before and stored after, and bash goes to the conversation's sandbox through jarvis-bash.
import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmod, rm, stat } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { BUDGET_ERROR_TYPE, budgetMessage, budgetStatus, type BudgetStatus } from "../common/budget.ts";
import { effectiveModel } from "../common/config.ts";
import { channel } from "../common/db.ts";
import { log, logError, redact } from "../common/log.ts";
import { opsEvent } from "../common/ops.ts";
import { sha256hex } from "../common/tokens.ts";
import { blobKey, type WebDeps } from "./ctx.ts";
import { appendEvent } from "./events.ts";
import { dirs, restoreWorkdir, storeWorkdir } from "./files.ts";
import { mergePresented, presentedOf, type PresentedFile } from "../common/presented.ts";
import { sha256File } from "./workspace.ts";
import { fallbackTitle, titleFromResponse, titleRequest } from "./title.ts";
import { PoolFullError, type SandboxManager } from "./sandbox.ts";
import type { SandboxGrant } from "./bash.ts";

const TEXT_BATCH_MS = 250;
const MAX_TOOL_OUTPUT = 20_000;
/** claw and jarvis-bash run in the workdir: relative paths become absolute (bare command names stay PATH lookups). */
const abs = (p: string) => (p.includes("/") ? resolve(p) : p);
/** How often a running turn keeps its sandbox alive at a provider with its own timeout (E2B). */
const KEEPALIVE_MS = 60_000;
export { RESET_NOTE } from "./bash.ts";

/**
 * Appended to the instance's system prompt on every turn: what the sandbox can reach (docs/CHAT.md, "The sandbox
 * allowlist"). The runtime's fact, so it holds whatever the instance's prompt says.
 */
export const SANDBOX_NETWORK_NOTE = `## The sandbox's internet access
The sandbox (bash) reaches only an automatic allowlist: package registries (pip, npm, apt), code hosts and container
registries (GitHub, Docker Hub, ghcr.io), government and statistics sites, major news sites and other widely used
public sites. Everything else is refused (HTTP 403 from the proxy), and no one can enable a site from the chat, so do
not ask for it. Pastebins, file-sharing, form and webhook sites are always refused. To read a web page or search,
use the web_fetch and web_search tools (they work for any public site); use the sandbox's internet only to install
packages or download data files.
`;

/**
 * Appended too: how the answer's files reach the staff member (only through present_files) and that the model can see
 * images (claw's CLAW_IMAGES: read_file on an image or PDF page; staff-uploaded images are attached to their message).
 */
export const FILES_NOTE = `## Showing files and seeing images
The user sees none of the workspace's files unless you present them. When you finish, call present_files with the
deliverables only (reports, charts, data the user asked for) — never scripts, fonts or intermediate files. Its tool
name is mcp__tools__present_files (there is no tool called just "present_files"); pass \`files\` as a list, e.g.
[{"path": "report.pdf", "caption": "…"}]. Don't paste paths or download links for them in your answer.
You can see images: read_file on a PNG, JPEG, WebP or GIF shows it to you (HEIC, SVG, BMP and TIFF are converted, and
read_file on a PDF shows one page as an image: \`page\`, default 1). Files a bash command writes under /work in the
sandbox are in the workspace right after that command, so to check a chart you made, save it (e.g. matplotlib
savefig to /work/chart.png) and read_file it. read_file and present_files take paths relative to the workspace
(chart.png, not /work/chart.png). Images the user attached to their message are shown to you with it.
`;

/** Staff-uploaded images attached to their message as image blocks (claw: CLAW_PROMPT_IMAGES); at most this many. */
export const PROMPT_IMAGE_EXT = /\.(png|jpe?g|gif|webp|heic|heif|bmp|tiff?|svg)$/i;
export const MAX_PROMPT_IMAGES = 20;

/**
 * Appended after SANDBOX_NETWORK_NOTE: how to install software in the sandbox (docs/CHAT.md, "Sandbox lifecycle";
 * docs/SANDBOX-VM.md, "Installing packages"). System packages only where the provider gives root inside the sandbox
 * (our own gVisor hosts); a hint to sandbox_environment when the tools server offers it.
 */
export function sandboxInstallNote(provider: string, hasEnvTool: boolean): string {
  const apt =
    provider === "vm"
      ? "- System packages: `sudo apt-get install -y <pkg>` (passwordless sudo; root exists only inside the sandbox; the\n  package lists are fetched on the first install).\n"
      : "- System packages (apt) cannot be installed in this sandbox (no root).\n";
  return `## Installing software in the sandbox
You may install what a task needs; installs last while the sandbox lives (it is reset after a long idle time, then
reinstall them):
${apt}- Python packages: \`pip install <pkg>\` (a virtualenv, no sudo). Node packages: \`npm install -g <pkg>\`.
- Other tools: installers into ~/.local/bin (on PATH), e.g. \`curl -LsSf https://astral.sh/uv/install.sh | sh\`.
${hasEnvTool ? "Call sandbox_environment to see how the sandbox is set up (preinstalled packages, limits, Docker, network).\n" : ""}`;
}

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

interface TurnSandboxCtx {
  turnId: string;
  conv: string;
  emit: (type: string, data: Record<string, unknown>) => Promise<unknown>;
  /** the "unavailable" event was sent already (once per turn) */
  unavailable: boolean;
}

export class TurnRunner {
  readonly #deps: WebDeps;
  readonly #sandboxes: SandboxManager;
  readonly #running = new Map<string, Running>();
  /** running turns by sha256 of their token: what jarvis-bash's sandbox requests are checked against */
  readonly #byToken = new Map<string, TurnSandboxCtx>();
  #server: Promise<{ server: Server; path: string }> | null = null;
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
        this.#byToken.delete(sha256hex(token));
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

  /**
   * This replica's socket for jarvis-bash (JARVIS_SANDBOX_SOCKET): `POST /sandbox` with the turn's token as bearer
   * gives the conversation's sandbox, made now if there is none (docs/CHAT.md, "jarvis-bash"). A unix socket in the
   * work root (mode 600): nothing outside this pod can reach it, and only a running turn's token is accepted.
   */
  #socket(): Promise<string> {
    this.#server ??= (async () => {
      await mkdir(this.#deps.workRoot, { recursive: true });
      const path = join(resolve(this.#deps.workRoot), `.sbx-${randomBytes(4).toString("hex")}.sock`);
      await rm(path, { force: true });
      const server = createServer((req, res) => {
        const reply = (status: number, body: unknown) => res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(body));
        const token = /^Bearer (.+)$/.exec(req.headers.authorization ?? "")?.[1] ?? "";
        const t = token ? this.#byToken.get(sha256hex(token)) : undefined;
        if (req.method !== "POST" || (req.url !== "/sandbox" && req.url !== "/sandbox/used")) return reply(404, { error: "not found" });
        if (!t) return reply(403, { error: "no such turn" });
        req.resume();
        // a bash command ended: the sandbox was used (its idle time starts now)
        if (req.url === "/sandbox/used") {
          return void this.#sandboxes.touch(t.conv).then(
            () => reply(200, {}),
            (e) => (logError("sandbox used", e, { turn: t.turnId }), reply(500, {})),
          );
        }
        this.#grant(t).then(
          (g) => reply(200, g),
          (e) => (logError("sandbox for bash", e, { turn: t.turnId }), reply(500, { unavailable: "it could not be started" })),
        );
      });
      await new Promise<void>((ok, fail) => server.once("error", fail).listen(path, () => ok()));
      server.unref();
      await chmod(path, 0o600);
      return { server, path };
    })();
    return this.#server.then((s) => s.path);
  }

  /** The turn's sandbox for a bash command: made if needed, with the reset flag when it replaced an earlier one. */
  async #grant(t: TurnSandboxCtx): Promise<SandboxGrant> {
    const db = this.#deps.db;
    const { rows: live } = await db.query("select 1 from sandboxes where conversation_id = $1 and status = 'ready'", [t.conv]);
    if (!live.length) await t.emit("sandbox", { status: "starting" });
    let sandbox;
    try {
      sandbox = await this.#sandboxes.ensure(t.conv);
    } catch (e) {
      if (!t.unavailable) await t.emit("sandbox", { status: "unavailable", message: (e as Error).message });
      t.unavailable = true;
      // what the bash tool tells the model (and so the staff member): plain words, no internals
      return { unavailable: e instanceof PoolFullError ? e.message : "it could not be started" };
    }
    if (!live.length) await t.emit("sandbox", { status: "ready" });
    const key = `turn_sandbox:${t.conv}`;
    const { rows: seen } = await db.query<{ value: string }>("select value from kv where key = $1", [key]);
    const reset = !!seen[0] && seen[0].value !== sandbox.id;
    if (reset) await t.emit("sandbox", { status: "reset" });
    if (!seen[0] || reset) await db.query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [key, JSON.stringify(sandbox.id)]);
    return { id: sandbox.id, url: sandbox.url, token: sandbox.execToken, headers: sandbox.headers, reset };
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
      `update turns t set status = 'running', replica = $2, presented = coalesce(t.presented, '[]') from conversations c
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

      // the sandbox is made at the turn's first bash command (jarvis-bash asks over this replica's socket); a turn
      // that never runs bash never gets one
      const socket = await this.#socket();
      this.#byToken.set(sha256hex(token), { turnId, conv, emit, unavailable: false });
      let prompt = msg.content;
      const attachments = Array.isArray(msg.attachments) ? msg.attachments : [];
      if (attachments.length) prompt += `\n\n[Attached files, in the workspace: ${attachments.join(", ")}]`;
      // attached images go to the model as images too (claw downsizes them); the same files stay in the workspace
      const promptImages = await imageAttachments(d.work, attachments);

      // claw's config: the tools MCP server through the stdio bridge; nothing is read from the workdir
      const toolNames = await this.#toolNames(token);
      const settings: Record<string, unknown> = { permissions: { defaultMode: "danger-full-access" } };
      if (toolNames.length) {
        settings.mcpServers = {
          tools: { command: abs(deps.mcpBridgePath), args: [], env: { JARVIS_TOOLS_URL: deps.toolsUrl, JARVIS_TURN_TOKEN: token, JARVIS_WORKDIR: d.work } },
        };
      }
      await writeFile(join(d.clawConfig, "settings.json"), JSON.stringify(settings, null, 2));
      // the instance's system prompt plus the runtime's notes on the sandbox's internet access and installing software
      const promptFile = join(d.clawConfig, "system-prompt.md");
      const instancePrompt = await readFile(abs(deps.config.systemPromptFile), "utf8");
      const installNote = sandboxInstallNote(deps.config.sandbox.provider, toolNames.includes("sandbox_environment"));
      await writeFile(promptFile, `${instancePrompt.trimEnd()}\n\n${SANDBOX_NETWORK_NOTE}\n${installNote}\n${FILES_NOTE}`);
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
        CLAW_SYSTEM_PROMPT_FILE: promptFile,
        CLAW_BASH_COMMAND: abs(deps.bashPath),
        CLAW_CONFIG_DIR: d.clawConfig,
        CLAW_CONFIG_HOME: d.clawConfig,
        CLAW_SESSIONS_DIR: d.sessions,
        CLAW_PROMPT_CACHE: "1",
        CLAW_IMAGES: "1",
        ANTHROPIC_BASE_URL: `${deps.egressBase}/x/model`,
        ANTHROPIC_AUTH_TOKEN: token,
        JARVIS_SYNC_STATE: d.syncState,
        ...(deps.clawEnv ?? {}),
      };
      if (sess[0]) env.CLAW_RESUME_SESSION = resumePath;
      if (promptImages.length) env.CLAW_PROMPT_IMAGES = JSON.stringify(promptImages);
      env.JARVIS_SANDBOX_SOCKET = socket;
      env.JARVIS_TURN_TOKEN = token;
      if (r.cancelled) throw new CancelledError();

      const child = spawn(abs(deps.clawPath), args, { cwd: d.work, env, stdio: ["pipe", "pipe", "pipe"] });
      r.child = child;
      let stderrTail = "";
      child.stderr!.on("data", (c: Buffer) => (stderrTail = (stderrTail + c.toString("utf8")).slice(-4000)));
      child.stdin!.on("error", () => {});
      const exited = new Promise<number | null>((res) => child.on("close", (code) => res(code)));
      // the agent's bash calls go straight to the sandbox: keep it (and a provider's own timeout) alive meanwhile
      // a provider's own timeout (E2B) must not end the sandbox mid-turn; its idle time counts only use (bash)
      const keep = setInterval(() => void this.#sandboxes.keepAlive(conv).catch((e) => logError("sandbox keep-alive", e, { turn: turnId })), KEEPALIVE_MS);
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
            // danger-full-access asks nothing; anything that does ask is refused (nobody is asked anything)
            child.stdin!.write(JSON.stringify({ type: "permission_response", request_id: data.request_id, allow: false, reason: "not allowed here" }) + "\n");
            break;
          case "tool_result":
            // an image the model looked at: its bytes go to the bucket now, so the step's thumbnail loads mid-turn
            if (Array.isArray(data.images) && data.images.length) {
              const imgs = data.images as { path?: unknown; sha256?: unknown }[];
              queue(() => this.#keepViewed(conv, d.work, imgs));
            }
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
      // kept for the record; the UI shows only the presented files (below), never this list
      if (changed.length) await emit("files", { changed });
      await this.#finalPresented(turnId, conv, emit);
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
    if (!turn.title) {
      const title = await this.#title(token, turn, finalText, status === "done").catch((e) => (logError("title", e, { turn: turnId }), ""));
      if (title) {
        const { rowCount } = await db.query("update conversations set title = $2 where id = $1 and title = ''", [conv, title]);
        if (rowCount) await emit("title", { title });
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
    log("turn", { turn: turnId, conversation: conv, status, ms: Date.now() - startedAt.getTime() });
  }

  /**
   * The turn's presented files once its files are stored: each path at the version the turn left (the latest wins),
   * files gone since dropped, copies shown once. Sent again only when that changed what the UI has.
   */
  async #finalPresented(turnId: string, conv: string, emit: (type: string, data: Record<string, unknown>) => Promise<unknown>): Promise<void> {
    const db = this.#deps.db;
    const { rows } = await db.query<{ presented: unknown }>("select presented from turns where id = $1", [turnId]);
    const before = presentedOf(rows[0]?.presented);
    if (!before?.length) return;
    const { rows: live } = await db.query<{ path: string; sha256: string; size: string }>(
      "select path, sha256, size from files where conversation_id = $1 and path = any($2) and not deleted",
      [conv, before.map((f) => f.path)],
    );
    const now = new Map(live.map((r) => [r.path, r]));
    const kept: PresentedFile[] = [];
    for (const f of before) {
      const r = now.get(f.path);
      if (r) kept.push({ ...f, sha256: r.sha256, size: Number(r.size) });
    }
    const after = mergePresented([], kept);
    if (JSON.stringify(after) === JSON.stringify(before)) return;
    await db.query("update turns set presented = $2 where id = $1", [turnId, JSON.stringify(after)]);
    await emit("files_presented", { files: after });
  }

  /** Images the model viewed with read_file: put in the bucket under their sha256 (what the thumbnail URL names). */
  async #keepViewed(conv: string, work: string, images: { path?: unknown; sha256?: unknown }[]): Promise<void> {
    for (const img of images.slice(0, 5)) {
      const rel = typeof img.path === "string" ? img.path : "";
      const sha = typeof img.sha256 === "string" && /^[0-9a-f]{64}$/.test(img.sha256) ? img.sha256 : "";
      if (!rel || !sha || rel.startsWith("/") || rel.split("/").includes("..")) continue;
      const p = join(work, rel);
      const st = await stat(p).catch(() => null);
      if (!st?.isFile() || st.size > 50 * 1024 * 1024) continue;
      if ((await sha256File(p).catch(() => "")) !== sha) continue;
      const key = blobKey(this.#deps, conv, sha);
      if (await this.#deps.blobs.exists(key).catch(() => false)) continue;
      await this.#deps.blobs.putFile(key, p, st.size);
    }
  }

  /** The budget is used up: the turn ends at once with the budget message (no sandbox, no claw, no model call). */
  async #refuseForBudget(turn: TurnInfo, b: BudgetStatus, startedAt: Date): Promise<void> {
    const db = this.#deps.db;
    const conv = turn.conversation_id;
    const error = budgetMessage(this.#deps.config.product.locale, b.limitUsd, b.ownerName);
    await appendEvent(db, conv, turn.id, "turn_started", { turnId: turn.id, model: turn.model });
    await db.query("update turns set status = 'error', finished_at = now(), error = $2, cost_usd = 0 where id = $1 and status in ('running','waiting')", [turn.id, error]);
    // no model call for the title either: the first words of the message
    const title = turn.title ? "" : await this.#title("", turn, "", false).catch(() => "");
    if (title && (await db.query("update conversations set title = $2 where id = $1 and title = ''", [conv, title])).rowCount) {
      await appendEvent(db, conv, turn.id, "title", { title });
    }
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

  /**
   * The conversation's title: the title model's (only after a turn that finished), else the first words of the
   * user's first message. A failed model call is logged and recorded (ops_events kind 'title', the monitor's errors).
   */
  async #title(token: string, turn: TurnInfo, reply: string, askModel: boolean): Promise<string> {
    const { rows } = await this.#deps.db.query<{ content: string }>(
      "select content from messages where conversation_id = $1 and role = 'user' order by id limit 1",
      [turn.conversation_id],
    );
    const first = rows[0]?.content ?? "";
    const fallback = fallbackTitle(first) || fallbackTitle(reply);
    if (!askModel) return fallback;
    const t0 = performance.now();
    try {
      const r = await fetch(`${this.#deps.egressBase}/x/model/v1/messages`, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json", "anthropic-version": "2023-06-01" },
        body: JSON.stringify(titleRequest(this.#deps.config.titleModel, first, reply)),
        signal: AbortSignal.timeout(30_000),
      });
      const body = (await r.json().catch(() => null)) as { error?: { message?: unknown } } | null;
      if (!r.ok) {
        const msg = typeof body?.error?.message === "string" ? `: ${body.error.message.slice(0, 200)}` : "";
        throw Object.assign(new Error(`title: HTTP ${r.status}${msg}`), { reason: `HTTP ${r.status}` });
      }
      const title = titleFromResponse(body);
      if (!title) throw Object.assign(new Error("title: the model returned no text"), { reason: "no text" });
      return title;
    } catch (e) {
      const err = e as Error & { reason?: string };
      const reason = err.reason ?? (err.name === "TimeoutError" ? "timeout" : "request failed");
      logError("title", err, { turn: turn.id, conversation: turn.conversation_id, reason, fallback: Boolean(fallback) });
      await opsEvent(this.#deps.db, "title", {
        ok: false,
        ms: performance.now() - t0,
        conversationId: turn.conversation_id,
        detail: { reason, error: redact(err.message).slice(0, 300), fallback: Boolean(fallback) },
      });
      return fallback;
    }
  }

}

class CancelledError extends Error {}

/** The message's attachments that are images (in the workspace, at most MAX_PROMPT_IMAGES): claw's CLAW_PROMPT_IMAGES. */
export async function imageAttachments(work: string, attachments: string[]): Promise<string[]> {
  const out: string[] = [];
  for (const a of attachments) {
    if (out.length >= MAX_PROMPT_IMAGES) break;
    if (!PROMPT_IMAGE_EXT.test(a)) continue;
    const st = await stat(join(work, a)).catch(() => null);
    if (st?.isFile()) out.push(a);
  }
  return out;
}

/** What a budget refusal's events carry: the UI words it in its own locale from these (staff see the limit, not the spend). */
function budgetEventData(b: Pick<BudgetStatus, "limitUsd" | "ownerName">, message: string): Record<string, unknown> {
  return { code: "budget", message, limitUsd: b.limitUsd, ownerName: b.ownerName };
}

const emptyBudget = (): BudgetStatus => ({
  limitUsd: 0, since: new Date(0).toISOString(), ownerName: null, spentUsd: 0, remainingUsd: 0, exhausted: true, source: "config", override: null, config: null,
});
