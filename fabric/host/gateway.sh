#!/usr/bin/env bash
# Makes a k3s node the sandbox hosts' gateway (crates/sandboxd/docs/HOST.md, "Gateway"): sandboxd as the hub with
# `gateway: true` runs no sandboxes; it is the WireGuard hub for the AWS hosts (host.json `wgAddress`), translates the
# egress address (`egressVip`, `egressPorts`) to an egress pod, and serves sandbox images to the AWS hosts. Every
# address is the deployment's and comes from the host.json it is given (hermit spec 14.2: the DaemonSet's ConfigMap).
#
# The node is shared with the cluster, so this changes nothing of it beyond: crane and sandboxd (one static binary) in
# /usr/local/bin, /etc/sbx, /var/lib/sbx (images), the unit sandboxd.service, wg0
# (UDP port host.json `wgPort`; flannel's own WireGuard has 51820) and nft table `ip sbxhost`, whose rules only concern wg0's
# traffic. No packages, no sysctl beyond ip_forward (already on), no nftables.service.
#
# Usage (as root, from a copy of aviary's fabric/: host/ and bin/sandboxd side by side, host/fetch-sandboxd.sh fills
# bin/; or from the sandboxd image's /opt/hermit-gateway, which holds gateway.sh, sandboxd, crane and jq in one folder):
#   gateway.sh [image ref]
# Beforehand: /etc/sbx/tls/{ca.crt,host.crt,host.key} (the gateway's server certificate, make-certs.sh `gateway`) and,
# optionally, /etc/sbx/wg.key (a kept key, so AWS hosts that know its public key keep working).
# Environment:
#   SBX_HOST_JSON  (required) the gateway's host.json, installed as /etc/sbx/host.json as it is: role hub,
#                  "gateway": true, wgAddress, hostCidr, wgPort, controlCidr, sandboxCidr, egressVip, egressPorts,
#                  podCidr, privateGateway, reserveMemory 0, limits, and tls naming the three files above (the fields:
#                  crates/sandboxd/docs/HOST.md "host.json")
#   SBX_START      1 (default): start sandboxd, or restart it when its binary or host.json changed since it last
#                  started (also by an SBX_START=0 run: /var/lib/sbx/restart-pending); 0: install only
set -euo pipefail
IMAGE=${1:-}
CRANE_VERSION=0.20.6
here=$(cd "$(dirname "$0")" && pwd)

for t in nft wg zstd curl ip cmp ss; do command -v $t >/dev/null || { echo "missing $t on this node" >&2; exit 1; }; done

if [ -x "$here/crane" ] && [ "$("$here/crane" version)" = "$CRANE_VERSION" ]; then
  install -m755 "$here/crane" /usr/local/bin/crane.new && mv /usr/local/bin/crane.new /usr/local/bin/crane
elif [ "$(crane version 2>/dev/null)" != "$CRANE_VERSION" ]; then
  curl -fsSL "https://github.com/google/go-containerregistry/releases/download/v$CRANE_VERSION/go-containerregistry_Linux_x86_64.tar.gz" | tar -xz -C /usr/local/bin crane
fi

# jq checks host.json: the image's static copy beside this script, else the node's
jq=$here/jq; [ -x "$jq" ] || jq=$(command -v jq) || { echo "missing jq on this node (the sandboxd image brings one)" >&2; exit 1; }

# systemd-networkd dropped the gateway's routes (the sandbox range dev wg0, table 51822) when it restarted on a node
# (2026-10-21): keep it off routes and rules it didn't make (k3s/flannel's are of the same kind); sandboxd re-makes
# its own within 10 s too. Takes effect at networkd's next start; nothing is restarted here.
mkdir -p /etc/systemd/networkd.conf.d
printf '[Network]\nManageForeignRoutes=no\nManageForeignRoutingPolicyRules=no\n' > /etc/systemd/networkd.conf.d/50-sbx.conf

bin=$here/../bin/sandboxd; [ -x "$bin" ] || bin=$here/sandboxd
[ -x "$bin" ] || { echo "missing $here/../bin/sandboxd (host/fetch-sandboxd.sh)" >&2; exit 1; }

install -d -m 700 /etc/sbx /etc/sbx/tls /var/lib/sbx /var/lib/sbx/images /var/log/sbx
[ -s /etc/sbx/wg.key ] || (umask 077; wg genkey > /etc/sbx/wg.key)
for f in ca.crt host.crt host.key; do [ -s /etc/sbx/tls/$f ] || { echo "missing /etc/sbx/tls/$f" >&2; exit 1; }; done

# host.json: the deployment's, as given; only checked to be a gateway's (top-level role hub and gateway true)
src=${SBX_HOST_JSON:?the gateway host.json (hermit spec 14.2: ConfigMap hermit-gateway)}
[ -s "$src" ] || { echo "missing $src" >&2; exit 1; }
"$jq" -e '.gateway == true and .role == "hub"' "$src" >/dev/null \
  || { echo "$src is not a gateway's host.json (\"role\": \"hub\", \"gateway\": true)" >&2; exit 1; }
# a change waits for a restart in a marker, so a later run restarts sandboxd even when an SBX_START=0 run installed it
pending=/var/lib/sbx/restart-pending
cmp -s "$src" /etc/sbx/host.json || touch "$pending"
cmp -s "$bin" /usr/local/bin/sandboxd || touch "$pending"
install -m600 "$src" /etc/sbx/host.json.new && mv /etc/sbx/host.json.new /etc/sbx/host.json
install -m755 "$bin" /usr/local/bin/sandboxd.new && mv /usr/local/bin/sandboxd.new /usr/local/bin/sandboxd

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
  # a restart never touches wg0 or nft (the data plane keeps flowing); without a change there is none
  # a running sandboxd on another binary than the installed one (/proc/<pid>/exe) also needs it
  pid=$(systemctl show -p MainPID --value sandboxd 2>/dev/null || echo 0)
  if [ -n "$pid" ] && [ "$pid" != 0 ] && ! cmp -s "/proc/$pid/exe" /usr/local/bin/sandboxd; then touch "$pending"; fi
  if [ -e "$pending" ] || ! systemctl is-active -q sandboxd; then systemctl restart sandboxd; fi
  rm -f "$pending"
  for _ in $(seq 30); do systemctl is-active -q sandboxd && ss -Hltn 'sport = :7443' | grep -q . && break; sleep 1; done
  systemctl is-active sandboxd >/dev/null || { journalctl -u sandboxd -n 30 --no-pager >&2; exit 1; }
  echo "gateway ready: wg $(wg show wg0 public-key) port $(wg show wg0 listen-port)"
fi
