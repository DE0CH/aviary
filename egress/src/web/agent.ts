// Client of the sandbox agent's HTTP API (docs/CHAT.md, "sandbox-agent"): used by the sandbox manager (health,
// the final save) and by jarvis-bash (files in, exec, changes out).
import { tarUnpack, type TarEntry } from "../common/tar.ts";
export interface ExecResult {
  stdout: string;
  stderr: string;
  exit_code: number | null;
  interrupted: boolean;
  ms?: number;
  truncated?: boolean;
  background_id?: string;
  pid?: number;
  log?: string;
}

export interface Changes {
  entries: TarEntry[];
  cursor: string;
  deleted: string[];
  skipped: string[];
  /** A full listing (first call, unknown cursor, agent restarted): files absent from it are gone. */
  full: boolean;
}

const META = ".jarvis-meta/changes.json";

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
  #h(extra: Record<string, string> = {}): Record<string, string> {
    // its own header: a provider's port proxy may strip or reuse Authorization
    return { ...this.#headers, "x-jarvis-exec-token": this.#token, ...extra };
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
  async changes(since: string): Promise<Changes> {
    const r = await fetch(`${this.url}/changes?since=${encodeURIComponent(since)}`, { headers: this.#h(), signal: AbortSignal.timeout(300_000) });
    await this.#check(r, "GET /changes");
    const list = (h: string | null): string[] => {
      try {
        const v = JSON.parse(h || "[]");
        return Array.isArray(v) ? v.map(String) : [];
      } catch {
        return [];
      }
    };
    let deleted = list(r.headers.get("x-deleted"));
    let skipped = list(r.headers.get("x-skipped"));
    let entries = tarUnpack(Buffer.from(await r.arrayBuffer()));
    const metaName = r.headers.get("x-meta-entry") || META;
    const meta = entries.find((e) => e.path === metaName);
    if (meta) {
      try {
        const m = JSON.parse(Buffer.from(meta.data).toString("utf8"));
        deleted = Array.isArray(m.deleted) ? m.deleted.map(String) : deleted;
        skipped = Array.isArray(m.skipped) ? m.skipped.map(String) : skipped;
      } catch {}
    }
    entries = entries.filter((e) => !e.path.startsWith(".jarvis-meta/"));
    return { entries, cursor: r.headers.get("x-cursor") || "", deleted, skipped, full: r.headers.get("x-full") === "1" || !since };
  }
}
