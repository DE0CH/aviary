const { withEntitlementsPlist } = require("expo/config-plugins");

// YYC AI has no push notifications (docs/paseo-ui.md), but expo-notifications stays installed and prebuild applies its
// plugin by itself, which asks for aps-environment. The app id has no Push capability, so signing would fail.
module.exports = function withNoPushEntitlement(config) {
  return withEntitlementsPlist(config, (next) => {
    delete next.modResults["aps-environment"];
    return next;
  });
};
