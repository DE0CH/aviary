#!/usr/bin/env bash
# Makes a k3s node the sandbox hosts' gateway (crates/sandboxd/docs/HOST.md, "Gateway"): sandboxd as the hub with
# `gateway: true` runs no sandboxes; it is the WireGuard hub for the AWS hosts (overlay address 10.202.0.1), translates
# the egress address 10.202.1.1:3128 to an egress pod, and serves sandbox images to the AWS hosts. The private network's
# route 10.202.0.0/15 points at this node (gateway-net.sh).
#
# The node is shared with the cluster, so this changes nothing of it beyond: crane and sandboxd (one static binary) in
# /usr/local/bin, /etc/sbx, /var/lib/sbx (images), the unit sandboxd.service, wg0
# (UDP port $SBX_WG_PORT; flannel's own WireGuard has 51820) and nft table `ip sbxhost`, whose rules only concern wg0's
# traffic. No packages, no sysctl beyond ip_forward (already on), no nftables.service.
#
# Usage (as root, from a copy of aviary's fabric/: host/ and bin/sandboxd side by side, host/fetch-sandboxd.sh fills
# bin/; or from the sandboxd image's /opt/hermit-gateway, which holds gateway.sh, sandboxd and crane in one folder):
#   gateway.sh [image ref]
# Beforehand: /etc/sbx/tls/{ca.crt,host.crt,host.key} (the gateway's server certificate, make-certs.sh `gateway`) and,
# optionally, /etc/sbx/wg.key (a kept key, so AWS hosts that know its public key keep working).
# Environment: SBX_NAME (hermit-gw), SBX_WG_PORT (51821), SBX_PRIVATE_GATEWAY (10.0.0.1), SBX_CONTROL_CIDR (10.0.0.0/16),
#              SBX_START (1: start or restart sandboxd now; 0: install only)
set -euo pipefail
IMAGE=${1:-}
CRANE_VERSION=0.20.6
here=$(cd "$(dirname "$0")" && pwd)

for t in nft wg zstd curl ip; do command -v $t >/dev/null || { echo "missing $t on this node" >&2; exit 1; }; done

if [ -x "$here/crane" ] && [ "$("$here/crane" version)" = "$CRANE_VERSION" ]; then
  install -m755 "$here/crane" /usr/local/bin/crane.new && mv /usr/local/bin/crane.new /usr/local/bin/crane
elif [ "$(crane version 2>/dev/null)" != "$CRANE_VERSION" ]; then
  curl -fsSL "https://github.com/google/go-containerregistry/releases/download/v$CRANE_VERSION/go-containerregistry_Linux_x86_64.tar.gz" | tar -xz -C /usr/local/bin crane
fi

# systemd-networkd dropped the gateway's routes (the sandbox range dev wg0, table 51822) when it restarted on a node
# (2026-10-21): keep it off routes and rules it didn't make (k3s/flannel's are of the same kind); sandboxd re-makes
# its own within 10 s too. Takes effect at networkd's next start; nothing is restarted here.
mkdir -p /etc/systemd/networkd.conf.d
printf '[Network]\nManageForeignRoutes=no\nManageForeignRoutingPolicyRules=no\n' > /etc/systemd/networkd.conf.d/50-sbx.conf

bin=$here/../bin/sandboxd; [ -x "$bin" ] || bin=$here/sandboxd
[ -x "$bin" ] || { echo "missing $here/../bin/sandboxd (host/fetch-sandboxd.sh)" >&2; exit 1; }
install -m755 "$bin" /usr/local/bin/sandboxd.new && mv /usr/local/bin/sandboxd.new /usr/local/bin/sandboxd
rm -rf /opt/node /opt/sandboxd /opt/sandboxd.old   # the Node sandboxd's leftovers

install -d -m 700 /etc/sbx /etc/sbx/tls /var/lib/sbx /var/lib/sbx/images /var/log/sbx
[ -s /etc/sbx/wg.key ] || (umask 077; wg genkey > /etc/sbx/wg.key)
for f in ca.crt host.crt host.key; do [ -s /etc/sbx/tls/$f ] || { echo "missing /etc/sbx/tls/$f" >&2; exit 1; }; done

umask 077
cat > /etc/sbx/host.json.new <<EOF
{
  "name": "${SBX_NAME:-hermit-gw}", "role": "hub", "gateway": true,
  "wgAddress": "10.202.0.1", "hostCidr": "10.202.0.0/24", "wgPort": ${SBX_WG_PORT:-51821},
  "controlCidr": "${SBX_CONTROL_CIDR:-10.0.0.0/16}", "sandboxCidr": "10.203.0.0/16", "egressVip": "10.202.1.1",
  "privateGateway": "${SBX_PRIVATE_GATEWAY:-10.0.0.1}", "reserveMemory": 0,
  "tls": {"ca": "/etc/sbx/tls/ca.crt", "cert": "/etc/sbx/tls/host.crt", "key": "/etc/sbx/tls/host.key"}
}
EOF
mv /etc/sbx/host.json.new /etc/sbx/host.json
umask 022

cat > /etc/systemd/system/sandboxd.service <<'EOF'
[Unit]
Description=sandboxd as the sandbox hosts' gateway (no sandboxes; crates/sandboxd/docs/HOST.md "Gateway")
After=network-online.target k3s-agent.service
Wants=network-online.target
ConditionPathExists=/etc/sbx/host.json

[Service]
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
MemoryHigh=192M
MemoryMax=256M
# sandboxd's own cgroup (daemon/: memory.min = memory.max = its budget) and its commands' (helpers/); crates/sandboxd/docs/MEMORY.md
Delegate=yes
DelegateSubgroup=daemon
LimitMEMLOCK=infinity
ExecStart=/usr/local/bin/sandboxd
# wg0 and the nft table stay across restarts (peers and routes keep working); to remove the gateway:
# systemctl disable --now sandboxd; ip link del wg0; nft delete table ip sbxhost
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable -q sandboxd.service

# the sandbox image, unpacked once per digest, for the AWS hosts to fetch
if [ -n "$IMAGE" ]; then
  digest=$(crane digest "$IMAGE"); digest=${digest#sha256:}; dir=/var/lib/sbx/images/$digest
  if [ ! -f "$dir/.done" ]; then
    rm -rf "$dir"; mkdir -p "$dir/rootfs"
    crane export "${IMAGE%%[:@]*}@sha256:$digest" - | tar -x -C "$dir/rootfs"
    crane config "${IMAGE%%[:@]*}@sha256:$digest" > "$dir/config.json"; touch "$dir/.done"
  fi
  echo "image $digest ready"
fi

if [ "${SBX_START:-1}" = 1 ]; then
  systemctl restart sandboxd
  for _ in $(seq 30); do systemctl is-active -q sandboxd && ss -Hltn 'sport = :7443' | grep -q . && break; sleep 1; done
  systemctl is-active sandboxd >/dev/null || { journalctl -u sandboxd -n 30 --no-pager >&2; exit 1; }
  echo "gateway ready: wg $(wg show wg0 public-key) port $(wg show wg0 listen-port)"
fi
