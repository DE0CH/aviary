// tools' HTTP side (aviary platform spec 17.2 tools items 2 and 5): `POST /mcp` from the gateway only. Every call
// carries the gateway's pair bearer (one of TOOLS_ACCEPT_GATEWAY) and the sandbox the gateway verified
// (`Aviary-Sandbox-Id`, `-Conversation`, `-Person`, `Aviary-Request`; platform 3.11.3); the person must be in the
// staff catalogue and hermit must say the sandbox is live and starting or ready (liveness.ts). Every tools/call is a
// row in tool_calls (written without waiting). Only read-only tools exist here: the built-in ones and the upstream
// tools each upstream's `tools` list names (upstream.ts); nothing asks for or waits on a confirmation.
import { createHash, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { Socket } from "node:net";
import { logError } from "../common/log.ts";
import { personKey } from "../egress/ids.ts";
import { BUILTIN } from "./builtin.ts";
import type { Catalogue } from "./catalogue.ts";
import type { CallLog } from "./db.ts";
import type { Liveness } from "./liveness.ts";
import { handleMessage, type McpHandlers } from "./mcp.ts";
import type { Caller, Deps, ToolCtx, ToolDef, ToolResult } from "./types.ts";
import type { Upstream } from "./upstream.ts";

/** tools' own cap on a request body (platform 3.11.3); a bigger one is answered 413 too_large. */
export const MAX_BODY = 2_000_000;
class TooLarge extends Error {}
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** In-flight calls finish within this bound after SIGTERM (an upstream call's own timeout is 120 s). The shutdown
 * fits the pod's 180 s grace period (platform 9.7) with 10 s to spare, whatever the database does: preStop 10 s +
 * this drain 150 s + the final tool_calls flush, bounded at FLUSH_MS 8 s + closing the pool, bounded at
 * POOL_END_MS 2 s = 170 s (main.ts). */
export const DRAIN_MS = 150_000;
export const FLUSH_MS = 8_000;
export const POOL_END_MS = 2_000;

const digest = (s: string): Buffer => createHash("sha256").update(s).digest();

/** `TOOLS_ACCEPT_GATEWAY`: a comma-separated list (current first; two during a rotation), kept as SHA-256 digests. A
 * missing or empty value (or an empty entry) is a start failure, named, never its value (platform 3.7). */
export function acceptList(env: Record<string, string | undefined>, name = "TOOLS_ACCEPT_GATEWAY"): Buffer[] {
  const v = env[name];
  if (!v) throw new Error(`environment variable ${name} is not set`);
  const parts = v.split(",").map((x) => x.trim());
  if (parts.some((x) => !x)) throw new Error(`environment variable ${name} has an empty entry`);
  return parts.map(digest);
}

function accepted(list: Buffer[], header: string | undefined): boolean {
  const m = /^Bearer\s+(\S+)$/i.exec(header ?? "");
  if (!m) return false;
  const d = digest(m[1]!);
  let ok = false;
  for (const x of list) ok = timingSafeEqual(d, x) || ok;
  return ok;
}

/** The built-in tools this instance can run (a tool whose provider is not configured is left out), then upstreams'. */
export function toolList(deps: Deps, upstreams: Upstream[]): ToolDef[] {
  const t = deps.config;
  const have: Record<string, boolean> = {
    web_search: !!t.exa,
    web_fetch: true,
    social_search: !!t.tikhub,
    anyapi_search: !!t.anyapi,
    anyapi_describe: !!t.anyapi,
    anyapi_run: !!t.anyapi,
    anyapi_result: !!t.anyapi,
  };
  const own = BUILTIN.filter((d) => have[d.name]);
  const names = new Set(own.map((d) => d.name));
  const ups = upstreams.flatMap((u) => u.tools()).filter((d) => !names.has(d.name) && (names.add(d.name), true));
  return [...own, ...ups];
}

export interface ServerParts {
  /** SHA-256 digests of the accepted gateway bearers (acceptList) */
  accept: Buffer[];
  liveness: Pick<Liveness, "check">;
  catalogue: Pick<Catalogue, "allowed">;
  calls: Pick<CallLog, "write">;
}

export interface ToolsServer {
  server: Server;
  /** Stops new connections; a request on a kept-alive connection is served with `Connection: close`; in-flight calls
   * finish up to `boundMs`, then what is left is cut: each tool call still running has its tool_calls row written
   * (ok false, "cut by the drain") and is aborted. Resolves with the number of requests cut. */
  drain(boundMs?: number): Promise<number>;
  readonly inFlight: number;
  metrics(): string[];
}

export function createToolsServer(deps: Deps, upstreams: Upstream[], parts: ServerParts): ToolsServer {
  let draining = false;
  let active = 0;
  let cutTotal = 0;
  const answered = new Map<string, number>();
  const callsDone = new Map<string, number>();
  const count = (m: Map<string, number>, k: string) => m.set(k, (m.get(k) ?? 0) + 1);
  /** the tool calls running now: each entry writes the call's row as cut and aborts it */
  const runningCalls = new Set<() => void>();

  const handlers: McpHandlers = {
    serverName: "tools",
    serverVersion: "1",
    instructions: "Read-only search and reading tools for news, PR and data analysis work. None of them sends, posts or changes anything outside.",
    tools: () => toolList(deps, upstreams),
    async call(tool, args, ctx) {
      const t0 = Date.now();
      let r: ToolResult | undefined;
      let err: string | null = null;
      let written = false;
      // the call's tool_calls row, written once: when the call ends, or at the drain's cut (then the row says so and is
      // in CallLog's pending writes before main flushes them)
      const writeRow = (cut: boolean) => {
        if (written) return;
        written = true;
        const ok = !cut && !err && !r?.isError;
        count(callsDone, ok ? "true" : "false");
        parts.calls.write({
          conversationId: ctx.caller.conversationId,
          sandboxId: ctx.caller.sandboxId,
          person: ctx.caller.person,
          request: ctx.caller.request,
          tool: tool.name,
          args,
          ok,
          ms: Date.now() - t0,
          error: cut
            ? `cut by the drain after ${Math.round((Date.now() - t0) / 1000)} s`
            : (err ?? (r?.isError ? r.content.map((c) => (c as { text?: string }).text ?? "").join(" ").slice(0, 500) : null)),
          costUsd: cut ? null : (r?.costUsd ?? null),
        });
      };
      const ac = new AbortController();
      const cutThis = () => {
        writeRow(true);
        ac.abort(new Error("the tools server is shutting down"));
      };
      runningCalls.add(cutThis);
      try {
        r = await tool.run(args, { ...ctx, signal: ac.signal });
        return r;
      } catch (e) {
        err = (e as Error).message;
        throw e;
      } finally {
        runningCalls.delete(cutThis);
        writeRow(false);
      }
    },
  };

  const send = (res: ServerResponse, status: number, body?: unknown, headers: Record<string, string> = {}) => {
    const b = body === undefined ? "" : JSON.stringify(body);
    count(answered, String(status));
    res.writeHead(status, {
      ...(b ? { "content-type": "application/json; charset=utf-8" } : {}),
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...(draining ? { connection: "close" } : {}),
      ...headers,
    });
    res.end(b);
  };
  const refuse = (res: ServerResponse, status: number, code: string, message: string, headers: Record<string, string> = {}) =>
    send(res, status, { code, message }, headers);

  // over MAX_BODY, the rest is read and dropped (requestTimeout bounds it) and the answer, 413 with Connection: close,
  // goes once the body has ended, so the gateway reads a clear refusal rather than a reset connection
  const readBody = (req: IncomingMessage) =>
    new Promise<string>((ok, fail) => {
      const chunks: Buffer[] = [];
      let n = 0;
      req.on("data", (c: Buffer) => {
        n += c.length;
        if (n <= MAX_BODY) chunks.push(c);
        else chunks.length = 0;
      });
      req.on("end", () => (n > MAX_BODY ? fail(new TooLarge()) : ok(Buffer.concat(chunks).toString("utf8"))));
      req.on("error", fail);
    });

  const header = (req: IncomingMessage, name: string): string => {
    const v = req.headers[name];
    return (Array.isArray(v) ? v[0] : v)?.trim() ?? "";
  };

  const server = createServer(async (req, res) => {
    const url = new URL(req.url || "/", "http://x");
    if (url.pathname !== "/mcp") return refuse(res, 404, "not_found", "only /mcp is served here");
    if (req.method !== "POST")
      return req.method === "GET"
        ? refuse(res, 405, "method_not_allowed", "this server does not stream; POST JSON-RPC", { allow: "POST" })
        : refuse(res, 405, "method_not_allowed", "POST only", { allow: "POST" });
    active++;
    let caller: Caller | null = null;
    try {
      if (!accepted(parts.accept, req.headers.authorization)) return refuse(res, 401, "unauthenticated", "the gateway's bearer is required", { "www-authenticate": "Bearer" });
      const sandboxId = header(req, "aviary-sandbox-id").toLowerCase();
      const conversationId = header(req, "aviary-sandbox-conversation").toLowerCase();
      const person = header(req, "aviary-sandbox-person").toLowerCase();
      if (!UUID.test(sandboxId) || !UUID.test(conversationId) || !person.includes("@"))
        return refuse(res, 400, "bad_request", "Aviary-Sandbox-Id, Aviary-Sandbox-Conversation and Aviary-Sandbox-Person are required");
      caller = { sandboxId, conversationId, person, request: header(req, "aviary-request") || null };
      if (!parts.catalogue.allowed(person)) return refuse(res, 403, "not_allowed", "this person may not use YYC AI");
      const v = await parts.liveness.check(sandboxId, conversationId, person);
      if (!v.ok)
        return refuse(res, 403, "not_allowed", v.why === "mismatch" ? "the sandbox belongs to another conversation or person" : `the sandbox is not live (${v.state ?? "unknown"})`);
      let body: string;
      try {
        body = await readBody(req);
      } catch (e) {
        if (e instanceof TooLarge) return refuse(res, 413, "too_large", `the request body is over tools' ${MAX_BODY} byte cap`, { connection: "close" });
        throw e;
      }
      let msg: unknown;
      try {
        msg = JSON.parse(body);
      } catch {
        return send(res, 400, { jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } });
      }
      const ctx: ToolCtx = { caller, deps };
      if (Array.isArray(msg)) {
        const out = (await Promise.all(msg.map((m) => handleMessage(handlers, m, ctx)))).filter((x) => x !== null);
        return out.length ? send(res, 200, out) : send(res, 202);
      }
      const out = await handleMessage(handlers, msg, ctx);
      return out ? send(res, 200, out) : send(res, 202);
    } catch (e) {
      logError("mcp request", e, caller ? { request: caller.request, conversation: caller.conversationId, person_key: personKey(caller.person) } : {});
      if (!res.headersSent) refuse(res, 500, "internal", "internal error");
    } finally {
      active--;
    }
  });
  // an upstream call may take up to 2 minutes (its own timeout); the gateway's tools route waits 180 s
  server.requestTimeout = 180_000;
  server.headersTimeout = 60_000;
  server.keepAliveTimeout = 65_000;
  const sockets = new Set<Socket>();
  server.on("connection", (s) => {
    sockets.add(s);
    s.on("close", () => sockets.delete(s));
  });

  return {
    server,
    get inFlight() {
      return active;
    },
    metrics() {
      const lines = [`tools_in_flight ${active}`, `tools_draining ${draining ? 1 : 0}`, `tools_drain_cut_total ${cutTotal}`];
      for (const [k, n] of answered) lines.push(`tools_requests_total{status="${k}"} ${n}`);
      for (const [k, n] of callsDone) lines.push(`tools_calls_total{ok="${k}"} ${n}`);
      return lines;
    },
    async drain(boundMs = DRAIN_MS) {
      draining = true;
      const closed = new Promise<void>((r) => server.close(() => r()));
      server.closeIdleConnections();
      const end = Date.now() + boundMs;
      while (active > 0 && Date.now() < end) {
        await new Promise((r) => setTimeout(r, 100));
        server.closeIdleConnections();
      }
      const cut = active;
      cutTotal += cut;
      // every tool call still running gets its tool_calls row now (marked cut) and is aborted, so main's flush writes it
      for (const c of [...runningCalls]) c();
      for (const s of sockets) s.destroy();
      await closed;
      return cut;
    },
  };
}
