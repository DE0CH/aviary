#!/usr/bin/env bash
# Simulator smoke test of the YYC AI iOS build: install, launch, and check with Maestro that it reaches its sign-in
# screen (CI has no Access login, so the chat itself is not reached here). Usage: simulator-test.sh <path to .app>
set -euo pipefail
APP="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${GITHUB_WORKSPACE:-$PWD}/yyc-ios-test-output"
mkdir -p "$OUT"

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

status=0
maestro --device "$UDID" test --test-output-dir "$OUT/sign-in" "$HERE/maestro/sign-in-screen.yaml" || status=$?
xcrun simctl io "$UDID" screenshot "$OUT/final.png" || true
xcrun simctl shutdown "$UDID" || true
xcrun simctl delete "$UDID" || true
exit $status
