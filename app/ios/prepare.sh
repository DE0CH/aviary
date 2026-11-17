#!/usr/bin/env bash
# Generates an iOS project (docs/paseo-ui.md "The iOS app") and installs pods. Run from paseo/ on macOS after `npm ci`.
# Env: APP_VARIANT (which app: app.config.js), YYC_SITE (its server), BUILD_NUMBER (CFBundleVersion).
set -euo pipefail
cd "$(dirname "$0")/.."
export CI=1
: "${YYC_SITE:?YYC_SITE (the app's server) is required}"

npm run build:app-deps

# react-native-enriched-markdown compiles the codegen output it ships (ios/generated), which this nightly built before
# its spec gained code.fontScale; its own iOS sources use that field. Regenerate it from the spec.
rn="$(dirname "$(node -p "require.resolve('react-native/package.json', { paths: ['packages/app'] })")")"
(cd packages/app/node_modules/react-native-enriched-markdown \
  && node "$rn/scripts/generate-codegen-artifacts.js" -p . -t ios -s library)

cd packages/app
npx expo prebuild --platform ios --no-install --clean
plist="$(ls ios/*/Info.plist | grep -v Pods | head -1)"
if [[ -n "${BUILD_NUMBER:-}" ]]; then
  /usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${BUILD_NUMBER}" "$plist"
fi
echo "NODE_BINARY=$(command -v node)" > ios/.xcode.env.local
(cd ios && pod install)
