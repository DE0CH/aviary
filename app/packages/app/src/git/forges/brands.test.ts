import { describe, expect, it } from "vitest";
import { CLIENT_FORGE_BRANDS } from "./brands";
import { CLIENT_FORGE_VIEW_MODULES } from "./view";

describe("CLIENT_FORGE_BRANDS", () => {
  it("has every forge view's mark and colour, in the same order", () => {
    const marks = (modules: typeof CLIENT_FORGE_BRANDS) =>
      modules.map(({ id, icon, brandColor }) => ({ id, icon, brandColor }));
    expect(marks(CLIENT_FORGE_BRANDS)).toEqual(marks(CLIENT_FORGE_VIEW_MODULES));
  });
});
