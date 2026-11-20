import { createRequire } from "node:module";
import { afterEach, describe, expect, it } from "vitest";

// Load the copies the Expo CLI uses for prebuild, not older hoisted ones.
const requireFromExpo = createRequire(require.resolve("expo/package.json"));
const requireFromExpoCli = createRequire(requireFromExpo.resolve("@expo/cli/package.json"));
const { getPrebuildConfigAsync } = requireFromExpoCli("@expo/prebuild-config");
const { compileModsAsync } = requireFromExpoCli(
  "@expo/config-plugins/build/plugins/mod-compiler.js",
);

interface ManifestEntry {
  $: Record<string, string>;
}

const originalFdroidFlag = process.env.PASEO_FDROID_BUILD;
const originalEnv = { ...process.env };

async function resolveAndroidConfig() {
  // app.config.js reads APP_VARIANT and ANDROID_BUILD_NUMBER when it is loaded: load it afresh for each environment
  for (const key of Object.keys(requireFromExpo.cache)) {
    if (key.endsWith("/app.config.js")) delete requireFromExpo.cache[key];
  }
  const { exp } = await getPrebuildConfigAsync(__dirname, { platforms: ["android"] });
  await compileModsAsync(exp, {
    projectRoot: __dirname,
    introspect: true,
    platforms: ["android"],
    assertMissingModProviders: false,
  });
  return exp;
}

function mainActivity(manifest: { application: { activity?: ManifestEntry[] }[] }): ManifestEntry {
  const activity = manifest.application[0].activity?.find((a) => a.$["android:name"] === ".MainActivity");
  if (!activity) throw new Error("no MainActivity in the manifest");
  return activity;
}

async function resolveAndroidManifest() {
  const exp = await resolveAndroidConfig();
  return exp._internal.modResults.android.manifest.manifest;
}

describe("Android app config", () => {
  afterEach(() => {
    if (originalFdroidFlag === undefined) {
      delete process.env.PASEO_FDROID_BUILD;
    } else {
      process.env.PASEO_FDROID_BUILD = originalFdroidFlag;
    }
  });

  it.each([
    ["Google Play", "0"],
    ["F-Droid", "1"],
  ])(
    "%s build installs on devices without a camera",
    async (_build, fdroidFlag) => {
      process.env.PASEO_FDROID_BUILD = fdroidFlag;
      const manifest = await resolveAndroidManifest();
      const permissions = (manifest["uses-permission"] ?? []).map(
        (entry: ManifestEntry) => entry.$["android:name"],
      );
      const features = (manifest["uses-feature"] ?? []).map((entry: ManifestEntry) => entry.$);

      expect(permissions).toContain("android.permission.CAMERA");
      expect(features).toEqual(
        expect.arrayContaining([
          { "android:name": "android.hardware.camera", "android:required": "false" },
          { "android:name": "android.hardware.camera.autofocus", "android:required": "false" },
        ]),
      );
    },
    30_000,
  );
});

// The Android app on a foldable (docs/paseo-ui.md "The Android app"): a fold changes the window's size, smallest width
// and density; any of them missing from configChanges recreates the activity, which restarts the app mid-conversation.
describe("Android app on a foldable", () => {
  afterEach(() => {
    for (const key of ["APP_VARIANT", "ANDROID_BUILD_NUMBER", "ANDROID_KEYSTORE"]) {
      if (originalEnv[key] === undefined) delete process.env[key];
      else process.env[key] = originalEnv[key];
    }
  });

  it("keeps the activity through a fold and an unfold", async () => {
    const activity = mainActivity(await resolveAndroidManifest());
    const changes = activity.$["android:configChanges"].split("|");
    for (const change of ["orientation", "screenSize", "smallestScreenSize", "screenLayout", "density"]) {
      expect(changes).toContain(change);
    }
    expect(activity.$["android:resizeableActivity"]).toBe("true");
  }, 30_000);

  it("turns freely: the iPhone's portrait lock is not applied to Android", async () => {
    const activity = mainActivity(await resolveAndroidManifest());
    expect(activity.$["android:screenOrientation"]).toBeUndefined();
  }, 30_000);

  it.each([
    ["production", "dev.de0ch.yycai", "./assets/images/yyc-android-icon-foreground.png"],
    ["jarvis-chat", "dev.de0ch.jarvis4chat", "./assets/images/jarvis-chat-android-icon-foreground.png"],
  ])("%s has its own package, scheme and launcher icon", async (variant, packageId, foreground) => {
    process.env.APP_VARIANT = variant;
    const exp = await resolveAndroidConfig();
    expect(exp.android.package).toBe(packageId);
    expect(exp.scheme).toBe(packageId);
    expect(exp.android.adaptiveIcon.foregroundImage).toBe(foreground);
  }, 30_000);

  it("installs over the previous CI build: the versionCode grows with the run", async () => {
    process.env.ANDROID_BUILD_NUMBER = "41";
    const earlier = (await resolveAndroidConfig()).android.versionCode;
    process.env.ANDROID_BUILD_NUMBER = "42";
    const later = (await resolveAndroidConfig()).android.versionCode;
    expect(later).toBe(earlier + 1);
    expect(later).toBeLessThanOrEqual(2_100_000_000);
  }, 30_000);
});

describe("iOS app's server", () => {
  afterEach(() => {
    for (const key of Object.keys(process.env)) if (!(key in originalEnv)) delete process.env[key];
    for (const [key, value] of Object.entries(originalEnv)) process.env[key] = value;
  });

  // the iPhone walk's build (app-ios-walk.yml) talks to the stand-in over plain HTTP; a deployment's never does
  it("may load a plain-HTTP server, and only that build", async () => {
    process.env.YYC_SITE = "http://127.0.0.1:8787";
    expect((await resolveAndroidConfig()).ios.infoPlist.NSAppTransportSecurity).toEqual({ NSAllowsArbitraryLoads: true });
    process.env.YYC_SITE = "https://chat.deyaochen.com";
    expect((await resolveAndroidConfig()).ios.infoPlist.NSAppTransportSecurity).toBeUndefined();
  }, 30_000);
});
