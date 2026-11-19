#!/bin/bash
# gateway.sh: YYC AI's sandbox gateway, the plain server yyc-sbx-gw1 (10.0.0.8 in network yyc; README "Sandboxes").
# It is the one box every sandbox's traffic passes, so it only passes packets: WireGuard, sandboxd's fixed rules and
# sandboxd in gateway mode as a systemd unit (aviary fabric/host/gateway-setup.sh, crates/sandboxd/docs/HOST.md
# "Gateway on a plain server"). It is no cluster node and no workflow touches it: this script, run from a session, is
# the only way it changes, and it changes rarely (its own pin, deploy/hermit/gateway.release, not the cluster's).
#
#   gateway.sh setup [--keys]   install or upgrade: sandboxd of the aviary release in deploy/hermit/gateway.release and
#                               deploy/hermit/gateway-host.json, through aviary's gateway-setup.sh of that release.
#                               No packet is lost (sandboxd adopts the data plane it left). --keys (the first time, or
#                               after a key rotation): the WireGuard key and the TLS files from the store first.
#   gateway.sh status           the unit, the release, the peers and their last handshakes
#   gateway.sh ssh [cmd...]     a shell on the server
#   gateway.sh move OLD         replace the Hetzner server OLD as the gateway by this one (aviary gateway-move.sh: OLD's
#                               primary IPv6 and the route 10.202.0.0/15 move here; 38 to 67 s without the gateway on
#                               the replica, 2026-11-17). Run as the node work of the maintenance runbook:
#                               hermit-admin.sh gateway-maintenance --node-work "yyc/infra/gateway.sh move OLD"
#
# The server is reached over network yyc through the jump host (as clusters/talos/talos.sh tunnel): no public SSH.
# Env: SECRETS (default ~/.yyc-cluster: the admin key `admin`, known_hosts); TUNNEL_SERVICE_TOKEN_ID/_SECRET (the
# deploy tunnel's Access service token, YYC_DEPLOY_ACCESS_CLIENT_ID/_SECRET); setup --keys: YYC_HERMIT_GATEWAY_WG_KEY,
# YYC_HERMIT_FABRIC_GATEWAY_CERT, YYC_HERMIT_FABRIC_GATEWAY_KEY, YYC_HERMIT_FABRIC_CA; move: HCLOUD_TOKEN (project
# yyc, read-write).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
deploy=$here/../deploy/hermit
SERVER=yyc-sbx-gw1 ADDR=10.0.0.8 JUMP=10.0.0.30
S=${SECRETS:-$HOME/.yyc-cluster}
SSHO=(-i "$S/admin" -o UserKnownHostsFile="$S/known_hosts" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20 -o LogLevel=ERROR)
gw() {
  ssh "${SSHO[@]}" -o "ProxyCommand=ssh -i $S/admin -o UserKnownHostsFile=$S/known_hosts -o LogLevel=ERROR \
-o 'ProxyCommand=cloudflared access tcp --hostname jump.yyc-internal.com' -W %h:%p root@$JUMP" "root@$ADDR" "$@"
}
release() {
  local r; r=$(tr -d ' \n' < "$deploy/gateway.release")
  [[ $r =~ ^aviary-[0-9a-f]{12}$ ]] || { echo "gateway.sh: $deploy/gateway.release is not aviary-<sha12>: '$r'" >&2; exit 1; }
  echo "$r"
}

case "${1:-}" in
  setup)
    rel=$(release)
    t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
    mkdir -p "$t/fabric/host"
    for f in gateway-setup.sh fetch-sandboxd.sh; do
      curl -fsSL -o "$t/fabric/host/$f" "https://raw.githubusercontent.com/DE0CH/aviary/$rel/fabric/host/$f"
    done
    bash "$t/fabric/host/fetch-sandboxd.sh" "$t/fabric" "$rel" >/dev/null
    cp "$deploy/gateway-host.json" "$t/fabric/host.json"
    if [ "${2:-}" = --keys ]; then
      : "${YYC_HERMIT_GATEWAY_WG_KEY:?}" "${YYC_HERMIT_FABRIC_GATEWAY_CERT:?}" "${YYC_HERMIT_FABRIC_GATEWAY_KEY:?}" "${YYC_HERMIT_FABRIC_CA:?}"
      gw 'install -d -m 700 /etc/sbx /etc/sbx/tls'
      put() { printf '%s\n' "$1" | gw "umask 077; cat > $2"; }
      put "$YYC_HERMIT_GATEWAY_WG_KEY" /etc/sbx/wg.key
      put "$YYC_HERMIT_FABRIC_GATEWAY_CERT" /etc/sbx/tls/host.crt
      put "$YYC_HERMIT_FABRIC_GATEWAY_KEY" /etc/sbx/tls/host.key
      put "$YYC_HERMIT_FABRIC_CA" /etc/sbx/tls/ca.crt
    fi
    tar -cz -C "$t" fabric | gw 'rm -rf /root/fabric && tar -xz -C /root &&
      SBX_HOST_JSON=/root/fabric/host.json bash /root/fabric/host/gateway-setup.sh && cat /root/fabric/bin/RELEASE > /etc/sbx/release'
    ;;
  status)
    gw 'echo "release $(cat /etc/sbx/release 2>/dev/null) ($(/usr/local/bin/sandboxd --version)), unit $(systemctl is-active sandboxd), up since $(systemctl show sandboxd -p ActiveEnterTimestamp --value)"
        ip -6 -o addr show scope global | awk "{print \"public \" \$4}"
        now=$(date +%s); wg show wg0 latest-handshakes | awk -v now="$now" "{print \"peer \" substr(\$1,1,8) \"… handshake \" (\$2 ? now-\$2 \" s ago\" : \"never\")}"
        nft list table ip sbxhost | grep -c . | sed "s/^/nft lines /"'
    ;;
  ssh) shift; gw "$@" ;;
  move)
    old=${2:?usage: gateway.sh move OLD}
    : "${HCLOUD_TOKEN:?}"
    rel=$(release)
    t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
    curl -fsSL -o "$t/gateway-move.sh" "https://raw.githubusercontent.com/DE0CH/aviary/$rel/fabric/host/gateway-move.sh"
    gw 'systemctl is-active -q sandboxd && [ -s /var/lib/sbx/peers.json ]' \
      || { echo "gateway.sh: $SERVER is not set up, or has no peers.json (copy the running gateway's first)" >&2; exit 1; }
    NETWORK=yyc FIREWALL=yyc-sandbox-gateway bash "$t/gateway-move.sh" "$old" "$SERVER"
    # the hosts are back when their handshakes are (none to wait for when no host runs)
    for _ in $(seq 24); do gw true 2>/dev/null && break; sleep 5; done
    if [ "$(gw 'wg show wg0 peers | wc -l')" = 0 ]; then echo "gateway.sh: $SERVER is up; no host runs"; exit 0; fi
    for _ in $(seq 60); do
      if gw 'now=$(date +%s); wg show wg0 latest-handshakes | awk -v now="$now" "\$2 && now-\$2 < 30 {n++} END {exit !n}"' 2>/dev/null; then
        echo "gateway.sh: hosts are connected to $SERVER"; exit 0
      fi
      sleep 5
    done
    echo "gateway.sh: no host has connected to $SERVER within 5 min" >&2; exit 1
    ;;
  *) sed -n 2,26p "$0"; exit 2 ;;
esac
