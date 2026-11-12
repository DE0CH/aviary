import type { AgentPendingMessage } from "@getpaseo/protocol/messages";
import { api } from "@/yyc/api";

/**
 * The conversation's queue of messages sent while a reply runs, where the server keeps it (yyc/me.ts `midTurn`:
 * aviary's magpie). The app sends such a message at once and shows the server's list above the message box; nothing
 * is queued in the app, so the queue is the same on every device and survives a reload.
 */

/** A message of the queue as the composer lists it. */
export interface ServerQueueItem {
  id: string;
  text: string;
  /** It waits for the reply to end and can be taken back. False: it is on its way into the running reply. */
  held: boolean;
  /** How many images and files it carries. */
  attachments: number;
  /** Sent from this page and not in the server's list yet (shown at once, so nothing blinks). */
  sending: boolean;
}

/** A message this page has just sent mid-reply. */
export interface SentMidTurn {
  id: string;
  text: string;
  attachments: number;
  sentAtMs: number;
}

/** How long a message this page sent is listed before the server's list has it (or its row has come). */
export const SENDING_SHOWN_MS = 4000;

/**
 * The queue to show: the server's list, then the messages this page sent that the server's list does not hold yet
 * (for at most `SENDING_SHOWN_MS`: after that the message has been taken into the reply, or the send failed and
 * its error is shown).
 */
export function serverQueueItems(
  server: readonly AgentPendingMessage[] | undefined,
  sent: readonly SentMidTurn[],
  nowMs: number,
): ServerQueueItem[] {
  const listed = new Set((server ?? []).map((m) => m.messageId));
  const items: ServerQueueItem[] = (server ?? []).map((m) => ({
    id: m.messageId,
    text: m.text,
    held: m.held === true,
    attachments: m.attachments ?? 0,
    sending: false,
  }));
  for (const m of sent) {
    if (listed.has(m.id) || nowMs - m.sentAtMs >= SENDING_SHOWN_MS) continue;
    items.push({ id: m.id, text: m.text, held: false, attachments: m.attachments, sending: true });
  }
  return items;
}

/** Takes a held message back (the server forgets it). Rejects when it is already on its way into the reply. */
export async function withdrawQueuedMessage(agentId: string, messageId: string): Promise<void> {
  await api(`/api/jarvis/sessions/${encodeURIComponent(agentId)}/pending/${encodeURIComponent(messageId)}`, {
    method: "DELETE",
  });
}
