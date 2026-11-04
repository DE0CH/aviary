// The old stack's tools role (deploy/chat/tools.yaml, `JARVIS_ROLE=tools`; docs/CHAT.md "tools"), until A6: the same
// tools as the new stack's (builtin.ts, upstream.ts) behind the old stack's front door. A sandbox calls it through
// egress's route `tools`, which passes its egress token on (keepAuth); the token names the sandbox, its conversation
// and its owner (table `sandboxes` in database jarvis), and every call is a row in that database's tool_calls. The
// configuration is the shared config.json's `tools` section. The REST API is the new stack's (server.ts):
//   GET /v1/tools, POST /v1/tools/<name>, GET /v1/openapi.json; GET /healthz for the probes.
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { loadConfig } from "../common/config.ts";
import { connect, type Db } from "../common/db.ts";
import { log, logError } from "../common/log.ts";
import { bearerOf, sha256hex } from "../common/tokens.ts";
import type { ToolsConfig } from "./config.ts";
import { answer, openapi, toolList } from "./server.ts";
import type { Caller, Deps, ToolDef, ToolResult } from "./types.ts";
import { ToolInputError } from "./types.ts";
import { startRefresh, Upstream } from "./upstream.ts";

const MAX_BODY = 2_000_000;

/** The sandbox an egress token belongs to, with its conversation and owner (a live sandbox only). */
export async function callerForToken(db: Pick<Db, "query">, token: string): Promise<Caller | null> {
  if (!token) return null;
  const { rows } = await db.query(
    `select id, conversation_id, owner from sandboxes where egress_token_hash = $1 and status in ('starting','ready') and owner is not null`,
    [sha256hex(token)],
  );
  const r = rows[0];
  return r ? { sandboxId: String(r.id), conversationId: String(r.conversation_id), person: String(r.owner).trim().toLowerCase(), request: null } : null;
}

export function legacyServer(deps: Deps, db: Pick<Db, "query">, upstreams: Upstream[]) {
  let draining = false;
  let active = 0;
  const send = (res: ServerResponse, status: number, body?: unknown, headers: Record<string, string> = {}) => {
    const b = body === undefined ? "" : JSON.stringify(body);
    res.writeHead(status, { ...(b ? { "content-type": "application/json; charset=utf-8" } : {}), "cache-control": "no-store", ...headers });
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
  const run = async (tool: ToolDef, args: Record<string, unknown>, caller: Caller): Promise<ToolResult> => {
    const t0 = Date.now();
    let r: ToolResult | undefined;
    let err: string | null = null;
    try {
      r = await tool.run(args, { caller, deps });
      return r;
    } catch (e) {
      err = (e as Error).message;
      throw e;
    } finally {
      await db
        .query(
          `insert into tool_calls (conversation_id, user_email, tool, args, ok, ms, error, cost_usd) values ($1,$2,$3,$4,$5,$6,$7,$8)`,
          [caller.conversationId, caller.person, tool.name, JSON.stringify(args), !err && !r?.isError, Date.now() - t0,
            err ?? (r?.isError ? r.content.map((c) => (c as { text?: string }).text ?? "").join(" ").slice(0, 500) : null), r?.costUsd ?? null],
        )
        .catch((e) => logError("tool_calls insert", e, { tool: tool.name }));
    }
  };

  const server = createServer(async (req, res) => {
    const url = new URL(req.url || "/", "http://x");
    const path = url.pathname;
    if (path === "/healthz") return send(res, draining ? 503 : 200, { ok: !draining });
    const call = /^\/v1\/tools\/([a-z0-9_]{1,64})$/.exec(path);
    if (!(path === "/v1/tools" || path === "/v1/openapi.json" || call)) return send(res, 404, { error: "GET /v1/tools, POST /v1/tools/<name>, GET /v1/openapi.json" });
    const want = call ? "POST" : "GET";
    if (req.method !== want) return send(res, 405, { error: `${want} only` }, { allow: want });
    if (draining) return send(res, 503, { error: "draining" }, { "retry-after": "1" });
    active++;
    try {
      const caller = await callerForToken(db, bearerOf(req.headers.authorization));
      if (!caller) return send(res, 401, { error: "a live sandbox's token is required" }, { "www-authenticate": "Bearer" });
      const tools = toolList(deps, upstreams);
      if (path === "/v1/tools") return send(res, 200, tools.map((t) => ({ name: t.name, ...(t.title ? { title: t.title } : {}), description: t.description, inputSchema: t.inputSchema })));
      if (path === "/v1/openapi.json") return send(res, 200, openapi(tools));
      const tool = tools.find((t) => t.name === call![1]);
      if (!tool) return send(res, 404, { error: `no tool ${call![1]}: GET /v1/tools lists them` });
      let args: unknown;
      try {
        const body = await readBody(req);
        args = body.trim() ? JSON.parse(body) : {};
      } catch {
        return send(res, 400, { error: "the body must be a JSON object of the tool's arguments" });
      }
      if (!args || typeof args !== "object" || Array.isArray(args)) return send(res, 400, { error: "the body must be a JSON object of the tool's arguments" });
      let r: ToolResult;
      try {
        r = await run(tool, args as Record<string, unknown>, caller);
      } catch (e) {
        if (e instanceof ToolInputError) return send(res, 400, { error: e.message });
        return send(res, 502, { error: `${tool.name} failed: ${(e as Error).message}`.slice(0, 500) });
      }
      const a = answer(r);
      res.writeHead(a.status, { "content-type": a.type, "cache-control": "no-store" });
      res.end(a.body);
    } catch (e) {
      logError("tools request", e);
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
    async drain(graceMs: number) {
      draining = true;
      const end = Date.now() + graceMs;
      while (active > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 200));
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
}

export async function main(): Promise<void> {
  const config = loadConfig();
  const listAs = config.admins[0] ?? config.users[0]!;
  const tools: ToolsConfig = {
    ...(config.tools as Omit<ToolsConfig, "upstreams">),
    upstreams: config.tools.upstreams.map((u) => ({ ...u, listAs })),
  } as ToolsConfig;
  const db = connect();
  const deps: Deps = { config: tools, env: process.env, fetch: globalThis.fetch };
  const upstreams = tools.upstreams.map((u) => new Upstream(u, deps));
  const refresh = startRefresh(upstreams);
  await refresh.ready;
  const { server, drain } = legacyServer(deps, db, upstreams);
  const port = Number(process.env.PORT || 8080);
  server.listen(port, () => log("tools (old stack) listening", { port }));
  let stopping = false;
  const stop = async () => {
    if (stopping) return;
    stopping = true;
    log("tools draining");
    refresh.stop();
    // an upstream call may take up to 2 minutes; the pod's grace period bounds this
    await drain(Number(process.env.JARVIS_DRAIN_MS || 150_000));
    await db.end();
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}
