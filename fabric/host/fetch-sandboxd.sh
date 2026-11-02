#!/usr/bin/env bash
# Puts the sandboxd binary (static musl, from DE0CH/aviary's release aviary-<sha12>, platform 5.4) into DIR/bin/sandboxd,
# checked against the release's SHA256SUMS. setup.sh and gateway.sh install it from there; DIR/bin/RELEASE records
# which release it is.
# Usage: fetch-sandboxd.sh DIR [aviary-<sha12>]   (default: SANDBOXD_RELEASE, else the newest aviary-* release;
#        SANDBOXD_BINARY=<path> to use a local build instead, e.g. target/x86_64-unknown-linux-musl/sandboxd/sandboxd)
set -euo pipefail
dir=${1:?dir}
rel=${2:-${SANDBOXD_RELEASE:-}}
mkdir -p "$dir/bin"
if [ -n "${SANDBOXD_BINARY:-}" ]; then
  install -m755 "$SANDBOXD_BINARY" "$dir/bin/sandboxd"
  echo local > "$dir/bin/RELEASE"
else
  # aviary releases are not marked "latest" (platform 5.4): the newest by creation date
  [ -n "$rel" ] || rel=$(curl -fsS "https://api.github.com/repos/DE0CH/aviary/releases?per_page=100" |
    jq -r '[.[] | select(.tag_name | startswith("aviary-"))] | sort_by(.created_at) | last | .tag_name')
  if [ -z "$rel" ] || [ "$rel" = null ]; then echo "no aviary release found" >&2; exit 1; fi
  t=$(mktemp -d); u=https://github.com/DE0CH/aviary/releases/download/$rel
  curl -fsSL -o "$t/sandboxd" "$u/sandboxd"; curl -fsSL -o "$t/SHA256SUMS" "$u/SHA256SUMS"
  # the release holds every binary; check sandboxd's line, which must be there
  (cd "$t" && grep -E '^[0-9a-f]{64}  \*?sandboxd$' SHA256SUMS | sha256sum -c --quiet -)
  install -m755 "$t/sandboxd" "$dir/bin/sandboxd"; rm -rf "$t"
  echo "$rel" > "$dir/bin/RELEASE"
fi
"$dir/bin/sandboxd" --version
