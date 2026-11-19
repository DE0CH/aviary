// Reanimated's development-only check that react-native-worklets matches it (scripts/validate-worklets-version.js),
// in place of that script on web (metro.config.cjs, docs/paseo-ui.md "Loading"): the script pulls semver into the
// entry, and only __DEV__ builds call it. The installed pair is checked by Reanimated on native and at install time.
module.exports = function validateWorkletsVersion() {
  return { ok: true };
};
