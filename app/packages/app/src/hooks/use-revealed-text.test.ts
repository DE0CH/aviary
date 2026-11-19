import { describe, expect, it } from "vitest";
import { useRevealedText } from "./use-revealed-text";

// Deyao, 2026-11-13: the streaming must not be faked. The text on screen is the text that has arrived, at once: no
// typing it out over time.
describe("streamed text", () => {
  it("shows all the text that has arrived, while it streams and when it is complete", () => {
    const arrived = "The answer so far, with a long burst that came in one piece.";
    expect(useRevealedText(arrived, "streaming")).toBe(arrived);
    expect(useRevealedText(arrived, "complete")).toBe(arrived);
    expect(useRevealedText("", "streaming")).toBe("");
  });
});
