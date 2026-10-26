#!/usr/bin/env bash
# Generates the YYC AI iOS project (docs/paseo-ui.md) and installs pods. Run from paseo/ on macOS after `npm ci`.
# Env: YYC_SITE (the server, a build secret), BUILD_NUMBER (CFBundleVersion).
set -euo pipefail
cd "$(dirname "$0")/.."
export CI=1
: "${YYC_SITE:?YYC_SITE (the YYC AI server) is required}"

npm run build:app-deps

cd packages/app
npx expo prebuild --platform ios --no-install --clean
plist="$(ls ios/*/Info.plist | grep -v Pods | head -1)"
if [[ -n "${BUILD_NUMBER:-}" ]]; then
  /usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${BUILD_NUMBER}" "$plist"
fi
echo "NODE_BINARY=$(command -v node)" > ios/.xcode.env.local
(cd ios && pod install)
