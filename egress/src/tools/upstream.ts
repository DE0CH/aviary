// Upstream MCP servers (config.tools.upstreams): their tools appear as <prefix>_<tool>, calls are forwarded with the
// configured headers, ${USER_EMAIL} = the staff member whose sandbox calls. Only the read-only tools the upstream's
// `tools` list names are offered (default deny: a new upstream tool stays hidden until it is added to the list). The
// tool list is read at startup and every 10 minutes (a failed refresh keeps the last good list). The first read is
// bounded at 10 s per upstream, so an upstream that hangs never holds the pod unready. An upstream with no list yet
// (its first read failed or ran out of time) is read again soon, after 5 s, 10 s, 30 s, 1, 2 and 5 minutes, then at
// the 10-minute cadence, since Claude Code reads the tool list once per conversation and a conversation started
// meanwhile never sees that upstream's tools; /metrics shows tools_upstream_tools{prefix} so an empty list can be
// alerted on. Each later read is bounded at 2 minutes as a whole. Streamable HTTP client: JSON or SSE answers, a
// session id when the server hands one out.
import { expandEnv } from "../common/config.ts";
import type { UpstreamMcp } from "./config.ts";
import { logError, log } from "../common/log.ts";
import type { Annotations, Content, Deps, ToolDef, ToolResult } from "./types.ts";

const PROTOCOL = "2025-06-18";
const CALL_TIMEOUT_MS = 120_000;
/** The first tool-list read of each upstream at start; then each periodic read as a whole. */
export const FIRST_LIST_MS = 10_000;
export const LIST_MS = 120_000;
/** The pauses before reading again an upstream that has no list yet (then the regular cadence). */
export const FIRST_RETRY_MS = [5_000, 10_000, 30_000, 60_000, 120_000, 300_000];

interface RpcOpts {
  retried?: boolean;
  /** aborts the request (with the 120 s timeout) */
  signal?: AbortSignal;
}

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
  /** a tool-list read has worked once */
  listed = false;
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

  async #rpc(user: string, method: string, params: Record<string, unknown>, o: RpcOpts = {}): Promise<any> {
    const timeout = AbortSignal.timeout(CALL_TIMEOUT_MS);
    const signal = o.signal ? AbortSignal.any([timeout, o.signal]) : timeout;
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
      signal,
    });
    const body = await r.text();
    if ((r.status === 400 || r.status === 404) && !o.retried && method !== "initialize") {
      // a server that wants a session: open one and ask again
      await this.#initialize(user, o.signal);
      return this.#rpc(user, method, params, { retried: true, signal: o.signal });
    }
    if (!r.ok) throw new Error(`${this.cfg.prefix} answered HTTP ${r.status}: ${body.slice(0, 300)}`);
    const sid = r.headers.get("mcp-session-id");
    if (sid) this.#sessions.set(user, sid);
    const m = pickResponse(body, r.headers.get("content-type") || "", id);
    if (m.error) throw new Error(`${this.cfg.prefix}: ${m.error.message ?? JSON.stringify(m.error)}`);
    return m.result;
  }

  async #initialize(user: string, signal?: AbortSignal): Promise<void> {
    this.#sessions.delete(user);
    await this.#rpc(user, "initialize", { protocolVersion: PROTOCOL, capabilities: {}, clientInfo: { name: "chat-tools", version: "1" } }, { retried: true, signal });
    const sid = this.#sessions.get(user);
    await this.#deps
      .fetch(this.cfg.url, {
        method: "POST",
        headers: { ...this.#headers(user), "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-protocol-version": PROTOCOL, ...(sid ? { "mcp-session-id": sid } : {}) },
        body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
        signal: signal ? AbortSignal.any([AbortSignal.timeout(CALL_TIMEOUT_MS), signal]) : AbortSignal.timeout(CALL_TIMEOUT_MS),
      })
      .then((r) => r.text())
      .catch(() => {});
  }

  /** Re-read the tool list (as `listUser`, default the upstream's `listAs`), all pages within `boundMs`; a read that
   * fails or runs out of time keeps the last list. */
  async refresh(listUser = this.cfg.listAs, boundMs = LIST_MS): Promise<void> {
    const signal = AbortSignal.timeout(boundMs);
    const tools: UpstreamTool[] = [];
    let cursor: string | undefined;
    for (let i = 0; i < 20; i++) {
      const r = await this.#rpc(listUser, "tools/list", cursor ? { cursor } : {}, { signal });
      tools.push(...((r?.tools ?? []) as UpstreamTool[]));
      cursor = r?.nextCursor;
      if (!cursor) break;
    }
    const all = tools.filter((t) => t && typeof t.name === "string");
    this.#tools = all.filter((t) => readOnlyAllowed(this.cfg, t));
    this.listed = true;
    const hidden = all.filter((t) => !this.#tools.includes(t)).map((t) => t.name);
    log("upstream tools", { prefix: this.cfg.prefix, count: this.#tools.length, hidden });
  }

  /** How many tools this upstream offers now (tools_upstream_tools). */
  get count(): number {
    return this.#tools.length;
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
        const r = await this.#rpc(ctx.caller.person, "tools/call", { name: t.name, arguments: args }, { signal: ctx.signal });
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
export function readOnlyAllowed(cfg: Pick<UpstreamMcp, "tools">, t: { name: string; annotations?: Annotations }): boolean {
  if (!(cfg.tools ?? []).includes(t.name)) return false;
  return t.annotations?.readOnlyHint !== false && t.annotations?.destructiveHint !== true;
}

/** Keep every upstream's list fresh (each read as its `listAs`): now (each bounded at `firstMs`; `ready` resolves when
 * every first read has worked, failed or run out of time), then every `everyMs`; a failure is logged and the old list
 * stays. An upstream with no list yet is read again after each pause of `retryMs` in turn (capped at `everyMs`), then
 * every `everyMs`. */
export function startRefresh(
  ups: Upstream[],
  everyMs = 600_000,
  firstMs = FIRST_LIST_MS,
  retryMs: number[] = FIRST_RETRY_MS,
): { stop(): void; ready: Promise<void> } {
  let stopped = false;
  const timers = new Set<NodeJS.Timeout>();
  const read = (u: Upstream, boundMs: number) => u.refresh(undefined, boundMs).catch((e) => logError("upstream refresh", e, { prefix: u.cfg.prefix, listed: u.listed }));
  const schedule = (u: Upstream, attempt: number) => {
    if (stopped) return;
    const ms = u.listed ? everyMs : Math.min(retryMs[attempt] ?? everyMs, everyMs);
    const t = setTimeout(() => {
      timers.delete(t);
      // only the first read is bounded short (it holds readiness); a slow but working upstream gets the full bound here
      void read(u, LIST_MS).then(() => schedule(u, u.listed ? 0 : attempt + 1));
    }, ms);
    t.unref();
    timers.add(t);
  };
  const ready = Promise.all(
    ups.map((u) =>
      read(u, firstMs).then(() => {
        schedule(u, 0);
      }),
    ),
  ).then(() => {});
  return {
    ready,
    stop: () => {
      stopped = true;
      for (const t of timers) clearTimeout(t);
      timers.clear();
    },
  };
}
