import { describe, expect, it } from "vitest";
import { heightMotion, withLeaving } from "./ease-height-motion";

describe("heightMotion", () => {
  const base = { to: 120, first: false, fromZero: false, curve: "ease" as const, reduceMotion: false };

  it("eases a change of height over 240 ms", () => {
    expect(heightMotion(base)).toEqual({ kind: "timing", to: 120, duration: 240 });
  });

  it("shows a row that was already there at its height at once (history, a remount)", () => {
    expect(heightMotion({ ...base, first: true })).toEqual({ kind: "set", to: 120 });
  });

  it("eases a new row in from zero when asked to", () => {
    expect(heightMotion({ ...base, first: true, fromZero: true })).toEqual({ kind: "timing", to: 120, duration: 240 });
  });

  it("follows on the follow spring and opens on the open spring", () => {
    expect(heightMotion({ ...base, curve: "follow" })).toEqual({ kind: "spring", to: 120, spring: "follow" });
    expect(heightMotion({ ...base, curve: "open" })).toEqual({ kind: "spring", to: 120, spring: "open" });
  });

  it("sets every height at once with Reduce Motion on", () => {
    expect(heightMotion({ ...base, reduceMotion: true })).toEqual({ kind: "set", to: 120 });
    expect(heightMotion({ ...base, to: 0, reduceMotion: true })).toEqual({ kind: "set", to: 0 });
  });
});

describe("withLeaving", () => {
  const key = (s: string) => s;

  it("keeps an item that went, marked leaving, where it was", () => {
    const shown = [
      { item: "a", leaving: false },
      { item: "b", leaving: false },
      { item: "c", leaving: false },
    ];
    expect(withLeaving(shown, ["a", "c"], key)).toEqual([
      { item: "a", leaving: false },
      { item: "b", leaving: true },
      { item: "c", leaving: false },
    ]);
  });

  it("adds new items and takes back one that returns", () => {
    const shown = [{ item: "a", leaving: true }];
    expect(withLeaving(shown, ["a", "b"], key)).toEqual([
      { item: "a", leaving: false },
      { item: "b", leaving: false },
    ]);
  });

  it("keeps the newest copy of an item that is still there", () => {
    const shown = [{ item: { id: "a", v: 1 }, leaving: false }];
    expect(withLeaving(shown, [{ id: "a", v: 2 }], (x) => x.id)).toEqual([{ item: { id: "a", v: 2 }, leaving: false }]);
  });
});
