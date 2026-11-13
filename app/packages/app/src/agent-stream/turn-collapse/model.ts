import { isScheduleCallDetail } from "@/yyc/schedules-text";
import { getStreamItemMessageId, type StreamItem, type ToolCallItem } from "@/types/stream";
import { summarizeToolCalls, type OverviewSummary } from "@/tool-calls/detail-level/overview/model";

export interface TurnSummary {
  messageCount: number;
  tools: OverviewSummary;
}

export interface CollapsedTurn {
  /**
   * The row that carries the turn's summary line: the user message the turn answers, under it; or, for a turn whose
   * prompt is above the loaded history (`headless`), the turn's reply, above it.
   */
  hostId: string;
  expanded: boolean;
  summary: TurnSummary;
  /** The turn's prompt is not loaded: the summary counts the loaded part and sits above the reply. */
  headless?: true;
}

export interface TurnCollapse {
  tail: StreamItem[];
  turnsByHostId: ReadonlyMap<string, CollapsedTurn>;
  /** Rows of expanded turns, which render inside the turn's indent rail. */
  nestedItemIds: ReadonlySet<string>;
  /** The turn a folded message belongs to, for surfaces that must reveal it. */
  hostIdByFoldedMessageId: ReadonlyMap<string, string>;
}

export interface CollapseTurnsInput {
  tail: StreamItem[];
  /** The latest turn is still producing output, so it stays unfolded. */
  isLatestTurnOpen: boolean;
  expandedHostIds: ReadonlySet<string>;
  /** The calls a tool-call group row stands for; undefined for a single call. */
  getGroupedCalls: (itemId: string) => readonly ToolCallItem[] | undefined;
}

const EMPTY_TURNS: ReadonlyMap<string, CollapsedTurn> = new Map();
const EMPTY_IDS: ReadonlySet<string> = new Set();
const EMPTY_HOSTS: ReadonlyMap<string, string> = new Map();

function summarizeTurn(
  items: readonly StreamItem[],
  getGroupedCalls: CollapseTurnsInput["getGroupedCalls"],
): TurnSummary {
  const calls: ToolCallItem[] = [];
  let messageCount = 0;
  for (const item of items) {
    if (item.kind === "assistant_message") messageCount += 1;
    else if (item.kind === "tool_call") calls.push(...(getGroupedCalls(item.id) ?? [item]));
  }
  return { messageCount, tools: summarizeToolCalls(calls) };
}

function isSentFileCall(item: StreamItem): boolean {
  return (
    item.kind === "tool_call" &&
    item.payload.source === "agent" &&
    (item.payload.data.name === "SendUserFile" || isScheduleCallDetail(item.payload.data.detail))
  );
}

function hasWork(summary: TurnSummary): boolean {
  return summary.messageCount > 0 || Object.values(summary.tools).some((count) => count > 0);
}

function findLastAssistantIndex(tail: StreamItem[], start: number, end: number): number {
  for (let index = end - 1; index >= start; index -= 1) {
    if (tail[index]?.kind === "assistant_message") return index;
  }
  return -1;
}

/**
 * Folds each answered turn into its user message: everything the agent did before its last
 * message collapses into one summary line, and that message stays in the chat. A turn folds
 * only once it is complete and it sent messages or called tools before its reply. Thoughts fold
 * with the turn but are not counted.
 *
 * The turn at the top of the loaded history, whose prompt is on a page not loaded yet, folds the
 * same way into its reply (the line above it): a long turn is pages of steps, and showing them one
 * by one until the prompt arrived, then folding them all at once, made scrolling back slow and jumpy.
 */
export function collapseTurns(input: CollapseTurnsInput): TurnCollapse {
  const { tail } = input;
  const output: StreamItem[] = [];
  const turnsByHostId = new Map<string, CollapsedTurn>();
  const nestedItemIds = new Set<string>();
  const hostIdByFoldedMessageId = new Map<string, string>();

  let runStart = 0;
  const flushRun = (runEnd: number) => {
    const prompt = tail[runStart - 1];
    const isClosed = runEnd < tail.length || !input.isLatestTurnOpen;
    const replyIndex = findLastAssistantIndex(tail, runStart, runEnd);
    // the top of the loaded history: the run's prompt is above it, its reply carries the line
    const headless = runStart === 0 && prompt === undefined;
    const host = headless ? tail[replyIndex] : prompt;
    if (
      (host?.kind !== "user_message" && !headless) ||
      host === undefined ||
      !isClosed ||
      replyIndex <= runStart
    ) {
      output.push(...tail.slice(runStart, runEnd));
      return;
    }
    // Files Claude sent and its schedules cards stay in the chat under the reply when the turn folds (docs/paseo-ui.md)
    const before = tail.slice(runStart, replyIndex);
    const pinned = before.filter(isSentFileCall);
    const folded = pinned.length > 0 ? before.filter((item) => !isSentFileCall(item)) : before;
    const summary = summarizeTurn(folded, input.getGroupedCalls);
    if (!hasWork(summary)) {
      output.push(...tail.slice(runStart, runEnd));
      return;
    }
    const expanded = input.expandedHostIds.has(host.id);
    turnsByHostId.set(host.id, { hostId: host.id, expanded, summary, ...(headless ? { headless } : {}) });
    for (const item of folded) {
      hostIdByFoldedMessageId.set(getStreamItemMessageId(item), host.id);
      if (expanded) nestedItemIds.add(item.id);
    }
    if (expanded) output.push(...folded);
    output.push(...tail.slice(replyIndex, runEnd));
    output.push(...pinned);
  };

  for (let index = 0; index < tail.length; index += 1) {
    if (tail[index]?.kind !== "user_message") continue;
    flushRun(index);
    output.push(tail[index]!);
    runStart = index + 1;
  }
  flushRun(tail.length);

  if (turnsByHostId.size === 0) {
    return {
      tail,
      turnsByHostId: EMPTY_TURNS,
      nestedItemIds: EMPTY_IDS,
      hostIdByFoldedMessageId: EMPTY_HOSTS,
    };
  }
  return {
    tail: output.length === tail.length ? tail : output,
    turnsByHostId,
    nestedItemIds,
    hostIdByFoldedMessageId,
  };
}
