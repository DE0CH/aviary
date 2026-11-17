#!/bin/bash
# gateway-move.sh OLD NEW: move the fabric's gateway from the Hetzner server OLD to the server NEW, both in network
# NETWORK (default yyc). NEW was prepared with gateway-setup.sh (the same WireGuard key, TLS files and host.json as
# OLD, and OLD's peers.json) and has no public address of its own. What moves:
#   - OLD's primary IPv6: the address the hosts' WireGuard was given at launch, so running hosts find the gateway
#     again by themselves (Hetzner moves a primary IP only between powered-off servers);
#   - the network's route NET_ROUTE (default 10.202.0.0/15), to NEW's address in the network.
# The gateway is down from OLD's power-off until NEW has booted and the hosts' next handshake (measured on a replica,
# 2026-11-17: see crates/sandboxd/docs/HOST.md "Gateway on a plain server"). Running it with OLD and NEW swapped moves
# everything back. Each step is skipped when already done, so a run that stopped half way is run again.
# Env: HCLOUD_TOKEN (the project's read-write token).
set -euo pipefail
old=${1:?usage: gateway-move.sh OLD NEW}; new=${2:?usage: gateway-move.sh OLD NEW}
NETWORK=${NETWORK:-yyc} NET_ROUTE=${NET_ROUTE:-10.202.0.0/15}
: "${HCLOUD_TOKEN:?}"
t0=$(date +%s)
say() { echo "[$(( $(date +%s) - t0 )) s] $*"; }
srv() { hcloud server describe "$1" -o json; }
nid=$(hcloud network describe "$NETWORK" -o json | jq -r .id)
new_ip=$(srv "$new" | jq -r --argjson n "$nid" '.private_net[] | select(.network == $n) | .ip')
[ -n "$new_ip" ] || { echo "gateway-move: $new has no address in network $NETWORK" >&2; exit 1; }
new_id=$(srv "$new" | jq -r .id); old_id=$(srv "$old" | jq -r .id)
# the address that moves: OLD's primary IPv6, or the one NEW already holds (a second run)
pip=$(hcloud primary-ip list -o json | jq -r --argjson o "$old_id" '.[] | select(.type == "ipv6" and .assignee_id == $o) | .id')
if [ -z "$pip" ]; then
  pip=$(hcloud primary-ip list -o json | jq -r --argjson o "$new_id" '.[] | select(.type == "ipv6" and .assignee_id == $o) | .id')
  [ -n "$pip" ] || { echo "gateway-move: neither $old nor $new holds a primary IPv6" >&2; exit 1; }
  say "$new already holds the primary IPv6 $pip"
else
  [ "$(srv "$new" | jq -r '.public_net.ipv6.ip // empty')" = "" ] \
    || { echo "gateway-move: $new has a primary IPv6 of its own: remove it first" >&2; exit 1; }
  # the address outlives whichever server holds it
  hcloud primary-ip update "$pip" --auto-delete=false >/dev/null
  [ "$(srv "$new" | jq -r .status)" = off ] || { hcloud server poweroff "$new" >/dev/null; say "$new powered off"; }
  hcloud server poweroff "$old" >/dev/null; say "$old powered off: the gateway is down"
  hcloud primary-ip unassign "$pip" >/dev/null
  hcloud primary-ip assign "$pip" --server "$new" >/dev/null; say "primary IPv6 $pip moved to $new"
fi
# NEW boots while the route moves
[ "$(srv "$new" | jq -r .status)" = running ] || { hcloud server poweron "$new" >/dev/null; say "$new powered on"; }
cur=$(hcloud network describe "$NETWORK" -o json | jq -r --arg d "$NET_ROUTE" '.routes[] | select(.destination == $d) | .gateway')
if [ "$cur" != "$new_ip" ]; then
  [ -z "$cur" ] || hcloud network remove-route "$NETWORK" --destination "$NET_ROUTE" --gateway "$cur" >/dev/null
  hcloud network add-route "$NETWORK" --destination "$NET_ROUTE" --gateway "$new_ip" >/dev/null
fi
say "route $NET_ROUTE -> $new_ip ($new)"
echo "moved: $new holds $(srv "$new" | jq -r .public_net.ipv6.ip) and $NET_ROUTE; $old is powered off and holds neither"
