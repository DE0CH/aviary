import { beforeEach, describe, expect, it } from "vitest";
import { useSearchJumpStore } from "./search-jump-store";

describe("the search jump", () => {
  beforeEach(() => useSearchJumpStore.setState({ pending: null }));

  it("is taken once, and only by the conversation it is for", () => {
    useSearchJumpStore.getState().request({ agentId: "a", seq: 12 });
    expect(useSearchJumpStore.getState().take("b")).toBeNull();
    expect(useSearchJumpStore.getState().take("a")).toEqual({ agentId: "a", seq: 12 });
    expect(useSearchJumpStore.getState().take("a")).toBeNull();
  });

  it("keeps only the last message chosen", () => {
    useSearchJumpStore.getState().request({ agentId: "a", seq: 1 });
    useSearchJumpStore.getState().request({ agentId: "b", seq: 2 });
    expect(useSearchJumpStore.getState().take("a")).toBeNull();
    expect(useSearchJumpStore.getState().take("b")?.seq).toBe(2);
  });
});
