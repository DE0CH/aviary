#!/usr/bin/env bash
# ci.yml `publish` (platform 6.1): pushes the images job's tars as ghcr.io/de0ch/<name>:<sha12> (never `latest`) and
# writes {"<name>": "sha256:…"} of the pushed manifests. A tag that already exists (a re-run of the same commit) is
# left as it is and its digest read back: tags are never moved.
#   publish-images.sh <dir of tars> <sha12> <digests.json out>
set -euo pipefail
dir=$1
sha12=$2
out=$3
echo '{}' > "$out"
for name in pigeon magpie hermit sandboxd aviary-testing hermit-sandbox; do
  tar=$dir/$name.tar
  [ -s "$tar" ] || { echo "publish-images: no $tar" >&2; exit 1; }
  docker load -q -i "$tar" >/dev/null
  remote=ghcr.io/de0ch/$name:$sha12
  if digest=$(docker buildx imagetools inspect "$remote" --format '{{json .Manifest.Digest}}' 2>/dev/null | jq -r .); then
    echo "$remote exists ($digest): not pushed again"
  else
    docker tag "$name:$sha12" "$remote"
    log=$(docker push "$remote")
    echo "$log" | tail -1
    digest=$(grep -o 'digest: sha256:[0-9a-f]\{64\}' <<< "$log" | tail -1 | cut -d' ' -f2)
    [ -n "$digest" ] || { echo "publish-images: no digest for $remote" >&2; exit 1; }
  fi
  jq --arg n "$name" --arg d "$digest" '.[$n] = $d' "$out" > "$out.tmp" && mv "$out.tmp" "$out"
done
cat "$out"
