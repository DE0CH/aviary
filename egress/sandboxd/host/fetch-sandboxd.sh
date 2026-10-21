#!/usr/bin/env bash
# Puts the sandboxd binary (Rust, static; built and published by the `sandboxd` workflow as release sandboxd-<commit>)
# into DIR/bin/sandboxd, checked against the release's SHA256SUMS. setup.sh and gateway.sh install it from there.
# Usage: fetch-sandboxd.sh DIR   (SANDBOXD_RELEASE=sandboxd-<commit 12> for a given one; default: the newest;
#        SANDBOXD_BINARY=<path> to use a local build instead, e.g. daemon/target/x86_64-unknown-linux-musl/release/sandboxd)
set -euo pipefail
dir=${1:?dir}
mkdir -p "$dir/bin"
if [ -n "${SANDBOXD_BINARY:-}" ]; then
  install -m755 "$SANDBOXD_BINARY" "$dir/bin/sandboxd"
else
  rel=${SANDBOXD_RELEASE:-$(curl -fsS "https://api.github.com/repos/DE0CH/jarvis3/releases?per_page=100" | jq -r '[.[] | select(.tag_name | startswith("sandboxd-"))][0].tag_name')}
  [ -n "$rel" ] && [ "$rel" != null ] || { echo "no sandboxd release found" >&2; exit 1; }
  t=$(mktemp -d); u=https://github.com/DE0CH/jarvis3/releases/download/$rel
  curl -fsSL -o "$t/sandboxd" "$u/sandboxd"; curl -fsSL -o "$t/SHA256SUMS" "$u/SHA256SUMS"
  (cd "$t" && sha256sum -c --quiet SHA256SUMS)
  install -m755 "$t/sandboxd" "$dir/bin/sandboxd"; rm -rf "$t"
  echo "$rel" > "$dir/bin/RELEASE"
fi
"$dir/bin/sandboxd" --version
