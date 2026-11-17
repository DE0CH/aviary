#!/usr/bin/env bash
# Simulator smoke test of an iOS build (YYC AI, Jarvis Chat): install, launch, and check with Maestro that it reaches
# its sign-in screen showing its product's name, whose button a tap reaches (CI has no Access login, so the chat itself is not reached here).
# Usage: PRODUCT_NAME="YYC AI" simulator-test.sh <path to .app>
set -euo pipefail
APP="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${GITHUB_WORKSPACE:-$PWD}/yyc-ios-test-output"
mkdir -p "$OUT"
: "${PRODUCT_NAME:?PRODUCT_NAME (the name the sign-in screen shows) is required}"
APP_ID="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$APP/Info.plist")"
FLOW_ENV=(-e "APP_ID=$APP_ID" -e "PRODUCT_NAME=$PRODUCT_NAME")

if ! command -v maestro >/dev/null; then
  curl -fsSL "https://get.maestro.mobile.dev" | bash
  export PATH="$HOME/.maestro/bin:$PATH"
fi

RUNTIME=$(xcrun simctl list runtimes -j | python3 -c '
import json,sys
rs=[r for r in json.load(sys.stdin)["runtimes"] if r["isAvailable"] and r["platform"]=="iOS"]
print(sorted(rs,key=lambda r:[int(x) for x in r["version"].split(".")])[-1]["identifier"])')
DEVICE_TYPE=$(xcrun simctl list devicetypes -j | python3 -c '
import json,sys
ts=[t["identifier"] for t in json.load(sys.stdin)["devicetypes"] if "iPhone" in t["name"]]
pick=[t for t in ts if t.endswith("iPhone-17")] or ts
print(pick[-1])')
UDID=$(xcrun simctl create "yyc-e2e" "$DEVICE_TYPE" "$RUNTIME")
echo "simulator $UDID ($DEVICE_TYPE, $RUNTIME)"
xcrun simctl boot "$UDID"
xcrun simctl bootstatus "$UDID" -b
xcrun simctl install "$UDID" "$APP"

# A fresh simulator on a CI runner can take minutes before Maestro's XCTest driver answers: its first-boot data
# migration ran ~5 min on 2026-10-29 (runs 40814709068, 40865741585) and the driver then missed a 3-minute window.
# Ten minutes, and one more try when the driver never came up (not when the app failed the flow).
export MAESTRO_DRIVER_STARTUP_TIMEOUT=600000
status=0
maestro --device "$UDID" test "${FLOW_ENV[@]}" --test-output-dir "$OUT/sign-in" "$HERE/maestro/sign-in-screen.yaml" 2>&1 | tee "$OUT/maestro-1.log" || status=$?
if [ "$status" -ne 0 ] && grep -q "IOSDriverTimeoutException" "$OUT/maestro-1.log"; then
  echo "Maestro's driver did not start; trying once more"
  status=0
  maestro --device "$UDID" test "${FLOW_ENV[@]}" --test-output-dir "$OUT/sign-in-2" "$HERE/maestro/sign-in-screen.yaml" || status=$?
fi
xcrun simctl io "$UDID" screenshot "$OUT/final.png" || true
# The flow opens iOS's sign-in prompt, which names the server's domain: when the address is a build secret (YYC AI:
# no SITE_IS_PUBLIC) a failed run's screenshots may show it, and the artifacts of this public repo are not private
if [ "$status" -ne 0 ] && [ "${SITE_IS_PUBLIC:-}" != "1" ]; then
  find "$OUT" -name '*.png' -delete
  echo "screenshots of the failed run removed (the server's address is a build secret)"
fi
xcrun simctl shutdown "$UDID" || true
xcrun simctl delete "$UDID" || true
exit $status
