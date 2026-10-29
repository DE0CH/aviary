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
vi.mock("./reply-chunk", () => {
  chunks.order.push("reply");
  return { AgentStreamView: () => null, MarkdownRenderer: () => null };
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

  it("fetches the reply chunk after the validator, then the sidebar, then reaches the hosts stage", async () => {
    stages.startLoadingStages();
    await Promise.resolve();
    expect(stages.hasReachedStage(1)).toBe(false);
    expect(chunks.order).toEqual([]);
    validator.resolve();
    await stages.waitForStage(2);
    await stages.loadSidebarChunk();
    expect(chunks.order).toEqual(["reply", "sidebar"]);
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
