import { describe, expect, it } from "vitest";
import { shownSelection } from "./tapped-selection";

const a = { serverId: "s", workspaceId: "a" };
const b = { serverId: "s", workspaceId: "b" };
const c = { serverId: "s", workspaceId: "c" };

describe("shownSelection", () => {
  it("is the route when nothing was tapped", () => {
    expect(shownSelection(a, null)).toEqual(a);
  });
  it("is the tapped row while the route has not moved", () => {
    expect(shownSelection(a, { from: a, to: b })).toEqual(b);
    expect(shownSelection(null, { from: null, to: b })).toEqual(b);
  });
  it("is the route once it has moved, to the tapped row or elsewhere", () => {
    expect(shownSelection(b, { from: a, to: b })).toEqual(b);
    expect(shownSelection(c, { from: a, to: b })).toEqual(c);
  });
});
