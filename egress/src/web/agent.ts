// Client of the sandbox agent's HTTP API (chat/sandbox/agent.mjs): the sandbox manager checks its health. The CloudCLI
// UI talks to the same agent directly (`GET /proc`, docs/CLAUDE-CODE.md) with the URL, token and headers that
// /internal/cc/sandbox hands it.
export class AgentClient {
  readonly url: string;
  readonly #token: string;
  readonly #headers: Record<string, string>;
  /** `headers`: the provider's own (E2B's traffic access token), sent on every call. */
  constructor(url: string, token: string, headers: Record<string, string> = {}) {
    this.url = url.replace(/\/$/, "");
    this.#token = token;
    this.#headers = headers;
  }
  async health(timeoutMs = 3000): Promise<{ ok: boolean; boot_id?: string }> {
    // its own header: a provider's port proxy may strip or reuse Authorization
    const r = await fetch(`${this.url}/healthz`, { headers: { ...this.#headers, "x-jarvis-exec-token": this.#token }, signal: AbortSignal.timeout(timeoutMs) });
    if (!r.ok) throw new Error(`sandbox healthz: HTTP ${r.status} ${(await r.text()).slice(0, 300)}`);
    return (await r.json()) as { ok: boolean; boot_id?: string };
  }
}
