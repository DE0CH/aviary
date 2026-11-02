#!/usr/bin/env bash
# The app build's contract check against pigeon (pigeon spec 17.1 step 5, 18.1): every frame of pigeon's frame corpus
# must pass this checkout's paseo/packages/protocol (WSOutboundMessageSchema, the zod-aot validators the bundle runs,
# the status payload schemas; client frames against WSInboundMessageSchema), with the harness of DE0CH/aviary
# testing/paseo-contract (validate-frames.mjs --protocol-dir).
#
#   check-frames.sh <work dir> [extra.ndjson...]
#
# The corpus and the harness come from the newest aviary-* release of DE0CH/aviary (assets pigeon-frames.ndjson and
# paseo-contract.tgz); a release without them is an error. Until aviary has published its first release there is no
# pigeon corpus yet: the harness then comes from aviary main and the check runs on the frame fixtures alone, with a
# warning. Either way the harness's fixtures of frames the old CloudCLI bridge sends (fixtures/cloudcli-bridge-*.ndjson,
# which LU carries into pigeon-served pages until A5, DESIGN 9.6) and of pigeon-only frames
# (fixtures/pigeon-spec-frames.ndjson) are checked too. Extra frame logs (a seeded incompatible frame, in the
# workflow's self-test) are added to the corpus. Exit status: validate-frames.mjs's (0 valid, 1 a frame invalid,
# 2 setup error).
set -euo pipefail
work=$(mkdir -p "$1" && cd "$1" && pwd)
shift
extra=()
for file in "$@"; do
  [ -f "$file" ] || { echo "check-frames: no frame log $file" >&2; exit 2; }
  extra+=("$(cd "$(dirname "$file")" && pwd)/$(basename "$file")")
done
repo=$(cd "$(dirname "$0")/../.." && pwd)
protocol="$repo/paseo/packages/protocol"
api=https://api.github.com/repos/DE0CH/aviary
auth=()
if [ -n "${GH_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GH_TOKEN"); fi

if [ ! -f "$work/harness/validate-frames.mjs" ]; then
  rm -rf "$work/harness" "$work/corpus"
  mkdir -p "$work/harness" "$work/corpus"
  releases=$(curl -sSf "${auth[@]}" "$api/releases?per_page=100")
  release=$(jq -c '[.[] | select(.draft | not) | select(.tag_name | startswith("aviary-"))] | sort_by(.created_at) | last // empty' <<<"$releases")
  if [ -n "$release" ]; then
    tag=$(jq -r .tag_name <<<"$release")
    for asset in pigeon-frames.ndjson paseo-contract.tgz; do
      url=$(jq -r --arg name "$asset" '.assets[] | select(.name == $name) | .browser_download_url' <<<"$release")
      [ -n "$url" ] || { echo "check-frames: release $tag has no $asset" >&2; exit 2; }
      curl -sSfL -o "$work/$asset" "$url"
    done
    mv "$work/pigeon-frames.ndjson" "$work/corpus/"
    tar -xzf "$work/paseo-contract.tgz" -C "$work/harness"
    # the tarball holds the harness at its root or in one top directory
    if [ ! -f "$work/harness/validate-frames.mjs" ]; then
      inner=$(dirname "$(find "$work/harness" -maxdepth 3 -name validate-frames.mjs -print -quit)")
      [ "$inner" != . ] || { echo "check-frames: $tag's paseo-contract.tgz has no validate-frames.mjs" >&2; exit 2; }
      tmp=$(mktemp -d "$work/h.XXXXXX")
      mv "$inner"/* "$inner"/.[!.]* "$tmp"/ 2>/dev/null || mv "$inner"/* "$tmp"/
      rm -rf "$work/harness"
      mv "$tmp" "$work/harness"
    fi
    echo "check-frames: pigeon's frame corpus and harness of DE0CH/aviary $tag"
  else
    head=$(curl -sSf "${auth[@]}" "$api/commits/main" | jq -r .sha)
    curl -sSfL "${auth[@]}" "$api/tarball/$head" | tar -xz -C "$work/harness" --strip-components=3 --wildcards '*/testing/paseo-contract/*'
    echo "::warning::DE0CH/aviary has no aviary-* release yet, so there is no pigeon frame corpus: checking the harness's frame fixtures of aviary main ${head:0:12} only"
  fi
  (cd "$work/harness" && npm ci --no-audit --no-fund --silent)
fi

frames=("$work"/corpus/*.ndjson "$work"/harness/fixtures/cloudcli-bridge-*.ndjson "$work"/harness/fixtures/pigeon-spec-frames.ndjson "${extra[@]}")
existing=()
for file in "${frames[@]}"; do [ -f "$file" ] && existing+=("$file"); done
[ "${#existing[@]}" -gt 0 ] || { echo "check-frames: no frame log to check" >&2; exit 2; }
printf 'check-frames: %s\n' "${existing[@]#"$work"/}"
set +e
(cd "$work/harness" && node validate-frames.mjs --protocol-dir "$protocol" "${existing[@]}")
status=$?
set -e
exit "$status"
