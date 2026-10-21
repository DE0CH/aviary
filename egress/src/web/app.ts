// The web role's HTTP surface (docs/CHAT.md, "Public API"): Access + allowlist on every request but /healthz and
// /internal/*, a user sees only their own conversations, SSE streams read from Postgres so any replica can serve any
// conversation, and the static UI with its SPA fallback.
import { randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { extname, join, normalize } from "node:path";
import { accessToken } from "../common/access.ts";
import { budgetAudit, budgetStatus, changeBudget } from "../common/budget.ts";
import { effectiveModel } from "../common/config.ts";
import { channel } from "../common/db.ts";
import { logError } from "../common/log.ts";
import { bearerOf, newToken, sha256hex, turnForToken } from "../common/tokens.ts";
import { blobKey, type WebDeps } from "./ctx.ts";
import { appendEvent, publicEvent, type EventRow } from "./events.ts";
import { contentType, liveFiles, saveUpload } from "./files.ts";
import { Monitor } from "./monitor.ts";
import { bad, HttpError, notFound, readJson, Router, sendHtml, sendJson, uuidParam } from "./http.ts";
import type { SandboxManager } from "./sandbox.ts";
import type { TurnRunner } from "./turns.ts";

export interface Caller {
  email: string;
  isAdmin: boolean;
}

interface Ctx {
  deps: WebDeps;
  sandboxes: SandboxManager;
  runner: TurnRunner;
  caller: Caller;
}

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function notAllowedPage(product: string, email: string): string {
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(product)}</title><style>body{font:16px/1.5 system-ui,sans-serif;margin:0;display:grid;place-items:center;min-height:100vh;background:#f7f7f8;color:#222}
main{max-width:28rem;padding:2rem}h1{font-size:1.3rem}@media(prefers-color-scheme:dark){body{background:#17181a;color:#e8e8e8}}</style></head>
<body><main><h1>${esc(product)}</h1><p>You are signed in as <b>${esc(email)}</b>, which is not on the list of people who can use ${esc(product)}.</p>
<p>Ask your administrator to add this address, or sign in with another account: <a href="/cdn-cgi/access/logout">sign out</a>.</p></main></body></html>`;
}

export class WebApp {
  readonly deps: WebDeps;
  readonly sandboxes: SandboxManager;
  readonly runner: TurnRunner;
  readonly #router = new Router<Ctx>();
  readonly #seen = new Map<string, number>();
  readonly #streams = new Set<ServerResponse>();
  readonly #warming = new Set<string>();
  readonly #monitor: Monitor;

  constructor(deps: WebDeps, sandboxes: SandboxManager, runner: TurnRunner) {
    this.deps = deps;
    this.sandboxes = sandboxes;
    this.runner = runner;
    this.#monitor = new Monitor({ deps, runner, streamCount: () => this.#streams.size });
    const r = this.#router;
    r.add("GET", "/api/me", (q, s, p, c) => this.#me(s, c));
    r.add("GET", "/api/conversations", (q, s, p, c) => this.#list(s, c));
    r.add("POST", "/api/conversations", (q, s, p, c) => this.#create(q, s, c));
    r.add("GET", "/api/conversations/:id", (q, s, p, c) => this.#get(s, p, c));
    r.add("PATCH", "/api/conversations/:id", (q, s, p, c) => this.#rename(q, s, p, c));
    r.add("DELETE", "/api/conversations/:id", (q, s, p, c) => this.#delete(s, p, c));
    r.add("POST", "/api/conversations/:id/warm", (q, s, p, c) => this.#warm(s, p, c));
    r.add("POST", "/api/conversations/:id/messages", (q, s, p, c) => this.#message(q, s, p, c));
    r.add("GET", "/api/conversations/:id/stream", (q, s, p, c) => this.#stream(q, s, p, c));
    r.add("POST", "/api/turns/:id/cancel", (q, s, p, c) => this.#cancel(s, p, c));
    r.add("POST", "/api/confirmations/:id", (q, s, p, c) => this.#confirm(q, s, p, c));
    r.add("POST", "/api/conversations/:id/files", (q, s, p, c) => this.#upload(q, s, p, c));
    r.add("GET", "/api/conversations/:id/files", (q, s, p, c) => this.#files(s, p, c));
    r.add("GET", "/api/conversations/:id/files/*", (q, s, p, c) => this.#download(s, p, c));
    r.add("GET", "/api/admin/usage", (q, s, p, c) => this.#usage(q, s, c));
    r.add("GET", "/api/admin/egress", (q, s, p, c) => this.#egress(s, c));
    r.add("GET", "/api/admin/monitor", (q, s, p, c) => this.#monitorSnapshot(q, s, c));
    r.add("GET", "/api/admin/budget", (q, s, p, c) => this.#budget(s, c));
    r.add("POST", "/api/admin/budget", (q, s, p, c) => this.#budgetChange(q, s, c));
  }

  /** Open SSE streams on this replica (the admin monitor). */
  get streamCount(): number {
    return this.#streams.size;
  }

  /** Ends open SSE streams (clients reconnect to another replica) when this one drains. */
  closeStreams(): void {
    for (const s of this.#streams) s.end();
  }

  async handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = new URL(req.url || "/", "http://x");
    const path = url.pathname;
    try {
      if (path === "/healthz") {
        const ok = !this.runner.draining && (await this.deps.db.query("select 1").then(() => true, () => false));
        return sendJson(res, ok ? 200 : 503, { ok, draining: this.runner.draining });
      }
      if (path === "/internal/events" && req.method === "POST") return await this.#internalEvent(req, res);
      const caller = await this.#authenticate(req);
      if (!caller.ok) {
        if (caller.status === 403 && !path.startsWith("/api/")) return sendHtml(res, 403, notAllowedPage(this.deps.config.product.name, caller.email));
        return sendJson(res, caller.status, { error: caller.message });
      }
      if (path.startsWith("/api/")) {
        const origin = req.headers.origin;
        if (req.method !== "GET" && req.method !== "HEAD" && origin && origin !== this.deps.config.publicUrl) throw new HttpError(403, "cross-origin request refused");
        const m = this.#router.match(req.method || "GET", path);
        if (m === "method") throw new HttpError(405, "method not allowed");
        if (!m) throw notFound();
        return await m.h(req, res, m.params, { deps: this.deps, sandboxes: this.sandboxes, runner: this.runner, caller: caller.caller });
      }
      if (req.method !== "GET" && req.method !== "HEAD") throw new HttpError(405, "method not allowed");
      // the admin pages and their bundle exist only for admins (staff get 404, never "Jarvis 3")
      if (!caller.caller.isAdmin && (path === "/admin" || path.startsWith("/admin/") || /^\/admin-[^/]*$/.test(path))) throw notFound();
      return await this.#static(path, res);
    } catch (e) {
      const status = e instanceof HttpError ? e.status : 500;
      if (status === 500) logError("request", e, { path });
      if (!res.headersSent) sendJson(res, status, { error: status === 500 ? "internal error" : (e as Error).message });
      else res.end();
    }
  }

  async #authenticate(req: IncomingMessage): Promise<{ ok: true; caller: Caller } | { ok: false; status: number; message: string; email: string }> {
    const token = accessToken(req);
    if (!token) return { ok: false, status: 401, message: "not signed in", email: "" };
    let email: string;
    try {
      email = await this.deps.verifier.email(token);
    } catch {
      return { ok: false, status: 401, message: "not signed in", email: "" };
    }
    if (!this.deps.config.users.includes(email)) return { ok: false, status: 403, message: "this account is not allowed", email };
    const isAdmin = this.deps.config.admins.includes(email);
    const last = this.#seen.get(email) ?? 0;
    if (Date.now() - last > 60_000) {
      await this.deps.db.query(
        `insert into users (email, name, is_admin, last_seen_at) values ($1, $2, $3, now())
         on conflict (email) do update set is_admin = excluded.is_admin, last_seen_at = now()`,
        [email, email.split("@")[0], isAdmin],
      );
      this.#seen.set(email, Date.now());
    }
    return { ok: true, caller: { email, isAdmin } };
  }

  /** The caller's own, not deleted conversation, or 404 (never 403: others' ids don't exist for them). */
  async #own(c: Ctx, id: string | undefined): Promise<{ id: string; title: string; model: string }> {
    const conv = uuidParam(id);
    const { rows } = await c.deps.db.query("select id, title, model from conversations where id = $1 and owner = $2 and deleted_at is null", [conv, c.caller.email]);
    if (!rows[0]) throw notFound();
    // a model no longer offered (e.g. Opus after a switch to Sonnet only): the conversation continues on the default
    return { ...rows[0], model: effectiveModel(c.deps.config, rows[0].model) };
  }

  async #me(res: ServerResponse, c: Ctx) {
    const cfg = c.deps.config;
    sendJson(res, 200, {
      email: c.caller.email,
      name: c.caller.email.split("@")[0],
      isAdmin: c.caller.isAdmin,
      product: { name: cfg.product.name, logoUrl: cfg.product.logo ?? null, locale: cfg.product.locale },
      models: cfg.models,
      defaultModel: cfg.defaultModel,
    });
  }

  async #list(res: ServerResponse, c: Ctx) {
    const { rows } = await c.deps.db.query(
      `select c.id, c.title, c.model, c.updated_at as "updatedAt",
              exists (select 1 from turns t where t.conversation_id = c.id and t.status in ('queued','running','waiting')) as running
         from conversations c where c.owner = $1 and c.deleted_at is null
          and exists (select 1 from messages m where m.conversation_id = c.id)
        order by c.updated_at desc limit 500`,
      [c.caller.email],
    );
    sendJson(res, 200, rows);
  }

  async #create(req: IncomingMessage, res: ServerResponse, c: Ctx) {
    const body = await readJson(req);
    const model = body.model ?? c.deps.config.defaultModel;
    if (!c.deps.config.models.some((m) => m.id === model)) throw bad("unknown model");
    const id = randomUUID();
    await c.deps.db.query("insert into conversations (id, owner, model) values ($1, $2, $3)", [id, c.caller.email, model]);
    sendJson(res, 201, { id });
  }

  async #get(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const db = c.deps.db;
    const [{ rows: messages }, { rows: turns }, { rows: events }, { rows: pending }] = await Promise.all([
      db.query(`select id, role, content, turn_id as "turnId", attachments, created_at as "createdAt" from messages where conversation_id = $1 order by id`, [conv.id]),
      db.query(`select id, status, model, started_at as "startedAt", finished_at as "finishedAt", error, cost_usd as "costUsd" from turns where conversation_id = $1 order by started_at`, [conv.id]),
      db.query<EventRow>("select * from events where conversation_id = $1 order by id", [conv.id]),
      db.query(`select id, turn_id as "turnId", kind, summary, detail, status from confirmations where conversation_id = $1 and status = 'pending'`, [conv.id]),
    ]);
    const byTurn = new Map<string, unknown[]>();
    for (const e of events) {
      const list = byTurn.get(e.turn_id) ?? [];
      list.push(publicEvent(e));
      byTurn.set(e.turn_id, list);
    }
    const lastEventId = events.length ? Number(events[events.length - 1]!.id) : 0;
    sendJson(res, 200, {
      id: conv.id,
      title: conv.title,
      model: conv.model,
      messages,
      turns: turns.map((t: any) => ({ ...t, costUsd: t.costUsd == null ? null : Number(t.costUsd), events: byTurn.get(t.id) ?? [] })),
      pendingConfirmations: pending,
      lastEventId,
    });
  }

  async #rename(req: IncomingMessage, res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const body = await readJson(req);
    if (body.title === undefined && body.model === undefined) throw bad("title or model is required");
    if (body.model !== undefined && !c.deps.config.models.some((m) => m.id === body.model)) throw bad("unknown model");
    const title = body.title === undefined ? null : String(body.title).replace(/\s+/g, " ").trim().slice(0, 120);
    await c.deps.db.query("update conversations set title = coalesce($2, title), model = coalesce($3, model) where id = $1", [
      conv.id,
      title,
      body.model ?? null,
    ]);
    res.writeHead(204).end();
  }

  async #delete(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    await c.deps.db.query("update conversations set deleted_at = now() where id = $1", [conv.id]);
    const { rows } = await c.deps.db.query<{ id: string; replica: string | null }>(
      "select id, replica from turns where conversation_id = $1 and status in ('queued','running','waiting')",
      [conv.id],
    );
    for (const t of rows) await this.#cancelTurn(c, t.id, t.replica);
    void c.sandboxes.releaseConversation(conv.id, "deleted").catch((e) => logError("release on delete", e));
    res.writeHead(204).end();
  }

  async #warm(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    if (!this.#warming.has(conv.id) && !this.runner.draining) {
      this.#warming.add(conv.id);
      c.sandboxes
        .ensure(conv.id)
        .catch((e) => logError("warm", e, { conversation: conv.id }))
        .finally(() => this.#warming.delete(conv.id));
    }
    res.writeHead(204).end();
  }

  async #message(req: IncomingMessage, res: ServerResponse, p: Record<string, string>, c: Ctx) {
    if (this.runner.draining) throw new HttpError(503, "this server is restarting, try again");
    const conv = await this.#own(c, p.id);
    const body = await readJson(req);
    const text = String(body.text ?? "").trim();
    if (!text) throw bad("text is required");
    if (text.length > 100_000) throw bad("the message is too long");
    const attachments: string[] = Array.isArray(body.attachments) ? body.attachments.map(String).slice(0, 50) : [];
    if (attachments.length) {
      const have = new Set((await liveFiles(c.deps, conv.id)).map((f) => f.path));
      for (const a of attachments) if (!have.has(a)) throw bad(`no such file: ${a}`);
    }
    const turnId = randomUUID();
    const token = newToken();
    const db = c.deps.db;
    try {
      await db.query("insert into turns (id, conversation_id, status, token_hash, model) values ($1, $2, 'queued', $3, $4)", [turnId, conv.id, sha256hex(token), conv.model]);
      await db.query("update conversations set model = $2 where id = $1 and model <> $2", [conv.id, conv.model]);
    } catch (e) {
      if ((e as { code?: string }).code === "23505") throw new HttpError(409, "a reply is still being written in this conversation");
      throw e;
    }
    await db.query("insert into messages (conversation_id, role, content, turn_id, attachments) values ($1, 'user', $2, $3, $4)", [conv.id, text, turnId, JSON.stringify(attachments)]);
    await db.query("update conversations set updated_at = now() where id = $1", [conv.id]);
    this.runner.start(turnId, token);
    sendJson(res, 202, { turnId });
  }

  async #stream(req: IncomingMessage, res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const q = new URL(req.url || "/", "http://x").searchParams;
    let last = Math.max(Number(req.headers["last-event-id"]) || 0, Number(q.get("after")) || 0);
    res.writeHead(200, { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-store", "x-accel-buffering": "no", connection: "keep-alive" });
    res.write("retry: 2000\n\n");
    this.#streams.add(res);
    let busy = false;
    let again = false;
    let closed = false;
    const pump = async () => {
      if (busy) {
        again = true;
        return;
      }
      busy = true;
      try {
        do {
          again = false;
          const { rows } = await c.deps.db.query<EventRow>("select * from events where conversation_id = $1 and id > $2 order by id limit 500", [conv.id, last]);
          for (const e of rows) {
            if (closed) return;
            res.write(`id: ${e.id}\ndata: ${JSON.stringify(publicEvent(e))}\n\n`);
            last = Number(e.id);
          }
          if (rows.length === 500) again = true;
        } while (again && !closed);
      } catch (e) {
        logError("stream", e);
      } finally {
        busy = false;
      }
    };
    const unlisten = await c.deps.listener.listen(channel("conv", conv.id), () => void pump());
    const beat = setInterval(() => res.write(": hb\n\n"), 15_000);
    const done = new Promise<void>((resolve) => {
      res.on("close", () => {
        closed = true;
        resolve();
      });
    });
    await pump();
    await done;
    clearInterval(beat);
    this.#streams.delete(res);
    await unlisten();
  }

  async #cancelTurn(c: Ctx, turnId: string, replica: string | null): Promise<void> {
    if (!replica) {
      const { rows } = await c.deps.db.query<{ conversation_id: string }>(
        "update turns set status = 'cancelled', finished_at = now() where id = $1 and status = 'queued' and replica is null returning conversation_id",
        [turnId],
      );
      if (rows[0]) await appendEvent(c.deps.db, rows[0].conversation_id, turnId, "turn_finished", { turnId, status: "cancelled", error: null, costUsd: null });
      this.runner.cancel(turnId);
    } else if (replica === c.deps.replica) {
      this.runner.cancel(turnId);
    } else {
      await c.deps.db.query("select pg_notify($1, $2)", [channel("cancel", replica), turnId]);
    }
  }

  async #cancel(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const turnId = uuidParam(p.id);
    const { rows } = await c.deps.db.query<{ id: string; replica: string | null; status: string }>(
      "select t.id, t.replica, t.status from turns t join conversations c on c.id = t.conversation_id where t.id = $1 and c.owner = $2",
      [turnId, c.caller.email],
    );
    const t = rows[0];
    if (!t) throw notFound();
    if (["queued", "running", "waiting"].includes(t.status)) await this.#cancelTurn(c, t.id, t.replica);
    res.writeHead(204).end();
  }

  async #confirm(req: IncomingMessage, res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const id = uuidParam(p.id);
    const body = await readJson(req);
    if (typeof body.approve !== "boolean") throw bad("approve must be true or false");
    const status = body.approve ? "approved" : "denied";
    const db = c.deps.db;
    const { rows } = await db.query<{ conversation_id: string; turn_id: string }>(
      `update confirmations f set status = $2, decided_at = now(), decided_by = $3 from conversations c
        where f.id = $1 and f.status = 'pending' and c.id = f.conversation_id and c.owner = $3
        returning f.conversation_id, f.turn_id`,
      [id, status, c.caller.email],
    );
    const row = rows[0];
    if (!row) {
      const { rows: any } = await db.query(
        "select f.status from confirmations f join conversations c on c.id = f.conversation_id where f.id = $1 and c.owner = $2",
        [id, c.caller.email],
      );
      if (!any[0]) throw notFound();
      throw new HttpError(409, `already ${any[0].status}`);
    }
    await db.query("select pg_notify($1, $2)", [channel("confirm", id), status]);
    await db.query("update turns set status = 'running' where id = $1 and status = 'waiting'", [row.turn_id]);
    await appendEvent(db, row.conversation_id, row.turn_id, "confirmation_resolved", { id, status });
    res.writeHead(204).end();
  }

  async #upload(req: IncomingMessage, res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const name = new URL(req.url || "/", "http://x").searchParams.get("name") || "";
    if (!name.trim()) throw bad("name is required");
    const r = await saveUpload(c.deps, conv.id, name, req);
    await c.deps.db.query("update conversations set updated_at = now() where id = $1", [conv.id]);
    sendJson(res, 201, r);
  }

  async #files(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const rows = await liveFiles(c.deps, conv.id);
    sendJson(res, 200, rows.map((r) => ({ path: r.path, size: Number(r.size), source: r.source, updatedAt: r.updated_at })));
  }

  async #download(res: ServerResponse, p: Record<string, string>, c: Ctx) {
    const conv = await this.#own(c, p.id);
    const path = p.rest ?? "";
    const { rows } = await c.deps.db.query<{ sha256: string; size: string }>(
      "select sha256, size from files where conversation_id = $1 and path = $2 and not deleted",
      [conv.id, path],
    );
    const f = rows[0];
    if (!f) throw notFound();
    const body = await c.deps.blobs.getStream(blobKey(c.deps, conv.id, f.sha256));
    if (!body) throw notFound("the file's contents are missing");
    const name = path.split("/").pop() || "file";
    res.writeHead(200, {
      "content-type": contentType(path),
      "content-length": f.size,
      "content-disposition": `attachment; filename="${name.replace(/[^\x20-\x7e]|["\\]/g, "_")}"; filename*=UTF-8''${encodeURIComponent(name)}`,
      "cache-control": "private, no-store",
      "x-content-type-options": "nosniff",
    });
    body.on("error", () => res.destroy());
    body.pipe(res);
  }

  async #usage(req: IncomingMessage, res: ServerResponse, c: Ctx) {
    if (!c.caller.isAdmin) throw notFound();
    const q = new URL(req.url || "/", "http://x").searchParams;
    const from = q.get("from") || new Date(Date.now() - 30 * 86400_000).toISOString();
    const to = q.get("to") || new Date().toISOString();
    const { rows } = await c.deps.db.query(
      `with t as (select c.owner, t.id from turns t join conversations c on c.id = t.conversation_id where t.started_at >= $1 and t.started_at < $2)
       select u.email,
              (select count(*) from t where t.owner = u.email)::int as turns,
              coalesce((select sum(e.cost_usd) from egress_log e join turns tt on tt.id = e.turn_id join conversations cc on cc.id = tt.conversation_id
                         where cc.owner = u.email and e.kind = 'model' and e.at >= $1 and e.at < $2), 0)::float as "modelUsd",
              (select count(*) from tool_calls k where k.user_email = u.email and k.at >= $1 and k.at < $2)::int as "toolCalls",
              coalesce((select sum(k.cost_usd) from tool_calls k where k.user_email = u.email and k.at >= $1 and k.at < $2), 0)::float as "toolUsd",
              coalesce((select sum(coalesce(e.bytes_up,0) + coalesce(e.bytes_down,0)) from egress_log e join conversations cc on cc.id = e.conversation_id
                         where cc.owner = u.email and e.kind <> 'model' and e.at >= $1 and e.at < $2), 0)::bigint as "egressBytes"
         from users u order by u.email`,
      [from, to],
    );
    sendJson(res, 200, { from, to, users: rows });
  }

  async #monitorSnapshot(req: IncomingMessage, res: ServerResponse, c: Ctx) {
    if (!c.caller.isAdmin) throw notFound();
    const tz = new URL(req.url || "/", "http://x").searchParams.get("tz") || "UTC";
    sendJson(res, 200, await this.#monitor.snapshot(tz));
  }

  /** The shared budget (admins only): spend, limit, remaining, where the limit comes from, recent changes. */
  async #budget(res: ServerResponse, c: Ctx) {
    if (!c.caller.isAdmin) throw notFound();
    const [status, audit] = await Promise.all([budgetStatus(c.deps.db, c.deps.config.budget), budgetAudit(c.deps.db)]);
    sendJson(res, 200, { budget: status, config: c.deps.config.budget ?? null, audit });
  }

  /** An admin's reset ({action: "reset"}), new limit ({action: "set", limitUsd}) or return to config ({action: "clear"}). */
  async #budgetChange(req: IncomingMessage, res: ServerResponse, c: Ctx) {
    if (!c.caller.isAdmin) throw notFound();
    const body = await readJson(req);
    const action = body.action;
    if (action !== "reset" && action !== "set" && action !== "clear") throw bad('action must be "reset", "set" or "clear"');
    if (action === "set" && typeof body.limitUsd !== "number") throw bad("limitUsd must be a number");
    if (action === "reset" && !c.deps.config.budget) {
      const { rows } = await c.deps.db.query("select 1 from kv where key = 'budget:override'");
      if (!rows.length) throw bad("there is no budget to reset: set a limit first");
    }
    try {
      const budget = await changeBudget(c.deps.db, c.deps.config.budget, c.caller.email, { action, limitUsd: body.limitUsd });
      sendJson(res, 200, { budget, config: c.deps.config.budget ?? null, audit: await budgetAudit(c.deps.db) });
    } catch (e) {
      if (e instanceof RangeError) throw bad(e.message);
      throw e;
    }
  }

  async #egress(res: ServerResponse, c: Ctx) {
    if (!c.caller.isAdmin) throw notFound();
    const { rows } = await c.deps.db.query(
      `select e.id, e.at, cc.owner as "user", e.conversation_id as "conversationId", e.kind, e.method, e.host, e.port, e.path,
              e.status, e.decision, e.bytes_up as "bytesUp", e.bytes_down as "bytesDown", e.ms, e.cost_usd as "costUsd"
         from egress_log e left join conversations cc on cc.id = e.conversation_id order by e.id desc limit 500`,
    );
    sendJson(res, 200, rows);
  }

  /** From the tools role: one event for a running turn (e.g. a confirmation request), authorised by the turn token. */
  async #internalEvent(req: IncomingMessage, res: ServerResponse) {
    const turn = await turnForToken(this.deps.db, bearerOf(req.headers.authorization));
    if (!turn) throw new HttpError(401, "unknown or finished turn");
    const body = await readJson(req);
    const type = String(body.type ?? "");
    if (!/^[a-z_]{1,40}$/.test(type)) throw bad("bad event type");
    const { type: _t, ...data } = body;
    if (type === "confirmation_request") await this.deps.db.query("update turns set status = 'waiting' where id = $1 and status = 'running'", [turn.id]);
    const id = await appendEvent(this.deps.db, turn.conversation_id, turn.id, type, data);
    sendJson(res, 201, { id: Number(id) });
  }

  /** The UI's web manifest with the instance's product name (the static build says "AI"). */
  async #manifest(res: ServerResponse): Promise<void> {
    const name = this.deps.config.product.name;
    let m: Record<string, unknown> = {};
    try {
      m = JSON.parse(await readFile(join(this.deps.uiDir, "manifest.webmanifest"), "utf8"));
    } catch {
      m = { start_url: "/", display: "standalone" };
    }
    const data = Buffer.from(JSON.stringify({ ...m, name, short_name: name }));
    res.writeHead(200, { "content-type": "application/manifest+json", "content-length": data.length, "cache-control": "no-cache" });
    res.end(data);
  }

  async #static(path: string, res: ServerResponse): Promise<void> {
    const serve = async (root: string, rel: string, cache: string): Promise<boolean> => {
      const p = normalize(join(root, rel));
      if (!p.startsWith(normalize(root + "/"))) return false;
      const st = await stat(p).catch(() => null);
      if (!st?.isFile()) return false;
      res.writeHead(200, { "content-type": MIME[extname(p).toLowerCase()] || "application/octet-stream", "content-length": st.size, "cache-control": cache });
      createReadStream(p).pipe(res);
      return true;
    };
    let rel: string;
    try {
      rel = decodeURIComponent(path);
    } catch {
      throw notFound();
    }
    if (rel.startsWith("/brand/")) {
      if (await serve(this.deps.brandDir, rel.slice("/brand/".length), "public, max-age=3600")) return;
      throw notFound();
    }
    if (rel === "/manifest.webmanifest") return await this.#manifest(res);
    if (rel !== "/" && (await serve(this.deps.uiDir, rel.slice(1), rel.startsWith("/assets/") ? "public, max-age=31536000, immutable" : "no-cache"))) return;
    if (rel.startsWith("/assets/") || extname(rel)) throw notFound();
    if (rel === "/admin" || rel.startsWith("/admin/")) {
      if (await serve(this.deps.uiDir, "admin.html", "no-cache")) return;
      throw notFound();
    }
    if (await serve(this.deps.uiDir, "index.html", "no-cache")) return;
    sendHtml(res, 200, `<!doctype html><title>${esc(this.deps.config.product.name)}</title><p>The app is not installed on this server.</p>`);
  }
}

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8", ".json": "application/json", ".svg": "image/svg+xml", ".png": "image/png",
  ".ico": "image/x-icon", ".webmanifest": "application/manifest+json", ".woff2": "font/woff2", ".txt": "text/plain; charset=utf-8",
  ".jpg": "image/jpeg", ".webp": "image/webp",
};
