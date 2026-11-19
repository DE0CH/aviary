import { afterEach, describe, expect, it } from "vitest";
import { parseViewAsToken, useViewAsStore, viewAsReadOnly, withViewAs } from "./view-as";

const b64url = (value: string) => btoa(value).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const token = `${b64url(JSON.stringify({ a: "admin@yyc.hk", u: "staff@yyc.hk", s: "v1", e: 2_000_000_000 }))}.c2ln`;

describe("view as", () => {
  afterEach(() => useViewAsStore.setState({ viewAs: null, override: false }));

  it("reads the person, the admin and the expiry from the token", () => {
    expect(parseViewAsToken(token)).toEqual({ token, user: "staff@yyc.hk", admin: "admin@yyc.hk", expiresAt: 2_000_000_000_000 });
    expect(parseViewAsToken("garbage")).toBeNull();
    expect(parseViewAsToken(`${b64url(JSON.stringify({ a: "x" }))}.sig`)).toBeNull();
  });

  it("adds the token to every URL in a view-as tab, nothing in a normal tab", () => {
    expect(withViewAs("/api/x?y=1")).toBe("/api/x?y=1");
    useViewAsStore.setState({ viewAs: parseViewAsToken(token) });
    expect(withViewAs("/api/x")).toBe(`/api/x?jarvis_view_as=${encodeURIComponent(token)}`);
    expect(withViewAs("wss://h/paseo-ws?a=1#f")).toBe(`wss://h/paseo-ws?a=1&jarvis_view_as=${encodeURIComponent(token)}#f`);
  });

  it("is read-only until the override is switched on", () => {
    expect(viewAsReadOnly()).toBe(false);
    useViewAsStore.setState({ viewAs: parseViewAsToken(token) });
    expect(viewAsReadOnly()).toBe(true);
    useViewAsStore.getState().setOverride(true);
    expect(viewAsReadOnly()).toBe(false);
  });
});
