import { afterEach, describe, expect, it, vi } from "vitest";

// A grammar fetch that fails is retried while code is still waiting for it (its component's effect never runs again)
let grammarCalls = 0;
vi.mock("@getpaseo/highlight", () => ({
  isLanguageSupported: () => true,
  isGrammarLoaded: () => grammarCalls >= 2,
  loadGrammarForFile: () => {
    grammarCalls += 1;
    return grammarCalls === 1 ? Promise.reject(new Error("chunk failed")) : Promise.resolve({});
  },
}));

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("highlight grammars after a failed fetch", () => {
  it("fetches again with backoff and tells the waiting code", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    vi.useFakeTimers();
    const { __private__, isGrammarReady, requestGrammar } = await import("./highlight-grammars");
    const listener = vi.fn();
    const unsubscribe = __private__.subscribe(listener);

    requestGrammar("py");
    await vi.advanceTimersByTimeAsync(0);
    expect(grammarCalls).toBe(1);
    expect(listener).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1000);
    expect(grammarCalls).toBe(2);
    expect(isGrammarReady("py")).toBe(true);
    expect(listener).toHaveBeenCalled();
    unsubscribe();
  });
});
