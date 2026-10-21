// Upstream MCP servers (config.tools.upstreams): their tools appear as <prefix>_<tool>, calls are forwarded with the
// configured headers, ${USER_EMAIL} = the staff member whose sandbox calls. Only the read-only tools the upstream's
// `tools` list names are offered (default deny: a new upstream tool stays hidden until it is added to the list). The tool list is read at startup and every
// 10 minutes (a failed refresh keeps the last good list). Streamable HTTP client: JSON or SSE answers, a session id
// when the server hands one out.
import { expandEnv, type UpstreamMcp } from "../common/config.ts";
import { logError, log } from "../common/log.ts";
import type { Annotations, Content, Deps, ToolDef, ToolResult } from "./types.ts";

const PROTOCOL = "2025-06-18";

interface UpstreamTool {
  name: string;
  title?: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
  annotations?: Annotations;
}

export const exposedName = (prefix: string, name: string): string =>
  `${prefix}_${name}`.toLowerCase().replace(/[^a-z0-9_]/g, "_").replace(/_+/g, "_").slice(0, 64);

/** The JSON-RPC answer with `id` out of a JSON or text/event-stream body. */
export function pickResponse(body: string, contentType: string, id: number): any {
  if (contentType.includes("text/event-stream")) {
    for (const block of body.split(/\r?\n\r?\n/)) {
      const data = block
        .split(/\r?\n/)
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(5).replace(/^ /, ""))
        .join("\n");
      if (!data) continue;
      try {
        const m = JSON.parse(data);
        for (const x of Array.isArray(m) ? m : [m]) if (x && x.id === id) return x;
      } catch {
        /* not JSON: skip */
      }
    }
    throw new Error("the upstream's event stream carried no answer");
  }
  const m = JSON.parse(body);
  const x = Array.isArray(m) ? m.find((y) => y && y.id === id) : m;
  if (!x) throw new Error("the upstream sent no answer");
  return x;
}

export class Upstream {
  readonly cfg: UpstreamMcp;
  readonly #deps: Deps;
  #tools: UpstreamTool[] = [];
  #nextId = 1;
  #sessions = new Map<string, string>(); // user -> Mcp-Session-Id (for servers that hand them out)
  constructor(cfg: UpstreamMcp, deps: Deps) {
    this.cfg = cfg;
    this.#deps = deps;
  }

  #headers(user: string): Record<string, string> {
    const vars = { ...this.#deps.env, USER_EMAIL: user };
    const h: Record<string, string> = {};
    for (const [k, v] of Object.entries(this.cfg.headers ?? {})) h[k] = expandEnv(v, vars);
    return h;
  }

  async #rpc(user: string, method: string, params: Record<string, unknown>, retried = false): Promise<any> {
    const id = this.#nextId++;
    const session = this.#sessions.get(user);
    const r = await this.#deps.fetch(this.cfg.url, {
      method: "POST",
      headers: {
        ...this.#headers(user),
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
        "mcp-protocol-version": PROTOCOL,
        ...(session ? { "mcp-session-id": session } : {}),
      },
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
      signal: AbortSignal.timeout(120_000),
    });
    const body = await r.text();
    if ((r.status === 400 || r.status === 404) && !retried && method !== "initialize") {
      // a server that wants a session: open one and ask again
      await this.#initialize(user);
      return this.#rpc(user, method, params, true);
    }
    if (!r.ok) throw new Error(`${this.cfg.prefix} answered HTTP ${r.status}: ${body.slice(0, 300)}`);
    const sid = r.headers.get("mcp-session-id");
    if (sid) this.#sessions.set(user, sid);
    const m = pickResponse(body, r.headers.get("content-type") || "", id);
    if (m.error) throw new Error(`${this.cfg.prefix}: ${m.error.message ?? JSON.stringify(m.error)}`);
    return m.result;
  }

  async #initialize(user: string): Promise<void> {
    this.#sessions.delete(user);
    await this.#rpc(user, "initialize", { protocolVersion: PROTOCOL, capabilities: {}, clientInfo: { name: "chat-tools", version: "1" } }, true);
    const sid = this.#sessions.get(user);
    await this.#deps
      .fetch(this.cfg.url, {
        method: "POST",
        headers: { ...this.#headers(user), "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-protocol-version": PROTOCOL, ...(sid ? { "mcp-session-id": sid } : {}) },
        body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
      })
      .then((r) => r.text())
      .catch(() => {});
  }

  /** Re-read the tool list (as `listUser`: the identity used for listing). */
  async refresh(listUser: string): Promise<void> {
    const tools: UpstreamTool[] = [];
    let cursor: string | undefined;
    for (let i = 0; i < 20; i++) {
      const r = await this.#rpc(listUser, "tools/list", cursor ? { cursor } : {});
      tools.push(...((r?.tools ?? []) as UpstreamTool[]));
      cursor = r?.nextCursor;
      if (!cursor) break;
    }
    const all = tools.filter((t) => t && typeof t.name === "string");
    this.#tools = all.filter((t) => readOnlyAllowed(this.cfg, t));
    const hidden = all.filter((t) => !this.#tools.includes(t)).map((t) => t.name);
    log("upstream tools", { prefix: this.cfg.prefix, count: this.#tools.length, hidden });
  }

  tools(): ToolDef[] {
    return this.#tools.map((t) => ({
      name: exposedName(this.cfg.prefix, t.name),
      ...(t.title ? { title: t.title } : {}),
      description: t.description ?? "",
      inputSchema: t.inputSchema ?? { type: "object", properties: {} },
      ...(t.outputSchema ? { outputSchema: t.outputSchema } : {}),
      ...(t.annotations ? { annotations: t.annotations } : {}),
      run: async (args, ctx): Promise<ToolResult> => {
        const r = await this.#rpc(ctx.caller.owner, "tools/call", { name: t.name, arguments: args });
        return {
          content: (Array.isArray(r?.content) ? r.content : [{ type: "text", text: JSON.stringify(r ?? null) }]) as Content[],
          ...(r?.isError ? { isError: true } : {}),
          ...(r?.structuredContent !== undefined ? { structuredContent: r.structuredContent } : {}),
        };
      },
    }));
  }
}

/** Is an upstream tool offered: named in the upstream's `tools` list and not marked as writing by the upstream. */
export function readOnlyAllowed(cfg: UpstreamMcp, t: { name: string; annotations?: Annotations }): boolean {
  if (!(cfg.tools ?? []).includes(t.name)) return false;
  return t.annotations?.readOnlyHint !== false && t.annotations?.destructiveHint !== true;
}

/** Keep every upstream's list fresh: now, then every `everyMs`; a failure is logged and the old list stays. */
export function startRefresh(ups: Upstream[], listUser: string, everyMs = 600_000): { stop(): void; ready: Promise<void> } {
  const once = () =>
    Promise.all(ups.map((u) => u.refresh(listUser).catch((e) => logError("upstream refresh", e, { prefix: u.cfg.prefix })))).then(() => {});
  const ready = once();
  const t = setInterval(once, everyMs);
  t.unref();
  return { ready, stop: () => clearInterval(t) };
}
