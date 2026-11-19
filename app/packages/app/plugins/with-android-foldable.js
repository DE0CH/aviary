// The Android app on a foldable (docs/paseo-ui.md "The Android app"): folding or unfolding changes the screen's size,
// its smallest width and often its density. Any of those not listed in MainActivity's configChanges makes Android
// destroy and recreate the activity, which restarts React Native: the conversation, the draft and the keyboard are
// gone after every fold. Listed, the activity stays and React Native re-lays itself out to the new window.
// The app turns freely there too: `orientation: "portrait"` (app.config.js, for the iPhone) is not applied to Android,
// where an unfolded book-style foldable is a small tablet.
const { AndroidConfig, withAndroidManifest } = require("expo/config-plugins");

const CONFIG_CHANGES = [
  "keyboard",
  "keyboardHidden",
  "orientation",
  "screenSize",
  "smallestScreenSize",
  "screenLayout",
  "density",
  "uiMode",
];

function applyFoldable(manifest) {
  const activity = AndroidConfig.Manifest.getMainActivityOrThrow(manifest);
  const listed = (activity.$["android:configChanges"] ?? "").split("|").filter(Boolean);
  activity.$["android:configChanges"] = [...new Set([...listed, ...CONFIG_CHANGES])].join("|");
  delete activity.$["android:screenOrientation"];
  activity.$["android:resizeableActivity"] = "true";
  return manifest;
}

function withAndroidFoldable(config) {
  return withAndroidManifest(config, (modConfig) => {
    applyFoldable(modConfig.modResults);
    return modConfig;
  });
}

module.exports = withAndroidFoldable;
module.exports.applyFoldable = applyFoldable;
module.exports.CONFIG_CHANGES = CONFIG_CHANGES;
