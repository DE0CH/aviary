import { describe, expect, it, vi } from "vitest";

const reply = vi.hoisted(() => ({ attempts: 0 }));

vi.mock("@getpaseo/client/internal/daemon-client", () => ({
  loadWSOutboundValidator: () => new Promise(() => undefined),
}));
vi.mock("@/constants/platform", () => ({ isWeb: true }));
vi.mock("@/utils/highlight-grammars", () => ({ loadHighlighter: () => Promise.resolve({}) }));
vi.mock("./reply-chunk", () => {
  reply.attempts += 1;
  if (reply.attempts === 1) throw new Error("offline");
  return { AgentStreamView: () => null, MarkdownRenderer: () => null };
});

const stages = await import("./stages");

describe("loading stages after a failed fetch", () => {
  it("fetches the reply chunk again on the next try", async () => {
    await expect(stages.loadReplyChunk()).rejects.toThrow();
    await expect(stages.loadReplyChunk()).resolves.toHaveProperty("MarkdownRenderer");
    expect(reply.attempts).toBe(2);
  });
});
