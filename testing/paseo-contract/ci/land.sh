#!/bin/bash
# land.sh PATCH BASE MESSAGE: lands a tested client-list change on main (.github/workflows/clients.yml, push job).
#
# Run in a fresh clone of the repository whose `origin` may be pushed to; it runs no client code. PATCH is the change
# to testing/paseo-contract (clients.txt and schema/) that the build job made and tested on BASE. The patch is applied
# on BASE and committed, then pushed to main (a plain push, never forced). When main moved meanwhile:
#   - if the move left testing/paseo-contract alone, the commit is replayed on the new main (the tested state is still
#     what lands) and pushed again;
#   - otherwise the tested list is stale: exit 3, and the caller redoes the build on the new main.
# Git hooks are off throughout (core.hooksPath=/dev/null). Exit 0 landed (or nothing to land), 1 error, 3 stale.
set -euo pipefail
patch=$1 base=$2 message=$3
dir=testing/paseo-contract
g() {
  git -c core.hooksPath=/dev/null -c commit.gpgsign=false -c user.name="github-actions[bot]" \
    -c user.email="41898282+github-actions[bot]@users.noreply.github.com" "$@"
}

g checkout -q --detach "$base"
if [ ! -s "$patch" ]; then
  echo "nothing to land"
  exit 0
fi
g apply --index --binary "$patch"
git diff --cached --name-only | grep -qv "^$dir/" && { echo "the patch touches files outside $dir" >&2; exit 1; }
g commit -q -m "$message"

for attempt in $(seq 1 8); do
  if g push -q origin HEAD:main; then
    echo "landed $(git rev-parse --short HEAD) on main"
    exit 0
  fi
  g fetch -q origin main
  if ! git diff --quiet "$base" FETCH_HEAD -- "$dir"; then
    echo "$dir changed on main since $base: the tested client list is stale" >&2
    exit 3
  fi
  # main moved elsewhere only: replay the one commit on it (no conflict is possible in this directory)
  g rebase -q FETCH_HEAD || { g rebase --abort || true; echo "rebase onto main failed" >&2; exit 1; }
  base=$(git rev-parse FETCH_HEAD)
  sleep $((attempt * 2))
done
echo "could not push after 8 attempts" >&2
exit 1
