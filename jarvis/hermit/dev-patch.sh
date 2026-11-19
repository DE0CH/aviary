#!/bin/bash
# Build or test against a local DE0CH/aviary checkout instead of the pinned commit (before an aviary change is pushed):
#   ./dev-patch.sh ~/workspace/aviary test
set -euo pipefail
av=$(cd "$1" && pwd); shift
args=()
for c in aviary-core aviary-pg hermit-api gateway-api magpie-api supervisor-api; do
  args+=(--config "patch.'https://github.com/DE0CH/aviary'.$c.path='$av/crates/$c'")
done
args+=(--config "patch.'https://github.com/DE0CH/aviary'.aviary-fakes.path='$av/testing/fakes'")
exec cargo "$1" "${args[@]}" "${@:2}"
