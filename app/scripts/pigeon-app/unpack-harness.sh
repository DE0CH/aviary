#!/usr/bin/env bash
# Unpacks an aviary release's paseo-contract.tgz (the testing/paseo-contract harness) so that validate-frames.mjs sits
# at the root of <dest>: the tarball holds the harness at its root, or in one top directory (npm pack's `package/`,
# `./package/`, or any other name), dotfiles included.
#
#   unpack-harness.sh <paseo-contract.tgz> <dest>
#
# Exit 0 unpacked, 2 a tarball without validate-frames.mjs at its root or one level down.
set -euo pipefail
tgz=$1
dest=$2
rm -rf "$dest"
mkdir -p "$dest"
tar -xzf "$tgz" -C "$dest"
[ -f "$dest/validate-frames.mjs" ] && exit 0
found=$(find "$dest" -mindepth 2 -maxdepth 2 -name validate-frames.mjs -type f -print)
[ "$(printf '%s' "$found" | grep -c .)" = 1 ] || { echo "unpack-harness: $tgz has no validate-frames.mjs at its root or one directory down" >&2; exit 2; }
inner=$(dirname "$found")
tmp=$(mktemp -d "$dest.XXXXXX")
shopt -s dotglob nullglob
mv "$inner"/* "$tmp"/
shopt -u dotglob nullglob
rm -rf "$dest"
mv "$tmp" "$dest"
