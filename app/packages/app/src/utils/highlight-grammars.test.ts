import { describe, expect, it } from "vitest";
import { tokenizeToLines } from "./highlight-cache";
import { isGrammarReady, loadedHighlighter, requestGrammar } from "./highlight-grammars";

function until(check: () => boolean): Promise<void> {
  return new Promise((resolve) => {
    const poll = () => (check() ? resolve() : setTimeout(poll, 5));
    poll();
  });
}

describe("highlight grammars", () => {
  it("shows code plain until its grammar arrives, fetching only that language", async () => {
    expect(loadedHighlighter()).toBeNull();
    expect(isGrammarReady("py")).toBe(false);
    expect(tokenizeToLines("x = 1", "py")).toBeNull();

    requestGrammar("py");
    await until(() => isGrammarReady("py"));

    expect(tokenizeToLines("x = 1", "py")?.[0].some((token) => token.style !== null)).toBe(true);
    expect(isGrammarReady("rs")).toBe(false);
  });

  it("asks for nothing for a language without a grammar", async () => {
    requestGrammar("txt");
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(isGrammarReady("txt")).toBe(false);
    expect(tokenizeToLines("plain", "txt")).toBeNull();
  });
});
