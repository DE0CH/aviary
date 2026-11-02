// The web role's HTTP surface (docs/CHAT.md, "Control plane"): `/healthz`, the CloudCLI UI pods' calls under
// /internal/cc/* and the operator page under /internal/ops/* (the UI router forwards /ops/* there for config `admins`).
// Nothing here is public: the instance's hostname routes to the UI (DE0CH/claudecodeui), and a
// NetworkPolicy admits only the UI pods to web.
import type { IncomingMessage, ServerResponse } from "node:http";
import { isUser } from "../common/config.ts";
import { logError } from "../common/log.ts";
import { newToken, sha256hex } from "../common/tokens.ts";
import type { WebDeps } from "./ctx.ts";
import { randomUUID } from "node:crypto";
import { bad, HttpError, notFound, readJson, sendJson } from "./http.ts";
import { OPS_PAGE } from "./ops-page.ts";
import { OpsSnapshot } from "./ops.ts";
import { noteSandboxDemand, type SandboxManager } from "./sandbox.ts";

/** Live relay tokens one person may hold at once. */
const RELAY_TOKENS_MAX = 10;

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export class WebApp {
  readonly deps: WebDeps;
  readonly sandboxes: SandboxManager;
  /** Set on SIGTERM: /healthz answers 503 so the Service stops sending this replica new calls. */
  draining = false;
  /** Calls being answered now (a sandbox start takes up to 3 minutes): the drain waits for them. */
  active = 0;
  readonly ops: OpsSnapshot;

  constructor(deps: WebDeps, sandboxes: SandboxManager) {
    this.deps = deps;
    this.sandboxes = sandboxes;
    this.ops = new OpsSnapshot(deps);
  }

  async handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const path = new URL(req.url || "/", "http://x").pathname;
    try {
      if (path === "/healthz") {
        const ok = !this.draining && (await this.deps.db.query("select 1").then(() => true, () => false));
        return sendJson(res, ok ? 200 : 503, { ok, draining: this.draining });
      }
      if (path === "/internal/ops" || path.startsWith("/internal/ops/")) return await this.#ops(req, res, path);
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
   * JARVIS_CC_TOKEN: `POST /internal/cc/sandbox {owner, conversation}` → the conversation's sandbox (made if needed),
   * `POST /internal/cc/touch {conversation}` (it is in use: its idle time
   * starts again), `POST /internal/cc/release {conversation}` (the conversation was deleted), `POST /internal/cc/warm
   * {owner, conversation?}` (the user is typing; #warm), `POST /internal/cc/view-as {…}` (an admin viewing as someone;
   * #viewAs), `POST /internal/cc/relay-tokens {owner, action, …}` (a person's relay tokens; #relayTokens).
   */
  async #claudeCodeInternal(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    checkCcToken(req);
    if (req.method !== "POST") throw new HttpError(405, "method not allowed");
    const b = await readJson(req);
    if (path === "/internal/cc/warm") return this.#warm(res, b);
    if (path === "/internal/cc/view-as") return this.#viewAs(res, b);
    if (path === "/internal/cc/relay-tokens") return this.#relayTokens(res, b);
    const conv = String(b.conversation || "");
    if (!UUID_RE.test(conv)) throw bad("conversation must be a uuid");
    if (path === "/internal/cc/sandbox") {
      const owner = String(b.owner || "").trim().toLowerCase();
      if (!isUser(this.deps.config, owner)) throw new HttpError(403, "not a user of this instance");
      const sb = await this.sandboxes.ensure(conv, owner);
      return sendJson(res, 200, { id: sb.id, url: sb.url, token: sb.execToken, headers: sb.headers });
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

  /**
   * The user is typing in the composer (debounced by the UI; `conversation` absent for a new chat). Nothing is created
   * and a live sandbox's idle time is not reset (only use counts): when the conversation has no live sandbox, it is a
   * hint to the vm scaler (noteSandboxDemand, counted for DEMAND_WINDOW_S), which starts an AWS host at once when no
   * host has a free slot. A host started for a hint nobody follows ends by the usual rule (empty and past its minimum
   * lifetime), so a warm costs at most that.
   */
  async #warm(res: ServerResponse, b: Record<string, unknown>): Promise<void> {
    const owner = String(b.owner || "").trim().toLowerCase();
    if (!isUser(this.deps.config, owner)) throw new HttpError(403, "not a user of this instance");
    const conv = b.conversation == null || b.conversation === "" ? null : String(b.conversation);
    if (conv !== null && !UUID_RE.test(conv)) throw bad("conversation must be a uuid");
    const { rows } = conv
      ? await this.deps.db.query("select 1 from sandboxes where conversation_id = $1 and status in ('starting','ready')", [conv])
      : { rows: [] };
    const hinted = !rows.length && !this.draining;
    if (hinted) await noteSandboxDemand(this.deps.db, owner);
    return sendJson(res, 200, { ok: true, hinted });
  }

  /**
   * A person's relay tokens (docs/CLAUDE-CODE.md, "Relay"), for their own Claude Code to call the model routes flagged
   * relay through egress's relay listener. `action` list → {tokens: [{id, label, createdAt, lastUsedAt}]} (live ones);
   * create {label?} → {id, token} (the token is shown once: only its sha256 is kept); revoke {id} → {ok}. The UI router
   * calls this for the signed-in person only.
   */
  async #relayTokens(res: ServerResponse, b: Record<string, unknown>): Promise<void> {
    const owner = String(b.owner || "").trim().toLowerCase();
    if (!owner || owner.startsWith("@") || !isUser(this.deps.config, owner)) throw new HttpError(403, "not a user of this instance");
    const db = this.deps.db;
    const action = String(b.action || "");
    if (action === "list") {
      const { rows } = await db.query(
        `select id, label, created_at as "createdAt", last_used_at as "lastUsedAt" from relay_tokens
          where owner = $1 and revoked_at is null order by created_at desc`,
        [owner],
      );
      return sendJson(res, 200, { tokens: rows });
    }
    if (action === "create") {
      const label = String(b.label ?? "").trim().slice(0, 80) || null;
      const { rows } = await db.query<{ n: number }>("select count(*)::int as n from relay_tokens where owner = $1 and revoked_at is null", [owner]);
      if ((rows[0]?.n ?? 0) >= RELAY_TOKENS_MAX) throw new HttpError(409, `at most ${RELAY_TOKENS_MAX} relay tokens: revoke one first`);
      const id = randomUUID();
      const token = `jrelay_${newToken()}`;
      await db.query("insert into relay_tokens (id, owner, token_hash, label) values ($1, $2, $3, $4)", [id, owner, sha256hex(token), label]);
      return sendJson(res, 200, { id, token });
    }
    if (action === "revoke") {
      const id = String(b.id || "");
      if (!UUID_RE.test(id)) throw bad("id must be a uuid");
      const r = await db.query("update relay_tokens set revoked_at = now() where id = $1 and owner = $2 and revoked_at is null", [id, owner]);
      if (!r.rowCount) throw notFound();
      return sendJson(res, 200, { ok: true });
    }
    throw bad("action must be list, create or revoke");
  }

  /**
   * View as (docs/CHAT.md, "View as"): the UI router records an admin starting to view the app as a staff member
   * (`event` start), and that person's CloudCLI process records each message (`message`) and new conversation
   * (`conversation`) the admin sends as them, before sending it; nothing is sent unless this answers 200. Rows are
   * ops_events kind view_as, kept for good, shown in the operator page's History. `admin` must be in config `admins`
   * and `user` a user, checked here too. Never the message itself: its length only.
   */
  async #viewAs(res: ServerResponse, b: Record<string, unknown>): Promise<void> {
    const admin = String(b.admin || "").trim().toLowerCase();
    const user = String(b.user || "").trim().toLowerCase();
    const event = String(b.event || "");
    if (!this.deps.config.admins.includes(admin)) throw new HttpError(403, "view as: admins only");
    if (!isUser(this.deps.config, user)) throw new HttpError(403, "not a user of this instance");
    if (!["start", "message", "conversation"].includes(event)) throw bad("event must be start, message or conversation");
    const conv = b.conversation == null || b.conversation === "" ? null : String(b.conversation).toLowerCase();
    if (conv !== null && !UUID_RE.test(conv)) throw bad("conversation must be a uuid");
    if (event !== "start" && conv === null) throw bad("a send names its conversation");
    const session = String(b.session || "").slice(0, 64);
    const chars = Number.isInteger(b.chars) && (b.chars as number) >= 0 ? (b.chars as number) : null;
    // awaited and allowed to fail: the caller must not go on unless this is recorded (opsEvent never throws)
    await this.deps.db.query("insert into ops_events (kind, ok, conversation_id, detail) values ('view_as', true, $1, $2)", [
      conv,
      JSON.stringify({ admin, user, event, session, ...(chars === null ? {} : { chars }) }),
    ]);
    return sendJson(res, 200, { ok: true });
  }

  /**
   * The operator page (docs/CHAT.md, "Operator page"), read-only: `GET /internal/ops/` the page, `GET
   * /internal/ops/snapshot?tz=<zone>` its data (ops.ts). The UI router forwards an Access-verified admin's /ops/* here
   * with the shared bearer token and `X-Jarvis-User`; both are checked here too (the token, and that the user is in
   * config `admins`), so a ui pod that forwarded anyone else is still refused.
   */
  async #ops(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    checkCcToken(req);
    const user = String(req.headers["x-jarvis-user"] || "").trim().toLowerCase();
    if (!user || !this.deps.config.admins.includes(user)) throw new HttpError(403, "operator page: admins only");
    if (req.method !== "GET" && req.method !== "HEAD") throw new HttpError(405, "method not allowed");
    if (path === "/internal/ops" || path === "/internal/ops/") {
      const body = Buffer.from(OPS_PAGE);
      res.writeHead(200, {
        "content-type": "text/html; charset=utf-8",
        "content-length": body.length,
        "cache-control": "no-store",
        "content-security-policy": "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'",
        "x-content-type-options": "nosniff",
        "referrer-policy": "no-referrer",
      });
      return void res.end(req.method === "HEAD" ? undefined : body);
    }
    if (path === "/internal/ops/snapshot") {
      const tz = new URL(req.url || "/", "http://x").searchParams.get("tz") || "";
      return sendJson(res, 200, await this.ops.get(tz));
    }
    throw notFound();
  }

  /** Waits (at most `ms`) for the calls being answered to finish. */
  async drain(ms: number): Promise<void> {
    const end = Date.now() + ms;
    while (this.active > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 200));
  }
}

/** The UI pods' shared bearer token (JARVIS_CC_TOKEN); throws 401 without it. */
function checkCcToken(req: IncomingMessage): void {
  const want = process.env.JARVIS_CC_TOKEN || "";
  const given = /^Bearer\s+(\S+)$/i.exec(String(req.headers.authorization || ""))?.[1] ?? "";
  if (!want || sha256hex(given) !== sha256hex(want)) throw new HttpError(401, "unauthorized");
}
