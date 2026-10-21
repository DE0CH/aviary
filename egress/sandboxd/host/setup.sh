#!/usr/bin/env bash
# Prepares an Ubuntu 24.04 VM as a sandbox host for provider "vm" (docs/SANDBOX-VM.md). Idempotent: run it again to
# upgrade sandboxd or runsc (the DE0CH/gvisor fork); running sandboxes are not touched (sandboxd restarts with
# KillMode=process, runsc sandboxes live in their own cgroups).
#
# Usage (as root, from a copy of chat/sandboxd: host/ and bin/sandboxd side by side; host/fetch-sandboxd.sh puts the
# released binary into bin/):
#   setup.sh hub   [image ref]   the Hetzner hub: writes /etc/sbx/host.json from the environment below, starts sandboxd
#   setup.sh burst [image ref]   an AWS image (AMI) bake: everything but host.json, which each instance gets from its
#                                user data at first boot (sbx-userdata.service)
# Environment:
#   GV           DE0CH/gvisor release tag (default below)
#   hub: SBX_NAME (default yyc-sbx-hub), SBX_RESERVE_MIB (default 1536; the cx23 hub: 700 = 6 sandboxes of 512 MiB), SBX_PRIVATE_GATEWAY (10.0.0.1),
#        SBX_POD_CIDR (10.244.0.0/16), SBX_CONTROL_CIDR (10.0.0.0/16); TLS files in /etc/sbx/tls/{ca.crt,host.crt,host.key}
#        (installed beforehand); a registry login for private images in /etc/sbx/docker/config.json (beforehand)
#   bake: REGISTRY_AUTH_FILE  a docker config.json used only while pre-pulling the image, then deleted
set -euo pipefail
ROLE=${1:?hub or burst}; IMAGE=${2:-}
GV=${GV:-precopy-20261021.1}
CRANE_VERSION=0.20.6
here=$(cd "$(dirname "$0")" && pwd)
export DEBIAN_FRONTEND=noninteractive

apt-get -qq update || apt-get -qq update
apt-get -qq install -y jq nftables wireguard-tools zstd curl iproute2 ca-certificates logrotate rsync >/dev/null
systemctl disable --now nftables.service >/dev/null 2>&1 || true   # sandboxd owns its tables; no flush at boot

# the Node sandboxd's leftovers (sandboxd is a static Rust binary now)
rm -rf /opt/node /opt/sandboxd /opt/sandboxd.old

# gVisor: the fork's release tarball (runsc + its sidecars in gvisor-bin/), checked against SHA256SUMS
if [ "$(cat /usr/local/bin/gvisor-bin/.release 2>/dev/null)" != "$GV" ]; then
  t=$(mktemp -d); u=https://github.com/DE0CH/gvisor/releases/download/$GV
  curl -fsSL -o "$t/gvisor.tar.zstd" "$u/gvisor.tar.zstd"; curl -fsSL -o "$t/SHA256SUMS" "$u/SHA256SUMS"
  (cd "$t" && grep ' gvisor.tar.zstd$' SHA256SUMS | sha256sum -c --quiet)
  mkdir -p "$t/x"; tar --zstd -xf "$t/gvisor.tar.zstd" -C "$t/x"
  install -m755 "$t/x/runsc" "$t/x/containerd-shim-runsc-v1" /usr/local/bin/
  rm -rf /usr/local/bin/gvisor-bin.new; mkdir -p /usr/local/bin/gvisor-bin.new
  install -m755 "$t/x/gvisor-bin/"* /usr/local/bin/gvisor-bin.new/
  echo "$GV" > /usr/local/bin/gvisor-bin.new/.release
  rm -rf /usr/local/bin/gvisor-bin; mv /usr/local/bin/gvisor-bin.new /usr/local/bin/gvisor-bin; rm -rf "$t"
fi

if [ "$(crane version 2>/dev/null)" != "$CRANE_VERSION" ]; then
  curl -fsSL "https://github.com/google/go-containerregistry/releases/download/v$CRANE_VERSION/go-containerregistry_Linux_x86_64.tar.gz" | tar -xz -C /usr/local/bin crane
fi

# JuiceFS (docs/CLAUDE-CODE.md): the conversations' /workspace and the users' ~/.claude, mounted at /var/lib/sbx/jfs
# by sbx-jfs.service from /etc/sbx/juicefs.env (META_URL without the password, META_PASSWORD; the bucket's keys are
# in the volume's metadata). No --writeback: after a live migration the destination host must see every write.
JFS=${JFS:-1.4.1}
if ! juicefs version 2>/dev/null | grep -q "version $JFS"; then
  t=$(mktemp -d)
  curl -fsSL -o "$t/jfs.tgz" "https://github.com/juicedata/juicefs/releases/download/v$JFS/juicefs-$JFS-linux-amd64.tar.gz"
  curl -fsSL -o "$t/sums" "https://github.com/juicedata/juicefs/releases/download/v$JFS/checksums.txt"
  (cd "$t" && grep "juicefs-$JFS-linux-amd64.tar.gz" sums | sed 's/  .*$/  jfs.tgz/' | sha256sum -c - >/dev/null)
  tar -xzf "$t/jfs.tgz" -C "$t" juicefs && install -m755 "$t/juicefs" /usr/local/bin/juicefs; rm -rf "$t"
fi
cat > /etc/systemd/system/sbx-jfs.service <<'EOF'
[Unit]
Description=JuiceFS: the sandboxes' /workspace and ~/.claude (/var/lib/sbx/jfs)
After=network-online.target sbx-userdata.service
Wants=network-online.target
Before=sandboxd.service
ConditionPathExists=/etc/sbx/juicefs.env
[Service]
Type=simple
EnvironmentFile=/etc/sbx/juicefs.env
ExecStartPre=/bin/mkdir -p /var/lib/sbx/jfs /var/cache/sbx-jfs
ExecStart=/bin/sh -c 'exec /usr/local/bin/juicefs mount --no-usage-report --cache-dir /var/cache/sbx-jfs --cache-size 4096 --open-cache 0 "$META_URL" /var/lib/sbx/jfs'
ExecStop=/bin/umount -l /var/lib/sbx/jfs
Restart=always
RestartSec=2
[Install]
WantedBy=multi-user.target
EOF

# sandboxd: one static binary (docs/SANDBOXD-MEMORY.md); replaced atomically, the running one keeps its inode
[ -x "$here/../bin/sandboxd" ] || { echo "missing $here/../bin/sandboxd (host/fetch-sandboxd.sh)" >&2; exit 1; }
install -m755 "$here/../bin/sandboxd" /usr/local/bin/sandboxd.new && mv /usr/local/bin/sandboxd.new /usr/local/bin/sandboxd

install -d -m 700 /etc/sbx /etc/sbx/tls /var/lib/sbx /var/lib/sbx/images /var/log/sbx
[ -s /etc/sbx/wg.key ] || (umask 077; wg genkey > /etc/sbx/wg.key)

cat > /etc/sysctl.d/90-sbx.conf <<'EOF'
net.ipv4.ip_forward = 1
net.ipv4.conf.all.rp_filter = 0
net.ipv4.conf.default.rp_filter = 0
# each page a dirty-tracked sandbox writes during a pre-copy round can split a host mapping of its memory
vm.max_map_count = 262144
EOF
sysctl -q -p /etc/sysctl.d/90-sbx.conf

# migration images live in RAM; a sandbox received by migration keeps its memory.img from here as its memory, so no
# cap below RAM (the sandboxes' cgroups limit their memory)
cat > /etc/systemd/system/run-sbx-ckpt.mount <<'EOF'
[Unit]
Description=sandboxd migration images (tmpfs)
[Mount]
What=sbx-ckpt
Where=/run/sbx/ckpt
Type=tmpfs
Options=size=100%,mode=700
[Install]
WantedBy=multi-user.target
EOF

# Memory isolation (docs/SANDBOX-VM.md "Capacity"): sandboxd's unit (sandboxd, the runsc CLI calls and image pulls it
# runs, and runsc's per-sandbox runsc-fd-parking sidecars, ~8 KiB each) has a reservation and a hard cap, and the
# sandboxes' parent cgroup /sbx is capped at RAM minus the host reserve, so neither side can starve the other. Each
# sandbox's own cgroup (/sbx/<name>: sentry, gofer and the sandbox's memory) has the sandbox's limit. The limits sit on
# the unit itself, not a slice of its own: the fd-parking sidecars stay in the unit's cgroup across restarts
# (KillMode=process), so the unit could never move to another slice while sandboxes run. memory.min only protects
# as far as every ancestor does, hence the same reservation on system.slice.
rm -f /etc/systemd/system/sbx-host.slice
mkdir -p /etc/systemd/system/system.slice.d
cat > /etc/systemd/system/system.slice.d/50-sbx.conf <<'EOF'
[Slice]
MemoryMin=160M
EOF

cat > /usr/local/sbin/sbx-cgroup <<'EOF'
#!/bin/bash
# /sbx: the sandboxes' parent cgroup, capped at MemTotal - reserveMemory (host.json). A backstop: the controller
# places no more sandbox limits than that.
set -euo pipefail
cg=/sys/fs/cgroup/sbx
mkdir -p "$cg"
for c in cpu memory pids; do grep -qw "$c" /sys/fs/cgroup/cgroup.subtree_control || echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control; done
echo "+cpu +memory +pids" > "$cg/cgroup.subtree_control"
total=$(( $(awk '/^MemTotal:/{print $2}' /proc/meminfo) * 1024 ))
reserve=$(jq -r '.reserveMemory // 1610612736' /etc/sbx/host.json)
echo $(( total - reserve )) > "$cg/memory.max"
EOF
chmod 755 /usr/local/sbin/sbx-cgroup

cat > /etc/systemd/system/sandboxd.service <<'EOF'
[Unit]
Description=sandboxd (sandbox provider "vm" host agent, docs/SANDBOX-VM.md)
After=network-online.target run-sbx-ckpt.mount sbx-userdata.service
Wants=network-online.target
Requires=run-sbx-ckpt.mount
ConditionPathExists=/etc/sbx/host.json

[Service]
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
MemoryMin=160M
MemoryHigh=384M
MemoryMax=512M
# sandboxd splits its cgroup: daemon/ (itself: memory.min = memory.max = its budget, docs/SANDBOXD-MEMORY.md) and
# helpers/ (the commands it runs, capped by limits.helperMemoryMaxMiB); systemd starts it in daemon/
Delegate=yes
DelegateSubgroup=daemon
LimitMEMLOCK=infinity
ExecStartPre=/usr/local/sbin/sbx-cgroup
ExecStart=/usr/local/bin/sandboxd
# sandboxes are runsc processes in their own cgroups (/sbx/<name>); a restart of sandboxd must never touch them
KillMode=process
Restart=always
RestartSec=2
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF

cat > /etc/logrotate.d/sbx <<'EOF'
/var/log/sbx/*.log /var/log/sbx/*.out {
  daily
  rotate 7
  maxsize 20M
  compress
  missingok
  notifempty
  copytruncate
}
EOF

if [ "$ROLE" = burst ]; then
  # AWS: each instance's host.json, TLS files and WireGuard key come from its user data (IMDSv2) at first boot
  cat > /usr/local/sbin/sbx-userdata <<'EOF'
#!/bin/bash
# Writes /etc/sbx/host.json, /etc/sbx/wg.key and /etc/sbx/tls/* from this instance's user data (JSON from the scaler).
set -euo pipefail
[ -s /etc/sbx/host.json ] && exit 0
tok=$(curl -fsS -X PUT -H 'X-aws-ec2-metadata-token-ttl-seconds: 300' http://169.254.169.254/latest/api/token)
ud=$(mktemp); chmod 600 "$ud"
curl -fsS -H "X-aws-ec2-metadata-token: $tok" http://169.254.169.254/latest/user-data > "$ud"
umask 077
jq -r .wgPrivateKey "$ud" > /etc/sbx/wg.key
jq -r .tls.ca "$ud" > /etc/sbx/tls/ca.crt
jq -r .tls.cert "$ud" > /etc/sbx/tls/host.crt
jq -r .tls.key "$ud" > /etc/sbx/tls/host.key
if jq -e .juicefs "$ud" >/dev/null; then
  { echo "META_URL=$(jq -r .juicefs.metaUrl "$ud")"; echo "META_PASSWORD=$(jq -r .juicefs.metaPassword "$ud")"; } > /etc/sbx/juicefs.env
fi
jq '.host + {tls: {ca: "/etc/sbx/tls/ca.crt", cert: "/etc/sbx/tls/host.crt", key: "/etc/sbx/tls/host.key"}, wgKeyFile: "/etc/sbx/wg.key"}' "$ud" > /etc/sbx/host.json
rm -f "$ud"
EOF
  chmod 755 /usr/local/sbin/sbx-userdata
  cat > /etc/systemd/system/sbx-userdata.service <<'EOF'
[Unit]
Description=sandboxd host configuration from EC2 user data
After=network-online.target
Wants=network-online.target
Before=sandboxd.service
ConditionPathExists=!/etc/sbx/host.json
[Service]
Type=oneshot
ExecStart=/usr/local/sbin/sbx-userdata
RemainAfterExit=yes
[Install]
WantedBy=multi-user.target
EOF
  rm -f /etc/sbx/wg.key   # per instance, from user data
fi

systemctl daemon-reload
systemctl enable -q run-sbx-ckpt.mount sandboxd.service sbx-jfs.service
[ "$ROLE" = burst ] && systemctl enable -q sbx-userdata.service
systemctl start run-sbx-ckpt.mount

# pre-pull the sandbox image (unpacked once per digest; every sandbox shares it)
if [ -n "$IMAGE" ]; then
  if [ "$ROLE" = hub ]; then export DOCKER_CONFIG=/etc/sbx/docker; fi
  if [ -n "${REGISTRY_AUTH_FILE:-}" ]; then d=$(mktemp -d); cp "$REGISTRY_AUTH_FILE" "$d/config.json"; export DOCKER_CONFIG=$d; fi
  digest=$(crane digest "$IMAGE"); digest=${digest#sha256:}; dir=/var/lib/sbx/images/$digest
  if [ ! -f "$dir/.done" ]; then
    rm -rf "$dir"; mkdir -p "$dir/rootfs"
    crane export "${IMAGE%%[:@]*}@sha256:$digest" - | tar -x -C "$dir/rootfs"
    crane config "${IMAGE%%[:@]*}@sha256:$digest" > "$dir/config.json"; touch "$dir/.done"
  fi
  [ -n "${REGISTRY_AUTH_FILE:-}" ] && rm -rf "$DOCKER_CONFIG"
  echo "image $digest ready"
fi

if [ "$ROLE" = hub ]; then
  for f in ca.crt host.crt host.key; do [ -s /etc/sbx/tls/$f ] || { echo "missing /etc/sbx/tls/$f" >&2; exit 1; }; done
  jq -n --arg name "${SBX_NAME:-yyc-sbx-hub}" --argjson reserve "$(( ${SBX_RESERVE_MIB:-1536} * 1024 * 1024 ))" \
        --arg gw "${SBX_PRIVATE_GATEWAY:-10.0.0.1}" --arg pods "${SBX_POD_CIDR:-10.244.0.0/16}" --arg ctl "${SBX_CONTROL_CIDR:-10.0.0.0/16}" '{
    name: $name, role: "hub", wgAddress: "10.201.0.1", hostCidr: "10.201.0.0/24", controlCidr: $ctl,
    sandboxCidr: "10.200.0.0/16", egressVip: "10.201.1.1", podCidr: $pods, privateGateway: $gw,
    reserveMemory: $reserve, registryConfigDir: "/etc/sbx/docker",
    tls: {ca: "/etc/sbx/tls/ca.crt", cert: "/etc/sbx/tls/host.crt", key: "/etc/sbx/tls/host.key"}
  }' > /etc/sbx/host.json.new
  chmod 600 /etc/sbx/host.json.new; mv /etc/sbx/host.json.new /etc/sbx/host.json
  systemctl restart sandboxd
  for i in $(seq 30); do systemctl is-active -q sandboxd && ss -Hltn 'sport = :7443' | grep -q . && break; sleep 1; done
  systemctl is-active sandboxd >/dev/null || { journalctl -u sandboxd -n 30 --no-pager >&2; exit 1; }
  echo "hub ready: $(runsc --version | head -1) ($GV), wg $(wg show wg0 public-key)"
else
  # an image bake: leave no instance identity behind
  rm -f /etc/sbx/host.json /etc/sbx/tls/* /etc/sbx/wg.key /etc/sbx/juicefs.env
  cloud-init clean --logs >/dev/null 2>&1 || true
  echo "burst image ready: $(runsc --version | head -1) ($GV)"
fi
