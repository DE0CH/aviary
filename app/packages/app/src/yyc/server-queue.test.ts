import { describe, expect, it } from "vitest";
import {
  LEAVING_SHOWN_MS,
  SENDING_SHOWN_MS,
  leavingQueueItems,
  queuedIdsOnTimeline,
  serverQueueItems,
} from "./server-queue";

describe("the conversation's queue, kept by the server", () => {
  const server = [
    { messageId: "m2", text: "also this", attachments: 0, held: false },
    { messageId: "m3", text: "and this file", attachments: 1, held: true },
  ];

  it("lists what the server holds, in its order", () => {
    expect(serverQueueItems(server, [], 0)).toEqual([
      { id: "m2", text: "also this", held: false, attachments: 0, sending: false },
      { id: "m3", text: "and this file", held: true, attachments: 1, sending: false },
    ]);
    expect(serverQueueItems(undefined, [], 0)).toEqual([]);
  });

  it("shows a message this page just sent until the server's list has it", () => {
    const sent = [{ id: "m4", text: "one more", attachments: 0, sentAtMs: 1000 }];
    // not in the server's list yet: shown at once, after the server's
    expect(serverQueueItems(server, sent, 1200).map((m) => [m.id, m.sending])).toEqual([
      ["m2", false],
      ["m3", false],
      ["m4", true],
    ]);
    // the server has it now: once, from the server
    const withIt = [...server, { messageId: "m4", text: "one more", attachments: 0, held: false }];
    expect(serverQueueItems(withIt, sent, 1300).filter((m) => m.id === "m4")).toEqual([
      { id: "m4", text: "one more", held: false, attachments: 0, sending: false },
    ]);
    // never listed by the server (taken into the reply at once): gone after a moment
    expect(serverQueueItems(server, sent, 1000 + SENDING_SHOWN_MS).map((m) => m.id)).toEqual(["m2", "m3"]);
  });

  // The server's list and the chat's rows come on two feeds, in either order (Deyao, 2026-11-13: the queued message
  // did not leave the queue at the moment it showed in the chat).
  it("leaves the queue at the moment its row is in the chat, in either order of the two feeds", () => {
    const none = new Set<string>();
    // the row came first: the server's list still holds the message, the queue no longer shows it
    expect(serverQueueItems(server, [], 0, new Set(["m2"])).map((m) => m.id)).toEqual(["m3"]);
    // the list dropped it first: it stays listed until its row comes
    const after = [server[1]];
    const leaving = leavingQueueItems([], server, after, none, none, 5000);
    expect(leaving).toEqual([{ id: "m2", text: "also this", attachments: 0, leftAtMs: 5000 }]);
    expect(serverQueueItems(after, [], 5100, none, leaving).map((m) => [m.id, m.held])).toEqual([
      ["m3", true],
      ["m2", false],
    ]);
    // its row is in the chat: gone from the queue, and no longer waited for
    expect(serverQueueItems(after, [], 5200, new Set(["m2"]), leaving).map((m) => m.id)).toEqual(["m3"]);
    expect(leavingQueueItems(leaving, after, after, new Set(["m2"]), none, 5200)).toEqual([]);
    // a row that never comes is not waited for for ever
    expect(
      serverQueueItems(after, [], 5000 + LEAVING_SHOWN_MS, none, leaving).map((m) => m.id),
    ).toEqual(["m3"]);
    // a message this page took back is not kept
    expect(leavingQueueItems([], server, [server[0]], none, new Set(["m3"]), 0)).toEqual([]);
    // nothing changed: the same list (no re-render)
    expect(leavingQueueItems(leaving, after, after, none, none, 5300)).toBe(leaving);
    // a message this page sent shows once, in the chat
    const sent = [{ id: "m4", text: "one more", attachments: 0, sentAtMs: 1000 }];
    expect(serverQueueItems([], sent, 1100, new Set(["m4"]))).toEqual([]);
  });

  it("finds the queued messages' rows among the chat's newest rows", () => {
    const tail = [
      { kind: "user_message", id: "a", clientMessageId: "m1" },
      { kind: "assistant_message", id: "m2" },
      { kind: "user_message", id: "m3" },
    ];
    const head = [{ kind: "user_message", id: "x", messageId: "m4" }];
    expect(queuedIdsOnTimeline(["m2", "m4", "m3", "m9"], tail, head, undefined)).toBe("m4 m3");
    expect(queuedIdsOnTimeline([], tail)).toBe("");
  });
});
