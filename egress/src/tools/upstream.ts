// Upstream REST APIs (config.tools.upstreams): an API described by OpenAPI 3.1 at `<url>/openapi.json`, one operation per
// tool at `POST <url>/<tool>` (JSON arguments in, JSON out; yyc-mcp's /api/v1). Its tools appear as <prefix>_<tool>,
// calls are forwarded with the configured headers, ${USER_EMAIL} = the staff member whose sandbox calls. Only the
// tools the upstream's `tools` list names are offered (default deny: a new upstream tool stays hidden until it is added
// to the list; the list names read-only tools only). The tool list is read at startup and every 10 minutes (a failed
// refresh keeps the last good list). The first read is bounded at 10 s per upstream, so an upstream that hangs never
// holds the pod unready. An upstream with no list yet (its first read failed or ran out of time) is read again soon,
// after 5 s, 10 s, 30 s, 1, 2 and 5 minutes, then at the 10-minute cadence; /metrics shows tools_upstream_tools{prefix}
// so an empty list can be alerted on. Each later read is bounded at 2 minutes as a whole.
import { expandEnv } from "../common/config.ts";
import type { UpstreamApi } from "./config.ts";
import { logError, log } from "../common/log.ts";
import type { Deps, ToolDef, ToolResult } from "./types.ts";

const CALL_TIMEOUT_MS = 120_000;
/** The first tool-list read of each upstream at start; then each periodic read as a whole. */
export const FIRST_LIST_MS = 10_000;
export const LIST_MS = 120_000;
/** The pauses before reading again an upstream that has no list yet (then the regular cadence). */
export const FIRST_RETRY_MS = [5_000, 10_000, 30_000, 60_000, 120_000, 300_000];

interface UpstreamTool {
  name: string;
  title?: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
}

export const exposedName = (prefix: string, name: string): string =>
  `${prefix}_${name}`.toLowerCase().replace(/[^a-z0-9_]/g, "_").replace(/_+/g, "_").slice(0, 64);

/** The tools an OpenAPI document offers: each `POST <base>/<name>` operation with a JSON request body. */
export function toolsOfOpenApi(doc: any): UpstreamTool[] {
  const out: UpstreamTool[] = [];
  for (const [path, item] of Object.entries((doc?.paths ?? {}) as Record<string, any>)) {
    const op = item?.post;
    const name = path.split("/").pop();
    if (!op || !name) continue;
    out.push({
      name,
      ...(op.summary ? { title: op.summary } : {}),
      description: op.description ?? op.summary ?? "",
      inputSchema: op.requestBody?.content?.["application/json"]?.schema ?? { type: "object", properties: {} },
    });
  }
  return out;
}

export class Upstream {
  readonly cfg: UpstreamApi;
  readonly #deps: Deps;
  #tools: UpstreamTool[] = [];
  /** a tool-list read has worked once */
  listed = false;
  constructor(cfg: UpstreamApi, deps: Deps) {
    this.cfg = cfg;
    this.#deps = deps;
  }

  #headers(user: string): Record<string, string> {
    const vars = { ...this.#deps.env, USER_EMAIL: user };
    const h: Record<string, string> = {};
    for (const [k, v] of Object.entries(this.cfg.headers ?? {})) h[k] = expandEnv(v, vars);
    return h;
  }

  #base(): string {
    return this.cfg.url.replace(/\/+$/, "");
  }

  /** Re-read the tool list (as `listUser`, default the upstream's `listAs`) within `boundMs`; a read that fails or runs
   * out of time keeps the last list. */
  async refresh(listUser = this.cfg.listAs, boundMs = LIST_MS): Promise<void> {
    const r = await this.#deps.fetch(`${this.#base()}/openapi.json`, { headers: { ...this.#headers(listUser), accept: "application/json" }, signal: AbortSignal.timeout(boundMs) });
    const body = await r.text();
    if (!r.ok) throw new Error(`${this.cfg.prefix} answered HTTP ${r.status}: ${body.slice(0, 300)}`);
    const all = toolsOfOpenApi(JSON.parse(body));
    this.#tools = all.filter((t) => offered(this.cfg, t));
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
      run: async (args, ctx): Promise<ToolResult> => {
        const timeout = AbortSignal.timeout(CALL_TIMEOUT_MS);
        const r = await this.#deps.fetch(`${this.#base()}/${encodeURIComponent(t.name)}`, {
          method: "POST",
          headers: { ...this.#headers(ctx.caller.person), "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify(args),
          signal: ctx.signal ? AbortSignal.any([timeout, ctx.signal]) : timeout,
        });
        const body = await r.text();
        let v: unknown;
        try {
          v = JSON.parse(body);
        } catch {
          v = body;
        }
        if (r.status >= 400 && r.status < 500) {
          // the upstream's own words on what to change (its `error`), as the tool's error
          const msg = (v as { error?: string })?.error ?? body.slice(0, 500);
          return { content: [{ type: "text", text: String(msg) }], isError: true };
        }
        if (!r.ok) throw new Error(`${this.cfg.prefix} answered HTTP ${r.status}: ${body.slice(0, 300)}`);
        // the API's JSON as it is (no text: the REST answer is that JSON)
        return typeof v === "string" ? { content: [{ type: "text", text: v }] } : { content: [], structuredContent: v };
      },
    }));
  }
}

/** Is an upstream tool offered: named in the upstream's `tools` list. */
export function offered(cfg: Pick<UpstreamApi, "tools">, t: { name: string }): boolean {
  return (cfg.tools ?? []).includes(t.name);
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
