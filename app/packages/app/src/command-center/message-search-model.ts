/**
 * The conversation search's hits (pigeon's `POST /api/jarvis/search`): the person's messages across their
 * conversations, found on the server by their words and, where the deployment has an embedding model, by their
 * meaning. The palette lists them under the conversations it matched by title; choosing one opens the conversation
 * at that message (`search-jump-store.ts`).
 */

export interface MessageSearchHit {
  conversationId: string;
  title: string | null;
  epoch: string;
  /** The message's row as its timeline cursor names it (`seqEnd`). */
  seq: number;
  kind: string;
  at: string;
  snippet: string;
  /** `words`, `meaning` or `both`. */
  matched: string;
}

export interface MessageSearchBody {
  query?: string;
  hits?: MessageSearchHit[];
  semantic?: boolean;
}

/** Queries shorter than this (in characters) are not sent: two characters are a word in Chinese. */
export const MESSAGE_SEARCH_MIN_CHARS = 2;
export const MESSAGE_SEARCH_DEBOUNCE_MS = 250;
export const MESSAGE_SEARCH_LIMIT = 20;

/** The query as it is sent, or null when it is too short to search. */
export function messageSearchQuery(query: string): string | null {
  const trimmed = query.trim();
  return Array.from(trimmed).length >= MESSAGE_SEARCH_MIN_CHARS ? trimmed : null;
}

/** The hits of a server answer that can be shown: well formed, and only for the query that was asked. */
export function usableHits(body: MessageSearchBody | null, asked: string): MessageSearchHit[] {
  if (!body || body.query !== asked || !Array.isArray(body.hits)) return [];
  return body.hits.filter(
    (hit) =>
      typeof hit?.conversationId === "string" &&
      typeof hit.seq === "number" &&
      typeof hit.snippet === "string",
  );
}

/** The palette's id of a hit (one row per message). */
export function messageResultId(hit: Pick<MessageSearchHit, "conversationId" | "seq">): string {
  return `message:${hit.conversationId}:${hit.seq}`;
}

/** The Paseo workspace a conversation is (pigeon names it `wks_<conversation id>`). */
export function conversationWorkspaceId(conversationId: string): string {
  return `wks_${conversationId}`;
}
