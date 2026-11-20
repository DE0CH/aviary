import { describe, expect, it } from "vitest";
import { androidAppDownloadUrl } from "./android-app";

const ORIGIN = "https://ai.example";
const ANDROID_CHROME =
  "Mozilla/5.0 (Linux; Android 16; SM-F966B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36";
const IPHONE_SAFARI =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 26_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Mobile/15E148 Safari/604.1";
const DESKTOP_CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

describe("the Android app's download row", () => {
  it("shows in an Android browser, pointing at the deployment's own APK", () => {
    expect(androidAppDownloadUrl({ platform: "web", userAgent: ANDROID_CHROME, origin: ORIGIN })).toBe(
      "https://ai.example/apps/android.apk",
    );
  });

  it("shows in the Android app itself, for its updates", () => {
    expect(androidAppDownloadUrl({ platform: "android", userAgent: null, origin: ORIGIN })).toBe(
      "https://ai.example/apps/android.apk",
    );
  });

  it("does not show on an iPhone, a desktop browser or the iPhone app", () => {
    expect(androidAppDownloadUrl({ platform: "web", userAgent: IPHONE_SAFARI, origin: ORIGIN })).toBeNull();
    expect(androidAppDownloadUrl({ platform: "web", userAgent: DESKTOP_CHROME, origin: ORIGIN })).toBeNull();
    expect(androidAppDownloadUrl({ platform: "ios", userAgent: null, origin: ORIGIN })).toBeNull();
  });

  it("does not show without a known server", () => {
    expect(androidAppDownloadUrl({ platform: "android", userAgent: null, origin: null })).toBeNull();
  });
});
