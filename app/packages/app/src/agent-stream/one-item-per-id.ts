import type { StreamItem } from "@/types/stream";

/**
 * One item per id in what the conversation draws (Deyao, 2026-11-19: one question card drawn twice, and the list
 * scrolling wrongly on the repeated key). However a second copy of an item comes about (an item whose identity changed
 * after it was made, a page merged into what was already there), the newest copy stands in the first copy's place in
 * the history, and a live copy wins over a history one. Returns the inputs themselves when nothing repeats.
 */
export function oneItemPerId(tail: StreamItem[], head: StreamItem[]): { tail: StreamItem[]; head: StreamItem[] } {
  const live = new Set(head.map((item) => item.id));
  const first = new Map<string, number>();
  let out: StreamItem[] | null = null;
  tail.forEach((item, index) => {
    const at = first.get(item.id);
    if (live.has(item.id) || at !== undefined) {
      out ??= tail.slice(0, index);
      if (at !== undefined && !live.has(item.id)) out[at] = item;
      return;
    }
    first.set(item.id, out ? out.length : index);
    out?.push(item);
  });
  const seen = new Set<string>();
  const dedupedHead = head.filter((item) => !seen.has(item.id) && seen.add(item.id));
  return {
    tail: out ?? tail,
    head: dedupedHead.length === head.length ? head : dedupedHead,
  };
}
