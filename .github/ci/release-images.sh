#!/usr/bin/env bash
# nightly.yml: pulls the images of release aviary-<sha12> (public on ghcr.io, no login) and tags them as the harness
# takes them (<name>:<sha12>).
#   release-images.sh <sha12>
set -euo pipefail
s=${1:?sha12}
for name in pigeon magpie hermit aviary-testing hermit-sandbox; do
  docker pull -q "ghcr.io/de0ch/$name:$s" >/dev/null
  docker tag "ghcr.io/de0ch/$name:$s" "$name:$s"
done
docker images --format '{{.Repository}}:{{.Tag}} {{.ID}}' | grep ":$s "
