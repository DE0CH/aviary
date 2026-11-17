import type { ActiveTurnBehavior, AgentAttachment } from "@getpaseo/protocol/messages";
import type { AttachmentMetadata } from "@/attachments/types";
import { createUserMessage, type StreamItem, type UserMessageItem } from "@/types/stream";

/**
 * The outbox: the messages this device has sent that the server has not confirmed yet (Deyao, 2026-11-17: a third
 * state beside "queued" and "sent"). A message goes into it the moment it is sent and the message box is free again
 * at once; the chat shows it faded with a clock (design A) until the server confirms it. When the connection is down
 * it stays here, on the device (a reload or a closed app keeps it), and is sent again with the same id when the
 * connection comes back: the server admits a message id once (magpie's idempotency on `clientMessageId`), so a send
 * whose answer was lost is never doubled. A message the server refuses leaves the outbox and the chat, and its error
 * is shown under the message box as before.
 */

/** A message in the outbox. Everything it needs to be sent again later, small enough to keep on the device. */
export interface OutboxEntry {
  /** The message's id (`clientMessageId`): the same on every attempt. */
  id: string;
  serverId: string;
  agentId: string;
  text: string;
  /** Images by reference: their bytes stay in the app's attachment store until the send encodes them. */
  images: AttachmentMetadata[];
  /** Already uploaded files and other references (no file bytes). */
  attachments: AgentAttachment[];
  activeTurnBehavior?: ActiveTurnBehavior;
  /** Sent while a reply ran: shown in the queue above the message box, not in the chat. */
  queued: boolean;
  createdAtMs: number;
  /** The server answered the send: it is not sent again, and stays listed only until its row comes. */
  deliveredAtMs?: number;
}

/** How long a message waits before its line says it waits for the network (design A: "still offline after 5 s"). */
export const PENDING_WAIT_MS = 5000;

/** How long a delivered message stays listed while its row has not come (the row is on another feed). */
export const DELIVERED_SHOWN_MS = 10_000;

/** What a send of an outbox message came to. */
export type OutboxSendOutcome =
  | { kind: "delivered" }
  /** The connection failed: the message stays and is sent again when the connection is back. */
  | { kind: "offline" }
  /** The server refused it: it leaves the outbox, the error is shown. */
  | { kind: "refused"; error: Error };

/**
 * A failure of the connection, not an answer from the server: the message may not have arrived, so it is kept and
 * sent again. The client's own errors for this (`DaemonConnectionError`: connection lost, request timed out; a plain
 * "Transport not connected" from a send while the socket is down) and the browser's for a fetch that never reached the
 * server. Anything else is the server's answer.
 */
export function isConnectionFailure(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  if (error.name === "DaemonConnectionError") return true;
  return /transport not connected|timed out|timeout waiting|network ?error|failed to fetch|connection (lost|closed)|host disconnected|not connected/i.test(
    error.message,
  );
}

/** The outbox's entries for one conversation, in the order they were sent. */
export function entriesFor(
  entries: readonly OutboxEntry[],
  serverId: string,
  agentId: string,
): OutboxEntry[] {
  return entries
    .filter((e) => e.serverId === serverId && e.agentId === agentId)
    .sort((a, b) => a.createdAtMs - b.createdAtMs);
}

/** The entries still to be sent for a server, oldest first. */
export function unsentEntries(entries: readonly OutboxEntry[], serverId: string): OutboxEntry[] {
  return entries
    .filter((e) => e.serverId === serverId && e.deliveredAtMs === undefined)
    .sort((a, b) => a.createdAtMs - b.createdAtMs);
}

/** The ids of the chat's user rows (the ones a message names), newest rows looked at first. */
function userRowIds(rows: readonly (readonly StreamItem[] | undefined)[]): Set<string> {
  const ids = new Set<string>();
  for (const list of rows) {
    if (!list) continue;
    for (const row of list) {
      if (row.kind !== "user_message") continue;
      for (const id of [row.clientMessageId, row.messageId, row.id]) {
        if (id !== undefined) ids.add(id);
      }
    }
  }
  return ids;
}

/**
 * The chat rows to add for the conversation's outbox messages: one for each message that belongs in the chat (not
 * the queue) and has no row there (after a reload, or after the history was loaded again on a reconnect, which drops
 * rows the app drew itself). A delivered message is drawn for `DELIVERED_SHOWN_MS` at most, until the server's row
 * comes.
 */
export function outboxRows(
  entries: readonly OutboxEntry[],
  serverId: string,
  agentId: string,
  nowMs: number,
  ...rows: readonly (readonly StreamItem[] | undefined)[]
): UserMessageItem[] {
  const mine = entriesFor(entries, serverId, agentId).filter((e) => !e.queued);
  if (mine.length === 0) return [];
  const present = userRowIds(rows);
  return mine
    .filter((e) => !present.has(e.id))
    .filter((e) => e.deliveredAtMs === undefined || nowMs - e.deliveredAtMs < DELIVERED_SHOWN_MS)
    .map((e) =>
      createUserMessage({
        clientMessageId: e.id,
        text: e.text,
        timestamp: new Date(e.createdAtMs),
        images: e.images,
        attachments: e.attachments,
      }),
    );
}

/** The ids of the conversation's messages the server has not confirmed (drawn faded). */
export function unconfirmedIds(
  entries: readonly OutboxEntry[],
  serverId: string,
  agentId: string,
): string[] {
  return entriesFor(entries, serverId, agentId)
    .filter((e) => e.deliveredAtMs === undefined)
    .map((e) => e.id);
}

/** What the line under an unconfirmed message says. */
export type PendingPhase = "sending" | "waiting";

export function pendingPhase(sentAtMs: number, nowMs: number): PendingPhase {
  return nowMs - sentAtMs >= PENDING_WAIT_MS ? "waiting" : "sending";
}

/** The outbox after a load from the device: delivered entries are not kept (their rows have come or never will). */
export function afterLoad(entries: readonly OutboxEntry[]): OutboxEntry[] {
  return entries.filter((e) => e.deliveredAtMs === undefined);
}

/** The image ids the outbox still needs (kept from the attachment store's clean-up). */
export function referencedImageIds(entries: readonly OutboxEntry[]): string[] {
  return entries.flatMap((e) => e.images.map((image) => image.id));
}

export interface DrainInput {
  /** The unsent entries of one server, oldest first. */
  entries: readonly OutboxEntry[];
  send: (entry: OutboxEntry) => Promise<void>;
  onDelivered: (entry: OutboxEntry) => void;
  onRefused: (entry: OutboxEntry, error: Error) => void;
}

/**
 * Sends a server's outbox in order. A connection failure stops the run (what is after it waits too, so the order of
 * the messages is kept); a refused message is dropped and the next one is sent.
 */
export async function drainOutbox(input: DrainInput): Promise<"done" | "offline"> {
  for (const entry of input.entries) {
    const outcome = await sendOutboxEntry(entry, input.send);
    if (outcome.kind === "offline") return "offline";
    if (outcome.kind === "refused") input.onRefused(entry, outcome.error);
    else input.onDelivered(entry);
  }
  return "done";
}

export async function sendOutboxEntry(
  entry: OutboxEntry,
  send: (entry: OutboxEntry) => Promise<void>,
): Promise<OutboxSendOutcome> {
  try {
    await send(entry);
    return { kind: "delivered" };
  } catch (error) {
    if (isConnectionFailure(error)) return { kind: "offline" };
    return { kind: "refused", error: error instanceof Error ? error : new Error(String(error)) };
  }
}
