const fs = require("node:fs");
const path = require("node:path");
const pkg = require("./package.json");
const withAndroidAsyncStorageSize = require("./plugins/with-android-async-storage-size");
const withAndroidOptionalCamera = require("./plugins/with-android-optional-camera");
const withAndroidProfileable = require("./plugins/with-android-profileable");
const withFdroidAutolinking = require("./plugins/with-fdroid-autolinking");
const withPasteInput = require("./plugins/with-paste-input");
const withAndroidScroll = require("./modules/paseo-scroll/app.plugin");
const { getNativeReleaseVersion } = require("./native-release-version");
const appVariant = process.env.APP_VARIANT ?? "production";
const isFdroidBuild = process.env.PASEO_FDROID_BUILD === "1";
const isWatchdogBuild = process.env.PASEO_WATCHDOG_BUILD === "1";
const isProfileBuild = process.env.PASEO_PROFILE_BUILD === "1";

const buildProfile = isFdroidBuild
  ? {
      androidPermissions: [
        "RECORD_AUDIO",
        "android.permission.RECORD_AUDIO",
        "android.permission.MODIFY_AUDIO_SETTINGS",
      ],
      cameraPlugins: [],
      fdroidPlugins: [withFdroidAutolinking],
      notificationPlugins: [],
    }
  : {
      androidPermissions: [
        "RECORD_AUDIO",
        "android.permission.RECORD_AUDIO",
        "android.permission.MODIFY_AUDIO_SETTINGS",
        "CAMERA",
        "android.permission.CAMERA",
      ],
      cameraPlugins: [
        [
          "expo-camera",
          {
            cameraPermission:
              "Allow $(PRODUCT_NAME) to access your camera to scan pairing QR codes.",
          },
        ],
      ],
      fdroidPlugins: [],
      notificationPlugins: [
        [
          "expo-notifications",
          {
            icon: "./assets/images/notification-icon.png",
            color: "#20744A",
          },
        ],
      ],
    };

function resolveSecretFile(params) {
  const fromEnv = process.env[params.envKey];
  if (typeof fromEnv === "string" && fromEnv.trim().length > 0) {
    return fromEnv.trim();
  }

  const fallbackAbsolutePath = path.resolve(__dirname, params.fallbackRelativePath);
  if (fs.existsSync(fallbackAbsolutePath)) {
    return params.fallbackRelativePath;
  }

  return undefined;
}

// YYC AI (docs/paseo-ui.md): the iOS app is "YYC AI" (dev.de0ch.yycai, the TestFlight app the SwiftUI client
// shipped as before), and the URL scheme the router's sign-in returns to is the bundle id.
const variants = {
  production: {
    name: "YYC AI",
    packageId: "dev.de0ch.yycai",
  },
  development: {
    name: "YYC AI Debug",
    packageId: "dev.de0ch.yycai.debug",
  },
};

const variant = variants[appVariant] ?? variants.production;
const nativeReleaseVersion = getNativeReleaseVersion(pkg.version);

export default {
  expo: {
    name: variant.name,
    slug: "voice-mobile",
    version: nativeReleaseVersion.appVersion,
    orientation: "portrait",
    icon: "./assets/images/yyc-icon.png",
    scheme: variant.packageId,
    userInterfaceStyle: "automatic",
    newArchEnabled: true,
    ios: {
      supportsTablet: true,
      infoPlist: {
        NSMicrophoneUsageDescription: "This app needs access to the microphone for voice commands.",
        ITSAppUsesNonExemptEncryption: false,
      },
      bundleIdentifier: variant.packageId,
      buildNumber: nativeReleaseVersion.iosBuildNumber,
    },
    android: {
      adaptiveIcon: {
        backgroundColor: "#000000",
        foregroundImage: "./assets/images/android-icon-foreground.png",
      },
      edgeToEdgeEnabled: true,
      predictiveBackGestureEnabled: false,
      softwareKeyboardLayoutMode: "resize",
      // Allow HTTP connections for local network hosts (required for release builds)
      usesCleartextTraffic: true,
      permissions: buildProfile.androidPermissions,
      package: variant.packageId,
      versionCode: nativeReleaseVersion.androidVersionCode,
    },
    web: {
      output: "single",
      favicon: "./assets/images/yyc-favicon.png",
    },
    autolinking: {
      searchPaths: ["../../node_modules", "./node_modules"],
    },
    plugins: [
      "expo-router",
      withPasteInput,
      withAndroidScroll,
      [withAndroidAsyncStorageSize, 64],
      // No QR pairing and no push (one server, no APNs key): no camera or notifications plugin
      withAndroidOptionalCamera,
      [
        "expo-splash-screen",
        {
          image: "./assets/images/yyc-splash.png",
          imageWidth: 200,
          resizeMode: "contain",
          backgroundColor: "#ffffff",
          dark: {
            backgroundColor: "#000000",
          },
        },
      ],
      "expo-audio",
      [
        "expo-gradle-jvmargs",
        {
          xmx: "4096m",
          maxMetaspace: "1024m",
        },
      ],
      [
        "expo-build-properties",
        {
          ios: { enableSceneSupport: true },
          android: {
            minSdkVersion: 29,
            kotlinVersion: "2.1.20",
            // Allow HTTP connections for local network hosts in release builds
            usesCleartextTraffic: true,
          },
        },
      ],
      ...buildProfile.fdroidPlugins,
      ...(isProfileBuild || isWatchdogBuild ? [withAndroidProfileable] : []),
    ],
    experiments: {
      typedRoutes: true,
      reactCompiler: true,
      autolinkingModuleResolution: true,
    },
    extra: {
      // The YYC AI server (yyc/site.ts): a build secret, not in this public repo
      yycSite: process.env.YYC_SITE,
      fdroidBuild: isFdroidBuild,
      profileBuild: isProfileBuild,
      watchdogBuild: isWatchdogBuild,
      diagnosticRevision: process.env.PASEO_DIAGNOSTIC_REVISION,
      diagnosticBuildTime: process.env.PASEO_DIAGNOSTIC_BUILD_TIME,
      router: {},
    },
  },
};
