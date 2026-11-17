import { describe, expect, it } from "vitest";
import { brandLogoSource } from "./native-brand";

const uri = "https://example.org/branding/logo.png";
const headers = { "cf-access-token": "t", Cookie: "CF_Authorization=t" };

describe("brandLogoSource", () => {
  it("is the server's logo on the web", () => {
    expect(brandLogoSource({ native: false, uri, headers: {}, bundled: 7 })).toEqual({ uri });
    expect(brandLogoSource({ native: false, uri: null, headers: {}, bundled: 7 })).toBeNull();
  });

  it("is the build's logo in a native app while nobody is signed in (the server's may be behind Access)", () => {
    expect(brandLogoSource({ native: true, uri, headers: {}, bundled: 7 })).toBe(7);
  });

  it("is the server's logo, asked for with the Access token, once signed in", () => {
    expect(brandLogoSource({ native: true, uri, headers, bundled: 7 })).toEqual({ uri, headers });
  });

  it("falls back to what there is", () => {
    expect(brandLogoSource({ native: true, uri, headers: {}, bundled: null })).toEqual({ uri });
    expect(brandLogoSource({ native: true, uri: null, headers, bundled: 7 })).toBe(7);
    expect(brandLogoSource({ native: true, uri: null, headers: {}, bundled: null })).toBeNull();
  });
});
