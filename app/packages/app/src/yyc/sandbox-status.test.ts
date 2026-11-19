import { describe, expect, it } from "vitest";
import { nextPreparing, withoutHost } from "./sandbox-status";

describe("nextPreparing", () => {
  it("shows the chip on preparing and ends it on ready", () => {
    const shown = nextPreparing({}, "h:a", { state: "preparing" }, 1000);
    expect(shown).toEqual({ "h:a": { since: 1000, waiting: null } });
    expect(nextPreparing(shown, "h:a", { state: "ready" }, 2000)).toEqual({});
  });

  it("says a start waits for the person's approval, then preparing from the moment it was approved", () => {
    const waiting = nextPreparing({}, "h:a", { state: "preparing", waiting: "approval" }, 1000);
    expect(waiting["h:a"]).toEqual({ since: 1000, waiting: "approval" });
    // the hint about the making counts from the approval, not from the first message
    const approved = nextPreparing(waiting, "h:a", { state: "preparing" }, 600_000);
    expect(approved["h:a"]).toEqual({ since: 600_000, waiting: null });
  });

  it("keeps the same object for a status that changes nothing, and the clock of a chip that goes on", () => {
    const shown = nextPreparing({}, "h:a", { state: "preparing" }, 1000);
    expect(nextPreparing(shown, "h:a", { state: "preparing" }, 5000)).toBe(shown);
    const none = {};
    expect(nextPreparing(none, "h:a", { state: "ready" }, 5000)).toBe(none);
  });

  it("reads a waiting it does not know as plain preparing", () => {
    const shown = nextPreparing({}, "h:a", { state: "preparing", waiting: "something-new" }, 1000);
    expect(shown["h:a"]).toEqual({ since: 1000, waiting: null });
  });
});

describe("withoutHost", () => {
  it("drops one host's chips and keeps the others'", () => {
    const state = {
      "h:a": { since: 1, waiting: null },
      "h:b": { since: 2, waiting: null },
      "hx:a": { since: 3, waiting: null },
    };
    expect(withoutHost(state, "h")).toEqual({ "hx:a": { since: 3, waiting: null } });
    expect(withoutHost(state, "other")).toBe(state);
  });
});
