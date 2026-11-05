#!/bin/bash
# gateway-net.sh NODE: make the Kubernetes node NODE hermit's sandbox gateway (hermit 13.1, 14.2; YYC DESIGN 15.6, N7)
# through the Hetzner and Kubernetes APIs only:
#   - the Hetzner route NET_ROUTE (default 10.202.0.0/15) of network NETWORK (default yyc) points at the node's address
#     in that network (moved when it pointed elsewhere; the old fabric's 10.200.0.0/15 is a different route);
#   - the node's Hetzner firewall admits udp 51821 (the sandbox hosts' WireGuard) from any address: checked, not set
#     (the firewall is the node's own, e.g. yyc-talos-gateway);
#   - the node label hermit.yyc/gateway=true, after the route (the hermit-gateway pod lands only then), and no other
#     node keeps it (one gateway).
# Idempotent: a second run changes nothing. Env: HCLOUD_TOKEN (the project's read-write token), KUBECONFIG.
set -euo pipefail
node=${1:?usage: gateway-net.sh NODE}
NETWORK=${NETWORK:-yyc} NET_ROUTE=${NET_ROUTE:-10.202.0.0/15}
: "${HCLOUD_TOKEN:?}"

nid=$(hcloud network describe "$NETWORK" -o json | jq -r .id)
ip=$(hcloud server describe "$node" -o json | jq -r --argjson n "$nid" '.private_net[] | select(.network == $n) | .ip')
[ -n "$ip" ] || { echo "gateway-net: $node has no address in network $NETWORK" >&2; exit 1; }

cur=$(hcloud network describe "$NETWORK" -o json | jq -r --arg d "$NET_ROUTE" '.routes[] | select(.destination == $d) | .gateway')
if [ "$cur" != "$ip" ]; then
  [ -z "$cur" ] || hcloud network remove-route "$NETWORK" --destination "$NET_ROUTE" --gateway "$cur"
  hcloud network add-route "$NETWORK" --destination "$NET_ROUTE" --gateway "$ip"
fi
echo "route $NET_ROUTE -> $ip (network $NETWORK)"

hcloud server describe "$node" -o json | jq -r '.public_net.firewalls[].id' | while read -r fw; do
  hcloud firewall describe "$fw" -o json
done | jq -s -e 'any(.[].rules[]; .direction == "in" and .protocol == "udp" and .port == "51821")' >/dev/null \
  || { echo "gateway-net: no firewall of $node admits udp 51821" >&2; exit 1; }
echo "firewall admits udp 51821"

for other in $(kubectl get nodes -l hermit.yyc/gateway=true -o jsonpath='{.items[*].metadata.name}'); do
  [ "$other" = "$node" ] || kubectl label node "$other" hermit.yyc/gateway-
done
kubectl label node "$node" hermit.yyc/gateway=true --overwrite
