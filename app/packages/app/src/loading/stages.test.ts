import { beforeEach, describe, expect, it, vi } from "vitest";

const validator = vi.hoisted(() => {
  let resolve: () => void = () => undefined;
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve: () => resolve() };
});
const chunks = vi.hoisted(() => ({ order: [] as string[] }));

vi.mock("@getpaseo/client/internal/daemon-client", () => ({
  loadWSOutboundValidator: () => validator.promise,
}));
vi.mock("@/constants/platform", () => ({ isWeb: true }));
vi.mock("@/utils/highlight-grammars", () => ({ loadHighlighter: () => Promise.resolve({}) }));
vi.mock("./reply-chunk", () => {
  chunks.order.push("reply");
  return { AgentStreamView: () => null, MarkdownRenderer: () => null };
});
vi.mock("./workspace-chunk", () => {
  chunks.order.push("workspace");
  return { WorkspaceScreen: () => null };
});
vi.mock("./sidebar-chunk", () => {
  chunks.order.push("sidebar");
  return { LeftSidebar: () => null };
});

const stages = await import("./stages");

describe("loading stages", () => {
  beforeEach(() => {
    stages.__private__.reset();
  });

  it("fetches the reply and conversation chunks after the validator, then the sidebar, then the hosts", async () => {
    stages.startLoadingStages();
    await Promise.resolve();
    expect(stages.hasReachedStage(1)).toBe(false);
    expect(chunks.order).toEqual([]);
    validator.resolve();
    await stages.waitForStage(2);
    await stages.loadSidebarChunk();
    expect(chunks.order).toEqual(["reply", "workspace", "sidebar"]);
    await vi.waitFor(() => expect(stages.hasReachedStage(3)).toBe(true));
  });

  it("holds a merely rendered lazy area until its stage", async () => {
    let resolved = false;
    void stages.loadSidebarChunkInTurn().then(() => {
      resolved = true;
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(resolved).toBe(false);
    stages.__private__.reach(2);
    await vi.waitFor(() => expect(resolved).toBe(true));
  });
});

describe("reply display (send waits for it)", () => {
  beforeEach(() => {
    stages.__private__.reset();
  });

  it("is ready only once the reply chunk and the highlighter have arrived, before the conversation page", async () => {
    expect(stages.isReplyDisplayReady()).toBe(false);
    stages.startLoadingStages();
    validator.resolve();
    await stages.waitForStage(1);
    await vi.waitFor(() => expect(stages.isReplyDisplayReady()).toBe(true));
    await stages.waitForStage(2);
  });

  it("tries a failed fetch again by itself", async () => {
    vi.useFakeTimers();
    try {
      const highlight = await import("@/utils/highlight-grammars");
      const spy = vi
        .spyOn(highlight, "loadHighlighter")
        .mockRejectedValueOnce(new Error("offline"))
        .mockResolvedValue({} as never);
      await expect(stages.loadReplyDisplay()).rejects.toThrow("offline");
      expect(stages.isReplyDisplayReady()).toBe(false);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(stages.isReplyDisplayReady()).toBe(true);
      expect(spy).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });
});
