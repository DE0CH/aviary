import { describe, expect, it } from "vitest";
import {
  conversationWorkspaceId,
  messageResultId,
  messageSearchQuery,
  usableHits,
  type MessageSearchHit,
} from "./message-search-model";

const hit = (over: Partial<MessageSearchHit> = {}): MessageSearchHit => ({
  conversationId: "0190f6c2-0000-7000-8000-000000000002",
  title: "Shuttle bus",
  epoch: "1",
  seq: 9,
  kind: "assistant",
  at: "2026-11-17T08:01:02.000Z",
  snippet: "The shuttle bus leaves every 15 minutes.",
  matched: "meaning",
  ...over,
});

describe("the conversation search's model", () => {
  it("sends a query of two characters or more, trimmed (two characters are a word in Chinese)", () => {
    expect(messageSearchQuery("  ")).toBeNull();
    expect(messageSearchQuery(" a ")).toBeNull();
    expect(messageSearchQuery(" 簽證 ")).toBe("簽證");
    expect(messageSearchQuery("bus timetable")).toBe("bus timetable");
  });

  it("shows only well-formed hits of the query that was asked", () => {
    expect(usableHits({ query: "bus", hits: [hit()] }, "bus")).toHaveLength(1);
    expect(usableHits({ query: "bu", hits: [hit()] }, "bus")).toEqual([]);
    expect(usableHits(null, "bus")).toEqual([]);
    const broken = { ...hit(), seq: "9" } as unknown as MessageSearchHit;
    expect(usableHits({ query: "bus", hits: [broken, hit({ seq: 11 })] }, "bus").map((h) => h.seq)).toEqual([
      11,
    ]);
  });

  it("names one palette row per message and the conversation's workspace", () => {
    expect(messageResultId(hit())).toBe("message:0190f6c2-0000-7000-8000-000000000002:9");
    expect(conversationWorkspaceId("abc")).toBe("wks_abc");
  });
});
