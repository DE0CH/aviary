// The sandbox behind a call, as hermit sees it (aviary platform spec 17.2 tools item 2, hermit-api TokenLiveness):
// `GET http://hermit/v1/sandbox-tokens/{sandboxId}` with TOOLS_TOKEN_HERMIT, timeout 1 s, answers cached 30 s and
// refreshed in the background; while hermit fails (refused, timed out, 5xx) the last answer is served whatever its
// age, so a hermit outage never stops a running turn's tool calls. Never a database read.
//
// A call is served when hermit's answer (or, with none yet, nothing: the gateway has verified the token already) says
// the sandbox is live, in state `starting` or `ready` (as the old tools checked its sandbox row, A6.F29), and belongs to
// the conversation and person the gateway named. A 404 counts as "not live".
import type { HermitGet } from "../egress/tokens.ts";

const TIMEOUT_MS = 1000;
const TTL_MS = 30_000;
const RETRY_MS = 2000;
const FORGET_MS = 600_000;

/** What hermit said about one sandbox; null fields for a 404. */
export interface Answer {
  live: boolean;
  state: string | null;
  conversationId: string | null;
  person: string | null;
}

interface Entry {
  answer: Answer | null;
  at: number;
  pending: Promise<void> | null;
  retryAt: number;
  used: number;
}

export type Verdict = { ok: true; stale: boolean } | { ok: false; why: "not_live" | "state" | "mismatch"; state: string | null };

const SERVED_STATES = new Set(["starting", "ready"]);

export function judge(a: Answer | null, conversationId: string, person: string): Verdict {
  if (!a) return { ok: true, stale: false };
  if (!a.live) return { ok: false, why: "not_live", state: a.state };
  if (!SERVED_STATES.has(a.state ?? "")) return { ok: false, why: "state", state: a.state };
  if (a.conversationId !== conversationId || (a.person ?? "").trim().toLowerCase() !== person) return { ok: false, why: "mismatch", state: a.state };
  return { ok: true, stale: false };
}

export class Liveness {
  readonly #get: HermitGet;
  readonly #now: () => number;
  readonly #entries = new Map<string, Entry>();
  #sweeper: NodeJS.Timeout | null = null;
  /** lookups that got no answer (for /metrics) */
  failures = 0;
  /** calls served on an answer older than the TTL, or on none (hermit not answering) */
  staleServed = 0;

  constructor(get: HermitGet, now: () => number = Date.now) {
    this.#get = get;
    this.#now = now;
  }

  start(): void {
    this.#sweeper = setInterval(() => {
      const now = this.#now();
      for (const [id, e] of this.#entries) if (now - e.used >= FORGET_MS) this.#entries.delete(id);
    }, 60_000);
    this.#sweeper.unref();
  }

  stop(): void {
    if (this.#sweeper) clearInterval(this.#sweeper);
  }

  /** Whether a call for this sandbox, conversation and person is served now. The first call for a sandbox waits for
   * one lookup (at most 1 s); later ones use the cache and refresh it in the background. */
  async check(sandboxId: string, conversationId: string, person: string): Promise<Verdict> {
    const now = this.#now();
    let e = this.#entries.get(sandboxId);
    if (!e) {
      e = { answer: null, at: 0, pending: null, retryAt: 0, used: now };
      this.#entries.set(sandboxId, e);
      await this.#refresh(sandboxId, e);
    } else {
      e.used = now;
      if (now - e.at >= TTL_MS) void this.#refresh(sandboxId, e);
      // a lookup already running for a sandbox that has no answer yet: wait for it (it is bounded by the timeout)
      else if (!e.answer && e.pending) await e.pending;
    }
    const v = judge(e.answer, conversationId, person);
    if (!v.ok) return v;
    const stale = !e.answer || this.#now() - e.at >= TTL_MS;
    if (stale) this.staleServed++;
    return { ok: true, stale };
  }

  #refresh(id: string, e: Entry): Promise<void> {
    if (e.pending) return e.pending;
    if (this.#now() < e.retryAt) return Promise.resolve();
    e.pending = this.#lookup(id)
      .then((a) => {
        if (a === null) {
          this.failures++;
          e.retryAt = this.#now() + RETRY_MS;
          return;
        }
        e.answer = a;
        e.at = this.#now();
        e.retryAt = 0;
      })
      .finally(() => (e.pending = null));
    return e.pending;
  }

  async #lookup(id: string): Promise<Answer | null> {
    const r = await this.#get(`/v1/sandbox-tokens/${encodeURIComponent(id)}`, TIMEOUT_MS);
    if (!r) return null;
    if (r[0] === 404) return { live: false, state: null, conversationId: null, person: null };
    if (r[0] !== 200) return null;
    try {
      const v = JSON.parse(r[1]) as Record<string, unknown>;
      if (typeof v.live !== "boolean") return null;
      return {
        live: v.live,
        state: typeof v.state === "string" ? v.state : null,
        conversationId: typeof v.conversationId === "string" ? v.conversationId : null,
        person: typeof v.person === "string" ? v.person : null,
      };
    } catch {
      return null;
    }
  }
}
