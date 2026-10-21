// Who is calling egress: a sandbox, by its egress token (Basic "sbx:<token>" or Bearer). Lookups are cached for a few
// seconds; a released sandbox's token stops working within that time.
import type { IncomingHttpHeaders } from "node:http";
import type { Db } from "../common/db.ts";
import { bearerOf, sha256hex } from "../common/tokens.ts";

export interface SandboxId {
  sandboxId: string;
  conversationId: string;
  /** provider "vm": the sandbox's own address; its requests must come from there (null for other providers) */
  ip: string | null;
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
  constructor(db: Db, ttlMs = 5000) {
    this.#db = db;
    this.#ttlMs = ttlMs;
  }

  async sandbox(token: string): Promise<SandboxId | null> {
    if (!token) return null;
    const key = sha256hex(token);
    const hit = this.#sandboxes.get(key);
    if (hit && Date.now() - hit.at < this.#ttlMs) return hit.v;
    const { rows } = await this.#db.query<{ id: string; conversation_id: string; ip: string | null }>(
      "select id, conversation_id, ip from sandboxes where egress_token_hash = $1 and status <> 'gone'",
      [key],
    );
    const v = rows[0] ? { sandboxId: rows[0].id, conversationId: rows[0].conversation_id, ip: rows[0].ip ?? null } : null;
    this.#remember(this.#sandboxes, key, v);
    return v;
  }

  #remember<V>(m: Map<string, { at: number; v: V }>, k: string, v: V): void {
    if (m.size > 5000) m.clear();
    m.set(k, { at: Date.now(), v });
  }
}
