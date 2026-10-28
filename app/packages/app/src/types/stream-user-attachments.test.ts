import { describe, expect, it } from "vitest";
import type { AgentStreamEventPayload } from "@getpaseo/protocol/messages";
import { hydrateStreamState } from "./stream";

const file = { type: "uploaded_file" as const, id: "uploads/a.pdf", fileName: "a.pdf", mimeType: "application/pdf", size: 3, path: "uploads/a.pdf" };

function timelineUser(item: Record<string, unknown>): { event: AgentStreamEventPayload; timestamp: Date } {
  return {
    event: { type: "timeline", provider: "claude", item: { type: "user_message", ...item } } as AgentStreamEventPayload,
    timestamp: new Date("2026-10-28T03:00:00Z"),
  };
}

describe("timeline user messages with attachments (YYC: history keeps what the user attached)", () => {
  it("shows the images as remote-url attachments and keeps the files", () => {
    const [item] = hydrateStreamState([
      timelineUser({ text: "see", messageId: "m1", images: [{ url: "data:image/png;base64,AAAA", mimeType: "image/png" }], attachments: [file] }),
    ]);
    expect(item?.kind).toBe("user_message");
    if (item?.kind !== "user_message") return;
    expect(item.images).toHaveLength(1);
    expect(item.images?.[0]).toMatchObject({ storageType: "remote-url", storageKey: "data:image/png;base64,AAAA", mimeType: "image/png" });
    expect(item.attachments).toEqual([file]);
  });

  it("keeps a message that has only an image", () => {
    const items = hydrateStreamState([timelineUser({ text: "", messageId: "m2", images: [{ url: "data:image/jpeg;base64,BB", mimeType: "image/jpeg" }] })]);
    expect(items).toHaveLength(1);
  });
});
