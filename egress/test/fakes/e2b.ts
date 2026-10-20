// A fake E2B: the REST API the E2B provider uses (create, timeout, kill, list) and a port proxy in front of the
// sandboxes' agents. Creating a sandbox starts the REAL sandbox agent (chat/sandbox/agent.mjs) in JARVIS_INIT mode, as
// E2B's template start command does; the port proxy refuses calls without the sandbox's traffic access token and
// drops the Authorization header (so the agent must get its exec token another way).
import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, request, type Server } from "node:http";
import { createServer as netServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const AGENT = fileURLToPath(new URL("../../sandbox/agent.mjs", import.meta.url));
export const FAKE_E2B_KEY = "e2b_test_key";

async function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const s = netServer();
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as { port: number }).port;
      s.close(() => resolve(p));
    });
  });
}

export interface FakeSandbox {
  id: string;
  body: any;
  child: ChildProcess;
  port: number;
  work: string;
  traffic: string;
  startedAt: string;
  timeouts: number[];
}

export class FakeE2b {
  readonly sandboxes = new Map<string, FakeSandbox>();
  readonly killed: string[] = [];
  /** Sandboxes E2B has that this test made outside the manager (orphans, other instances). */
  readonly api: Server;
  readonly ports: Server;
  apiUrl = "";
  portsUrl = "";

  constructor() {
    this.api = createServer((req, res) => void this.#api(req, res));
    this.ports = createServer((req, res) => this.#port(req, res));
  }

  async start(): Promise<this> {
    for (const s of [this.api, this.ports]) await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
    this.apiUrl = `http://127.0.0.1:${(this.api.address() as { port: number }).port}`;
    this.portsUrl = `http://127.0.0.1:${(this.ports.address() as { port: number }).port}`;
    return this;
  }

  /** What the provider's agentUrlFor returns: the port proxy, path-prefixed by sandbox id. */
  agentUrlFor = (id: string): string => `${this.portsUrl}/${id}`;

  async add(body: any): Promise<FakeSandbox> {
    const id = randomBytes(10).toString("hex");
    const port = await freePort();
    const work = mkdtempSync(join(tmpdir(), "e2b-work-"));
    const child = spawn(process.execPath, [AGENT], {
      env: { PATH: process.env.PATH!, HOME: work, JARVIS_WORK_DIR: work, PORT: String(port), JARVIS_INIT: "1", JARVIS_WIRE_PORT: String(await freePort()) },
      stdio: "ignore",
    });
    const sb: FakeSandbox = { id, body, child, port, work, traffic: randomBytes(12).toString("hex"), startedAt: new Date().toISOString(), timeouts: [] };
    this.sandboxes.set(id, sb);
    return sb;
  }

  #kill(id: string): boolean {
    const sb = this.sandboxes.get(id);
    if (!sb) return false;
    this.sandboxes.delete(id);
    this.killed.push(id);
    sb.child.kill("SIGKILL");
    rmSync(sb.work, { recursive: true, force: true });
    return true;
  }

  async #api(req: import("node:http").IncomingMessage, res: import("node:http").ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : undefined;
    const send = (status: number, v?: unknown) => {
      res.writeHead(status, v === undefined ? {} : { "content-type": "application/json" });
      res.end(v === undefined ? undefined : JSON.stringify(v));
    };
    if (req.headers["x-api-key"] !== FAKE_E2B_KEY) return send(401, { code: 401, message: "Invalid API key" });
    const u = new URL(req.url || "/", "http://e2b");
    let m: RegExpExecArray | null;
    if (req.method === "POST" && u.pathname === "/v2/sandboxes") {
      const sb = await this.add(body);
      return send(201, { templateID: body.templateID, sandboxID: sb.id, clientID: "c", envdVersion: "0.4.0", envdAccessToken: "envd", trafficAccessToken: sb.traffic, domain: null });
    }
    if (req.method === "POST" && (m = /^\/sandboxes\/([^/]+)\/timeout$/.exec(u.pathname))) {
      const sb = this.sandboxes.get(m[1]!);
      if (!sb) return send(404, { code: 404, message: "not found" });
      sb.timeouts.push(body.timeout);
      return send(204);
    }
    if (req.method === "DELETE" && (m = /^\/sandboxes\/([^/]+)$/.exec(u.pathname))) return send(this.#kill(m[1]!) ? 204 : 404);
    if (req.method === "GET" && u.pathname === "/v2/sandboxes") {
      const want = new URLSearchParams(u.searchParams.get("metadata") || "");
      const items = [...this.sandboxes.values()]
        .filter((s) => [...want].every(([k, v]) => s.body?.metadata?.[k] === v))
        .map((s) => ({ templateID: s.body.templateID, sandboxID: s.id, startedAt: s.startedAt, metadata: s.body.metadata, state: "running", cpuCount: 1, memoryMB: 1024 }));
      return send(200, items);
    }
    send(404, { message: "no route" });
  }

  #port(req: import("node:http").IncomingMessage, res: import("node:http").ServerResponse): void {
    const m = /^\/([^/]+)(\/.*)$/.exec(req.url || "");
    const sb = m ? this.sandboxes.get(m[1]!) : undefined;
    if (!sb) return void res.writeHead(502).end("sandbox not found");
    if (req.headers["e2b-traffic-access-token"] !== sb.traffic) return void res.writeHead(403).end("missing traffic access token");
    const headers = { ...req.headers };
    delete headers.authorization;
    delete headers["e2b-traffic-access-token"];
    const out = request({ host: "127.0.0.1", port: sb.port, method: req.method, path: m![2], headers }, (r) => {
      res.writeHead(r.statusCode ?? 502, r.headers);
      r.pipe(res);
    });
    out.on("error", () => (res.headersSent ? res.destroy() : res.writeHead(502).end("agent not listening")));
    req.pipe(out);
  }

  async close(): Promise<void> {
    for (const id of [...this.sandboxes.keys()]) this.#kill(id);
    for (const s of [this.api, this.ports]) {
      s.closeAllConnections();
      await new Promise((r) => s.close(r));
    }
  }
}
