#!/usr/bin/env bash
# WireGuard overlay between the prototype machines, as in docs/SANDBOX-VM.md ("Cross-cloud network"): a full mesh
# on wg0 (10.199.0.0/24: egress/controller .1, host a .2, b .3, c .4), UDP 51820. Needed for a host outside the
# AWS VPC (host c: a Hetzner Cloud server created by hand and added to state/hosts.txt as "yyc-sbx-host-c <id>
# <public IP> <public IP>"). Host c also reaches the egress VM's private address through the overlay.
# Afterwards migrations to and from c use the overlay addresses (run-test.sh does this when c is involved).
# Usage: overlay-up.sh [names, default "egress a b c" (c only if in hosts.txt)]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
names=${1:-"egress a b $(col host-c 3 >/dev/null; [ -n "$(col host-c 3)" ] && echo c)"}
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
pub() { [ "$1" = egress ] && col egress 3 || col "host-$1" 3; }
wgip() { case $1 in egress) echo 10.199.0.1;; a) echo 10.199.0.2;; b) echo 10.199.0.3;; c) echo 10.199.0.4;; esac; }
Ep=$(col egress 4)
for n in $names; do
  ssh "${o[@]}" "ubuntu@$(pub $n)" "command -v wg >/dev/null || { sudo apt-get -qq update; sudo apt-get -qq install -y wireguard-tools >/dev/null; }
    sudo test -s /etc/wireguard/sbx.key || (umask 077; wg genkey | sudo tee /etc/wireguard/sbx.key >/dev/null)
    sudo cat /etc/wireguard/sbx.key | wg pubkey" > "$STATE/wg-$n.pub"
done
for n in $names; do
  peers=""
  for m in $names; do
    [ "$m" = "$n" ] && continue
    allowed=$(wgip $m)/32
    # host c reaches the egress VM's private address (where egress listens) through the overlay
    [ "$n" = c ] && [ "$m" = egress ] && allowed="$allowed,$Ep/32"
    peers+="sudo wg set wg0 peer $(cat "$STATE/wg-$m.pub") endpoint $(pub $m):51820 allowed-ips $allowed persistent-keepalive 25; "
  done
  ssh "${o[@]}" "ubuntu@$(pub $n)" "sudo ip link del wg0 2>/dev/null; sudo ip link add wg0 type wireguard
    sudo wg set wg0 private-key /etc/wireguard/sbx.key listen-port 51820
    sudo ip addr add $(wgip $n)/24 dev wg0; sudo ip link set wg0 mtu 1420 up
    $peers
    $( [ "$n" = c ] && echo "sudo ip route replace $Ep/32 dev wg0" )"
done
for n in $names; do echo "$n $(wgip $n) $(cat "$STATE/wg-$n.pub")"; done | tee "$STATE/wg.txt"
ssh "${o[@]}" "ubuntu@$(col egress 3)" "for i in 2 3 4; do ping -c1 -W1 10.199.0.\$i >/dev/null && echo ok 10.199.0.\$i || echo no 10.199.0.\$i; done"
