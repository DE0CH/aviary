// Client of the sandbox agent's HTTP API (docs/CHAT.md, "sandbox-agent"): used by the sandbox manager (health,
// the final save) and by jarvis-bash (files in, exec, changes out).
export interface ExecResult {
  stdout: string;
  stderr: string;
  exit_code: number | null;
  interrupted: boolean;
  ms?: number;
  truncated?: boolean;
  background_id?: string;
  pid?: number;
}

export class AgentClient {
  readonly url: string;
  readonly #token: string;
  constructor(url: string, token: string) {
    this.url = url.replace(/\/$/, "");
    this.#token = token;
  }
  #h(extra: Record<string, string> = {}): Record<string, string> {
    return { authorization: `Bearer ${this.#token}`, ...extra };
  }
  async #check(r: Response, what: string): Promise<Response> {
    if (!r.ok) throw new Error(`sandbox ${what}: HTTP ${r.status} ${(await r.text()).slice(0, 300)}`);
    return r;
  }
  async health(timeoutMs = 3000): Promise<{ ok: boolean; boot_id?: string }> {
    const r = await fetch(`${this.url}/healthz`, { headers: this.#h(), signal: AbortSignal.timeout(timeoutMs) });
    await this.#check(r, "healthz");
    return (await r.json()) as { ok: boolean; boot_id?: string };
  }
  async putFiles(tar: Buffer, deletes: string[]): Promise<void> {
    const r = await fetch(`${this.url}/files`, {
      method: "PUT",
      headers: this.#h({ "content-type": "application/x-tar", "x-delete": JSON.stringify(deletes) }),
      body: tar,
      signal: AbortSignal.timeout(300_000),
    });
    await this.#check(r, "PUT /files");
  }
  async exec(command: string, timeoutMs: number, background: boolean): Promise<ExecResult> {
    const r = await fetch(`${this.url}/exec`, {
      method: "POST",
      headers: this.#h({ "content-type": "application/json" }),
      body: JSON.stringify({ command, timeout_ms: timeoutMs, background }),
      signal: AbortSignal.timeout(timeoutMs + 60_000),
    });
    await this.#check(r, "POST /exec");
    return (await r.json()) as ExecResult;
  }
  async changes(since: string): Promise<{ tar: Buffer; cursor: string; deleted: string[] }> {
    const r = await fetch(`${this.url}/changes?since=${encodeURIComponent(since)}`, { headers: this.#h(), signal: AbortSignal.timeout(300_000) });
    await this.#check(r, "GET /changes");
    let deleted: string[] = [];
    try {
      deleted = JSON.parse(r.headers.get("x-deleted") || "[]");
    } catch {}
    return { tar: Buffer.from(await r.arrayBuffer()), cursor: r.headers.get("x-cursor") || "", deleted };
  }
}
