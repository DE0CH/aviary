import { describe, expect, it } from "vitest";
import { SENDING_SHOWN_MS, serverQueueItems } from "./server-queue";

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
});
