import { describe, expect, it } from "vitest";
import type { AgentStreamEventPayload } from "@getpaseo/protocol/messages";
import { applyStreamEvent, hydrateStreamState } from "./stream";

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

  // The server sends a small rendition beside each image (pigeon's `thumbnailUrl`): the chip in the message draws
  // it, and the viewer shows it at once while the full image comes (Deyao, 2026-11-13).
  it("keeps the image's small rendition for the chip and the viewer", () => {
    const [item] = hydrateStreamState([
      timelineUser({
        text: "see",
        messageId: "m3",
        images: [
          { url: "https://x/blob/1?v=display", mimeType: "image/png", thumbnailUrl: "https://x/blob/1?v=thumb" },
          { url: "data:image/png;base64,AAAA", mimeType: "image/png" },
        ],
      }),
    ]);
    if (item?.kind !== "user_message") throw new Error("no user message");
    expect(item.images?.[0]?.previewKey).toBe("https://x/blob/1?v=thumb");
    expect(item.images?.[1]?.previewKey).toBeUndefined();
  });

  // A message another page sent, or one that waited in the server's queue, has no row of this page's own: the live
  // row is all there is (Deyao, 2026-11-13: a queued message with a picture showed without it until a reload).
  it("shows the images and files of a row that arrives live", () => {
    const { event, timestamp } = timelineUser({
      text: "see",
      messageId: "m9",
      clientMessageId: "m9",
      images: [{ url: "https://x/blob/9?v=display", mimeType: "image/jpeg", thumbnailUrl: "https://x/blob/9?v=thumb" }],
      attachments: [file],
    });
    const { tail } = applyStreamEvent({ tail: [], head: [], event, timestamp });
    const item = tail[0];
    expect(item?.kind).toBe("user_message");
    if (item?.kind !== "user_message") return;
    expect(item.images?.[0]).toMatchObject({ storageKey: "https://x/blob/9?v=display", previewKey: "https://x/blob/9?v=thumb" });
    expect(item.attachments).toEqual([file]);
  });

  it("keeps a live row that has only an image", () => {
    const { event, timestamp } = timelineUser({
      text: "",
      messageId: "m10",
      images: [{ url: "https://x/blob/10?v=display", mimeType: "image/jpeg" }],
    });
    expect(applyStreamEvent({ tail: [], head: [], event, timestamp }).tail).toHaveLength(1);
  });
});
