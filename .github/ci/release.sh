#!/usr/bin/env bash
# ci.yml `publish` (platform 5.4): the GitHub release aviary-<sha12> of a main commit whose images were pushed. Assets:
# pigeon, hermit, supervisor, supervisor-keeper, sandboxd and aviary-conformance from dist/, pigeon's frame corpus
# (pigeon-frames.ndjson) and the contract harness (paseo-contract.tgz, testing/paseo-contract without node_modules) the
# app's CI validates against, and SHA256SUMS over them. The body is the commit subject, the image digests and whether
# the sandbox image's supervisor (label dev.de0ch.aviary.supervisor) changed against the previous release. Not
# "latest", not a prerelease. A release that exists already (a re-run) is left alone.
#   release.sh <dist dir> <40 hex commit> <digests.json> <work dir>
set -euo pipefail
dist=$(cd "$1" && pwd)
sha=$2
digests=$3
work=$(mkdir -p "$4" && cd "$4" && pwd)
tag=aviary-${sha::12}
repo=${GITHUB_REPOSITORY:-DE0CH/aviary}
if gh release view "$tag" -R "$repo" >/dev/null 2>&1; then
  echo "release $tag exists: left alone"
  exit 0
fi
assets=$work/assets
mkdir -p "$assets"
for b in pigeon hermit supervisor supervisor-keeper sandboxd aviary-conformance; do
  [ -x "$dist/$b" ] || chmod +x "$dist/$b"
  cp "$dist/$b" "$assets/"
done
cp testing/paseo-contract/fixtures/pigeon-frames.ndjson "$assets/"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$(git log -1 --format=%ct "$sha")" \
  --exclude=node_modules -czf "$assets/paseo-contract.tgz" -C testing paseo-contract
(cd "$assets" && sha256sum pigeon hermit supervisor supervisor-keeper sandboxd aviary-conformance \
  pigeon-frames.ndjson paseo-contract.tgz > SHA256SUMS)
supervisor=$(docker image inspect -f '{{index .Config.Labels "dev.de0ch.aviary.supervisor"}}' "hermit-sandbox:${sha::12}")
prev=$(gh release list -R "$repo" --limit 50 --json tagName,createdAt \
  --jq '[.[] | select(.tagName | startswith("aviary-"))] | sort_by(.createdAt) | last | .tagName // ""')
changed="no earlier release"
if [ -n "$prev" ]; then
  before=$(gh release view "$prev" -R "$repo" --json body --jq .body | sed -n 's/^supervisor: \([0-9a-f]\{64\}\).*/\1/p' | head -1)
  if [ -z "$before" ]; then changed="$prev names no supervisor"
  elif [ "$before" = "$supervisor" ]; then changed="unchanged against $prev"
  else changed="changed against $prev ($before)"; fi
fi
{
  git log -1 --format=%s "$sha"
  echo
  echo "Images (ghcr.io/de0ch/<name>:${sha::12}):"
  echo
  jq -r 'to_entries[] | "- \(.key)@\(.value)"' "$digests"
  echo
  echo "supervisor: $supervisor ($changed)"
} > "$work/body.md"
cat "$work/body.md"
gh release create "$tag" -R "$repo" --target "$sha" --title "$tag" --notes-file "$work/body.md" --latest=false \
  "$assets"/*
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then cat "$work/body.md" >> "$GITHUB_STEP_SUMMARY"; fi
