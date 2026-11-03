#!/usr/bin/env bash
# Checks that the A1 claudecodeui commit has its pigeon-app image and that it seeds every ui image a tab may still run
# (DESIGN 9.6, cutover checklist "including the A1 image's"): ghcr.io/de0ch/pigeon-app:<sha12> exists, was built from
# that commit, its meta/seeds.json lists yyc-ai-ui:<sha12> at that commit, and it lists every yyc-ai-ui tag the deploy
# repository served within the 720 h Access session lifetime (plus pins.mjs's 24 h margin) as of now: a ui pin made
# after the A1 build read the pin history (a rollback, a re-run) would otherwise lose its lazy chunks once pigeon serves
# its tabs. For the cutover runner before A1's pins are pushed; needs crane, jq, node and git (and a registry login
# only if the package is private).
#
#   a1-ready.sh <A1 commit, 40 hex> <DE0CH/YYC clone, up to date with its main>
#
# Exit 0: ready. 1: the image is missing, or does not seed its own ui image or a served one. 2: usage.
set -euo pipefail
commit=${1:-}
yyc=${2:-}
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] && [ -n "$yyc" ] && git -C "$yyc" rev-parse --verify --quiet HEAD >/dev/null \
  || { echo "usage: a1-ready.sh <commit, 40 hex> <DE0CH/YYC clone>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
tag=${commit::12}
image=ghcr.io/de0ch/pigeon-app:$tag
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
crane export --platform linux/amd64 "$image" - | tar -x -C "$work" meta \
  || {
    echo "a1-ready: no $image. pigeon-app.yml has not built this commit (its image job was cancelled behind a newer push):" \
      "re-run that commit's pigeon-app run (Actions, the run of $tag, Re-run all jobs), which builds at that commit;" \
      "a workflow_dispatch builds the head of main instead (scripts/pigeon-app/README.md, The A1-A5 freeze)" >&2
    exit 1
  }
built=$(jq -r '.[0].commit' "$work/meta/builds.json")
[ "$built" = "$commit" ] || { echo "a1-ready: $image was built from $built, not $commit" >&2; exit 1; }
seeded=$(jq -r --arg tag "$tag" '.[$tag].commit // empty' "$work/meta/seeds.json")
[ "$seeded" = "$commit" ] || { echo "a1-ready: $image does not seed yyc-ai-ui:$tag (its own ui image)" >&2; exit 1; }
node "$here/pins.mjs" --repo "$yyc" --image yyc-ai-ui --hours 720 --margin-hours 24 > "$work/pins.txt"
missing=()
while read -r served _; do
  jq -e --arg tag "$served" 'has($tag)' "$work/meta/seeds.json" >/dev/null || missing+=("$served")
done < "$work/pins.txt"
if [ "${#missing[@]}" -gt 0 ]; then
  echo "a1-ready: $image does not seed yyc-ai-ui ${missing[*]}, served within 744 h: a ui pin landed after its build read" \
    "the pin history. Re-run the A1 commit's pigeon-app run so it reads the pins again" >&2
  exit 1
fi
echo "a1-ready: $image seeds yyc-ai-ui:$tag and $(jq 'length - 1' "$work/meta/seeds.json") other ui images, every one served within 744 h ($(wc -l < "$work/pins.txt") pins)"
