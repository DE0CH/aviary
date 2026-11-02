#!/usr/bin/env bash
# One gate host (crates/supervisor/GATE.md): an ordinary Ubuntu 24.04 VM with the sandbox runtime YYC runs
# (DE0CH/gvisor release, the same tarball and SHA256SUMS check as jarvis3 chat/sandboxd/host/setup.sh), crane, the
# sandbox image unpacked as a rootfs, and a tmpfs for checkpoint images. Run as root.
# Usage: host-setup.sh <sandbox image ref> [release tag]
set -euo pipefail
IMAGE=${1:?image}; GV=${2:-precopy-20261102.1}
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update || true
apt-get -qq install -y jq zstd python3 strace >/dev/null
if [ "$(cat /usr/local/bin/gvisor-bin/.release 2>/dev/null)" != "$GV" ]; then
  t=$(mktemp -d)
  u=https://github.com/DE0CH/gvisor/releases/download/$GV
  curl -fsSL -o "$t/gvisor.tar.zstd" "$u/gvisor.tar.zstd"; curl -fsSL -o "$t/SHA256SUMS" "$u/SHA256SUMS"
  (cd "$t" && grep ' gvisor.tar.zstd$' SHA256SUMS | sha256sum -c --quiet)
  mkdir -p "$t/x"; tar --zstd -xf "$t/gvisor.tar.zstd" -C "$t/x"
  install -m755 "$t/x/runsc" "$t/x/containerd-shim-runsc-v1" /usr/local/bin/
  rm -rf /usr/local/bin/gvisor-bin; mkdir -p /usr/local/bin/gvisor-bin; install -m755 "$t/x/gvisor-bin/"* /usr/local/bin/gvisor-bin/
  echo "$GV" > /usr/local/bin/gvisor-bin/.release; rm -rf "$t"
fi
if ! command -v crane >/dev/null; then
  curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.20.6/go-containerregistry_Linux_x86_64.tar.gz | tar -xz -C /usr/local/bin crane
fi
digest=$(crane digest "$IMAGE"); dir=/var/lib/gate/images/${digest#sha256:}
if [ ! -f "$dir/.done" ]; then
  mkdir -p "$dir/rootfs"; crane export "$IMAGE" - | tar -x -C "$dir/rootfs"; crane config "$IMAGE" > "$dir/config.json"; touch "$dir/.done"
fi
ln -sfn "$dir" /var/lib/gate/image
mkdir -p /run/gate/ckpt
mountpoint -q /run/gate/ckpt || mount -t tmpfs -o size=100%,mode=700 gate-ckpt /run/gate/ckpt
sysctl -qw vm.max_map_count=262144
echo "ok: $(runsc --version | head -1) ($GV), image $IMAGE $digest"
