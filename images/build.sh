#!/usr/bin/env bash
# Builds one of aviary's service images (platform 5.2) from the static binaries of `dist/` with platform 5.3's labels,
# loads it into the local Docker as <tag> and, with a fourth argument, saves it as a tar (`docker save`: the OCI image
# layout, which `docker load` and `k3d image import` read) for ci.yml's `images` artifact.
#
#   images/build.sh <image> <dist dir> <tag> [<tar out>]
#     <image>: pigeon | magpie | hermit | sandboxd | aviary-testing (hermit-sandbox: images/sandbox/build.sh)
#
# The revision is AVIARY_REVISION (CI sets it) or the checkout's HEAD; the version label is its first 12 hex. The
# provides/accepts labels come from the built binary's own `--version` line (`<name> <sha12> protocols=<json>`), run
# inside the built image, so a label never disagrees with the code; images whose binary serves no numbered surface
# carry `{}`. Timestamps from the commit (SOURCE_DATE_EPOCH, rewrite-timestamp=true). Extra `docker buildx build`
# flags (a cache) in IMAGE_BUILD_FLAGS.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
image=${1:?image}
dist=$(cd "${2:?dist dir}" && pwd)
tag=${3:?tag}
out=${4:-}
rev=${AVIARY_REVISION:-$(git -C "$here" rev-parse HEAD 2>/dev/null || echo 0000000000000000000000000000000000000000)}
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$here" log -1 --format=%ct 2>/dev/null || echo 0)}
case $image in
  pigeon) files=(pigeon); bin=pigeon ;;
  magpie) files=(magpie); bin=magpie ;;
  hermit) files=(hermit); bin=hermit ;;
  sandboxd) files=(sandboxd); bin= ;;
  aviary-testing) files=(aviary-conformance test-gateway mock-model fake-tools fake-access fake-sandboxd aviary-e2e); bin= ;;
  *) echo "build.sh: unknown image $image" >&2; exit 2 ;;
esac
ctx=$(mktemp -d)
trap 'rm -rf "$ctx"' EXIT
mkdir -p "$ctx/dist"
for f in "${files[@]}"; do
  [ -x "$dist/$f" ] || { echo "build.sh: no executable $dist/$f" >&2; exit 1; }
  cp "$dist/$f" "$ctx/dist/"
done
build() { # build <extra flags…>
  # shellcheck disable=SC2086 # IMAGE_BUILD_FLAGS is a list of flags
  docker buildx build ${IMAGE_BUILD_FLAGS:-} -f "$here/$image/Dockerfile" \
    --build-arg REVISION="$rev" --build-arg SOURCE_DATE_EPOCH="$epoch" \
    --label org.opencontainers.image.version="${rev:0:12}" \
    "$@" --output type=docker,rewrite-timestamp=true -t "$tag" "$ctx"
}
build --label dev.de0ch.aviary.provides='{}' --label dev.de0ch.aviary.accepts='{}'
if [ -n "$bin" ]; then
  line=$(docker run --rm --network none --entrypoint "/usr/local/bin/$bin" "$tag" --version)
  protocols=${line#* protocols=}
  [ "$protocols" != "$line" ] || { echo "build.sh: '$line' has no protocols=" >&2; exit 1; }
  provides=$(jq -c '.provides' <<<"$protocols")
  accepts=$(jq -c '.accepts' <<<"$protocols")
  # the same layers from the cache, with the labels from the binary
  build --label dev.de0ch.aviary.provides="$provides" --label dev.de0ch.aviary.accepts="$accepts"
fi
docker image inspect -f '{{json .Config.Labels}}' "$tag"
if [ -n "$out" ]; then docker save -o "$out" "$tag"; fi
