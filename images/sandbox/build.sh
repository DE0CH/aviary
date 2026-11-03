#!/usr/bin/env bash
# Builds ghcr.io/de0ch/hermit-sandbox (images/sandbox/Dockerfile) from a context of this directory and the static
# musl `supervisor` and `supervisor-keeper` of the same commit, with platform 5.3's labels: the revision, the version
# (sha12), what the supervisor speaks (`supervisor --speaks`) and the supervisor binary's sha256 (says whether a new
# sandbox image actually changed the supervisor).
# Usage: build.sh <dir with supervisor and supervisor-keeper> <tag> [<40 hex revision>]
#   extra `docker build` flags (a cache, --progress) in SANDBOX_BUILD_FLAGS
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bin=$(cd "${1:?dir with supervisor and supervisor-keeper}" && pwd)
tag=${2:?image tag}
rev=${3:-$(git -C "$here" rev-parse HEAD 2>/dev/null || echo dev)}
for b in supervisor supervisor-keeper; do
  [ -x "$bin/$b" ] || { echo "build.sh: no executable $bin/$b" >&2; exit 1; }
  file "$bin/$b" | grep -q 'statically linked\|static-pie linked' || { echo "build.sh: $bin/$b is not static" >&2; exit 1; }
done
ctx=$(mktemp -d)
trap 'rm -rf "$ctx"' EXIT
cp "$here"/Dockerfile "$here"/*.sh "$here"/*.py "$here"/90sandbox "$here"/sudoers "$here"/profile "$here"/npmrc "$here"/pip.conf "$ctx/"
rm -f "$ctx/build.sh" "$ctx/smoke.sh"
mkdir -p "$ctx/dist"
cp "$bin/supervisor" "$bin/supervisor-keeper" "$ctx/dist/"
speaks=$("$bin/supervisor" --speaks)
sum=$(sha256sum "$bin/supervisor" | cut -d' ' -f1)
# the image config's timestamps from the commit (platform 5.2)
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$here" log -1 --format=%ct 2>/dev/null || echo 0)}
# shellcheck disable=SC2086 # SANDBOX_BUILD_FLAGS is a list of flags
docker buildx build ${SANDBOX_BUILD_FLAGS:-} -f "$ctx/Dockerfile" \
  --build-arg REVISION="$rev" --build-arg VERSION="${rev:0:12}" \
  --build-arg SOURCE_DATE_EPOCH="$epoch" \
  --build-arg SPEAKS="$speaks" --build-arg SUPERVISOR_SHA256="$sum" \
  -t "$tag" "$ctx"
