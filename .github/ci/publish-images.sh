#!/usr/bin/env bash
# ci.yml `publish` (platform 6.1): pushes the images job's tars as ghcr.io/de0ch/<name>:<sha12> (never `latest`) and
# writes {"<name>": "sha256:…"} of the pushed tags. The service images (pigeon, magpie, hermit, sandboxd,
# aviary-testing) are linux/amd64 and linux/arm64: each architecture's image is pushed as <sha12>-amd64 and
# <sha12>-arm64, and <sha12> is the index of the two (`docker buildx imagetools create`), so a node of either
# architecture pulls its own; the digest recorded is the index's. hermit-sandbox is linux/amd64 only, pushed as
# <sha12> itself. A <sha12> tag that already exists (a re-run of the same commit) is left as it is and its digest read
# back: tags are never moved.
#   publish-images.sh <dir of amd64 tars> <dir of arm64 tars> <sha12> <digests.json out>
set -euo pipefail
dir=$1
arm=$2
sha12=$3
out=$4
echo '{}' > "$out"
# release.sh reads hermit-sandbox's labels from the local image, so it is loaded even when its tag exists (a re-run)
[ -s "$dir/hermit-sandbox.tar" ] || { echo "publish-images: no $dir/hermit-sandbox.tar" >&2; exit 1; }
docker load -q -i "$dir/hermit-sandbox.tar" >/dev/null
push() { # push <local image> <remote>: prints the pushed manifest's digest
  docker tag "$1" "$2"
  local log digest
  log=$(docker push "$2")
  echo "$log" | tail -1 >&2
  digest=$(grep -o 'digest: sha256:[0-9a-f]\{64\}' <<< "$log" | tail -1 | cut -d' ' -f2)
  [ -n "$digest" ] || { echo "publish-images: no digest for $2" >&2; exit 1; }
  echo "$digest"
}
for name in pigeon magpie hermit sandboxd aviary-testing hermit-sandbox; do
  remote=ghcr.io/de0ch/$name:$sha12
  if digest=$(docker buildx imagetools inspect "$remote" --format '{{json .Manifest.Digest}}' 2>/dev/null | jq -r .); then
    echo "$remote exists ($digest): not pushed again"
  elif [ "$name" = hermit-sandbox ]; then
    digest=$(push "$name:$sha12" "$remote")
  else
    refs=()
    for a in amd64 arm64; do
      if [ "$a" = amd64 ]; then tar=$dir/$name.tar; else tar=$arm/$name.tar; fi
      [ -s "$tar" ] || { echo "publish-images: no $tar" >&2; exit 1; }
      # both tars carry the tag <name>:<sha12>: load, check the architecture, retag before the next load
      docker load -q -i "$tar" >/dev/null
      got=$(docker image inspect -f '{{.Os}}/{{.Architecture}}' "$name:$sha12")
      [ "$got" = "linux/$a" ] || { echo "publish-images: $tar is $got, not linux/$a" >&2; exit 1; }
      d=$(push "$name:$sha12" "$remote-$a")
      docker rmi -f "$name:$sha12" "$remote-$a" >/dev/null
      refs+=("ghcr.io/de0ch/$name@$d")
    done
    docker buildx imagetools create -t "$remote" "${refs[@]}"
    digest=$(docker buildx imagetools inspect "$remote" --format '{{json .Manifest.Digest}}' | jq -r .)
    docker buildx imagetools inspect "$remote"
  fi
  jq --arg n "$name" --arg d "$digest" '.[$n] = $d' "$out" > "$out.tmp" && mv "$out.tmp" "$out"
done
cat "$out"
