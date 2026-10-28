// The egress server (docs/CHAT.md, "egress"): the forward proxy every sandbox must use (CONNECT and absolute-URI
// HTTP, token identity, the sandbox allowlist with threat-intel DNS, public addresses only, an upload cap per
// connection), the package mirror and the credential-injecting routes. Every request ends up in egress_log.
import { createServer, request as httpRequest, type IncomingMessage, type OutgoingHttpHeaders, type Server, type ServerResponse } from "node:http";
import { request as httpsRequest } from "node:https";
import { connect as netConnect, type Socket } from "node:net";
import { createReadStream } from "node:fs";
import { Readable } from "node:stream";
import type { Duplex } from "node:stream";
import { createWebSocketStream, WebSocketServer, type WebSocket } from "ws";
import type { Db } from "../common/db.ts";
import { expandEnv, type EgressRoute, type ReasoningPlan } from "../common/config.ts";
import { logError } from "../common/log.ts";
import { BUDGET_ERROR_TYPE, budgetMessage, type BudgetStatus } from "../common/budget.ts";
import { DiskCache } from "./cache.ts";
import { EgressLog, type EgressRow } from "./egresslog.ts";
import { Identities, tokenFrom, type SandboxId } from "./identity.ts";
import { HostPolicy } from "./allowlist.ts";
import { AddressGuard, GuardError, normHost } from "./netguard.ts";
import { DnsRefusal, ThreatResolver } from "./threatdns.ts";
import { BodyBudget, forceZdr, type ModelBody } from "./modelbody.ts";

export interface EgressOptions {
  db: Db;
  /** Which hosts sandboxes may reach (allowlist.ts). */
  policy: HostPolicy;
  /** Bytes a sandbox may send through one tunnel or request (default 8 MiB); more cuts the connection. */
  maxUploadBytes?: number;
  routes: EgressRoute[];
  cacheDir: string;
  cacheBytes: number;
  /** Names this proxy answers to itself (an absolute-URI request to one of them is a mirror or route request). */
  selfHosts: string[];
  guard?: AddressGuard;
  allowedPorts?: number[];
  /** Name resolution for sandbox connections (default: ThreatResolver, malware-blocking DNS); may throw DnsRefusal. */
  resolver?: (host: string) => Promise<string[]>;
  upstreams?: { pypi: string; pypiFiles: string; npm: string };
  /** Hosts whose plain-HTTP GETs are cached (apt). */
  aptHosts?: string[];
  env?: Record<string, string | undefined>;
  identities?: Identities;
  /** The model route's allowed model ids (config models + titleModel); absent = any. */
  models?: string[];
  /** Thinking per model (common/config.ts reasoningPlan); absent = requests pass as sent. */
  reasoning?: ReasoningPlan;
  /** The shared budget (common/budget.ts); a model call is refused once it is used up. */
  budget?: () => Promise<BudgetStatus | null>;
  /** product.locale: the language of the budget message */
  locale?: string;
}

const HOP = new Set(["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade"]);
const CALLER_AUTH = new Set(["authorization", "x-api-key", "cookie", "host"]);
/** Identity headers only egress sets (route `identity`); a caller's own copies never pass. */
const IDENTITY = new Set(["x-jarvis-conversation", "x-jarvis-sandbox"]);
const INDEX_MAX_AGE = 5 * 60_000;

/** What a sandbox (and so the agent) reads when a host is refused: plain, final, no one to ask. */
export const denyText = (host: string): string =>
  `egress: ${host} is not on the sandbox's internet allowlist, and it cannot be enabled from the chat. The sandbox reaches ` +
  "package registries, code hosts and widely used public sites only. To read a web page, use the web_fetch and " +
  "web_search tools instead of the sandbox.\n";

export class Egress {
  readonly server: Server;
  readonly log: EgressLog;
  readonly cache: DiskCache;
  readonly ids: Identities;
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
  readonly counters = { requests: 0, denied: 0, tunnels: 0, bytesUp: 0, bytesDown: 0 };
  draining = false;

  constructor(o: EgressOptions) {
    this.#o = o;
    this.#guard = o.guard ?? new AddressGuard();
    const threat = o.resolver ? null : new ThreatResolver();
    this.#resolve = o.resolver ?? ((h) => threat!.resolve(h));
    this.#maxUp = o.maxUploadBytes ?? 8 * 1024 * 1024;
    this.#bodies = new BodyBudget(MODEL_BODY_MAX);
    this.#ports = new Set(o.allowedPorts ?? [80, 443]);
    this.#up = o.upstreams ?? { pypi: "https://pypi.org", pypiFiles: "https://files.pythonhosted.org", npm: "https://registry.npmjs.org" };
    // 127.0.0.1/localhost too: a hosted sandbox's proxy is its local end of the wire (sandbox/agent.mjs), so pip and
    // npm name that in their mirror requests (a forwarded request to a loopback address is refused anyway)
    this.#self = new Set([...o.selfHosts, "127.0.0.1", "localhost"].map(normHost));
    this.#apt = new Set((o.aptHosts ?? ["deb.debian.org", "security.debian.org"]).map(normHost));
    this.log = new EgressLog(o.db);
    this.cache = new DiskCache(o.cacheDir, o.cacheBytes);
    this.ids = o.identities ?? new Identities(o.db);
    this.server = createServer((req, res) => {
      this.counters.requests++;
      this.#onRequest(req, res).catch((e) => {
        logError("egress request", e, { url: safeUrl(req.url) });
        if (!res.headersSent) text(res, 502, "egress: upstream error\n");
        else res.destroy();
      });
    });
    this.server.on("connection", (s: Socket) => {
      this.#sockets.add(s);
      s.on("close", () => this.#sockets.delete(s));
    });
    this.server.on("upgrade", (req: IncomingMessage, sock: Duplex, head: Buffer) => {
      this.#onUpgrade(req, sock, head).catch((e) => {
        logError("egress wire", e);
        sock.destroy();
      });
    });
    this.server.on("connect", (req: IncomingMessage, sock: Duplex, head: Buffer) => {
      this.counters.requests++;
      this.#onConnect(req, sock, head).catch((e) => {
        logError("egress connect", e);
        sock.destroy();
      });
    });
  }

  // ---- the wire: the proxy over a WebSocket ---------------------------------------------------------------

  readonly #wss = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: 4 * 1024 * 1024 });

  /**
   * `GET /wire` (WebSocket, `Authorization: Bearer <egress token>`): a sandbox that can't reach port 3128 (a hosted
   * provider's, which reaches egress only through the Cloudflare tunnel's HTTPS edge) opens one WebSocket per TCP
   * connection of its local proxy (sandbox/agent.mjs). The WebSocket's binary messages are that connection's bytes,
   * fed into this same proxy server as a new connection, so CONNECT, plain HTTP, the mirror and every check and log
   * line work exactly as on 3128 (each request still needs its Proxy-Authorization). Pings every 30 s keep idle
   * tunnels open through the edge.
   */
  async #onUpgrade(req: IncomingMessage, sock: Duplex, head: Buffer): Promise<void> {
    sock.on("error", () => {});
    const refuse = (status: number, msg: string): void => void sock.end(`HTTP/1.1 ${status} ${status === 401 ? "Unauthorized" : "Refused"}\r\ncontent-type: text/plain\r\nconnection: close\r\ncontent-length: ${Buffer.byteLength(msg)}\r\n\r\n${msg}`);
    const path = (req.url || "").split("?")[0];
    if (path !== "/wire") return refuse(404, "egress: not found\n");
    if (this.draining) return refuse(503, "egress: restarting, try again\n");
    const sb = await this.ids.sandbox(tokenFrom(req.headers, "authorization"));
    if (!sb || sb.ip) {
      this.#deny({ kind: "connect", method: "WIRE", host: "-", decision: "deny:auth" }, "deny:auth", 401);
      return refuse(401, "egress: unknown or expired sandbox token\n");
    }
    this.#wss.handleUpgrade(req, sock, head, (ws: WebSocket) => {
      const stream = createWebSocketStream(ws);
      stream.on("error", () => stream.destroy());
      const ping = setInterval(() => ws.readyState === ws.OPEN && ws.ping(), 30_000);
      ws.on("close", () => clearInterval(ping));
      this.server.emit("connection", stream);
    });
  }

  /** The sandbox a token belongs to; a "vm" sandbox's token only counts from that sandbox's own address. */
  async #sandbox(req: IncomingMessage, token: string): Promise<SandboxId | null> {
    const sb = await this.ids.sandbox(token);
    if (!sb) return null;
    if (sb.ip && (req.socket.remoteAddress ?? "").replace(/^::ffff:/, "") !== sb.ip) return null;
    return sb;
  }

  get openSockets(): number {
    return this.#sockets.size;
  }

  // ---- routing ---------------------------------------------------------------------------------------------

  async #onRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const raw = req.url || "/";
    if (/^https?:\/\//i.test(raw)) {
      const u = new URL(raw);
      if (this.#self.has(normHost(u.hostname))) return this.#local(req, res, u.pathname + u.search);
      return this.#forward(req, res, u);
    }
    return this.#local(req, res, raw);
  }

  async #local(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    if (path.startsWith("/mirror/")) return this.#mirror(req, res, path);
    if (path.startsWith("/x/")) return this.#route(req, res, path);
    text(res, 404, "egress: not found\n");
  }

  // ---- forward proxy -------------------------------------------------------------------------------------

  /** Identity + allowlist + port + address checks; null after it has answered the caller with the refusal. */
  async #admit(
    req: IncomingMessage,
    host: string,
    port: number,
    deny: (status: number, msg: string, extra?: Record<string, string>) => void,
    row: EgressRow,
  ): Promise<{ sb: SandboxId; addrs: string[] } | null> {
    const sb = await this.#sandbox(req, tokenFrom(req.headers, "proxy-authorization"));
    if (!sb) {
      deny(407, "egress: proxy authentication required\n", { "proxy-authenticate": 'Basic realm="jarvis-egress"' });
      this.#deny(row, "deny:auth", 407);
      return null;
    }
    row.sandbox_id = sb.sandboxId;
    row.conversation_id = sb.conversationId;
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
      return { sb, addrs: await this.#guard.resolve(host, this.#resolve) };
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

  #deny(row: EgressRow, decision: string, status: number): void {
    this.counters.denied++;
    this.log.add({ ...row, decision, status });
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
    if (this.draining) return deny(503, "egress: restarting, try again\n");
    const ok = await this.#admit(req, host, port, deny, row);
    if (!ok) return;
    const upstream = await dial(ok.addrs, port).catch(() => null);
    if (!upstream) {
      deny(502, `egress: could not connect to ${normHost(host)}:${port}\n`);
      this.log.add({ ...row, decision: "allow", status: 502, ms: Date.now() - t0 });
      return;
    }
    this.counters.tunnels++;
    let up = head.length;
    let down = 0;
    client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
    if (head.length) upstream.write(head);
    let capped = false;
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
      this.log.add({ ...row, ...(capped ? { decision: "deny:upload" } : {}), status: 200, bytes_up: up, bytes_down: down, ms: Date.now() - t0 });
    };
    upstream.on("close", finish);
    upstream.on("error", finish);
    client.on("close", finish);
  }

  async #forward(req: IncomingMessage, res: ServerResponse, u: URL): Promise<void> {
    const t0 = Date.now();
    const port = u.port ? Number(u.port) : 80;
    const row: EgressRow = { kind: "http", method: req.method ?? "GET", host: normHost(u.hostname), port, path: u.pathname + u.search, decision: "allow" };
    if (u.protocol !== "http:") return text(res, 400, "egress: use CONNECT for https\n");
    const deny = (status: number, msg: string, extra: Record<string, string> = {}) => text(res, status, msg, extra);
    const ok = await this.#admit(req, u.hostname, port, deny, row);
    if (!ok) return;
    const cacheable = req.method === "GET" && this.#apt.has(normHost(u.hostname));
    const url = u.toString();
    if (cacheable) {
      const immutable = /\.(deb|udeb|dsc|tar\.(gz|xz|bz2))$/.test(u.pathname) || u.pathname.includes("/by-hash/");
      const hit = this.cache.get(url, immutable ? Infinity : INDEX_MAX_AGE);
      if (hit) {
        serveCached(res, hit.meta, hit.path, hit.size);
        this.log.add({ ...row, decision: "allow:cache", status: hit.meta.status, bytes_down: hit.size, ms: Date.now() - t0 });
        return;
      }
    }
    const headers = forwardHeaders(req.headers, new Set(["proxy-authorization"]));
    headers.host = u.host;
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
      const out = httpRequest(
        { host: ok.addrs[0], port, method: req.method, path: u.pathname + u.search, headers, setHost: false },
        (ures) => {
          status = ures.statusCode ?? null;
          res.writeHead(ures.statusCode ?? 502, stripHop(ures.headers));
          const w = cacheable && ures.statusCode === 200
            ? this.cache.writer({ url, status: 200, contentType: String(ures.headers["content-type"] || "application/octet-stream"), storedAt: Date.now() })
            : null;
          ures.on("data", (c: Buffer) => {
            down += c.length;
            this.counters.bytesDown += c.length;
            w?.write(c);
          });
          ures.on("end", () => void w?.end());
          ures.on("error", () => w?.abort());
          ures.pipe(res);
          res.on("close", () => {
            if (!ures.complete) w?.abort();
          });
        },
      );
      res.on("close", () => finish());
      out.on("error", () => {
        if (capped) return finish();
        if (!res.headersSent) text(res, 502, `egress: could not reach ${u.host}\n`);
        else res.destroy();
        finish({ status: 502 });
      });
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

  // ---- package mirror ------------------------------------------------------------------------------------

  async #mirror(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    const t0 = Date.now();
    const row: EgressRow = { kind: "mirror", method: req.method ?? "GET", host: "mirror", path, decision: "allow" };
    const sb = await this.#sandbox(req, tokenFrom(req.headers, "authorization") || tokenFrom(req.headers, "proxy-authorization"));
    if (!sb) {
      text(res, 401, "egress: authentication required\n", { "www-authenticate": 'Basic realm="jarvis-mirror"' });
      return this.#deny(row, "deny:auth", 401);
    }
    row.sandbox_id = sb.sandboxId;
    row.conversation_id = sb.conversationId;
    if (req.method !== "GET" && req.method !== "HEAD") return text(res, 405, "egress: the mirror is read-only\n");
    const [pathOnly, query = ""] = path.split("?") as [string, string?];
    const q = query ? `?${query}` : "";
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
      return text(res, 404, "egress: unknown mirror path (use /mirror/pypi/simple/ or /mirror/npm/)\n");
    }
    const key = immutable ? upstream : `${upstream}|${accept}|${selfBase}`;
    const hit = this.cache.get(key, immutable ? Infinity : INDEX_MAX_AGE);
    if (hit) {
      serveCached(res, hit.meta, hit.path, hit.size, req.method === "HEAD");
      this.log.add({ ...row, decision: "allow:cache", status: hit.meta.status, bytes_down: hit.size, ms: Date.now() - t0 });
      return;
    }
    // identity: fetch would decompress a gzip body (files.pythonhosted.org gzips .metadata) while the upstream
    // content-length counts the compressed bytes, and pip would get a truncated file
    const ures = await fetch(upstream, { headers: { "accept-encoding": "identity", ...(accept ? { accept } : {}) }, redirect: "follow" }).catch(() => null);
    if (!ures) {
      text(res, 502, `egress: mirror upstream ${row.host} unreachable\n`);
      return this.log.add({ ...row, status: 502, ms: Date.now() - t0 });
    }
    const contentType = ures.headers.get("content-type") || "application/octet-stream";
    if (rewrite) {
      const body = Buffer.from(ures.ok ? rewrite(await ures.text()) : await ures.text());
      if (ures.ok) this.cache.putBuffer({ url: key, status: ures.status, contentType, storedAt: Date.now() }, body);
      res.writeHead(ures.status, { "content-type": contentType, "content-length": String(body.length) });
      res.end(req.method === "HEAD" ? undefined : body);
      return this.log.add({ ...row, status: ures.status, bytes_down: body.length, ms: Date.now() - t0 });
    }
    const len = ures.headers.get("content-encoding") ? null : ures.headers.get("content-length");
    res.writeHead(ures.status, { "content-type": contentType, ...(len ? { "content-length": len } : {}) });
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

  // ---- credential-injecting routes -----------------------------------------------------------------------

  async #route(req: IncomingMessage, res: ServerResponse, path: string): Promise<void> {
    const t0 = Date.now();
    const m = /^\/x\/([^/?]+)(\/[^?]*)?(\?.*)?$/.exec(path);
    const route = m ? this.#o.routes.find((r) => r.name === m[1]) : undefined;
    if (!m || !route) return text(res, 404, "egress: unknown route\n");
    const rest = m[2] ?? "/";
    const up = new URL(route.upstream);
    const isModel = route.model === true;
    const row: EgressRow = { kind: isModel ? "model" : "inject", method: req.method ?? "GET", host: up.hostname, path: rest, decision: "allow" };
    const sb = await this.#sandbox(req, tokenFrom(req.headers, "authorization") || tokenFrom(req.headers, "proxy-authorization"));
    if (!sb) {
      text(res, 401, "egress: authentication required\n");
      return this.#deny(row, "deny:auth", 401);
    }
    row.sandbox_id = sb.sandboxId;
    row.conversation_id = sb.conversationId;
    if (route.allowPaths?.length && !route.allowPaths.some((p) => rest.startsWith(p))) {
      text(res, 403, `egress: path ${rest} is not allowed on route ${route.name}\n`);
      return this.#deny(row, "deny:path", 403);
    }
    let injected: Record<string, string>;
    try {
      injected = Object.fromEntries(Object.entries(route.inject).map(([k, v]) => [k.toLowerCase(), expandEnv(v, this.#o.env ?? process.env)]));
    } catch (e) {
      logError("egress route inject", e, { route: route.name });
      text(res, 500, `egress: route ${route.name} is misconfigured\n`);
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
        if (declared !== null && declared > MODEL_BODY_MAX) {
          text(res, 413, JSON.stringify({ type: "error", error: { type: "request_too_large", message: `egress: a model request body is limited to ${MODEL_BODY_MAX} bytes` } }), { "content-type": "application/json", connection: "close" });
          return this.#deny(row, "deny:upload", 413);
        }
        release = await this.#bodies.take(declared ?? MODEL_BODY_MAX);
        // forcedBody is the one reference to the body: it is dropped once the body is sent (below), before the budget
        // is given back, while the response may stream for minutes
        forcedBody = await readModelBody(req, this.#o.reasoning);
        if (forcedBody === "too-large") {
          text(res, 413, JSON.stringify({ type: "error", error: { type: "request_too_large", message: `egress: a model request body is limited to ${MODEL_BODY_MAX} bytes` } }), { "content-type": "application/json" });
          return this.#deny(row, "deny:upload", 413);
        }
        if (forcedBody === "invalid") {
          text(res, 400, '{"type":"error","error":{"type":"invalid_request_error","message":"egress: a model request body must be a JSON object"}}', { "content-type": "application/json" });
          return this.#deny(row, "deny:body", 400);
        }
        const model = forcedBody.model;
        row.model = typeof model === "string" && model ? model.slice(0, 200) : null;
        const refusal = await this.#modelRefusal(model);
        if (refusal) {
          text(res, refusal.status, JSON.stringify({ type: "error", error: { type: refusal.type, message: refusal.message } }), { "content-type": "application/json" });
          return this.#deny(row, refusal.decision, refusal.status);
        }
      }
      const passed = route.keepAuth ? new Set([...CALLER_AUTH].filter((h) => h !== "authorization")) : CALLER_AUTH;
      const headers: OutgoingHttpHeaders = { ...forwardHeaders(req.headers, new Set([...passed, ...IDENTITY])), ...injected, host: up.host, "accept-encoding": "identity" };
      if (route.identity) {
        headers["x-jarvis-conversation"] = sb.conversationId ?? "";
        headers["x-jarvis-sandbox"] = sb.sandboxId;
      }
      if (typeof forcedBody === "object" && forcedBody) {
        delete headers["transfer-encoding"];
        headers["content-length"] = String(forcedBody.length);
        headers["content-type"] = "application/json";
      }
      const basePath = up.pathname.replace(/\/$/, "");
      const target = `${basePath}${rest}${m[3] ?? ""}`;
      const sniff = new CostSniffer();
      let bytesUp = 0;
      let bytesDown = 0;
      await new Promise<void>((resolve) => {
        const mk = up.protocol === "https:" ? httpsRequest : httpRequest;
        const out = mk({ protocol: up.protocol, hostname: up.hostname, port: up.port || undefined, method: req.method, path: target, headers }, (ures) => {
          sniff.contentType = String(ures.headers["content-type"] || "");
          res.writeHead(ures.statusCode ?? 502, stripHop(ures.headers));
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
            this.log.add({ ...row, status: ures.statusCode ?? null, bytes_up: bytesUp, bytes_down: bytesDown, ms: Date.now() - t0, cost_usd: sniff.cost() });
            // a model call's cost counts toward the budget at once (Claude Code's next call is usually < 1 s away)
            if (isModel) void this.log.flush();
            resolve();
          });
        });
        out.on("error", (e) => {
          logError("egress route upstream", e, { route: route.name });
          if (!res.headersSent) text(res, 502, `egress: route ${route.name} upstream unreachable\n`);
          else res.destroy();
          this.log.add({ ...row, status: 502, ms: Date.now() - t0 });
          resolve();
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

  /** A model call this instance doesn't make: a model outside config `models`/`titleModel`, or the budget is used up. */
  async #modelRefusal(model: unknown): Promise<{ status: number; type: string; message: string; decision: string } | null> {
    if (this.#o.models?.length) {
      // a client may send the id without its provider prefix ("claude-sonnet-5.5" for "anthropic/claude-sonnet-5.5")
      const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
      const ok = typeof model === "string" && this.#o.models.some((m) => m === model || bare(m) === model);
      if (model !== undefined && !ok) {
        return { status: 400, type: "invalid_request_error", message: `egress: model ${String(model)} is not offered here`, decision: "deny:model" };
      }
    }
    const b = this.#o.budget ? await this.#o.budget() : null;
    if (b?.exhausted) return { status: 402, type: BUDGET_ERROR_TYPE, message: budgetMessage(this.#o.locale ?? "en", b.limitUsd, b.ownerName), decision: "deny:budget" };
    return null;
  }

  // ---- lifecycle -----------------------------------------------------------------------------------------

  listen(port: number, host = "::"): Promise<number> {
    return new Promise((resolve) => this.server.listen(port, host, () => resolve((this.server.address() as { port: number }).port)));
  }

  /** Stop accepting, let open requests and tunnels finish for up to graceMs, then cut the rest and flush the log. */
  async drain(graceMs: number): Promise<void> {
    this.draining = true;
    await new Promise<void>((resolve) => {
      this.server.close(() => resolve());
      this.server.closeIdleConnections();
      const until = Date.now() + graceMs;
      const tick = setInterval(() => {
        this.server.closeIdleConnections();
        if (this.#sockets.size === 0 || Date.now() >= until) {
          clearInterval(tick);
          for (const s of this.#sockets) s.destroy();
          resolve();
        }
      }, 200);
    });
    await this.log.flush();
  }
}

/** The largest model request body, and the bytes of model request bodies egress holds at once: a request waits until
 * its body fits (one without a Content-Length counts as the largest). */
const MODEL_BODY_MAX = 64 * 1024 * 1024;

/** A model request's body read and rewritten (modelbody.ts), in a frame of its own so that nothing but the result
 * keeps the raw bytes alive. */
async function readModelBody(req: IncomingMessage, plan: ReasoningPlan | undefined): Promise<ModelBody | "too-large" | "invalid"> {
  const raw = await readBody(req, MODEL_BODY_MAX);
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

function text(res: ServerResponse, status: number, body: string, extra: Record<string, string> = {}): void {
  if (res.headersSent) return void res.destroy();
  res.writeHead(status, { "content-type": "text/plain; charset=utf-8", "content-length": String(Buffer.byteLength(body)), ...extra });
  res.end(body);
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

function serveCached(res: ServerResponse, meta: { status: number; contentType: string }, path: string, size: number, head = false): void {
  res.writeHead(meta.status, { "content-type": meta.contentType, "content-length": String(size), "x-egress-cache": "hit" });
  if (head) return void res.end();
  createReadStream(path).on("error", () => res.destroy()).pipe(res);
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

function safeUrl(u: string | undefined): string {
  return (u || "").replace(/\/\/[^@/]*@/, "//***@").slice(0, 300);
}
