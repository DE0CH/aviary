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

/** The turn behind a turn token, if it is still running or waiting. */
export async function turnForToken(
  db: { query: (q: string, v: unknown[]) => Promise<{ rows: any[] }> },
  token: string,
): Promise<{ id: string; conversation_id: string; owner: string; status: string } | null> {
  if (!token) return null;
  const { rows } = await db.query(
    `select t.id, t.conversation_id, c.owner, t.status from turns t join conversations c on c.id = t.conversation_id
      where t.token_hash = $1 and t.status in ('running','waiting')`,
    [sha256hex(token)],
  );
  return rows[0] ?? null;
}

/**
 * The CloudCLI sandbox an egress token belongs to (docs/CLAUDE-CODE.md: Claude Code calls our MCP tools through egress
 * with its sandbox's token), as a turn-like reference without a turn.
 */
export async function sandboxForToken(
  db: { query: (q: string, v: unknown[]) => Promise<{ rows: any[] }> },
  token: string,
): Promise<{ id: null; conversation_id: string; owner: string; status: string } | null> {
  if (!token) return null;
  const { rows } = await db.query(
    `select conversation_id, owner from sandboxes where egress_token_hash = $1 and status in ('starting','ready') and owner is not null`,
    [sha256hex(token)],
  );
  return rows[0] ? { id: null, conversation_id: rows[0].conversation_id, owner: rows[0].owner, status: "running" } : null;
}
