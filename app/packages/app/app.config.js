const fs = require("node:fs");
const path = require("node:path");
const withAndroidAsyncStorageSize = require("./plugins/with-android-async-storage-size");
const withAndroidOptionalCamera = require("./plugins/with-android-optional-camera");
const withAndroidProfileable = require("./plugins/with-android-profileable");
const withAndroidFoldable = require("./plugins/with-android-foldable");
const withAndroidReleaseSigning = require("./plugins/with-android-release-signing");
const withFdroidAutolinking = require("./plugins/with-fdroid-autolinking");
const withPasteInput = require("./plugins/with-paste-input");
const withNoPushEntitlement = require("./plugins/with-no-push-entitlement");
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

// The native apps (docs/paseo-ui.md "The iOS app"), one per deployment: what a build cannot ask its server is here
// (the home-screen name, the bundle id, which is also the URL scheme the server's sign-in returns to, the icon and
// launch image, the product's name before the server has said it, and the texts of iOS's permission prompts).
// YYC AI is dev.de0ch.yycai, the TestFlight app the SwiftUI client shipped as before.
const yyc = {
  productName: "YYC AI",
  icon: "./assets/images/yyc-icon.png",
  // Android's adaptive icon: the mark on its own layer, inside the middle 72 of 108 dp that every launcher's mask keeps
  androidIcon: { foreground: "./assets/images/yyc-android-icon-foreground.png", background: "#1B2A3A" },
  splash: "./assets/images/yyc-splash.png",
  cameraPermission: "允許 $(PRODUCT_NAME) 使用相機，以拍攝相片並附加到訊息。",
  photosPermission: "允許 $(PRODUCT_NAME) 讀取相片，以將圖片附加到訊息。",
};
const variants = {
  production: { ...yyc, name: "YYC AI", packageId: "dev.de0ch.yycai" },
  development: { ...yyc, name: "YYC AI Debug", packageId: "dev.de0ch.yycai.debug" },
  // Jarvis 4's chat (DE0CH/jarvis4 aviary/README.md), the TestFlight app "Jarvis 4 Chat"
  "jarvis-chat": {
    name: "Jarvis Chat",
    packageId: "dev.de0ch.jarvis4chat",
    productName: "Jarvis 4",
    icon: "./assets/images/jarvis-chat-icon.png",
    androidIcon: { foreground: "./assets/images/jarvis-chat-android-icon-foreground.png", background: "#080E1A" },
    splash: "./assets/images/jarvis-chat-splash.png",
    cameraPermission: "Allow $(PRODUCT_NAME) to use the camera to take photos for a message.",
    photosPermission: "Allow $(PRODUCT_NAME) to read your photos to attach images to a message.",
  },
};

const variant = variants[appVariant] ?? variants.production;
// YYC AI's own version: the TestFlight app dev.de0ch.yycai shipped 1.0 from the SwiftUI app before this one
// (docs/paseo-ui.md), and a version below it reads as a downgrade. The web build keeps Paseo's package version.
const YYC_APP_VERSION = "1.1.0";
const nativeReleaseVersion = getNativeReleaseVersion(YYC_APP_VERSION);
// The Android app is installed from its APK (docs/paseo-ui.md "The Android app"), so every CI build must install over
// the one before: its versionCode is the version's plus the CI run (ANDROID_BUILD_NUMBER).
const androidBuildNumber = Number(process.env.ANDROID_BUILD_NUMBER ?? 0);
if (!Number.isSafeInteger(androidBuildNumber) || androidBuildNumber < 0 || androidBuildNumber > 999_999) {
  throw new Error(`ANDROID_BUILD_NUMBER must be a whole number from 0 to 999999: ${process.env.ANDROID_BUILD_NUMBER}`);
}

export default {
  expo: {
    name: variant.name,
    slug: "voice-mobile",
    version: nativeReleaseVersion.appVersion,
    orientation: "portrait",
    icon: variant.icon,
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
        backgroundColor: variant.androidIcon.background,
        foregroundImage: variant.androidIcon.foreground,
      },
      edgeToEdgeEnabled: true,
      predictiveBackGestureEnabled: false,
      softwareKeyboardLayoutMode: "resize",
      // Allow HTTP connections for local network hosts (required for release builds)
      usesCleartextTraffic: true,
      permissions: buildProfile.androidPermissions,
      package: variant.packageId,
      versionCode: nativeReleaseVersion.androidVersionCode * 1000 + androidBuildNumber,
    },
    web: {
      output: "single",
    },
    autolinking: {
      searchPaths: ["../../node_modules", "./node_modules"],
    },
    plugins: [
      "expo-router",
      withPasteInput,
      withNoPushEntitlement,
      withAndroidScroll,
      [withAndroidAsyncStorageSize, 64],
      // No QR pairing and no push (one server, no APNs key): no expo-camera or notifications plugin. The message
      // box attaches from the photo library and the system camera (expo-image-picker).
      withAndroidOptionalCamera,
      [
        "expo-image-picker",
        {
          cameraPermission: variant.cameraPermission,
          photosPermission: variant.photosPermission,
          // no `microphonePermission: false`: it deletes NSMicrophoneUsageDescription, which the app's audio code
          // needs in the Info.plist (Apple refuses the upload without it: error 90683)
        },
      ],
      [
        "expo-splash-screen",
        {
          image: variant.splash,
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
      withAndroidFoldable,
      withAndroidReleaseSigning,
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
      // The product's name until the server says it (yyc/product.ts), and the bundled logo (yyc/native-brand.ts)
      productName: variant.productName,
      brand: appVariant === "jarvis-chat" ? "jarvis-chat" : "yyc",
      fdroidBuild: isFdroidBuild,
      profileBuild: isProfileBuild,
      watchdogBuild: isWatchdogBuild,
      diagnosticRevision: process.env.PASEO_DIAGNOSTIC_REVISION,
      diagnosticBuildTime: process.env.PASEO_DIAGNOSTIC_BUILD_TIME,
      router: {},
    },
  },
};
