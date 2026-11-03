#!/usr/bin/env bash
# Checks that the A1 claudecodeui commit has its pigeon-app image and that it seeds that commit's own ui image (DESIGN
# 9.6, cutover checklist "including the A1 image's"): ghcr.io/de0ch/pigeon-app:<sha12> exists, was built from that
# commit, and its meta/seeds.json lists yyc-ai-ui:<sha12> at that commit. For the cutover runner before A1's pins are
# pushed; needs crane and jq (and a registry login only if the package is private).
#
#   a1-ready.sh <A1 commit, 40 hex>
#
# Exit 0: ready. 1: the image is missing or does not seed its own ui image. 2: usage.
set -euo pipefail
commit=${1:-}
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "usage: a1-ready.sh <commit, 40 hex>" >&2; exit 2; }
tag=${commit::12}
image=ghcr.io/de0ch/pigeon-app:$tag
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
crane export --platform linux/amd64 "$image" - | tar -x -C "$work" meta \
  || { echo "a1-ready: no $image (pigeon-app.yml has not built this commit: run it, see scripts/pigeon-app/README.md)" >&2; exit 1; }
built=$(jq -r '.[0].commit' "$work/meta/builds.json")
[ "$built" = "$commit" ] || { echo "a1-ready: $image was built from $built, not $commit" >&2; exit 1; }
seeded=$(jq -r --arg tag "$tag" '.[$tag].commit // empty' "$work/meta/seeds.json")
[ "$seeded" = "$commit" ] || { echo "a1-ready: $image does not seed yyc-ai-ui:$tag (its own ui image)" >&2; exit 1; }
echo "a1-ready: $image seeds yyc-ai-ui:$tag and $(jq 'length - 1' "$work/meta/seeds.json") other ui images"
