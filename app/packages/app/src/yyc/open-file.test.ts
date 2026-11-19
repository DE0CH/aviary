import { beforeEach, describe, expect, it, vi } from "vitest";

// The iOS app shows the history's images, which the server names by path (`/api/blobs/…`): a path alone loads
// nothing in the app, and the server is behind Cloudflare Access (an image that came back as "Couldn't load image").
const platform = vi.hoisted(() => ({ isWeb: false, isNative: true }));
vi.mock("@/constants/platform", () => platform);
vi.mock("expo-constants", () => ({
  default: { expoConfig: { extra: { yycSite: "https://chat.example.org/" }, scheme: "x" } },
}));
vi.mock("expo-secure-store", () => ({
  getItemAsync: async () => null,
  setItemAsync: async () => undefined,
  deleteItemAsync: async () => undefined,
}));
vi.mock("expo-file-system/legacy", () => ({ cacheDirectory: "/tmp/", downloadAsync: vi.fn() }));
vi.mock("expo-sharing", () => ({ shareAsync: vi.fn() }));
vi.mock("react-native", () => ({ Linking: { openURL: vi.fn() } }));

const { serverImageSource } = await import("./open-file");
const { saveAccessToken } = await import("./access-token");

describe("serverImageSource", () => {
  beforeEach(async () => {
    platform.isWeb = false;
    await saveAccessToken("tok");
  });

  it("reads a server path from the server, with the app's Access token", () => {
    expect(serverImageSource("/api/blobs/b1?v=display&sig=s")).toEqual({
      uri: "https://chat.example.org/api/blobs/b1?v=display&sig=s",
      headers: { "cf-access-token": "tok", Cookie: "CF_Authorization=tok" },
    });
  });

  it("sends the token with a full URL on the server too, and to nobody else", () => {
    expect(serverImageSource("https://chat.example.org/api/x").headers).toBeDefined();
    expect(serverImageSource("https://elsewhere.example.org/a.png")).toEqual({
      uri: "https://elsewhere.example.org/a.png",
    });
    expect(serverImageSource("https://chat.example.org.evil.test/a.png")).toEqual({
      uri: "https://chat.example.org.evil.test/a.png",
    });
  });

  it("loads a local file, blob or data URL as it is", () => {
    for (const uri of ["file:///var/a.png", "blob:x", "data:image/png;base64,AA", "//cdn.example.org/a.png"]) {
      expect(serverImageSource(uri)).toEqual({ uri });
    }
  });
});
