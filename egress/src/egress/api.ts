// egress's in-cluster API on 8082 (aviary platform spec 3.11.7, 17.2 item 4), never reached by sandboxes:
//   GET /v1/aviary/models                       pigeon, admin   (Service `model`; R18)
//   POST /v1/embeddings                         pigeon          (the conversation search's query, Egress.embedQuery)
//   GET /v1/ops/summary?tz=&recentMinutes=      hermit, admin   (15.3)
//   GET /v1/admin/egress-log?conversation=&since=&limit=   admin
// Callers present their pair tokens; egress reads EGRESS_ACCEPT_<CALLER> (comma-separated, current first) and compares
// SHA-256 digests in constant time (3.7).
import { createHash, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { logError } from "../common/log.ts";
import type { BudgetGate } from "./budget.ts";
import { modelsAnswer, type EgressConfig } from "./config.ts";
import type { Db } from "./db.ts";
import { egressLogRows, opsSummary } from "./summary.ts";

export type ApiCaller = "pigeon" | "hermit" | "admin";
export const API_CALLERS: ApiCaller[] = ["pigeon", "hermit", "admin"];

/** The route-to-caller table (in code, not configuration). */
const ROUTES: Record<string, ApiCaller[]> = {
  "/v1/aviary/models": ["pigeon", "admin"],
  "/v1/embeddings": ["pigeon"],
  "/v1/ops/summary": ["hermit", "admin"],
  "/v1/admin/egress-log": ["admin"],
};

const digest = (s: string) => createHash("sha256").update(s).digest();

/** `<CALLEE>_ACCEPT_<CALLER>` lists; a missing or empty one is a start failure (named, never its value). */
export function acceptLists(env: Record<string, string | undefined>): Record<ApiCaller, Buffer[]> {
  const out = {} as Record<ApiCaller, Buffer[]>;
  for (const c of API_CALLERS) {
    const name = `EGRESS_ACCEPT_${c.toUpperCase()}`;
    const v = (env[name] ?? "").split(",").map((s) => s.trim()).filter(Boolean);
    if (!v.length) throw new Error(`environment variable ${name} is not set`);
    out[c] = v.map(digest);
  }
  return out;
}

function callerOf(req: IncomingMessage, accept: Record<ApiCaller, Buffer[]>): ApiCaller | null {
  const m = /^Bearer\s+(\S+)$/i.exec(req.headers.authorization ?? "");
  if (!m) return null;
  const d = digest(m[1]!);
  let found: ApiCaller | null = null;
  for (const c of API_CALLERS) for (const h of accept[c]) if (timingSafeEqual(d, h) && !found) found = c;
  return found;
}

export interface ApiDeps {
  db: Db;
  config: EgressConfig;
  budget: BudgetGate | null;
  accept: Record<ApiCaller, Buffer[]>;
  /** the conversation search's query embedding (Egress.embedQuery); absent: 503 */
  embed?: (body: Buffer) => Promise<{ status: number; body: Buffer }>;
}

/** A query embedding's request body at most: one short text. */
const EMBED_BODY_MAX = 64 * 1024;

export class EgressApi {
  readonly server: Server;
  draining = false;
  readonly #d: ApiDeps;

  constructor(d: ApiDeps) {
    this.#d = d;
    this.server = createServer((req, res) => {
      this.#handle(req, res).catch((e) => {
        logError("egress api", e, { path: (req.url ?? "").split("?")[0] });
        this.#send(res, 503, { code: "unavailable", message: "the request failed; try again" }, { "retry-after": "1" });
      });
    });
  }

  listen(port: number, host = "::"): Promise<number> {
    return new Promise((resolve) => this.server.listen(port, host, () => resolve((this.server.address() as { port: number }).port)));
  }

  /** Stops accepting; answers in flight finish (they are short), then every connection is closed. */
  async drain(ms = 15_000): Promise<void> {
    this.draining = true;
    await new Promise<void>((resolve) => {
      const t = setTimeout(() => {
        this.server.closeAllConnections();
        resolve();
      }, ms);
      this.server.close(() => {
        clearTimeout(t);
        resolve();
      });
      this.server.closeIdleConnections();
    });
  }

  #send(res: ServerResponse, status: number, body: unknown, extra: Record<string, string> = {}): void {
    if (res.headersSent) return void res.destroy();
    const b = JSON.stringify(body);
    res.writeHead(status, {
      "content-type": "application/json; charset=utf-8",
      "content-length": String(Buffer.byteLength(b)),
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...(this.draining ? { connection: "close" } : {}),
      ...extra,
    });
    res.end(b);
  }

  async #embeddings(req: IncomingMessage, res: ServerResponse): Promise<void> {
    if (req.method !== "POST") return this.#send(res, 405, { code: "method_not_allowed", message: "POST only" }, { allow: "POST" });
    if (!this.#d.embed) return this.#send(res, 503, { code: "unavailable", message: "no query embeddings here" });
    const parts: Buffer[] = [];
    let size = 0;
    for await (const c of req) {
      size += (c as Buffer).length;
      if (size > EMBED_BODY_MAX) return this.#send(res, 413, { code: "too_large", message: "a query's embedding is one short text" });
      parts.push(c as Buffer);
    }
    const r = await this.#d.embed(Buffer.concat(parts));
    if (res.headersSent) return void res.destroy();
    res.writeHead(r.status, { "content-type": "application/json", "content-length": String(r.body.length), "cache-control": "no-store", ...(this.draining ? { connection: "close" } : {}) });
    res.end(r.body);
  }

  async #handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const u = new URL(req.url ?? "/", "http://egress");
    const allowed = ROUTES[u.pathname];
    if (!allowed) return this.#send(res, 404, { code: "not_found", message: "no such route" });
    const caller = callerOf(req, this.#d.accept);
    if (!caller) return this.#send(res, 401, { code: "unauthenticated", message: "a caller's bearer token is needed" }, { "www-authenticate": "Bearer" });
    if (!allowed.includes(caller)) return this.#send(res, 403, { code: "wrong_caller", message: `this route is not for ${caller}` });
    if (u.pathname === "/v1/embeddings") return this.#embeddings(req, res);
    if (req.method !== "GET") return this.#send(res, 405, { code: "method_not_allowed", message: "GET only" }, { allow: "GET" });
    const q = u.searchParams;
    switch (u.pathname) {
      case "/v1/aviary/models":
        return this.#send(res, 200, modelsAnswer(this.#d.config));
      case "/v1/ops/summary": {
        const rm = q.get("recentMinutes");
        return this.#send(res, 200, await opsSummary(this.#d.db, this.#d.budget, this.#d.config.budget, q.get("tz"), rm === null ? null : Number(rm)));
      }
      case "/v1/admin/egress-log": {
        const conversation = q.get("conversation");
        if (conversation && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(conversation)) return this.#send(res, 400, { code: "bad_request", message: "conversation must be a uuid" });
        const since = q.get("since");
        if (since && Number.isNaN(Date.parse(since))) return this.#send(res, 400, { code: "bad_request", message: "since must be an RFC 3339 time" });
        const limit = q.get("limit");
        return this.#send(res, 200, { rows: await egressLogRows(this.#d.db, { conversation: conversation?.toLowerCase() ?? null, since, limit: limit === null ? null : Number(limit) }) });
      }
    }
  }
}
