#!/bin/sh
# record.sh <claudecodeui checkout> <out.ndjson>: records the CloudCLI Paseo bridge of that checkout (the checkout must
# have its root dependencies installed: `npm ci`). The recorder is copied next to the bridge's tests and removed after.
set -eu
src=$(cd "$(dirname "$0")" && pwd)/record-cloudcli-bridge.ts
checkout=$(cd "$1" && pwd)
out=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
dest=$checkout/server/modules/paseo/tests/record-cloudcli-bridge.ts
cp "$src" "$dest"
trap 'rm -f "$dest"' EXIT
cd "$checkout"
npx tsx --tsconfig server/tsconfig.json "$dest" "$out"
