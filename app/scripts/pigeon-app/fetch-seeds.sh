#!/usr/bin/env bash
# Copies the hashed web assets out of yyc-ai-ui images (pigeon spec 17.1 step 6, cutover D13): for each tag,
# /app/dist/_expo/static/** and /app/dist/assets/** of ghcr.io/de0ch/yyc-ai-ui:<tag> (linux/amd64) into <dir>/<tag>/.
# A pre-Paseo image has no _expo/ tree and gives an empty seed. <dir>/<tag>/.digest (the image digest) is written
# last, so a directory with .digest is complete and is not fetched again (the workflow caches <dir>). A missing image
# or a failed pull fails the script.
#
#   fetch-seeds.sh <dir> <tag>...      (crane on PATH, logged in to ghcr.io)
set -euo pipefail
dir=$1
shift
mkdir -p "$dir"
image=ghcr.io/de0ch/yyc-ai-ui

fetch() {
  local tag=$1 out="$dir/$1" digest scratch
  if [ -f "$out/.digest" ]; then
    echo "cached  $tag ($(find "$out" -type f ! -name .digest | wc -l) files)"
    return 0
  fi
  digest=$(crane digest --platform linux/amd64 "$image:$tag")
  scratch=$(mktemp -d "$dir/.fetch-$tag.XXXXXX")
  # the flattened filesystem's /app/dist (every ui image has one; the hashed trees are picked from it below)
  crane export --platform linux/amd64 "$image@$digest" - | tar -x -C "$scratch" app/dist
  rm -rf "$out"
  mkdir -p "$out"
  # a Paseo build: its chunks and its hashed Expo assets (a pre-Paseo image's dist/assets is the CloudCLI UI's, which
  # cannot talk to pigeon, so it is not kept)
  if [ -d "$scratch/app/dist/_expo/static" ]; then
    mkdir -p "$out/_expo"
    mv "$scratch/app/dist/_expo/static" "$out/_expo/"
    if [ -d "$scratch/app/dist/assets" ]; then mv "$scratch/app/dist/assets" "$out/"; fi
  fi
  rm -rf "$scratch"
  echo "$digest" > "$out/.digest"
  echo "fetched $tag $digest ($(find "$out" -type f ! -name .digest | wc -l) files)"
}

export -f fetch
export dir image
printf '%s\n' "$@" | xargs -r -P 4 -I{} bash -c 'fetch "$@"' _ {}
