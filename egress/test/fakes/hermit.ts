// Test fakes for egress as the sandbox gateway: hermit (the token key set and liveness, with an outage switch),
// magpie (echoes what it got; a long-poll; byte counts of uploads) and tools, plus token minting with a hermit
// master key. Also used by the CI conformance job's model suite (test/conformance/fake-hermit.ts).
import { randomBytes, randomUUID } from "node:crypto";
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo, Socket } from "node:net";
import { signingKey, signToken, type Claims } from "../../src/egress/tokens.ts";

export const masterKey = (kid = 1): string => `${kid}:${randomBytes(32).toString("base64url")}`;

export interface Seen {
  method: string;
  url: string;
  headers: IncomingMessage["headers"];
  bytes: number;
}

export type Outage = "up" | "reset" | "hang" | "unavailable";

/** A fake hermit: `GET /v1/sandbox-token-keys` and `GET /v1/sandbox-tokens/{id}`; the bearer must be `bearer`. */
export class FakeHermit {
  readonly server: Server;
  readonly keys: Array<ReturnType<typeof signingKey>> = [];
  /** sandbox id → live (true/false) or "unknown" (404); a sandbox not listed is live */
  readonly live = new Map<string, boolean | "unknown">();
  outage: Outage = "up";
  keyFetches = 0;
  lookups = new Map<string, number>();
  readonly seen: Seen[] = [];
  readonly #sockets = new Set<Socket>();
  port = 0;

  readonly bearer: string;
  constructor(bearer: string, master = masterKey()) {
    this.bearer = bearer;
    this.keys.push(signingKey(master));
    this.server = createServer((req, res) => {
      if (this.outage === "reset") return void req.socket.destroy();
      if (this.outage === "hang") return; // never answers
      if (this.outage === "unavailable") return void res.writeHead(503).end('{"code":"unavailable"}');
      const ok = req.headers.authorization === `Bearer ${this.bearer}`;
      const url = req.url ?? "/";
      if (url.startsWith("/v1/sandbox/")) return this.#echo(req, res);
      if (!ok) return void res.writeHead(401).end('{"code":"unauthenticated"}');
      if (url === "/v1/sandbox-token-keys") {
        this.keyFetches++;
        res.writeHead(200, { "content-type": "application/json" });
        return void res.end(JSON.stringify({ keys: this.keys.map((k) => ({ kid: k.kid, alg: "Ed25519", publicKey: k.publicKey })) }));
      }
      const m = /^\/v1\/sandbox-tokens\/([0-9a-f-]{36})$/.exec(url);
      if (m) {
        const id = m[1]!;
        this.lookups.set(id, (this.lookups.get(id) ?? 0) + 1);
        const l = this.live.get(id) ?? true;
        if (l === "unknown") return void res.writeHead(404, { "content-type": "application/json" }).end('{"code":"unknown_sandbox"}');
        res.writeHead(200, { "content-type": "application/json" });
        return void res.end(JSON.stringify({ sandboxId: id, conversationId: randomUUID(), person: "x@example.org", state: l ? "ready" : "gone", live: l, expiresAt: new Date(Date.now() + 3600_000).toISOString() }));
      }
      res.writeHead(404).end();
    });
    this.server.on("connection", (s) => {
      this.#sockets.add(s);
      s.on("close", () => this.#sockets.delete(s));
    });
  }

  #echo(req: IncomingMessage, res: import("node:http").ServerResponse): void {
    let bytes = 0;
    req.on("data", (c: Buffer) => (bytes += c.length));
    req.on("end", () => {
      this.seen.push({ method: req.method ?? "", url: req.url ?? "", headers: req.headers, bytes });
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ name: "hermit", bytes }));
    });
  }

  async listen(port = 0, host = "127.0.0.1"): Promise<string> {
    await new Promise<void>((r) => this.server.listen(port, host, r));
    this.port = (this.server.address() as AddressInfo).port;
    return `http://127.0.0.1:${this.port}`;
  }

  close(): void {
    for (const s of this.#sockets) s.destroy();
    this.server.close();
  }

  /** A token for a new sandbox (live unless set otherwise), presented from `source` ("0.0.0.0" = anywhere). */
  token(o: Partial<Claims> & { key?: ReturnType<typeof signingKey> } = {}): { token: string; claims: Claims } {
    const claims: Claims = {
      sandboxId: o.sandboxId ?? randomUUID(),
      conversationId: o.conversationId ?? randomUUID(),
      source: o.source ?? "127.0.0.1",
      expiresAt: o.expiresAt ?? Math.floor(Date.now() / 1000) + 3600,
      person: o.person ?? "staff@example.org",
    };
    return { token: signToken(o.key ?? this.keys[0]!, claims), claims };
  }
}

/** A fake magpie: `/v1/sandbox/poll?ms=` answers after ms; anything else echoes what arrived (method, url, headers,
 * body bytes). */
export class FakeMagpie {
  readonly server: Server;
  readonly seen: Seen[] = [];
  port = 0;
  constructor() {
    this.server = createServer((req, res) => {
      let bytes = 0;
      req.on("data", (c: Buffer) => (bytes += c.length));
      req.on("end", () => {
        this.seen.push({ method: req.method ?? "", url: req.url ?? "", headers: req.headers, bytes });
        const poll = /^\/v1\/sandbox\/poll\?ms=(\d+)/.exec(req.url ?? "");
        const answer = () => {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ name: "magpie", bytes }));
        };
        if (poll) setTimeout(answer, Number(poll[1]));
        else answer();
      });
    });
  }
  async listen(): Promise<string> {
    await new Promise<void>((r) => this.server.listen(0, "127.0.0.1", r));
    this.port = (this.server.address() as AddressInfo).port;
    return `http://127.0.0.1:${this.port}`;
  }
  close(): void {
    this.server.closeAllConnections();
    this.server.close();
  }
}
