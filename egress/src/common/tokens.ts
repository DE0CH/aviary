// Bearer tokens of the chat runtime (docs/CHAT.md, "Tokens"): random, stored only as their sha256.
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";

export const newToken = (): string => randomBytes(32).toString("base64url");

export const sha256hex = (s: string | Uint8Array): string => createHash("sha256").update(s).digest("hex");

export function sameSecret(a: string, b: string): boolean {
  const x = Buffer.from(a);
  const y = Buffer.from(b);
  return x.length === y.length && timingSafeEqual(x, y);
}

export function bearerOf(header: string | string[] | undefined): string {
  const h = Array.isArray(header) ? header[0] : header;
  const m = /^Bearer\s+(\S+)$/i.exec(h || "");
  return m ? m[1]! : "";
}

/**
 * The sandbox an egress token belongs to, with its conversation and owner (docs/CLAUDE-CODE.md: Claude Code calls our
 * MCP tools through egress with its sandbox's token, which egress passes on).
 */
export async function sandboxForToken(
  db: { query: (q: string, v: unknown[]) => Promise<{ rows: any[] }> },
  token: string,
): Promise<{ conversation_id: string; owner: string } | null> {
  if (!token) return null;
  const { rows } = await db.query(
    `select conversation_id, owner from sandboxes where egress_token_hash = $1 and status in ('starting','ready') and owner is not null`,
    [sha256hex(token)],
  );
  return rows[0] ? { conversation_id: rows[0].conversation_id, owner: rows[0].owner } : null;
}
