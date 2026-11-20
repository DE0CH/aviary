import { describe, expect, it } from "vitest";
import { sidePanelOpen } from "./side-panel-state";

const shown = { sessionId: "s1", kind: "activity" as const };

describe("sidePanelOpen", () => {
  it("is the chosen panel beside the chat", () => {
    expect(
      sidePanelOpen({
        shown,
        sessionId: "s1",
        kind: "activity",
        compact: false,
        overlayOut: false,
      })
    ).toBe(true);
  });
  it("is not another kind or another conversation's", () => {
    expect(
      sidePanelOpen({
        shown,
        sessionId: "s1",
        kind: "schedules",
        compact: false,
        overlayOut: false,
      })
    ).toBe(false);
    expect(
      sidePanelOpen({
        shown,
        sessionId: "s2",
        kind: "activity",
        compact: false,
        overlayOut: false,
      })
    ).toBe(false);
    expect(
      sidePanelOpen({
        shown: null,
        sessionId: "s1",
        kind: "activity",
        compact: false,
        overlayOut: false,
      })
    ).toBe(false);
  });
  it("on a phone, only while the overlay is out", () => {
    expect(
      sidePanelOpen({
        shown,
        sessionId: "s1",
        kind: "activity",
        compact: true,
        overlayOut: true,
      })
    ).toBe(true);
    expect(
      sidePanelOpen({
        shown,
        sessionId: "s1",
        kind: "activity",
        compact: true,
        overlayOut: false,
      })
    ).toBe(false);
  });
});
