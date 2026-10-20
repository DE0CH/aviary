#!/usr/bin/env bash
# Prepares an ordinary Ubuntu 24.04 VM as a sandbox host (no KVM needed): gVisor runsc (systrap), the sandbox image
# unpacked as a read-only rootfs, nftables, forwarding, NFS (serves checkpoint images to the migration target).
# Run as root on the host. Usage: host-setup.sh <sandbox image ref> <gVisor release, e.g. 20261012.0>
set -euo pipefail
IMAGE=${1:?image}; GV=${2:-20261012.0}
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update || apt-get -qq update || true
apt-get -qq install -y jq nftables nfs-kernel-server nfs-common iperf3 python3 >/dev/null

# gVisor: the release tarball carries runsc plus its sidecars (gvisor-bin/), which runsc expects next to itself
if [ "$(runsc --version 2>/dev/null | head -1)" != "runsc version release-$GV" ]; then
  arch=$(uname -m); u=https://storage.googleapis.com/gvisor/releases/release/$GV/$arch
  t=$(mktemp -d); curl -fsSL -o "$t/g.tar.zstd" "$u/gvisor.tar.zstd"; curl -fsSL -o "$t/g.sha" "$u/gvisor.tar.zstd.sha512"
  (cd "$t" && echo "$(cut -d' ' -f1 g.sha)  g.tar.zstd" | sha512sum -c --quiet)
  apt-get -qq install -y zstd >/dev/null; mkdir -p "$t/x"; tar --zstd -xf "$t/g.tar.zstd" -C "$t/x"
  install -m755 "$t/x/runsc" "$t/x/containerd-shim-runsc-v1" /usr/local/bin/
  mkdir -p /usr/local/bin/gvisor-bin; install -m755 "$t/x/gvisor-bin/"* /usr/local/bin/gvisor-bin/; rm -rf "$t"
fi

# crane (Apache-2.0) to unpack OCI images without a container engine
if ! command -v crane >/dev/null; then
  curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.20.6/go-containerregistry_Linux_x86_64.tar.gz | tar -xz -C /usr/local/bin crane
fi
# the image, unpacked once per host under its digest: every sandbox shares it read-only (gVisor's overlay keeps writes
# in the sandbox's own memory-file-backed upper layer, which travels in the checkpoint)
digest=$(crane digest "$IMAGE"); dir=/var/lib/sbx/images/${digest#sha256:}
if [ ! -f "$dir/.done" ]; then
  mkdir -p "$dir/rootfs"; crane export "$IMAGE" - | tar -x -C "$dir/rootfs"; crane config "$IMAGE" > "$dir/config.json"; touch "$dir/.done"
fi
ln -sfn "$dir" /var/lib/sbx/images/current

sysctl -qw net.ipv4.ip_forward=1 net.ipv4.conf.all.rp_filter=0 net.ipv4.conf.default.rp_filter=0
mkdir -p /run/sbx/ckpt /etc/exports.d
# checkpoint images live in RAM: saving is a memory copy, and the target reads them from here over NFS
mountpoint -q /run/sbx/ckpt || mount -t tmpfs -o size=60%,mode=700 sbx-ckpt /run/sbx/ckpt
echo "/run/sbx/ckpt *(ro,no_subtree_check,no_root_squash,fsid=1)" > /etc/exports.d/sbx.exports
exportfs -ra
echo "ok: $(runsc --version | head -1), image $digest"
