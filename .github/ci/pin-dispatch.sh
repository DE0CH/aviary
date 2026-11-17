#!/usr/bin/env bash
# Starts each deployment's pin workflow after a release, so a deployment that pins aviary releases tries the new one
# at once instead of on its schedule (GitHub runs a */15 schedule only every few hours under load).
#   PIN_DISPATCH       repo variable: one target per line, "OWNER/REPO WORKFLOW_FILE" (# comments, blank lines)
#   PIN_DISPATCH_TOKEN secret (as GH_TOKEN): a fine-grained token with Actions read and write on those repos
# No targets: nothing to do. Every line is checked before the first dispatch.
#   .github/ci/pin-dispatch.sh            (test: .github/ci/test-pin-dispatch.sh)
set -euo pipefail
targets=()
while IFS= read -r line || [ -n "$line" ]; do
  line=${line%%#*}
  read -r -a f <<< "$line"
  [ ${#f[@]} -eq 0 ] && continue
  if [ ${#f[@]} -ne 2 ] || ! [[ ${f[0]} =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || ! [[ ${f[1]} =~ ^[A-Za-z0-9_.-]+\.ya?ml$ ]]; then
    echo "::error::PIN_DISPATCH: a line is not \"OWNER/REPO WORKFLOW_FILE\": $line" >&2
    exit 1
  fi
  targets+=("${f[0]} ${f[1]}")
done <<< "${PIN_DISPATCH:-}"
if [ ${#targets[@]} -eq 0 ]; then
  echo "PIN_DISPATCH is empty: no deployment to dispatch"
  exit 0
fi
if [ -z "${GH_TOKEN:-}" ]; then
  echo "::error::PIN_DISPATCH names ${#targets[@]} deployment(s), but the secret PIN_DISPATCH_TOKEN is not set" >&2
  exit 1
fi
for t in "${targets[@]}"; do
  read -r repo workflow <<< "$t"
  gh workflow run "$workflow" -R "$repo" --ref main
  echo "dispatched $repo $workflow"
done
