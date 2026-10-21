// The web role's HTTP surface (docs/CHAT.md, "Control plane"): `/healthz` and the CloudCLI UI pods' calls
// under /internal/cc/*. Nothing here is public: the instance's hostname routes to the UI (DE0CH/claudecodeui), and a
// NetworkPolicy admits only the UI pods to web.
import type { IncomingMessage, ServerResponse } from "node:http";
import { logError } from "../common/log.ts";
import { sha256hex } from "../common/tokens.ts";
import type { WebDeps } from "./ctx.ts";
import { bad, HttpError, notFound, readJson, sendJson } from "./http.ts";
import type { SandboxManager } from "./sandbox.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export class WebApp {
  readonly deps: WebDeps;
  readonly sandboxes: SandboxManager;
  /** Set on SIGTERM: /healthz answers 503 so the Service stops sending this replica new calls. */
  draining = false;
  /** Calls being answered now (a sandbox start takes up to 3 minutes): the drain waits for them. */
  active = 0;

  constructor(deps: WebDeps, sandboxes: SandboxManager) {
    this.deps = deps;
    this.sandboxes = sandboxes;
  }

  async handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const path = new URL(req.url || "/", "http://x").pathname;
    try {
      if (path === "/healthz") {
        const ok = !this.draining && (await this.deps.db.query("select 1").then(() => true, () => false));
        return sendJson(res, ok ? 200 : 503, { ok, draining: this.draining });
      }
      if (!path.startsWith("/internal/cc/")) throw notFound();
      this.active++;
      try {
        return await this.#claudeCodeInternal(req, res, path);
      } finally {
        this.active--;
      }
    } catch (e) {
      const status = e instanceof HttpError ? e.status : 500;
      if (status === 500) logError("request", e, { path });
      if (!res.headersSent) sendJson(res, status, { error: status === 500 ? "internal error" : (e as Error).message });
      else res.end();
    }
  }

  /**
   * The CloudCLI UI pods' calls (docs/CLAUDE-CODE.md), cluster-only (NetworkPolicy) and with the shared bearer token
   * JARVIS_CC_TOKEN: `POST /internal/cc/sandbox {owner, conversation}` → the conversation's sandbox (made if needed;
   * `reset` when it replaced an earlier one), `POST /internal/cc/touch {conversation}` (it is in use: its idle time
   * starts again), `POST /internal/cc/release {conversation}` (the conversation was deleted).
   */
  async #claudeCodeInternal(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    const want = process.env.JARVIS_CC_TOKEN || "";
    const given = /^Bearer\s+(\S+)$/i.exec(String(req.headers.authorization || ""))?.[1] ?? "";
    if (!want || sha256hex(given) !== sha256hex(want)) throw new HttpError(401, "unauthorized");
    if (req.method !== "POST") throw new HttpError(405, "method not allowed");
    const b = await readJson(req);
    const conv = String(b.conversation || "");
    if (!UUID_RE.test(conv)) throw bad("conversation must be a uuid");
    if (path === "/internal/cc/sandbox") {
      const owner = String(b.owner || "").trim().toLowerCase();
      if (!this.deps.config.users.includes(owner)) throw new HttpError(403, "not a user of this instance");
      const sb = await this.sandboxes.ensure(conv, owner);
      // reset: this conversation had a different sandbox before (its installs and processes are gone)
      const key = `cc_sandbox:${conv}`;
      const { rows: seen } = await this.deps.db.query<{ value: string }>("select value from kv where key = $1", [key]);
      const reset = !!seen[0] && seen[0].value !== sb.id;
      if (!seen[0] || reset) {
        await this.deps.db.query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [key, JSON.stringify(sb.id)]);
      }
      return sendJson(res, 200, { id: sb.id, url: sb.url, token: sb.execToken, headers: sb.headers, reset });
    }
    if (path === "/internal/cc/touch") {
      await this.sandboxes.touch(conv);
      return sendJson(res, 200, { ok: true });
    }
    if (path === "/internal/cc/release") {
      await this.sandboxes.releaseConversation(conv, "deleted");
      return sendJson(res, 200, { ok: true });
    }
    throw notFound();
  }

  /** Waits (at most `ms`) for the calls being answered to finish. */
  async drain(ms: number): Promise<void> {
    const end = Date.now() + ms;
    while (this.active > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 200));
  }
}
