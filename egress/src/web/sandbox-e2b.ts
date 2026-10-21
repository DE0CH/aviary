// Sandbox provider "e2b" (docs/CHAT.md, "Sandbox providers"): E2B's hosted Firecracker sandboxes, through its REST API
// with fetch (https://api.e2b.app, header X-API-Key; spec: github.com/e2b-dev/infra spec/openapi.yml). Each sandbox
// starts from our template (chat/sandbox, built by the chat workflow), whose start command already runs the agent in
// JARVIS_INIT mode; the manager's sandbox gets its exec token, env and wire with one POST /init.
//
// - Network: the sandbox may reach only the egress host (`network.allowOut` = that host, `denyOut` = everything);
//   its local proxy 127.0.0.1:3128 carries every connection to egress's /wire over that one WebSocket host.
// - The agent port is reached at https://7070-<sandbox id>.<domain> and is not public: every call carries the
//   sandbox's traffic access token (`e2b-traffic-access-token`), plus the agent's own exec token.
// - Lifetime: E2B kills a sandbox at its timeout; the manager sets it past the next idle release on every start
//   and touch (at most every 30 s) (POST /sandboxes/{id}/timeout), and kills it on release (DELETE).
// - The janitor's orphan check lists this instance's sandboxes by metadata `jarvis` = config.sandbox.e2b.instance.
import { AgentClient } from "./agent.ts";
import type { AgentEndpoint, ListedSandbox, SandboxProvider } from "./provider.ts";
import type { SandboxRow } from "./sandbox.ts";
import { sandboxEnv } from "./sandbox-k8s.ts";

export interface E2bOptions {
  apiKey: string;
  apiUrl: string;
  domain: string;
  template: string;
  egressUrl: string;
  egressHeaders: Record<string, string>;
  instance: string;
  /** Agent URL for a sandbox id (tests: a local agent). Default https://7070-<id>.<domain>. */
  agentUrlFor?: (sandboxId: string) => string;
}

interface Created {
  sandboxID: string;
  trafficAccessToken?: string | null;
  envdAccessToken?: string | null;
  domain?: string | null;
}

/** The timeout of a new sandbox before the manager's first keep-alive (seconds). */
const INITIAL_TIMEOUT_S = 15 * 60;

export class E2bProvider implements SandboxProvider {
  readonly kind = "e2b" as const;
  readonly #o: E2bOptions;
  readonly #egressHost: string;
  /** sandbox id -> traffic token and domain, from create until ready (the row keeps them afterwards) */
  readonly #created = new Map<string, Created>();
  /** row id -> its egress token, from create until /init (the manager never stores it) */
  readonly #pendingTokens = new Map<string, string>();

  constructor(o: E2bOptions) {
    this.#o = { ...o, apiUrl: o.apiUrl.replace(/\/$/, "") };
    this.#egressHost = new URL(o.egressUrl).hostname;
  }

  async #api(method: string, path: string, body?: unknown, ok404 = false): Promise<any> {
    const r = await fetch(`${this.#o.apiUrl}${path}`, {
      method,
      headers: { "x-api-key": this.#o.apiKey, ...(body === undefined ? {} : { "content-type": "application/json" }) },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(60_000),
    });
    if (r.status === 404 && ok404) return null;
    const text = await r.text();
    if (!r.ok) throw new Error(`e2b ${method} ${path.split("?")[0]}: HTTP ${r.status} ${text.slice(0, 300)}`);
    return text ? JSON.parse(text) : null;
  }

  #agentUrl(id: string, domain?: string | null): string {
    return this.#o.agentUrlFor ? this.#o.agentUrlFor(id) : `https://7070-${id}.${domain || this.#o.domain}`;
  }

  async create(row: SandboxRow, egressToken: string): Promise<{ name: string }> {
    const c = (await this.#api("POST", "/v2/sandboxes", {
      templateID: this.#o.template,
      timeout: INITIAL_TIMEOUT_S,
      autoPause: false,
      metadata: { jarvis: this.#o.instance, sandbox: row.id, conversation: row.conversation_id },
      // deny everything but the egress host (matched on TLS SNI; E2B then also allows its DNS server, 8.8.8.8)
      network: { allowPublicTraffic: false, allowOut: [this.#egressHost], denyOut: ["0.0.0.0/0"] },
    })) as Created;
    if (!c?.sandboxID) throw new Error("e2b: create answered without a sandbox id");
    this.#created.set(c.sandboxID, c);
    this.#pendingTokens.set(row.id, egressToken);
    return { name: c.sandboxID };
  }

  /** The agent is already running (the template's start command): configure it with POST /init, then check health. */
  async waitReady(row: SandboxRow, deadline: number): Promise<AgentEndpoint> {
    const c = this.#created.get(row.pod);
    this.#created.delete(row.pod);
    if (!c) throw new Error("e2b: unknown sandbox (made by another replica)");
    const url = this.#agentUrl(c.sandboxID, c.domain);
    const headers: Record<string, string> = c.trafficAccessToken ? { "e2b-traffic-access-token": c.trafficAccessToken } : {};
    const token = this.#pendingTokens.get(row.id);
    this.#pendingTokens.delete(row.id);
    if (!token) throw new Error("e2b: no egress token for the sandbox");
    const env = { ...sandboxEnv("http://127.0.0.1:3128", "127.0.0.1", token), HOME: "/home/sandbox" };
    const wire = { url: this.#o.egressUrl, headers: { ...this.#o.egressHeaders, authorization: `Bearer ${token}` } };
    let last = "";
    while (Date.now() < deadline) {
      try {
        const r = await fetch(`${url}/init`, {
          method: "POST",
          headers: { ...headers, "content-type": "application/json" },
          body: JSON.stringify({ token: row.exec_token, env, wire }),
          signal: AbortSignal.timeout(10_000),
        });
        if (r.ok || r.status === 409) {
          const h = await new AgentClient(url, row.exec_token, headers).health(5000);
          if (h.ok) return { url, headers };
        }
        last = `HTTP ${r.status} ${(await r.text().catch(() => "")).slice(0, 200)}`;
      } catch (e) {
        last = (e as Error).message;
      }
      await sleep(300);
    }
    throw new Error(`the sandbox's agent did not answer in time (${last})`);
  }

  async keepAlive(row: SandboxRow, until: Date): Promise<void> {
    const seconds = Math.max(60, Math.ceil((until.getTime() - Date.now()) / 1000));
    await this.#api("POST", `/sandboxes/${encodeURIComponent(row.pod)}/timeout`, { timeout: seconds });
  }

  async destroy(name: string): Promise<void> {
    if (!/^[a-z0-9]+$/i.test(name)) return; // a row that never got an E2B id
    await this.#api("DELETE", `/sandboxes/${encodeURIComponent(name)}`, undefined, true);
  }

  async list(): Promise<ListedSandbox[]> {
    const q = new URLSearchParams({ metadata: `jarvis=${this.#o.instance}`, limit: "100" });
    const items = ((await this.#api("GET", `/v2/sandboxes?${q}`)) ?? []) as { sandboxID: string; startedAt?: string; state?: string; metadata?: Record<string, string> }[];
    return items
      .filter((s) => s.metadata?.jarvis === this.#o.instance && s.state !== "paused")
      .map((s) => ({ name: s.sandboxID, createdAt: s.startedAt ? Date.parse(s.startedAt) : NaN }));
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
