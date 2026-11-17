import { describe, expect, it } from "vitest";
import { DaemonConnectionError } from "@getpaseo/client/internal/daemon-client";
import { createUserMessage, type StreamItem } from "@/types/stream";
import {
  DELIVERED_SHOWN_MS,
  PENDING_WAIT_MS,
  afterLoad,
  drainOutbox,
  isConnectionFailure,
  outboxRows,
  pendingPhase,
  referencedImageIds,
  unconfirmedIds,
  unsentEntries,
  type OutboxEntry,
} from "./outbox";

function entry(id: string, over: Partial<OutboxEntry> = {}): OutboxEntry {
  return {
    id,
    serverId: "s",
    agentId: "a",
    text: `text ${id}`,
    images: [],
    attachments: [],
    queued: false,
    createdAtMs: 1000,
    ...over,
  };
}

describe("the outbox: messages the server has not confirmed", () => {
  it("tells a failed connection from the server's refusal", () => {
    expect(isConnectionFailure(new DaemonConnectionError("Transport not connected"))).toBe(true);
    expect(
      isConnectionFailure(new DaemonConnectionError("Timeout waiting for message (60000ms)", "DAEMON_REQUEST_TIMEOUT")),
    ).toBe(true);
    expect(isConnectionFailure(new Error("Transport not connected (status: disconnected)"))).toBe(true);
    expect(isConnectionFailure(new TypeError("Failed to fetch"))).toBe(true);
    // the server's answer: refused, not retried
    expect(isConnectionFailure(new Error("budget_exceeded"))).toBe(false);
    expect(isConnectionFailure(new Error("sendAgentMessage rejected"))).toBe(false);
    expect(isConnectionFailure("offline")).toBe(false);
  });

  it("sends in order, stops at a failed connection and keeps the rest for later", async () => {
    const sent: string[] = [];
    const delivered: string[] = [];
    let online = true;
    const send = async (e: OutboxEntry) => {
      if (!online) throw new DaemonConnectionError("Transport not connected");
      sent.push(e.id);
      if (e.id === "m2") online = false;
    };
    const entries = [entry("m1", { createdAtMs: 1 }), entry("m2", { createdAtMs: 2 }), entry("m3", { createdAtMs: 3 })];
    const first = await drainOutbox({
      entries,
      send,
      onDelivered: (e) => delivered.push(e.id),
      onRefused: () => {
        throw new Error("nothing is refused here");
      },
    });
    expect(first).toBe("offline");
    expect(delivered).toEqual(["m1", "m2"]);
    // the connection is back: the one that waited goes, with its own id (the server admits an id once)
    online = true;
    const second = await drainOutbox({
      entries: entries.filter((e) => !delivered.includes(e.id)),
      send,
      onDelivered: (e) => delivered.push(e.id),
      onRefused: () => undefined,
    });
    expect(second).toBe("done");
    expect(sent).toEqual(["m1", "m2", "m3"]);
  });

  it("drops a message the server refuses and goes on with the next", async () => {
    const refused: Array<[string, string]> = [];
    const delivered: string[] = [];
    const result = await drainOutbox({
      entries: [entry("m1"), entry("m2")],
      send: async (e) => {
        if (e.id === "m1") throw new Error("budget_exceeded");
      },
      onDelivered: (e) => delivered.push(e.id),
      onRefused: (e, error) => refused.push([e.id, error.message]),
    });
    expect(result).toBe("done");
    expect(refused).toEqual([["m1", "budget_exceeded"]]);
    expect(delivered).toEqual(["m2"]);
  });

  it("lists a server's unsent messages oldest first", () => {
    const entries = [
      entry("late", { createdAtMs: 30 }),
      entry("other-server", { serverId: "t", createdAtMs: 10 }),
      entry("early", { createdAtMs: 20 }),
      entry("delivered", { createdAtMs: 5, deliveredAtMs: 6 }),
    ];
    expect(unsentEntries(entries, "s").map((e) => e.id)).toEqual(["early", "late"]);
  });

  it("draws a chat row for a message whose row is missing, and never a second one", () => {
    const entries = [entry("m1", { createdAtMs: 100 }), entry("m2", { createdAtMs: 200 })];
    const tail: StreamItem[] = [
      createUserMessage({ clientMessageId: "m1", text: "text m1", timestamp: new Date(100) }),
    ];
    // m1 is drawn by the send already; m2's row was lost (a reload, a history load after a reconnect)
    const rows = outboxRows(entries, "s", "a", 300, tail);
    expect(rows.map((r) => [r.clientMessageId, r.text, r.timestamp.getTime()])).toEqual([["m2", "text m2", 200]]);
    // the server's row names the message by its id too
    const fromServer = createUserMessage({ id: "srv-9", messageId: "m2", text: "text m2", timestamp: new Date(200) });
    expect(outboxRows(entries, "s", "a", 300, tail, [fromServer])).toEqual([]);
  });

  it("keeps a message sent during a reply out of the chat (it waits in the queue above the message box)", () => {
    expect(outboxRows([entry("q", { queued: true })], "s", "a", 0)).toEqual([]);
  });

  it("draws a delivered message only for a moment while its row has not come", () => {
    const delivered = [entry("m1", { deliveredAtMs: 1000 })];
    expect(outboxRows(delivered, "s", "a", 1000 + DELIVERED_SHOWN_MS - 1).length).toBe(1);
    expect(outboxRows(delivered, "s", "a", 1000 + DELIVERED_SHOWN_MS)).toEqual([]);
  });

  it("marks only the messages the server has not answered as unconfirmed", () => {
    const entries = [entry("m1"), entry("m2", { deliveredAtMs: 5 }), entry("m3", { agentId: "b" })];
    expect(unconfirmedIds(entries, "s", "a")).toEqual(["m1"]);
  });

  it("says it is sending, then after a few seconds that it waits for the network", () => {
    expect(pendingPhase(1000, 1000)).toBe("sending");
    expect(pendingPhase(1000, 1000 + PENDING_WAIT_MS - 1)).toBe("sending");
    expect(pendingPhase(1000, 1000 + PENDING_WAIT_MS)).toBe("waiting");
  });

  it("keeps unsent messages across a reload and the images they need", () => {
    const image = { id: "img-1", mimeType: "image/png", storageType: "web-indexeddb", storageKey: "k", createdAt: 1 };
    const loaded = afterLoad([
      entry("m1", { images: [image as never] }),
      entry("m2", { deliveredAtMs: 3 }),
    ]);
    expect(loaded.map((e) => e.id)).toEqual(["m1"]);
    expect(referencedImageIds(loaded)).toEqual(["img-1"]);
  });
});
