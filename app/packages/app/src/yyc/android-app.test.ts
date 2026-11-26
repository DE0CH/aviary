import { describe, expect, it } from "vitest";
import { androidAppDownloadUrl } from "./android-app";

const ORIGIN = "https://ai.example";
const APK = "https://ai.example/apps/android.apk";

describe("the Android app's download row", () => {
  it("shows on every device, pointing at the deployment's own APK (a phone's user agent is not a reliable test)", () => {
    expect(androidAppDownloadUrl({ origin: ORIGIN })).toBe(APK);
  });

  it("does not show without a known server", () => {
    expect(androidAppDownloadUrl({ origin: null })).toBeNull();
  });
});
