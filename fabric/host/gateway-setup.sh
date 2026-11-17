#!/usr/bin/env bash
# Makes a plain Ubuntu 24.04 server the fabric's gateway (crates/sandboxd/docs/HOST.md "Gateway on a plain server"):
# the one box every sandbox's traffic passes, kept to that one job. On it: WireGuard, nftables, and sandboxd in gateway
# mode as a systemd unit. Not on it: a container runtime, a cluster agent, gVisor, JuiceFS, crane, a registry login.
# Idempotent: a second run upgrades sandboxd or installs a changed host.json; sandboxd adopts the data plane it left
# (wg0, its nft table, rules and routes), so traffic through the gateway does not stop.
#
# Usage (as root, from a copy of aviary's fabric/: host/ and bin/sandboxd side by side; host/fetch-sandboxd.sh puts
# the released binary into bin/):
#   SBX_HOST_JSON=<the gateway's host.json> gateway-setup.sh
# Beforehand, on the server (mode 600, root):
#   /etc/sbx/tls/{ca.crt,host.crt,host.key}   the gateway's server certificate and the fabric's CA (make-certs.sh)
#   /etc/sbx/wg.key                           the gateway's WireGuard key. The hosts hold its public key, so a server
#                                             that replaces a gateway is given that gateway's key, never a new one
# and, replacing a running gateway, its state so the hosts are peers from the first second:
#   /var/lib/sbx/{peers.json,egress-targets}  (optional; hermit pushes both again on its next pass)
# host.json: role hub, gateway true, limits.cgroups false (the unit's MemoryMax is the bound); `tls` and `wgKeyFile`
# are set to the files above.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=${SBX_HOST_JSON:?the gateway\'s host.json}
[ "$(id -u)" = 0 ] || { echo "gateway-setup: run as root" >&2; exit 1; }
[ -s "$src" ] || { echo "gateway-setup: missing $src" >&2; exit 1; }
[ -x "$here/../bin/sandboxd" ] || { echo "gateway-setup: missing $here/../bin/sandboxd (host/fetch-sandboxd.sh)" >&2; exit 1; }
for f in tls/ca.crt tls/host.crt tls/host.key wg.key; do
  [ -s "/etc/sbx/$f" ] || { echo "gateway-setup: missing /etc/sbx/$f" >&2; exit 1; }
done
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_SUSPEND=1
if ! command -v jq >/dev/null || ! command -v wg >/dev/null || ! command -v nft >/dev/null; then
  apt-get -qq update || apt-get -qq update
  apt-get -qq install -y --no-install-recommends jq nftables wireguard-tools iproute2 >/dev/null
fi
jq -e '.role == "hub" and .gateway == true and .limits.cgroups == false' "$src" >/dev/null \
  || { echo "gateway-setup: $src must have role hub, gateway true and limits.cgroups false" >&2; exit 1; }

# sandboxd owns its nft table; nothing flushes the ruleset at boot
systemctl disable --now nftables.service >/dev/null 2>&1 || true
# systemd-networkd leaves sandboxd's routes and rules alone when it restarts
mkdir -p /etc/systemd/networkd.conf.d
printf '[Network]\nManageForeignRoutes=no\nManageForeignRoutingPolicyRules=no\n' > /etc/systemd/networkd.conf.d/50-sbx.conf

install -d -m 700 /etc/sbx /etc/sbx/tls /var/lib/sbx /var/log/sbx
chmod 600 /etc/sbx/wg.key /etc/sbx/tls/host.key
jq '. + {tls: {ca: "/etc/sbx/tls/ca.crt", cert: "/etc/sbx/tls/host.crt", key: "/etc/sbx/tls/host.key"},
         wgKeyFile: "/etc/sbx/wg.key"}' "$src" > /etc/sbx/host.json.new
chmod 600 /etc/sbx/host.json.new
# the checks sandboxd starts with, before the running gateway is touched
if ! out=$("$here/../bin/sandboxd" check-config /etc/sbx/host.json.new); then
  echo "gateway-setup: $out" >&2; rm -f /etc/sbx/host.json.new; exit 1
fi
mv /etc/sbx/host.json.new /etc/sbx/host.json
install -m755 "$here/../bin/sandboxd" /usr/local/bin/sandboxd.new && mv /usr/local/bin/sandboxd.new /usr/local/bin/sandboxd

# What the gateway itself answers (hermit's calls to its API arrive from pod addresses when the cluster does not
# masquerade them) leaves over the private network: sandboxd's table 51822 is only for what comes in on wg0.
cat > /usr/local/sbin/sbx-gateway-routes <<'EOF'
#!/bin/bash
set -euo pipefail
pods=$(jq -r '.podCidr // empty' /etc/sbx/host.json); gw=$(jq -r '.privateGateway // empty' /etc/sbx/host.json)
[ -n "$pods" ] && [ -n "$gw" ] || exit 0
dev=$(ip -o -4 route get "$gw" | sed -n 's/.* dev \([^ ]*\).*/\1/p')
ip route replace "$pods" via "$gw" dev "$dev" onlink
EOF
chmod 755 /usr/local/sbin/sbx-gateway-routes

cat > /etc/systemd/system/sandboxd.service <<'EOF'
[Unit]
Description=sandboxd in gateway mode (the fabric's WireGuard hub, crates/sandboxd/docs/HOST.md "Gateway")
After=network-online.target
Wants=network-online.target
ConditionPathExists=/etc/sbx/host.json
# restarts for ever: this unit is the sandboxes' only path
StartLimitIntervalSec=0

[Service]
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
Environment=SBX_CONFIG=/etc/sbx/host.json
ExecStartPre=/usr/local/sbin/sbx-gateway-routes
ExecStart=/usr/local/bin/sandboxd
MemoryMax=256M
Restart=always
RestartSec=2
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable -q sandboxd.service
systemctl restart sandboxd.service
addr=$(jq -r .wgAddress /etc/sbx/host.json); port=$(jq -r '.apiPort // 7443' /etc/sbx/host.json)
for _ in $(seq 60); do
  systemctl is-active -q sandboxd && (exec 3<>"/dev/tcp/$addr/$port") 2>/dev/null && break
  sleep 0.5
done
(exec 3<>"/dev/tcp/$addr/$port") 2>/dev/null || { journalctl -u sandboxd -n 30 --no-pager >&2; exit 1; }
echo "gateway ready: $(/usr/local/bin/sandboxd --version), wg $(wg show wg0 public-key), api $addr:$port"
