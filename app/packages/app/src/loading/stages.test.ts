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

  const fetchError = () => Object.assign(new Error("offline"), { name: "AsyncRequireError" });

  it("tries a failed fetch again by itself", async () => {
    vi.useFakeTimers();
    try {
      const highlight = await import("@/utils/highlight-grammars");
      // loadWithRetry's own three tries fail, then the backoff retry succeeds
      const spy = vi
        .spyOn(highlight, "loadHighlighter")
        .mockRejectedValueOnce(fetchError())
        .mockRejectedValueOnce(fetchError())
        .mockRejectedValueOnce(fetchError())
        .mockResolvedValue({} as never);
      const first = stages.loadReplyDisplay();
      const settled = expect(first).rejects.toThrow("offline");
      await vi.advanceTimersByTimeAsync(2_000);
      await settled;
      expect(stages.isReplyDisplayReady()).toBe(false);
      expect(stages.__private__.isReplyDisplayFailed()).toBe(false);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(stages.isReplyDisplayReady()).toBe(true);
      expect(spy).toHaveBeenCalledTimes(4);
      spy.mockRestore();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports a failure after three failed rounds and keeps trying; back online tries at once", async () => {
    vi.useFakeTimers();
    try {
      const highlight = await import("@/utils/highlight-grammars");
      let failing = true;
      const spy = vi.spyOn(highlight, "loadHighlighter").mockImplementation(() =>
        failing ? Promise.reject(fetchError()) : Promise.resolve({} as never),
      );
      void stages.loadReplyDisplay().catch(() => undefined);
      // round 1 (2 s of in-round retries), +1 s, round 2, +2 s, round 3
      await vi.advanceTimersByTimeAsync(2_000 + 1_000 + 2_000 + 2_000 + 2_000);
      expect(stages.__private__.isReplyDisplayFailed()).toBe(true);
      expect(stages.isReplyDisplayReady()).toBe(false);
      failing = false;
      await vi.advanceTimersByTimeAsync(30_000);
      expect(stages.isReplyDisplayReady()).toBe(true);
      expect(stages.__private__.isReplyDisplayFailed()).toBe(false);
      spy.mockRestore();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports an error the chunk threw at once and does not fetch it again", async () => {
    vi.useFakeTimers();
    try {
      const highlight = await import("@/utils/highlight-grammars");
      const spy = vi
        .spyOn(highlight, "loadHighlighter")
        .mockRejectedValue(new TypeError("x is not a function"));
      await expect(stages.loadReplyDisplay()).rejects.toThrow("x is not a function");
      expect(stages.__private__.isReplyDisplayFailed()).toBe(true);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(spy).toHaveBeenCalledTimes(1);
      spy.mockRestore();
    } finally {
      vi.useRealTimers();
    }
  });
});
