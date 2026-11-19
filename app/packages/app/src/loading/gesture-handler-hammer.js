// react-native-gesture-handler's legacy Hammer.js web implementation, in place of its web_hammer modules on web
// (metro.config.cjs, docs/paseo-ui.md "Loading"). The new web implementation is the default and nothing enables the
// legacy one (enableLegacyWebImplementation), so this code never runs; if something did, it fails loudly.
function legacyWebImplementation() {
  throw new Error("react-native-gesture-handler: the legacy (Hammer.js) web implementation is not bundled");
}

class LegacyGestureHandler {
  constructor() {
    legacyWebImplementation();
  }
}

module.exports = {
  __esModule: true,
  default: LegacyGestureHandler,
  createGestureHandler: legacyWebImplementation,
  getHandler: legacyWebImplementation,
  dropGestureHandler: legacyWebImplementation,
};
