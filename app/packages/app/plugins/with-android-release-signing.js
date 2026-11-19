// Signs the release build with the upload key from the environment (ANDROID_KEYSTORE = the .jks file's path,
// ANDROID_KEYSTORE_PASSWORD, ANDROID_KEY_ALIAS; the key's password is the store's). Without ANDROID_KEYSTORE the
// release build keeps Expo's debug signing, for local builds.
const { withAppBuildGradle } = require("expo/config-plugins");

const BLOCK = `
        release {
            storeFile file(System.getenv("ANDROID_KEYSTORE"))
            storePassword System.getenv("ANDROID_KEYSTORE_PASSWORD")
            keyAlias System.getenv("ANDROID_KEY_ALIAS")
            keyPassword System.getenv("ANDROID_KEYSTORE_PASSWORD")
        }`;

module.exports = function withReleaseSigning(config) {
  return withAppBuildGradle(config, (cfg) => {
    if (!process.env.ANDROID_KEYSTORE) return cfg;
    let gradle = cfg.modResults.contents;
    if (!gradle.includes('System.getenv("ANDROID_KEYSTORE")')) {
      gradle = gradle.replace(/signingConfigs\s*\{/, (m) => m + BLOCK);
    }
    // the release build type's signingConfig (the template points it at the debug key)
    gradle = gradle.replace(
      /(buildTypes\s*\{[\s\S]*?release\s*\{[\s\S]*?)signingConfig\s+signingConfigs\.debug/,
      "$1signingConfig signingConfigs.release",
    );
    if (!/release\s*\{[\s\S]*?signingConfig signingConfigs\.release/.test(gradle)) {
      throw new Error("with-release-signing: the release build type was not found in app/build.gradle");
    }
    cfg.modResults.contents = gradle;
    return cfg;
  });
};
