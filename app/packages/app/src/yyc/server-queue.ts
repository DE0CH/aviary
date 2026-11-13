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

/** A message the server's list no longer holds whose row has not shown in the chat yet. */
export interface LeavingQueueItem {
  id: string;
  text: string;
  attachments: number;
  leftAtMs: number;
}

/** How long a message that left the server's list stays listed while its row has not come. */
export const LEAVING_SHOWN_MS = 4000;

const NONE: ReadonlySet<string> = new Set();

/**
 * The queue to show: the server's list, then the messages that have just left it and are not in the chat yet, then
 * the messages this page sent that the server's list does not hold yet (for at most `SENDING_SHOWN_MS`: after that
 * the message has been taken into the reply, or the send failed and its error is shown).
 *
 * The server's list and the chat's rows reach the page on two feeds, in either order. So a message is listed until
 * its row is in the chat (`onTimeline`: the ids of the chat's user messages) and not after: it leaves the queue at
 * the moment it shows in the chat, never before and never while it is already there.
 */
export function serverQueueItems(
  server: readonly AgentPendingMessage[] | undefined,
  sent: readonly SentMidTurn[],
  nowMs: number,
  onTimeline: ReadonlySet<string> = NONE,
  leaving: readonly LeavingQueueItem[] = [],
): ServerQueueItem[] {
  const listed = new Set((server ?? []).map((m) => m.messageId));
  const items: ServerQueueItem[] = (server ?? [])
    .filter((m) => !onTimeline.has(m.messageId))
    .map((m) => ({
      id: m.messageId,
      text: m.text,
      held: m.held === true,
      attachments: m.attachments ?? 0,
      sending: false,
    }));
  for (const m of leaving) {
    if (listed.has(m.id) || onTimeline.has(m.id) || nowMs - m.leftAtMs >= LEAVING_SHOWN_MS) continue;
    items.push({ id: m.id, text: m.text, held: false, attachments: m.attachments, sending: false });
    listed.add(m.id);
  }
  for (const m of sent) {
    if (listed.has(m.id) || onTimeline.has(m.id) || nowMs - m.sentAtMs >= SENDING_SHOWN_MS) continue;
    items.push({ id: m.id, text: m.text, held: false, attachments: m.attachments, sending: true });
  }
  return items;
}

/**
 * The messages that have left the server's list and wait for their row, after the list changed from `before` to
 * `server`: the ones still waiting, and the ones the new list dropped. One that is in the chat, is listed again, was
 * taken back by this page (`withdrawn`) or has waited `LEAVING_SHOWN_MS` is no longer one. Returns `leaving` itself
 * when nothing changed.
 */
export function leavingQueueItems(
  leaving: readonly LeavingQueueItem[],
  before: readonly AgentPendingMessage[] | undefined,
  server: readonly AgentPendingMessage[] | undefined,
  onTimeline: ReadonlySet<string>,
  withdrawn: ReadonlySet<string>,
  nowMs: number,
): readonly LeavingQueueItem[] {
  const listed = new Set((server ?? []).map((m) => m.messageId));
  const gone = (id: string) => listed.has(id) || onTimeline.has(id) || withdrawn.has(id);
  const next = leaving.filter((m) => !gone(m.id) && nowMs - m.leftAtMs < LEAVING_SHOWN_MS);
  const known = new Set(next.map((m) => m.id));
  for (const m of before ?? []) {
    if (gone(m.messageId) || known.has(m.messageId)) continue;
    next.push({ id: m.messageId, text: m.text, attachments: m.attachments ?? 0, leftAtMs: nowMs });
  }
  const same = next.length === leaving.length && next.every((m, i) => m === leaving[i]);
  return same ? leaving : next;
}

/** A chat row as the queue needs it: a user message names the message it shows. */
interface TimelineRow {
  kind: string;
  id: string;
  clientMessageId?: string;
  messageId?: string;
}

/** How many of the chat's newest rows are looked at for a queued message's row. */
const TIMELINE_ROWS_LOOKED_AT = 400;

/**
 * Which of `ids` (queued messages) have their row in the chat, as one string (the ids in `ids`' order, joined by a
 * space): a value that only changes when one of them shows, so the page re-renders then and not with every row.
 */
export function queuedIdsOnTimeline(
  ids: readonly string[],
  ...rows: readonly (readonly TimelineRow[] | undefined)[]
): string {
  if (ids.length === 0) return "";
  const wanted = new Set(ids);
  const found = new Set<string>();
  for (const list of rows) {
    if (!list) continue;
    for (let i = list.length - 1; i >= Math.max(0, list.length - TIMELINE_ROWS_LOOKED_AT); i--) {
      const row = list[i];
      if (row.kind !== "user_message") continue;
      for (const id of [row.clientMessageId, row.messageId, row.id]) {
        if (id !== undefined && wanted.has(id)) found.add(id);
      }
    }
  }
  return ids.filter((id) => found.has(id)).join(" ");
}

/** Takes a held message back (the server forgets it). Rejects when it is already on its way into the reply. */
export async function withdrawQueuedMessage(agentId: string, messageId: string): Promise<void> {
  await api(`/api/jarvis/sessions/${encodeURIComponent(agentId)}/pending/${encodeURIComponent(messageId)}`, {
    method: "DELETE",
  });
}
