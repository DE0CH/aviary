import { describe, expect, it } from "vitest";
import type { StreamItem } from "@/types/stream";
import { oneItemPerId } from "./one-item-per-id";

const at = new Date("2026-11-19T02:23:00Z");
const item = (id: string, text: string): StreamItem => ({ kind: "assistant_message", id, text, timestamp: at });
const show = (items: StreamItem[]) => items.map((i) => `${i.id}:${(i as { text: string }).text}`);

describe("one item per id (a question card drew twice, 2026-11-19)", () => {
  it("puts the newest copy in the first copy's place in the history", () => {
    const tail = [item("a", "1"), item("q", "open"), item("b", "2"), item("q", "answered"), item("c", "3")];
    expect(show(oneItemPerId(tail, []).tail)).toEqual(["a:1", "q:answered", "b:2", "c:3"]);
  });

  it("keeps a live copy over a history one", () => {
    const r = oneItemPerId([item("a", "1"), item("q", "old")], [item("q", "new")]);
    expect(show(r.tail)).toEqual(["a:1"]);
    expect(show(r.head)).toEqual(["q:new"]);
  });

  it("returns the very inputs when nothing repeats", () => {
    const tail = [item("a", "1"), item("b", "2")];
    const head = [item("c", "3")];
    const r = oneItemPerId(tail, head);
    expect(r.tail).toBe(tail);
    expect(r.head).toBe(head);
  });
});
