#!/usr/bin/env bash
# Build magpie for glibc on Debian trixie with libheif-dev (platform 5.1 names bookworm, whose libheif 1.15 is older
# than libheif-rs's minimum 1.17; README "Where this differs"): a trixie Rust image (pinned by digest) plus libheif-dev, run as the calling user so target/ stays theirs, with the caller's cargo registry mounted so the
# CI cache (Swatinem/rust-cache, ~/.cargo/registry) covers it. Output: target/trixie/release/magpie.
#   build-magpie.sh <rust trixie image>
set -euo pipefail
base=$1
tag=aviary-magpie-build-trixie:local
docker build -q -t "$tag" - >/dev/null <<DOCKERFILE
FROM $base
RUN apt-get update && apt-get install -y --no-install-recommends libheif-dev && rm -rf /var/lib/apt/lists/*
DOCKERFILE
cargo_home=${CARGO_HOME:-$HOME/.cargo}
mkdir -p "$cargo_home/registry" "$cargo_home/git" target/trixie
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD:/src" -w /src \
  -v "$cargo_home/registry:/cargo/registry" -v "$cargo_home/git:/cargo/git" \
  -e CARGO_HOME=/cargo -e HOME=/tmp -e RUSTUP_TOOLCHAIN=1.99.1 \
  -e CARGO_TARGET_DIR=/src/target/trixie -e CARGO_TERM_COLOR \
  -e AVIARY_REVISION -e SOURCE_DATE_EPOCH \
  "$tag" cargo build --release --locked -p magpie --bin magpie
