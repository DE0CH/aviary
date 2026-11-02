// The egress server: YYC's sandbox gateway (aviary platform spec 3.11, 17.2; docs/CHAT.md "egress"). One HTTP proxy
// on 3128 that every sandbox client uses (absolute-form requests and CONNECT), deciding each request in this order:
//   1. the credential: `Proxy-Authorization: Basic sbx:<hst1 token>` checked locally against hermit's keys, then
//      hermit's liveness view (tokens.ts); 407 without one, 403 `not_allowed` for a bad one;
//   2. a fixed name (`magpie`, `hermit`, `model`, `tools`, exact host, no port or port 80): its route, with the client's
//      credentials and `Aviary-*` headers stripped and the verified `Aviary-Sandbox-*` triple and the callee's bearer
//      added; `magpie` and `hermit` are built in (`/v1/sandbox/**` only, no log row), `model` and `tools` are the
//      configured routes (the model route forces zero data retention, checks the model and the budget, logs the cost);
//   3. a self host (`selfHosts`, 127.0.0.1, localhost; any port): local paths, the package mirror;
//   4. anything else: the internet rules (port, allowlist, threat DNS, public addresses, upload cap).
// Every request but the magpie and hermit calls ends up in egress_log.
import { Agent, createServer, request as httpRequest, type IncomingMessage, type OutgoingHttpHeaders, type Server, type ServerResponse } from "node:http";
import { request as httpsRequest } from "node:https";
import { connect as netConnect, type Socket } from "node:net";
import { createReadStream } from "node:fs";
import { Readable } from "node:stream";
import type { Duplex } from "node:stream";
import { expandEnv, type ReasoningPlan } from "../common/config.ts";
import { logError } from "../common/log.ts";
import { BUDGET_ERROR_TYPE, budgetMessage, type BudgetGate } from "./budget.ts";
import { allowedModels, modelsAnswer, reasoningPlan, type EgressConfig, type EgressRoute } from "./config.ts";
import { DiskCache } from "./cache.ts";
import { EgressLog, type EgressRow } from "./egresslog.ts";
import { HostPolicy } from "./allowlist.ts";
import { AddressGuard, GuardError, normHost } from "./netguard.ts";
import { DnsRefusal, ThreatResolver } from "./threatdns.ts";
import { BodyBudget, forceZdr, type ModelBody } from "./modelbody.ts";
import { isRequestId, personKey, uuidv7 } from "./ids.ts";
import { PROXY_REALM, presentedToken, type Claims, type SandboxTokens } from "./tokens.ts";

/** What one in-flight piece of work is, for the drain bounds and egress_drain_cut_total{kind}. */
export type DrainKind = "model" | "tunnel" | "fixed" | "tools";
/** 3.11.6 step 3: model streams and internet tunnels up to 600 s (Claude Code's own request timeout), magpie and hermit
 * requests (25 s long-polls, cc writes, uploads) up to 60 s, tools calls up to their 180 s timeout. */
export const DRAIN_BOUNDS_MS: Record<DrainKind, number> = { model: 600_000, tunnel: 600_000, fixed: 60_000, tools: 180_000 };
const DRAIN_KINDS: DrainKind[] = ["model", "tunnel", "fixed", "tools"];

/** The per-request body limit on `magpie`, `hermit` and `model` (3.11.3); 64 MiB + 1 is 413 `too_large`. */
export const FIXED_BODY_MAX = 64 * 1024 * 1024;
/** A magpie or hermit upstream silent this long ends the request (above the 25-30 s long-polls). */
const FIXED_IDLE_MS = 120_000;
/** The tools route's request timeout when its config names none (3.11.4: at least 180 s). */
const TOOLS_TIMEOUT_MS = 180_000;
/** The client `Aviary-*` headers that reach magpie, hermit and tools unchanged (gateway-api headers::FORWARDED). */
const FORWARDED = new Set(["aviary-protocol", "aviary-lease"]);

export type FixedName = "magpie" | "hermit" | "model" | "tools";
const FIXED: ReadonlySet<string> = new Set(["magpie", "hermit", "model", "tools"]);

export interface EgressOptions {
  config: EgressConfig;
  log: EgressLog;
  /** hermit's keys and liveness (tokens.ts) */
  tokens: Pick<SandboxTokens, "verify">;
  /** Which hosts sandboxes may reach (allowlist.ts). */
  policy: HostPolicy;
  /** EGRESS_TOKEN_MAGPIE, EGRESS_TOKEN_HERMIT: the pair bearers egress presents to magpie and hermit */
  pairTokens: { magpie: string; hermit: string };
  cacheDir: string;
  cacheBytes: number;
  /** The shared budget; absent = no limit. */
  budget?: BudgetGate;
  guard?: AddressGuard;
  allowedPorts?: number[];
  /** Name resolution for sandbox connections (default: ThreatResolver, malware-blocking DNS); may throw DnsRefusal. */
  resolver?: (host: string) => Promise<string[]>;
  upstreams?: { pypi: string; pypiFiles: string; npm: string };
  /** Hosts whose plain-HTTP GETs are cached (apt). */
  aptHosts?: string[];
  env?: Record<string, string | undefined>;
  /** Tests only: where `magpie` and `hermit` are (always http://magpie and http://hermit in a deployment). */
  internal?: { magpie: string; hermit: string };
  /** Tests only: shorter drain bounds. */
  drainBounds?: Partial<Record<DrainKind, number>>;
}

const HOP = new Set(["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade"]);
/** The caller's own credentials and host: never sent upstream on a route. */
const CALLER_AUTH = new Set(["authorization", "proxy-authorization", "x-api-key", "cookie", "host"]);
const INDEX_MAX_AGE = 5 * 60_000;

/** What a sandbox (and so the agent) reads when a host is refused: plain, final, no one to ask. */
export const denyText = (host: string): string =>
  `egress: ${host} is not on the sandbox's internet allowlist, and it cannot be enabled from the chat. The sandbox reaches ` +
  "package registries, code hosts and widely used public sites only. To read a web page, use the web_fetch and " +
  "web_search tools instead of the sandbox.\n";

/** A request's target: the absolute-form authority split up, or (origin form, `host` null) a request to the proxy. */
interface Target {
  host: string | null;
  authority: string;
  port: number;
  pathq: string;
}

export function parseTarget(raw: string): Target | null {
  if (raw.startsWith("/")) return { host: null, authority: "", port: 0, pathq: raw };
  const m = /^http:\/\/([^/?#]*)([^#]*)/i.exec(raw);
  if (!m) return null;
  const at = m[1]!.lastIndexOf("@");
  const authority = at >= 0 ? m[1]!.slice(at + 1) : m[1]!;
  let pathq = m[2] || "/";
  if (!pathq.startsWith("/")) pathq = "/" + pathq;
  const hp = /^(\[[^\]]+\]|[^:]*)(?::(\d*))?$/.exec(authority);
  if (!hp || !hp[1]) return null;
  return { host: hp[1].replace(/^\[|\]$/g, "").toLowerCase(), authority, port: hp[2] ? Number(hp[2]) : 80, pathq };
}

/** The fixed name an authority names: exact, ASCII case-insensitive, no port or port 80 (gateway-api FixedName). */
export function fixedName(authority: string): FixedName | null {
  const m = /^([^:]+)(?::(\d+))?$/.exec(authority);
  if (!m || (m[2] !== undefined && m[2] !== "80")) return null;
  const n = m[1]!.toLowerCase();
  return FIXED.has(n) ? (n as FixedName) : null;
}

function cleanPath(path: string): boolean {
  if (!path.startsWith("/")) return false;
  const lower = path.toLowerCase();
  if (lower.includes("%2e") || lower.includes("%2f") || lower.includes("%5c") || path.includes("\\") || path.includes("//")) return false;
  return path.slice(1).split("/").every((s) => s !== "." && s !== "..");
}

/** Whether `method path` is routed on a fixed name (3.11.3; the query is ignored). */
export function routed(name: FixedName, method: string, pathq: string): boolean {
  const path = pathq.split("?")[0]!;
  if (!cleanPath(path)) return false;
  switch (name) {
    case "magpie":
    case "hermit":
      return path.startsWith("/v1/sandbox/");
    case "model":
      return (method === "POST" && (path === "/v1/messages" || path === "/v1/messages/count_tokens")) || (method === "GET" && path === "/v1/aviary/models");
    case "tools":
      return path === "/mcp";
  }
}

/** The verified caller of a request. */
interface Caller {
  claims: Claims;
  request: string;
}

interface Work {
  kind: DrainKind;
  cut: () => void;
}

export class Egress {
  readonly server: Server;
  readonly log: EgressLog;
  readonly cache: DiskCache;
  readonly #o: EgressOptions;
  readonly #guard: AddressGuard;
  readonly #resolve: (host: string) => Promise<string[]>;
  readonly #maxUp: number;
  readonly #ports: Set<number>;
  readonly #up: { pypi: string; pypiFiles: string; npm: string };
  readonly #self: Set<string>;
  readonly #apt: Set<string>;
  readonly #sockets = new Set<Duplex>();
  readonly #bodies: BodyBudget;
  readonly #models: string[];
  readonly #reasoning: ReasoningPlan;
  readonly #routes: Record<"model" | "tools", EgressRoute>;
  readonly #internal: Record<"magpie" | "hermit", URL>;
  readonly #agent = new Agent({ keepAlive: true, maxSockets: 256, maxFreeSockets: 64, keepAliveMsecs: 30_000 });
  readonly #work = new Set<Work>();
  readonly counters = { requests: 0, denied: 0, tunnels: 0, bytesUp: 0, bytesDown: 0 };
  /** egress_fixed_requests_total{route,status}: "route status" → n */
  readonly fixedRequests = new Map<string, number>();
  /** egress_fixed_bytes_total{route,dir}: "route dir" → bytes */
  readonly fixedBytes = new Map<string, number>();
  readonly drainCut: Record<DrainKind, number> = { model: 0, tunnel: 0, fixed: 0, tools: 0 };
  draining = false;

  constructor(o: EgressOptions) {
    this.#o = o;
    const c = o.config;
    this.#guard = o.guard ?? new AddressGuard(c.blockCidrs);
    const threat = o.resolver ? null : new ThreatResolver();
    this.#resolve = o.resolver ?? ((h) => threat!.resolve(h));
    this.#maxUp = c.maxUploadBytes;
    this.#bodies = new BodyBudget(FIXED_BODY_MAX);
    this.#ports = new Set(o.allowedPorts ?? [80, 443]);
    this.#up = o.upstreams ?? { pypi: "https://pypi.org", pypiFiles: "https://files.pythonhosted.org", npm: "https://registry.npmjs.org" };
    this.#self = new Set([...c.selfHosts, "127.0.0.1", "localhost"].map(normHost));
    this.#apt = new Set((o.aptHosts ?? ["deb.debian.org", "security.debian.org"]).map(normHost));
    this.#models = allowedModels(c);
    this.#reasoning = reasoningPlan(c);
    this.#routes = { model: c.routes.find((r) => r.name === "model")!, tools: c.routes.find((r) => r.name === "tools")! };
    const internal = o.internal ?? { magpie: "http://magpie", hermit: "http://hermit" };
    this.#internal = { magpie: new URL(internal.magpie), hermit: new URL(internal.hermit) };
    this.log = o.log;
    this.cache = new DiskCache(o.cacheDir, o.cacheBytes);
    this.server = createServer({ keepAliveTimeout: 120_000, requestTimeout: 0, headersTimeout: 60_000 }, (req, res) => {
      this.counters.requests++;
      this.#onRequest(req, res).catch((e) => {
        logError("egress request", e, { url: safeUrl(req.url) });
        if (!res.headersSent) this.#text(res, 502, "egress: upstream error\n");
        else res.destroy();
      });
    });
    this.server.on("connection", (s: Socket) => {
      s.setNoDelay(true);
      this.#sockets.add(s);
      s.on("close", () => this.#sockets.delete(s));
    });
    this.server.on("connect", (req: IncomingMessage, sock: Duplex, head: Buffer) => {
      this.counters.requests++;
      this.#onConnect(req, sock, head).catch((e) => {
        logError("egress connect", e);
        sock.destroy();
      });
    });
  }

  get openSockets(): number {
    return this.#sockets.size;
  }

  /** In-flight work per drain kind (for /metrics and the drain). */
  inFlight(): Record<DrainKind, number> {
    const n: Record<DrainKind, number> = { model: 0, tunnel: 0, fixed: 0, tools: 0 };
    for (const w of this.#work) n[w.kind]++;
    return n;
  }

  #track(kind: DrainKind, cut: () => void): Work {
    const w = { kind, cut };
    this.#work.add(w);
    return w;
  }

  #done(w: Work): void {
    this.#work.delete(w);
  }

  // ---- answers --------------------------------------------------------------------------------------------------

  /** Writes a response head; while draining every answer carries `Connection: close` (3.11.6 step 2). */
  #head(res: ServerResponse, status: number, headers: OutgoingHttpHeaders): void {
    if (this.draining) headers = { ...headers, connection: "close" };
    res.writeHead(status, headers);
  }

  #text(res: ServerResponse, status: number, body: string, extra: Record<string, string> = {}): void {
    if (res.headersSent) return void res.destroy();
    this.#head(res, status, { "content-type": "text/plain; charset=utf-8", "content-length": String(Buffer.byteLength(body)), ...extra });
    res.end(body);
  }

  /** The gateway's own JSON refusal (3.6 envelope) on fixed names. */
  #json(res: ServerResponse, status: number, code: string, message: string, extra: Record<string, string> = {}): void {
    if (res.headersSent) return void res.destroy();
    const body = JSON.stringify({ code, message });
    this.#head(res, status, { "content-type": "application/json; charset=utf-8", "content-length": String(Buffer.byteLength(body)), "cache-control": "no-store", ...extra });
    res.end(body);
  }

  /** An Anthropic-shaped error for the model route (Claude Code shows its message to the person). */
  #modelError(res: ServerResponse, status: number, type: string, message: string, extra: Record<string, string> = {}): void {
    if (res.headersSent) return void res.destroy();
    const body = JSON.stringify({ type: "error", error: { type, message } });
    this.#head(res, status, { "content-type": "application/json", "content-length": String(Buffer.byteLength(body)), ...extra });
    res.end(body);
  }

  #countFixed(route: string, status: number | string): void {
    const k = `${route} ${status}`;
    this.fixedRequests.set(k, (this.fixedRequests.get(k) ?? 0) + 1);
  }

  #countBytes(route: string, dir: "up" | "down", n: number): void {
    if (!n) return;
    const k = `${route} ${dir}`;
    this.fixedBytes.set(k, (this.fixedBytes.get(k) ?? 0) + n);
  }

  #deny(row: EgressRow, decision: string, status: number): void {
    this.counters.denied++;
    this.log.add({ ...row, decision, status });
  }

  // ---- routing --------------------------------------------------------------------------------------------------

  async #onRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const raw = req.url || "/";
    if (/^https:\/\//i.test(raw)) return this.#text(res, 400, "egress: use CONNECT for https\n");
    const t = parseTarget(raw);
    if (!t) return this.#text(res, 400, "egress: bad request target\n");
    const fixed = t.host === null ? null : fixedName(t.authority);
    const self = !fixed && (t.host === null || this.#self.has(normHost(t.host)));
    const sent = req.headers["aviary-request"];
    const request = typeof sent === "string" && isRequestId(sent) ? sent : uuidv7();
    // a request is drained like the fixed names until it is known to be something else
    const w = this.#track("fixed", () => {
      req.destroy();
      res.destroy();
    });
    res.on("close", () => this.#done(w));
    const internetRow = (): EgressRow => ({ kind: self ? "mirror" : "http", method: req.method ?? "GET", host: self ? "mirror" : normHost(t.host ?? "-"), port: t.port || null, path: t.pathq, decision: "allow", request });

    // (1) the credential
    const tok = presentedToken(req.headers, self);
    if (tok === "missing") {
      if (fixed) {
        if (fixed === "magpie" || fixed === "hermit") this.#countFixed(fixed, 407);
        return this.#json(res, 407, "proxy_auth_required", "the proxy needs the sandbox credential", { "proxy-authenticate": PROXY_REALM });
      }
      this.#deny(internetRow(), "deny:auth", 407);
      return this.#text(res, 407, "egress: proxy authentication required\n", { "proxy-authenticate": PROXY_REALM });
    }
    const v = tok === "malformed" ? ({ error: "malformed" } as const) : await this.#o.tokens.verify(tok, req.socket.remoteAddress);
    if ("error" in v) {
      if (fixed) {
        if (fixed === "magpie" || fixed === "hermit") this.#countFixed(fixed, 403);
        return this.#json(res, 403, "not_allowed", v.error === "not_live" ? "sandbox not live" : "sandbox token not valid");
      }
      this.#deny(internetRow(), "deny:auth", 403);
      return this.#text(res, 403, "egress: unknown or expired sandbox token\n");
    }
    const caller: Caller = { claims: v.claims, request };

    // (2) a fixed name, (3) a self host, (4) the internet
    if (fixed) return this.#fixed(fixed, req, res, t, caller, w);
    if (self) {
      w.kind = "tunnel";
      const path = t.pathq;
      if (path.startsWith("/mirror/")) return this.#mirror(req, res, path, caller);
      return this.#text(res, 404, "egress: not found\n");
    }
    w.kind = "tunnel";
    return this.#forward(req, res, t, caller);
  }

  async #fixed(name: FixedName, req: IncomingMessage, res: ServerResponse, t: Target, caller: Caller, w: Work): Promise<void> {
    const method = req.method ?? "GET";
    if (!routed(name, method, t.pathq)) {
      if (name === "magpie" || name === "hermit") this.#countFixed(name, 403);
      else this.log.add({ ...this.#row(name === "model" ? "model" : "tools", req, t, caller), decision: "deny:path", status: 403 });
      return this.#json(res, 403, "not_allowed", `${method} ${t.pathq.split("?")[0]} is not routed on ${name}`);
    }
    if (name === "magpie" || name === "hermit") return this.#internalRoute(name, req, res, t, caller);
    if (name === "model" && t.pathq.split("?")[0] === "/v1/aviary/models") {
      const body = JSON.stringify(modelsAnswer(this.#o.config));
      this.#head(res, 200, { "content-type": "application/json; charset=utf-8", "content-length": String(Buffer.byteLength(body)), "cache-control": "no-store" });
      return void res.end(body);
    }
    w.kind = name === "model" ? "model" : "tools";
    return this.#route(this.#routes[name], req, res, t, caller);
  }

  #row(kind: "model" | "tools", req: IncomingMessage, t: Target, c: Caller): EgressRow {
    const up = new URL(this.#routes[kind].upstream);
    return {
      kind,
      method: req.method ?? "GET",
      host: up.hostname,
      path: t.pathq.split("?")[0]!,
      decision: "allow",
      sandbox_id: c.claims.sandboxId,
      conversation_id: c.claims.conversationId,
      person: c.claims.person,
      request: c.request,
    };
  }

  /** The headers a fixed-name upstream gets (3.11.3): the client's minus its credentials and `Aviary-*` (but
   * FORWARDED), plus the verified identity, the request id and `bearer`. */
  #identityHeaders(h: IncomingMessage["headers"], c: Caller, bearer: string | null): OutgoingHttpHeaders {
    const out = forwardHeaders(h, CALLER_AUTH);
    for (const k of Object.keys(out)) if (k.startsWith("aviary-") && !FORWARDED.has(k)) delete out[k];
    out["aviary-sandbox-id"] = c.claims.sandboxId;
    out["aviary-sandbox-conversation"] = c.claims.conversationId;
    out["aviary-sandbox-person"] = c.claims.person.trim().toLowerCase();
    out["aviary-request"] = c.request;
    if (bearer) out.authorization = bearer;
    return out;
  }

  // ---- magpie and hermit: the hot path (no log row; counters in /metrics) ---------------------------------------------

  #internalRoute(name: "magpie" | "hermit", req: IncomingMessage, res: ServerResponse, t: Target, caller: Caller): Promise<void> {
    const declared = declaredLength(req);
    if (declared !== null && declared > FIXED_BODY_MAX) {
      this.#countFixed(name, 413);
      const refuse = () => this.#json(res, 413, "too_large", `a request body on ${name} is limited to ${FIXED_BODY_MAX} bytes`, { connection: "close" });
      // a body up to twice the limit is read and discarded first, so the client sees the answer rather than a reset
      if (declared > 2 * FIXED_BODY_MAX) refuse();
      else req.on("end", refuse);
      req.resume();
      return Promise.resolve();
    }
    const base = this.#internal[name];
    const headers = this.#identityHeaders(req.headers, caller, `Bearer ${this.#o.pairTokens[name]}`);
    headers.host = name;
    return new Promise<void>((resolve) => {
      let up = 0;
      let down = 0;
      let over = false;
      let status: number | null = null;
      const out = httpRequest({ agent: this.#agent, protocol: base.protocol, hostname: base.hostname, port: base.port || 80, method: req.method, path: t.pathq, headers });
      out.setTimeout(FIXED_IDLE_MS, () => out.destroy(new Error("upstream idle")));
      out.on("response", (ures) => {
        status = ures.statusCode ?? 502;
        this.#head(res, status, stripHop(ures.headers));
        res.flushHeaders();
        ures.on("data", (c: Buffer) => (down += c.length));
        ures.on("error", () => res.destroy());
        ures.pipe(res);
        res.on("close", () => {
          if (!ures.complete) ures.destroy();
        });
      });
      out.on("error", (e) => {
        if (over) return;
        if (!res.headersSent) {
          status = /idle/.test(e.message) ? 504 : 502;
          if (status === 504) this.#json(res, 504, "upstream_timeout", `${name} did not answer in ${FIXED_IDLE_MS / 1000} s`);
          else this.#json(res, 502, "upstream_unreachable", `${name} could not be reached`);
        } else res.destroy();
      });
      req.on("data", (c: Buffer) => {
        up += c.length;
        if (up > FIXED_BODY_MAX && !over) {
          over = true;
          out.destroy();
          status = 413;
          if (!res.headersSent) this.#json(res, 413, "too_large", `a request body on ${name} is limited to ${FIXED_BODY_MAX} bytes`, { connection: "close" });
          else res.destroy();
        }
      });
      req.pipe(out);
      res.on("close", () => {
        if (!out.destroyed && !res.writableFinished) out.destroy();
        this.#countFixed(name, status ?? "none");
        this.#countBytes(name, "up", up);
        this.#countBytes(name, "down", down);
        resolve();
      });
    });
  }

  // ---- model and tools: the configured routes -------------------------------------------------------------------

  async #route(route: EgressRoute, req: IncomingMessage, res: ServerResponse, t: Target, caller: Caller): Promise<void> {
    const t0 = Date.now();
    const [rest, query] = splitQuery(t.pathq);
    const up = new URL(route.upstream);
    const isModel = route.model === true;
    const row = this.#row(isModel ? "model" : "tools", req, t, caller);
    if (route.allowPaths?.length && !route.allowPaths.some((p) => rest.startsWith(p))) {
      this.#json(res, 403, "not_allowed", `path ${rest} is not allowed on ${route.name}`);
      return this.#deny(row, "deny:path", 403);
    }
    let injected: Record<string, string>;
    try {
      injected = Object.fromEntries(Object.entries(route.inject).map(([k, v]) => [k.toLowerCase(), expandEnv(v, this.#o.env ?? process.env)]));
    } catch (e) {
      logError("egress route inject", e, { route: route.name });
      this.#json(res, 500, "misconfigured", `route ${route.name} is misconfigured`);
      return this.log.add({ ...row, status: 500, decision: "deny:config" });
    }
    // The model route (OpenRouter) gets zero data retention forced into every request body: no config, caller or
    // conversation can route a model call to an endpoint that retains or collects prompts. The body is held in memory
    // for that (once, about its own size: modelbody.ts), within the #bodies budget, so large conversations queue
    // instead of taking egress past its memory limit.
    let forcedBody: ModelBody | "too-large" | "invalid" | null = null;
    let release: (() => void) | null = null;
    try {
      if (isModel && req.method !== "GET" && req.method !== "HEAD") {
        const declared = declaredLength(req);
        if (declared !== null && declared > FIXED_BODY_MAX) {
          this.#modelError(res, 413, "request_too_large", `egress: a model request body is limited to ${FIXED_BODY_MAX} bytes`, { connection: "close" });
          return this.#deny(row, "deny:upload", 413);
        }
        release = await this.#bodies.take(declared ?? FIXED_BODY_MAX);
        // forcedBody is the one reference to the body: it is dropped once the body is sent (below), before the budget
        // is given back, while the response may stream for minutes
        forcedBody = await readModelBody(req, this.#reasoning);
        if (forcedBody === "too-large") {
          this.#modelError(res, 413, "request_too_large", `egress: a model request body is limited to ${FIXED_BODY_MAX} bytes`);
          return this.#deny(row, "deny:upload", 413);
        }
        if (forcedBody === "invalid") {
          this.#modelError(res, 400, "invalid_request_error", "egress: a model request body must be a JSON object");
          return this.#deny(row, "deny:body", 400);
        }
        const model = forcedBody.model;
        row.model = typeof model === "string" && model ? model.slice(0, 200) : null;
        const refusal = await this.#modelRefusal(model);
        if (refusal) {
          this.#modelError(res, refusal.status, refusal.type, refusal.message);
          return this.#deny(row, refusal.decision, refusal.status);
        }
      }
      // the model upstream is outside: no Aviary-* header (staff identity) reaches it; tools gets the verified identity
      let headers: OutgoingHttpHeaders;
      if (isModel) {
        headers = forwardHeaders(req.headers, CALLER_AUTH);
        for (const k of Object.keys(headers)) if (k.startsWith("aviary-")) delete headers[k];
      } else headers = this.#identityHeaders(req.headers, caller, null);
      headers = { ...headers, ...injected, host: up.host, "accept-encoding": "identity" };
      if (typeof forcedBody === "object" && forcedBody) {
        delete headers["transfer-encoding"];
        headers["content-length"] = String(forcedBody.length);
        headers["content-type"] = "application/json";
      }
      const target = `${up.pathname.replace(/\/$/, "")}${rest}${query}`;
      const sniff = new CostSniffer();
      let bytesUp = 0;
      let bytesDown = 0;
      await new Promise<void>((resolve) => {
        let logged = false;
        // one log row per call, whichever way it ends
        const finish = (status: number | null) => {
          if (logged) return;
          logged = true;
          const cost = sniff.cost();
          this.log.add({ ...row, status, bytes_up: bytesUp, bytes_down: bytesDown, ms: Date.now() - t0, cost_usd: cost });
          // a model call's cost counts toward the budget at once (Claude Code's next call is usually < 1 s away)
          if (isModel) this.#o.budget?.noteCost(cost);
          resolve();
        };
        const mk = up.protocol === "https:" ? httpsRequest : httpRequest;
        const out = mk({ protocol: up.protocol, hostname: up.hostname, port: up.port || undefined, method: req.method, path: target, headers }, (ures) => {
          sniff.contentType = String(ures.headers["content-type"] || "");
          this.#head(res, ures.statusCode ?? 502, stripHop(ures.headers));
          res.flushHeaders();
          ures.on("data", (c: Buffer) => {
            bytesDown += c.length;
            sniff.push(c);
            res.write(c);
          });
          ures.on("end", () => res.end());
          ures.on("error", () => res.destroy());
          res.on("close", () => {
            if (!ures.complete) ures.destroy();
            finish(ures.statusCode ?? null);
          });
        });
        if (!isModel) {
          const ms = (route.timeoutSeconds ?? 0) * 1000 || TOOLS_TIMEOUT_MS;
          out.setTimeout(ms, () => out.destroy(new Error("upstream timeout")));
        }
        out.on("error", (e) => {
          logError("egress route upstream", e, { route: route.name, request: caller.request, conversation: caller.claims.conversationId, person_key: personKey(caller.claims.person) });
          if (!res.headersSent) {
            if (/timeout/.test(e.message)) this.#json(res, 504, "upstream_timeout", `${route.name} did not answer in time`);
            else this.#json(res, 502, "upstream_unreachable", `${route.name} could not be reached`);
          } else res.destroy();
          finish(res.statusCode);
        });
        res.on("close", () => {
          if (!out.destroyed && !res.writableFinished) out.destroy();
          // the client went away before an answer (the answer's own close handler logs first when there is one)
          queueMicrotask(() => finish(res.headersSent ? res.statusCode : null));
        });
        if (typeof forcedBody === "object" && forcedBody) {
          bytesUp = forcedBody.length;
          for (const part of forcedBody.parts) out.write(part);
          forcedBody = null;
          out.on("finish", () => release?.());
          out.end();
        } else {
          req.on("data", (c: Buffer) => (bytesUp += c.length));
          req.pipe(out);
        }
      });
    } finally {
      release?.();
    }
  }

  /** A model call this deployment doesn't make: a model outside the configured ids, or the budget is used up. */
  async #modelRefusal(model: unknown): Promise<{ status: number; type: string; message: string; decision: string } | null> {
    // a client may send the id without its provider prefix ("claude-sonnet-5.5" for "anthropic/claude-sonnet-5.5")
    const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
    const ok = typeof model === "string" && this.#models.some((m) => m === model || bare(m) === model);
    if (model !== undefined && !ok) {
      return { status: 400, type: "invalid_request_error", message: `egress: model ${String(model)} is not offered here`, decision: "deny:model" };
    }
    const b = this.#o.budget ? await this.#o.budget.status() : null;
    if (b?.exhausted) return { status: 402, type: BUDGET_ERROR_TYPE, message: budgetMessage(this.#o.config.product.locale, b.limitUsd, b.ownerName), decision: "deny:budget" };
    return null;
  }

  // ---- the internet ---------------------------------------------------------------------------------------------

  /** Port, allowlist and address checks; null after it has answered the caller with the refusal. */
  async #admit(host: string, port: number, deny: (status: number, msg: string) => void, row: EgressRow): Promise<string[] | null> {
    if (!this.#ports.has(port)) {
      deny(403, `egress: port ${port} is not allowed (only ${[...this.#ports].join(", ")}).\n`);
      this.#deny(row, "deny:port", 403);
      return null;
    }
    const verdict = this.#o.policy.check(host);
    if (!verdict.allow) {
      deny(403, denyText(normHost(host)));
      this.#deny(row, verdict.reason, 403);
      return null;
    }
    try {
      return await this.#guard.resolve(host, this.#resolve);
    } catch (e) {
      if (e instanceof DnsRefusal) {
        deny(403, `egress: ${e.message}; the sandbox cannot reach it.\n`);
        this.#deny(row, e.reason, 403);
      } else {
        const why = e instanceof GuardError ? e.message : `cannot resolve ${normHost(host)}`;
        deny(403, `egress: ${why}; only public internet addresses can be reached.\n`);
        this.#deny(row, "deny:address", 403);
      }
      return null;
    }
  }

  async #onConnect(req: IncomingMessage, client: Duplex, head: Buffer): Promise<void> {
    const t0 = Date.now();
    this.#sockets.add(client);
    client.on("close", () => this.#sockets.delete(client));
    client.on("error", () => {});
    const m = /^\[?([^\]]+?)\]?:(\d+)$/.exec(req.url || "");
    const host = m ? m[1]! : "";
    const port = m ? Number(m[2]) : 0;
    const row: EgressRow = { kind: "connect", method: "CONNECT", host: normHost(host) || "-", port, decision: "allow" };
    const deny = (status: number, msg: string, extra: Record<string, string> = {}) => {
      const hdrs = Object.entries({ "content-type": "text/plain; charset=utf-8", "content-length": String(Buffer.byteLength(msg)), connection: "close", ...extra })
        .map(([k, v]) => `${k}: ${v}\r\n`)
        .join("");
      client.end(`HTTP/1.1 ${status} ${status === 407 ? "Proxy Authentication Required" : "Forbidden"}\r\n${hdrs}\r\n${msg}`);
    };
    if (!m) return deny(400, "egress: bad CONNECT target\n");
    const w = this.#track("tunnel", () => client.destroy());
    client.on("close", () => this.#done(w));
    const tok = presentedToken(req.headers, false);
    if (tok === "missing") {
      this.#deny(row, "deny:auth", 407);
      return deny(407, "egress: proxy authentication required\n", { "proxy-authenticate": PROXY_REALM });
    }
    const v = tok === "malformed" ? ({ error: "malformed" } as const) : await this.#o.tokens.verify(tok, req.socket.remoteAddress);
    if ("error" in v) {
      this.#deny(row, "deny:auth", 403);
      return deny(403, "egress: unknown or expired sandbox token\n");
    }
    row.sandbox_id = v.claims.sandboxId;
    row.conversation_id = v.claims.conversationId;
    row.person = v.claims.person;
    const addrs = await this.#admit(host, port, deny, row);
    if (!addrs) return;
    const upstream = await dial(addrs, port).catch(() => null);
    if (!upstream) {
      deny(502, `egress: could not connect to ${normHost(host)}:${port}\n`);
      this.log.add({ ...row, decision: "allow", status: 502, ms: Date.now() - t0 });
      return;
    }
    w.cut = () => {
      client.destroy();
      upstream.destroy();
    };
    this.counters.tunnels++;
    let up = head.length;
    let down = 0;
    client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
    if (head.length) upstream.write(head);
    let capped = false;
    // the upload cap counts only internet traffic: this tunnel's client-to-upstream bytes (3.11.4)
    client.on("data", (c: Buffer) => {
      up += c.length;
      this.counters.bytesUp += c.length;
      if (up > this.#maxUp && !capped) {
        capped = true;
        this.counters.denied++;
        client.destroy();
      }
    });
    upstream.on("data", (c: Buffer) => {
      down += c.length;
      this.counters.bytesDown += c.length;
    });
    client.pipe(upstream);
    upstream.pipe(client);
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      client.destroy();
      upstream.destroy();
      this.#done(w);
      this.log.add({ ...row, ...(capped ? { decision: "deny:upload" } : {}), status: 200, bytes_up: up, bytes_down: down, ms: Date.now() - t0 });
    };
    upstream.on("close", finish);
    upstream.on("error", finish);
    client.on("close", finish);
  }

  async #forward(req: IncomingMessage, res: ServerResponse, t: Target, caller: Caller): Promise<void> {
    const t0 = Date.now();
    const host = t.host!;
    const port = t.port;
    const row: EgressRow = {
      kind: "http",
      method: req.method ?? "GET",
      host: normHost(host),
      port,
      path: t.pathq,
      decision: "allow",
      sandbox_id: caller.claims.sandboxId,
      conversation_id: caller.claims.conversationId,
      person: caller.claims.person,
      request: caller.request,
    };
    const deny = (status: number, msg: string) => this.#text(res, status, msg);
    const addrs = await this.#admit(host, port, deny, row);
    if (!addrs) return;
    const cacheable = req.method === "GET" && this.#apt.has(normHost(host));
    const url = `http://${t.authority}${t.pathq}`;
    if (cacheable) {
      const [p] = splitQuery(t.pathq);
      const immutable = /\.(deb|udeb|dsc|tar\.(gz|xz|bz2))$/.test(p) || p.includes("/by-hash/");
      const hit = this.cache.get(url, immutable ? Infinity : INDEX_MAX_AGE);
      if (hit) {
        this.#serveCached(res, hit.meta, hit.path, hit.size);
        this.log.add({ ...row, decision: "allow:cache", status: hit.meta.status, bytes_down: hit.size, ms: Date.now() - t0 });
        return;
      }
    }
    const headers = forwardHeaders(req.headers, new Set(["proxy-authorization"]));
    headers.host = t.authority;
    let up = 0;
    let down = 0;
    let capped = false;
    let status: number | null = null;
    await new Promise<void>((resolve) => {
      let finished = false;
      // one log line per request, whichever way it ends
      const finish = (extra: Partial<EgressRow> = {}) => {
        if (finished) return;
        finished = true;
        this.log.add({ ...row, status, bytes_up: up, bytes_down: down, ms: Date.now() - t0, ...extra, ...(capped ? { decision: "deny:upload" } : {}) });
        resolve();
      };
      const out = httpRequest({ host: addrs[0], port, method: req.method, path: t.pathq, headers, setHost: false }, (ures) => {
        status = ures.statusCode ?? null;
        this.#head(res, ures.statusCode ?? 502, stripHop(ures.headers));
        const wr = cacheable && ures.statusCode === 200
          ? this.cache.writer({ url, status: 200, contentType: String(ures.headers["content-type"] || "application/octet-stream"), storedAt: Date.now() })
          : null;
        ures.on("data", (c: Buffer) => {
          down += c.length;
          this.counters.bytesDown += c.length;
          wr?.write(c);
        });
        ures.on("end", () => void wr?.end());
        ures.on("error", () => wr?.abort());
        ures.pipe(res);
        res.on("close", () => {
          if (!ures.complete) wr?.abort();
        });
      });
      res.on("close", () => finish());
      out.on("error", () => {
        if (capped) return finish();
        if (!res.headersSent) this.#text(res, 502, `egress: could not reach ${t.authority}\n`);
        else res.destroy();
        finish({ status: 502 });
      });
      // the upload cap: this request's client-to-upstream bytes (internet traffic only, 3.11.4)
      req.on("data", (c: Buffer) => {
        up += c.length;
        this.counters.bytesUp += c.length;
        if (up > this.#maxUp && !capped) {
          capped = true;
          this.counters.denied++;
          out.destroy();
          res.destroy();
        }
      });
      req.pipe(out);
    });
  }

  // ---- package mirror (a self-host path) -------------------------------------------------------------------------

  async #mirror(req: IncomingMessage, res: ServerResponse, path: string, caller: Caller): Promise<void> {
    const t0 = Date.now();
    const row: EgressRow = {
      kind: "mirror",
      method: req.method ?? "GET",
      host: "mirror",
      path,
      decision: "allow",
      sandbox_id: caller.claims.sandboxId,
      conversation_id: caller.claims.conversationId,
      person: caller.claims.person,
      request: caller.request,
    };
    if (req.method !== "GET" && req.method !== "HEAD") return this.#text(res, 405, "egress: the mirror is read-only\n");
    const [pathOnly, q] = splitQuery(path);
    const selfBase = `http://${req.headers.host || "egress"}`;
    let upstream: string;
    let immutable = false;
    let rewrite: ((body: string) => string) | null = null;
    const accept = String(req.headers.accept || "");
    let m: RegExpExecArray | null;
    if ((m = /^\/mirror\/pypi\/simple(\/.*)?$/.exec(pathOnly))) {
      row.host = new URL(this.#up.pypi).hostname;
      upstream = `${this.#up.pypi}/simple${m[1] ?? "/"}${q}`;
      rewrite = (b) => b.split(`${this.#up.pypiFiles}/`).join("/mirror/pypi/files/");
    } else if ((m = /^\/mirror\/pypi\/files\/(.+)$/.exec(pathOnly))) {
      row.host = new URL(this.#up.pypiFiles).hostname;
      upstream = `${this.#up.pypiFiles}/${m[1]}${q}`;
      immutable = true;
    } else if ((m = /^\/mirror\/npm\/(.+)$/.exec(pathOnly))) {
      row.host = new URL(this.#up.npm).hostname;
      upstream = `${this.#up.npm}/${m[1]}${q}`;
      immutable = m[1]!.includes("/-/");
      if (!immutable) rewrite = (b) => b.split(`${this.#up.npm}/`).join(`${selfBase}/mirror/npm/`);
    } else {
      return this.#text(res, 404, "egress: unknown mirror path (use /mirror/pypi/simple/ or /mirror/npm/)\n");
    }
    const key = immutable ? upstream : `${upstream}|${accept}|${selfBase}`;
    const hit = this.cache.get(key, immutable ? Infinity : INDEX_MAX_AGE);
    if (hit) {
      this.#serveCached(res, hit.meta, hit.path, hit.size, req.method === "HEAD");
      this.log.add({ ...row, decision: "allow:cache", status: hit.meta.status, bytes_down: hit.size, ms: Date.now() - t0 });
      return;
    }
    // identity: fetch would decompress a gzip body (files.pythonhosted.org gzips .metadata) while the upstream
    // content-length counts the compressed bytes, and pip would get a truncated file
    const ures = await fetch(upstream, { headers: { "accept-encoding": "identity", ...(accept ? { accept } : {}) }, redirect: "follow" }).catch(() => null);
    if (!ures) {
      this.#text(res, 502, `egress: mirror upstream ${row.host} unreachable\n`);
      return this.log.add({ ...row, status: 502, ms: Date.now() - t0 });
    }
    const contentType = ures.headers.get("content-type") || "application/octet-stream";
    if (rewrite) {
      const body = Buffer.from(ures.ok ? rewrite(await ures.text()) : await ures.text());
      if (ures.ok) this.cache.putBuffer({ url: key, status: ures.status, contentType, storedAt: Date.now() }, body);
      this.#head(res, ures.status, { "content-type": contentType, "content-length": String(body.length) });
      res.end(req.method === "HEAD" ? undefined : body);
      return this.log.add({ ...row, status: ures.status, bytes_down: body.length, ms: Date.now() - t0 });
    }
    const len = ures.headers.get("content-encoding") ? null : ures.headers.get("content-length");
    this.#head(res, ures.status, { "content-type": contentType, ...(len ? { "content-length": len } : {}) });
    if (!ures.body || req.method === "HEAD") {
      res.end();
      return this.log.add({ ...row, status: ures.status, ms: Date.now() - t0 });
    }
    const w = ures.ok ? this.cache.writer({ url: key, status: ures.status, contentType, storedAt: Date.now() }) : null;
    let down = 0;
    const body = Readable.fromWeb(ures.body as any);
    body.on("data", (c: Buffer) => {
      down += c.length;
      w?.write(c);
    });
    await new Promise<void>((resolve) => {
      body.on("end", () => void w?.end());
      body.on("error", () => {
        w?.abort();
        res.destroy();
      });
      res.on("close", () => {
        if (!body.readableEnded) w?.abort();
        resolve();
      });
      body.pipe(res);
    });
    this.log.add({ ...row, status: ures.status, bytes_down: down, ms: Date.now() - t0 });
  }

  #serveCached(res: ServerResponse, meta: { status: number; contentType: string }, path: string, size: number, head = false): void {
    this.#head(res, meta.status, { "content-type": meta.contentType, "content-length": String(size), "x-egress-cache": "hit" });
    if (head) return void res.end();
    createReadStream(path).on("error", () => res.destroy()).pipe(res);
  }

  // ---- lifecycle --------------------------------------------------------------------------------------------------

  listen(port: number, host = "::"): Promise<number> {
    return new Promise((resolve) => this.server.listen(port, host, () => resolve((this.server.address() as { port: number }).port)));
  }

  /** 3.11.6: stop accepting connections, close idle kept-alive ones, answer everything after this with
   * `Connection: close`, let in-flight work finish within its kind's bound, cut and count what is left. */
  async drain(): Promise<Record<DrainKind, number>> {
    this.draining = true;
    const bounds = { ...DRAIN_BOUNDS_MS, ...this.#o.drainBounds };
    const cut: Record<DrainKind, number> = { model: 0, tunnel: 0, fixed: 0, tools: 0 };
    this.server.close();
    this.server.closeIdleConnections();
    const t0 = Date.now();
    await new Promise<void>((resolve) => {
      const tick = () => {
        this.server.closeIdleConnections();
        const elapsed = Date.now() - t0;
        for (const w of [...this.#work]) {
          if (elapsed >= bounds[w.kind]) {
            this.#work.delete(w);
            cut[w.kind]++;
            this.drainCut[w.kind]++;
            w.cut();
          }
        }
        if (this.#work.size === 0) return resolve();
        setTimeout(tick, 200);
      };
      tick();
    });
    for (const s of this.#sockets) s.destroy();
    this.#agent.destroy();
    return cut;
  }

  /** Prometheus lines of the proxy's own counters. */
  metrics(): string[] {
    const c = this.counters;
    const lines = [
      `egress_requests_total ${c.requests}`,
      `egress_denied_total ${c.denied}`,
      `egress_tunnels_total ${c.tunnels}`,
      `egress_bytes_up_total ${c.bytesUp}`,
      `egress_bytes_down_total ${c.bytesDown}`,
      `egress_open_sockets ${this.openSockets}`,
      `egress_cache_bytes ${this.cache.totalBytes}`,
      `egress_cache_hits_total ${this.cache.hits}`,
      `egress_cache_misses_total ${this.cache.misses}`,
      `egress_log_rows_total ${this.log.written}`,
      `egress_log_buffered ${this.log.buffered}`,
    ];
    for (const k of ["connect", "http", "mirror", "model", "tools"]) lines.push(`egress_log_dropped_total{kind="${k}"} ${this.log.dropped[k] ?? 0}`);
    for (const [k, n] of this.fixedRequests) {
      const [route, status] = k.split(" ");
      lines.push(`egress_fixed_requests_total{route="${route}",status="${status}"} ${n}`);
    }
    for (const [k, n] of this.fixedBytes) {
      const [route, dir] = k.split(" ");
      lines.push(`egress_fixed_bytes_total{route="${route}",dir="${dir}"} ${n}`);
    }
    const inflight = this.inFlight();
    for (const k of DRAIN_KINDS) lines.push(`egress_in_flight{kind="${k}"} ${inflight[k]}`);
    for (const k of DRAIN_KINDS) lines.push(`egress_drain_cut_total{kind="${k}"} ${this.drainCut[k]}`);
    return lines;
  }
}

/** A model request's body read and rewritten (modelbody.ts), in a frame of its own so that nothing but the result
 * keeps the raw bytes alive. */
async function readModelBody(req: IncomingMessage, plan: ReasoningPlan | undefined): Promise<ModelBody | "too-large" | "invalid"> {
  const raw = await readBody(req, FIXED_BODY_MAX);
  if (raw === null) return "too-large";
  return forceZdr(raw, plan) ?? "invalid";
}

/** The request's Content-Length, or null when it has none (chunked). */
function declaredLength(req: IncomingMessage): number | null {
  const v = req.headers["content-length"];
  if (v === undefined) return null;
  const n = Number(v);
  return Number.isSafeInteger(n) && n >= 0 ? n : null;
}

/** The whole request body, or null when it exceeds max bytes; read into one Buffer of the declared length when there is
 * one (no copy of the chunks). Rejects when the client goes away first. */
function readBody(req: IncomingMessage, max: number): Promise<Buffer | null> {
  return new Promise((resolve, reject) => {
    if (req.destroyed) return reject(new Error("egress: the client closed the request"));
    const declared = declaredLength(req);
    const fixed = declared !== null && declared <= max ? Buffer.allocUnsafe(declared) : null;
    const parts: Buffer[] = [];
    let n = 0;
    req.on("data", (c: Buffer) => {
      if (fixed) {
        if (n + c.length <= fixed.length) c.copy(fixed, n);
      } else if (n + c.length <= max) parts.push(c);
      n += c.length;
    });
    req.on("end", () => resolve(n > max || (fixed !== null && n !== fixed.length) ? null : (fixed ?? Buffer.concat(parts))));
    req.on("close", () => {
      if (!req.complete) reject(new Error("egress: the client closed the request"));
    });
    req.on("error", reject);
  });
}

/** Collects usage.cost from an Anthropic-format response: a JSON body, or the SSE events of a streamed one. */
export class CostSniffer {
  contentType = "";
  #buf = "";
  #json: Buffer[] = [];
  #jsonBytes = 0;
  #cost: number | null = null;
  push(c: Buffer): void {
    if (this.contentType.includes("text/event-stream")) {
      this.#buf += c.toString("utf8");
      let i: number;
      while ((i = this.#buf.indexOf("\n")) >= 0) {
        const line = this.#buf.slice(0, i).trim();
        this.#buf = this.#buf.slice(i + 1);
        if (line.startsWith("data:")) this.#take(line.slice(5).trim());
      }
    } else if (this.contentType.includes("json") && this.#jsonBytes < 20 * 1024 * 1024) {
      this.#json.push(c);
      this.#jsonBytes += c.length;
    }
  }
  #take(s: string): void {
    if (!s.startsWith("{")) return;
    try {
      const o = JSON.parse(s);
      const cost = o?.usage?.cost ?? o?.message?.usage?.cost;
      if (typeof cost === "number") this.#cost = cost;
    } catch {
      /* not JSON */
    }
  }
  cost(): number | null {
    if (this.#json.length) {
      this.#take(Buffer.concat(this.#json).toString("utf8"));
      this.#json = [];
    }
    return this.#cost;
  }
}

function splitQuery(pathq: string): [string, string] {
  const i = pathq.indexOf("?");
  return i < 0 ? [pathq, ""] : [pathq.slice(0, i), pathq.slice(i)];
}

function stripHop(h: IncomingMessage["headers"]): OutgoingHttpHeaders {
  const out: OutgoingHttpHeaders = {};
  for (const [k, v] of Object.entries(h)) if (!HOP.has(k) && v !== undefined) out[k] = v;
  return out;
}

function forwardHeaders(h: IncomingMessage["headers"], drop: Set<string>): OutgoingHttpHeaders {
  const out: OutgoingHttpHeaders = {};
  const connectionNamed = new Set(String(h.connection || "").toLowerCase().split(",").map((s) => s.trim()));
  for (const [k, v] of Object.entries(h)) {
    if (HOP.has(k) || drop.has(k) || connectionNamed.has(k) || v === undefined) continue;
    out[k] = v;
  }
  return out;
}

function dial(addrs: string[], port: number): Promise<Socket> {
  return new Promise((resolve, reject) => {
    let i = 0;
    const next = () => {
      const a = addrs[i++];
      if (a === undefined) return reject(new Error("no address answered"));
      const s = netConnect({ host: a, port, timeout: 10_000 });
      s.once("connect", () => {
        s.setTimeout(0);
        resolve(s);
      });
      s.once("timeout", () => s.destroy(new Error("timeout")));
      s.once("error", () => next());
    };
    next();
  });
}

export function safeUrl(u: string | undefined): string {
  return (u || "").replace(/\/\/[^@/]*@/, "//***@").slice(0, 300);
}
