// Who is calling egress: a sandbox (its egress token, Basic "sbx:<token>" or Bearer) or a running turn (its turn
// token: claw's model calls). Lookups are cached for a few seconds; a released sandbox's token stops working within
// that time. The conversation's allowlist (allowed_hosts) is cached for 2 s, so a host the user just allowed
// works on the agent's next try.
import type { IncomingHttpHeaders } from "node:http";
import type { Db } from "../common/db.ts";
import { bearerOf, sha256hex, turnForToken } from "../common/tokens.ts";

export interface SandboxId {
  sandboxId: string;
  conversationId: string;
}

export interface TurnId {
  turnId: string;
  conversationId: string;
  owner: string;
}

/** The token in `Proxy-Authorization` or `Authorization`: Basic (any user, the password is the token) or Bearer. */
export function tokenFrom(headers: IncomingHttpHeaders, which: "proxy-authorization" | "authorization"): string {
  const h = headers[which];
  const v = Array.isArray(h) ? h[0] : h;
  if (!v) return "";
  const basic = /^Basic\s+(\S+)$/i.exec(v);
  if (basic) {
    const dec = Buffer.from(basic[1]!, "base64").toString("utf8");
    const i = dec.indexOf(":");
    return i >= 0 ? dec.slice(i + 1) : "";
  }
  return bearerOf(v);
}

export class Identities {
  readonly #db: Db;
  readonly #ttlMs: number;
  readonly #sandboxes = new Map<string, { at: number; v: SandboxId | null }>();
  readonly #turns = new Map<string, { at: number; v: TurnId | null }>();
  readonly #hosts = new Map<string, { at: number; v: string[] }>();
  constructor(db: Db, ttlMs = 5000) {
    this.#db = db;
    this.#ttlMs = ttlMs;
  }

  async sandbox(token: string): Promise<SandboxId | null> {
    if (!token) return null;
    const key = sha256hex(token);
    const hit = this.#sandboxes.get(key);
    if (hit && Date.now() - hit.at < this.#ttlMs) return hit.v;
    const { rows } = await this.#db.query<{ id: string; conversation_id: string }>(
      "select id, conversation_id from sandboxes where egress_token_hash = $1 and status <> 'gone'",
      [key],
    );
    const v = rows[0] ? { sandboxId: rows[0].id, conversationId: rows[0].conversation_id } : null;
    this.#remember(this.#sandboxes, key, v);
    return v;
  }

  async turn(token: string): Promise<TurnId | null> {
    if (!token) return null;
    const key = sha256hex(token);
    const hit = this.#turns.get(key);
    if (hit && Date.now() - hit.at < this.#ttlMs) return hit.v;
    const t = await turnForToken(this.#db, token);
    const v = t ? { turnId: t.id, conversationId: t.conversation_id, owner: t.owner } : null;
    this.#remember(this.#turns, key, v);
    return v;
  }

  async allowedHosts(conversationId: string): Promise<string[]> {
    const hit = this.#hosts.get(conversationId);
    if (hit && Date.now() - hit.at < 2000) return hit.v;
    const { rows } = await this.#db.query<{ host: string }>("select host from allowed_hosts where conversation_id = $1", [conversationId]);
    const v = rows.map((r) => r.host);
    this.#remember(this.#hosts, conversationId, v);
    return v;
  }

  #remember<V>(m: Map<string, { at: number; v: V }>, k: string, v: V): void {
    if (m.size > 5000) m.clear();
    m.set(k, { at: Date.now(), v });
  }
}
