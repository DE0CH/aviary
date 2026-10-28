// The tools role's HTTP side: POST /mcp (a sandbox's egress token → its conversation and owner), GET /healthz. Every
// tools/call is a row in tool_calls. Only read-only tools exist here: the built-in ones and the upstream tools each upstream's `tools`
// list names (upstream.ts); nothing asks for or waits on a confirmation.
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { logError } from "../common/log.ts";
import { bearerOf, sandboxForToken } from "../common/tokens.ts";
import { BUILTIN } from "./builtin.ts";
import { handleMessage, type McpHandlers } from "./mcp.ts";
import type { Deps, ToolCtx, ToolDef, ToolResult } from "./types.ts";
import type { Upstream } from "./upstream.ts";

const MAX_BODY = 2_000_000;

/** The built-in tools this instance can run (a tool whose provider is not configured is left out), then upstreams'. */
export function toolList(deps: Deps, upstreams: Upstream[]): ToolDef[] {
  const t = deps.config.tools;
  const have: Record<string, boolean> = {
    web_search: !!t.exa,
    web_fetch: true,
    news_search: !!t.wisers,
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

export function createToolsServer(deps: Deps, upstreams: Upstream[]): { server: Server; drain(graceMs: number): Promise<void> } {
  let draining = false;
  let active = 0;
  const handlers: McpHandlers = {
    serverName: "tools",
    serverVersion: "1",
    instructions: "Read-only search and reading tools for news, PR and data analysis work. None of them sends, posts or changes anything outside.",
    tools: () => toolList(deps, upstreams),
    async call(tool, args, ctx) {
      const t0 = Date.now();
      let r: ToolResult | undefined;
      let err: string | null = null;
      try {
        r = await tool.run(args, ctx);
        return r;
      } catch (e) {
        err = (e as Error).message;
        throw e;
      } finally {
        await deps.db
          .query(
            `insert into tool_calls (conversation_id, user_email, tool, args, ok, ms, error, cost_usd) values ($1,$2,$3,$4,$5,$6,$7,$8)`,
            [ctx.caller.conversation_id, ctx.caller.owner, tool.name, JSON.stringify(args), !err && !r?.isError, Date.now() - t0,
              err ?? (r?.isError ? r.content.map((c) => (c as { text?: string }).text ?? "").join(" ").slice(0, 500) : null), r?.costUsd ?? null],
          )
          .catch((e) => logError("tool_calls insert", e, { tool: tool.name }));
      }
    },
  };

  const send = (res: ServerResponse, status: number, body?: unknown, headers: Record<string, string> = {}) => {
    const b = body === undefined ? "" : JSON.stringify(body);
    res.writeHead(status, { ...(b ? { "content-type": "application/json" } : {}), "cache-control": "no-store", ...headers });
    res.end(b);
  };

  const readBody = (req: IncomingMessage) =>
    new Promise<string>((ok, fail) => {
      const chunks: Buffer[] = [];
      let n = 0;
      req.on("data", (c: Buffer) => {
        n += c.length;
        if (n > MAX_BODY) {
          fail(new Error("too large"));
          req.destroy();
        } else chunks.push(c);
      });
      req.on("end", () => ok(Buffer.concat(chunks).toString("utf8")));
      req.on("error", fail);
    });

  const server = createServer(async (req, res) => {
    const url = new URL(req.url || "/", "http://x");
    if (url.pathname === "/healthz") return send(res, draining ? 503 : 200, { ok: !draining });
    if (url.pathname !== "/mcp") return send(res, 404, { error: "not found" });
    if (req.method === "GET") return send(res, 405, { error: "this server does not stream; POST JSON-RPC" }, { allow: "POST" });
    if (req.method === "DELETE") return send(res, 405, undefined, { allow: "POST" });
    if (req.method !== "POST") return send(res, 405, undefined, { allow: "POST" });
    if (draining) return send(res, 503, { error: "draining" }, { "retry-after": "1" });
    active++;
    try {
      const token = bearerOf(req.headers.authorization);
      const caller = await sandboxForToken(deps.db, token);
      if (!caller) return send(res, 401, { error: "a live sandbox's token is required" }, { "www-authenticate": "Bearer" });
      let msg: unknown;
      try {
        msg = JSON.parse(await readBody(req));
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
      logError("mcp request", e);
      if (!res.headersSent) send(res, 500, { error: "internal error" });
    } finally {
      active--;
    }
  });
  // an upstream call may take up to 2 minutes (its own timeout)
  server.requestTimeout = 180_000;
  server.headersTimeout = 60_000;
  server.keepAliveTimeout = 65_000;

  return {
    server,
    async drain(graceMs) {
      draining = true;
      const end = Date.now() + graceMs;
      while (active > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 200));
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
}
