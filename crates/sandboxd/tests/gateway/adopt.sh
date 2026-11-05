#!/usr/bin/env bash
# The gateway's data plane across its process (hermit spec 14.2, WP-19): sandboxd in gateway mode leaves wg0, its nft
# table, ip rules and routes in place when it exits and adopts them at start. Runs as root on a Linux machine with
# WireGuard, nftables and network namespaces (ci.yml job `sandboxd-gateway`; any test box), with real `ip`, `wg`,
# `nft` and the real sandboxd binary; it changes nothing outside its own three namespaces.
#
#   sgw    the gateway's host namespace: eth0 (the "public" side, default route), priv0 (the private network),
#          sandboxd in gateway mode (state under a temporary SBX_ROOT, as the pod's hostPath)
#   shost  an AWS host: wg0 peered with the gateway, a sandbox address 10.98.0.5 on a dummy device
#   spod   the private network's router 10.97.0.1 (privateGateway) and an "egress pod" 10.96.0.5 (echo server :3128)
#
# Checks:
#   1. kill -9 of the gateway process while the host sends (ICMP every 10 ms to the pod, and one TCP connection to the
#      egress VIP 10.99.1.1:3128 exchanging a line every 20 ms): no packet lost, the connection never stalls; the API
#      :7443 is back within seconds; the second start adopts wg0 (same ifindex, the gateway's handshake time with the
#      host unchanged) and leaves nft, rules, routes and peers as they were
#   2. a start with a host.json missing a field (podCidr) exits non-zero and leaves all of it as it is, under traffic
#   3. the MSS of SYNs forwarded from wg0 is clamped to gatewayMss (1280)
#   4. a wg0 that is gone (a node reboot) is made again with the saved peer set, with no PUT from hermit
#   5. on a node with Talos's rules (the local table's rule at 100, as Cilium moves it; KubeSpan's at 32500) and an
#      older sandboxd's two wg0 rules at 99/100: the first start leaves sandboxd's rules at 1000/1001 only, and the
#      host reaches the API on the gateway's own wgAddress over wg0
#
# Usage: adopt.sh <sandboxd binary>
#        SBX_GATEWAY_IMAGE=<image> adopt.sh: sandboxd from the gateway image (images/sandboxd: its own ip, wg, nft,
#        sysctl), run with docker in the gateway's namespace the way the pod runs it in the node's (privileged, the
#        host's network namespace, sandboxd as the main process); each kill -9 is of that main process
set -euo pipefail
image=${SBX_GATEWAY_IMAGE:-}
if [ -n "$image" ]; then
  bin=/usr/local/bin/sandboxd
  command -v docker >/dev/null || { echo "missing docker" >&2; exit 2; }
else
  bin=$(readlink -f "${1:?usage: adopt.sh <sandboxd binary>}")
fi
here=$(cd "$(dirname "$0")" && pwd)
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
for t in ip wg nft openssl curl python3 ping; do command -v $t >/dev/null || { echo "missing $t" >&2; exit 2; }; done
modprobe wireguard 2>/dev/null || true

T=$(mktemp -d /tmp/sbx-gw-adopt.XXXXXX)
NS="sgw shost spod"
pids=()
cids=()
cleanup() {
  set +e
  for c in "${cids[@]}"; do docker rm -f "$c" >/dev/null 2>&1; done
  for p in "${pids[@]}"; do kill -9 "$p" 2>/dev/null; done
  pkill -9 -f "$T/" 2>/dev/null
  for n in $NS; do ip netns pids "$n" 2>/dev/null | xargs -r kill -9; ip netns del "$n" 2>/dev/null; done
  rm -rf "$T"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; echo "--- sandboxd log" >&2; tail -40 "$T"/sandboxd-*.log >&2 2>/dev/null; exit 1; }
ok() { echo "ok: $*"; }
nsx() { local n=$1; shift; ip netns exec "$n" "$@"; }

# ---- the three namespaces ------------------------------------------------------------------------------------------
for n in $NS; do ip netns del "$n" 2>/dev/null || true; ip netns add "$n"; nsx "$n" ip link set lo up; done
veth() { # veth <ns a> <ns b> <name>: a pair named <name> on both sides (made under other names: the machine may have one)
  ip link add "sbxt-$1" type veth peer name "sbxt-$2"
  ip link set "sbxt-$1" netns "$1"; ip link set "sbxt-$2" netns "$2"
  nsx "$1" ip link set "sbxt-$1" name "$3"; nsx "$2" ip link set "sbxt-$2" name "$3"
}
veth sgw shost eth0
veth sgw spod priv0
nsx sgw ip addr add 192.0.2.1/24 dev eth0; nsx sgw ip link set eth0 up
nsx sgw ip addr add 10.97.0.6/16 dev priv0; nsx sgw ip link set priv0 up
nsx sgw ip route add default via 192.0.2.2
nsx shost ip addr add 192.0.2.2/24 dev eth0; nsx shost ip link set eth0 up
nsx spod ip addr add 10.97.0.1/16 dev priv0; nsx spod ip link set priv0 up
nsx spod ip link add pod0 type dummy; nsx spod ip addr add 10.96.0.5/32 dev pod0; nsx spod ip link set pod0 up
nsx spod ip route add 10.98.0.0/16 via 10.97.0.6
# a strict reverse-path check where the defaults would apply (sandboxd sets wg0 and priv0 loose itself)
nsx sgw sysctl -qw net.ipv4.conf.all.rp_filter=1 net.ipv4.conf.default.rp_filter=1
nsx sgw sysctl -qw net.ipv4.conf.priv0.rp_filter=1
# Talos's rules (yyc-talos-w1, 2026-11-05: Cilium's proxy rule, local moved from 0 to 100, KubeSpan) and an older
# sandboxd's wg0 rules at 99/100 (check 5)
nsx sgw ip rule add priority 9 fwmark 0x200/0xf00 lookup 2004
nsx sgw ip rule add priority 100 lookup local
nsx sgw ip rule del priority 0 lookup local
nsx sgw ip rule add priority 32500 fwmark 0x40/0x60 lookup 180
nsx sgw ip rule add priority 99 iif wg0 lookup main suppress_prefixlength 16
nsx sgw ip rule add priority 100 iif wg0 lookup 51822

# ---- PKI, keys, host.json ------------------------------------------------------------------------------------------
bash "$here/../../../../fabric/host/make-certs.sh" "$T/pki" >/dev/null
umask 077
wg genkey > "$T/gw.key"; wg genkey > "$T/host.key"
gwpub=$(wg pubkey < "$T/gw.key"); hostpub=$(wg pubkey < "$T/host.key")
cat > "$T/host.json" <<EOF
{"name":"gw-test","role":"hub","gateway":true,"wgAddress":"10.99.0.1","hostCidr":"10.99.0.0/24","wgPort":51821,
 "wgKeyFile":"$T/gw.key","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1",
 "podCidr":"10.96.0.0/16","privateGateway":"10.97.0.1","reserveMemory":0,"gatewayMss":1280,
 "tls":{"ca":"$T/pki/ca.crt","cert":"$T/pki/gateway.crt","key":"$T/pki/gateway.key"},
 "limits":{"cgroups":false,"mlock":false}}
EOF
python3 -c 'import json,sys; c=json.load(open(sys.argv[1])); del c["podCidr"]; json.dump(c,open(sys.argv[2],"w"))' \
  "$T/host.json" "$T/host-bad.json"
mkdir -p "$T/root"

# the AWS host's side
nsx shost ip link add wg0 type wireguard
nsx shost wg set wg0 private-key "$T/host.key" listen-port 51820 \
  peer "$gwpub" endpoint 192.0.2.1:51821 allowed-ips 10.99.0.0/24,10.99.1.1/32,10.97.0.0/16,10.96.0.0/16 persistent-keepalive 5
nsx shost ip addr add 10.99.0.10/24 dev wg0; nsx shost ip link set wg0 mtu 1420 up
nsx shost ip route add 10.99.1.1/32 dev wg0; nsx shost ip route add 10.96.0.0/16 dev wg0
nsx shost ip link add sbx0 type dummy; nsx shost ip addr add 10.98.0.5/32 dev sbx0; nsx shost ip link set sbx0 up

# ---- helpers -------------------------------------------------------------------------------------------------------
n_start=0
start_gw() { # start_gw <host.json>: sandboxd in sgw, in the background; its pid in $gw
  n_start=$((n_start + 1))
  local log=$T/sandboxd-$n_start.log
  if [ -n "$image" ]; then
    cid=$(docker run -d --name "sbx-gw-adopt-$$-$n_start" --privileged --network none -v /run/netns:/run/netns \
      -v "$T:$T" -e SBX_ROOT="$T/root" -e SBX_CONFIG="$1" --entrypoint ip "$image" netns exec sgw "$bin")
    cids+=("$cid")
    docker logs -f "$cid" > "$log" 2>&1 &
    pids+=("$!")
    gw=$(docker inspect -f '{{.State.Pid}}' "$cid")
  else
    # not through a function: $! must be sandboxd itself (ip netns exec and env exec it), the process kill -9 hits
    ip netns exec sgw env SBX_ROOT="$T/root" SBX_CONFIG="$1" "$bin" > "$log" 2>&1 &
    gw=$!; pids+=("$gw")
  fi
  for _ in $(seq 50); do [ "$(readlink "/proc/$gw/exe" 2>/dev/null)" = "$bin" ] && return 0; running || return 0; sleep 0.05; done
}
running() { # the sandboxd started last still runs
  if [ -n "$image" ]; then [ "$(docker inspect -f '{{.State.Running}}' "$cid" 2>/dev/null)" = true ]; else kill -0 "$gw" 2>/dev/null; fi
}
exit_code() { # the exit code of the sandboxd started last, once it has exited
  if [ -n "$image" ]; then docker wait "$cid"; docker logs "$cid" > "$T/sandboxd-$n_start.log" 2>&1; else local c=0; wait "$gw" || c=$?; echo "$c"; fi
}
wait_api() { # seconds until :7443 accepts (fails after 20 s, or when the sandboxd just started has exited)
  local t0; t0=$(date +%s.%N)
  for _ in $(seq 200); do
    running || fail "sandboxd (start $n_start) exited: $(tail -3 "$T/sandboxd-$n_start.log")"
    if nsx sgw bash -c 'exec 3<>/dev/tcp/10.99.0.1/7443' 2>/dev/null; then
      python3 -c "import sys; print(f'{float(sys.argv[2])-float(sys.argv[1]):.2f}')" "$t0" "$(date +%s.%N)"; return 0
    fi
    sleep 0.1
  done
  fail "the API :7443 did not come up"
}
api() { # api METHOD PATH [BODY]
  nsx sgw curl -sS --fail-with-body --max-time 10 --resolve sandboxd:7443:10.99.0.1 --cacert "$T/pki/ca.crt" \
    --cert "$T/pki/hermit.crt" --key "$T/pki/hermit.key" -X "$1" -H 'content-type: application/json' \
    ${3:+--data "$3"} "https://sandboxd:7443$2"
}
snapshot() { # the data plane as the kernel holds it, without counters and handshake times
  {
    nsx sgw ip -o link show wg0 | cut -d: -f1-2
    nsx sgw wg show wg0 dump | cut -f1-4
    nsx sgw nft list table ip sbxhost
    nsx sgw ip -4 rule show
    nsx sgw ip -4 route show table all
    nsx sgw sysctl net.ipv4.conf.wg0.rp_filter net.ipv4.conf.priv0.rp_filter net.ipv4.ip_forward
  } 2>&1
}
handshake() { nsx sgw wg show wg0 latest-handshakes | awk -v k="$hostpub" '$1 == k {print $2}'; }
traffic() { # traffic <seconds> <tag>: ICMP every 10 ms (80 per second asked for: the count ends it, not a deadline) and one TCP connection through the VIP, in the background
  local s=$1 tag=$2
  nsx shost ping -q -I 10.98.0.5 -i 0.01 -c $((s * 80)) -w $((s + 20)) 10.96.0.5 > "$T/ping-$tag.out" 2>&1 &
  ping_pid=$!; pids+=("$ping_pid")
  nsx shost python3 "$T/tcp-client.py" "$s" > "$T/tcp-$tag.out" 2>&1 &
  tcp_pid=$!; pids+=("$tcp_pid")
}
check_traffic() { # check_traffic <tag>
  local tag=$1
  wait "$ping_pid" || true
  wait "$tcp_pid" || fail "the TCP connection through the VIP broke ($tag): $(cat "$T/tcp-$tag.out")"
  local sent recv
  read -r sent recv < <(awk '/packets transmitted/ {print $1, $4}' "$T/ping-$tag.out")
  [ -n "$sent" ] && [ "$sent" -gt 0 ] || fail "no ping summary ($tag): $(cat "$T/ping-$tag.out")"
  [ "$sent" = "$recv" ] || fail "$((sent - recv)) of $sent packets lost ($tag)"
  ok "$tag: $sent of $sent ICMP packets through the gateway, TCP through the VIP: $(cat "$T/tcp-$tag.out") lines, no stall"
}

cat > "$T/echo-server.py" <<'EOF'
import socket, sys, threading
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("10.96.0.5", 3128))
srv.listen(16)
out = open(sys.argv[1], "a")
def serve(c):
    out.write("%d\n" % c.getsockopt(socket.IPPROTO_TCP, socket.TCP_MAXSEG)); out.flush()
    f = c.makefile("rwb")
    for line in f:
        f.write(line); f.flush()
while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
EOF
cat > "$T/tcp-client.py" <<'EOF'
import socket, sys, time
s = socket.socket()
s.bind(("10.98.0.5", 0))
s.settimeout(5)  # a stall of 5 s anywhere fails
s.connect(("10.99.1.1", 3128))
f = s.makefile("rwb")
end, n = time.time() + float(sys.argv[1]), 0
while time.time() < end:
    m = b"%d\n" % n
    f.write(m); f.flush()
    if f.readline() != m:
        sys.exit("echo mismatch at %d" % n)
    n += 1
    time.sleep(0.02)
print(n)
EOF
nsx spod python3 "$T/echo-server.py" "$T/mss.out" > "$T/echo.log" 2>&1 &
pids+=("$!")

# ---- first start: hermit's pushes -----------------------------------------------------------------------------------
start_gw "$T/host.json"
wait_api >/dev/null
api PUT /v1/peers "{\"peers\":[{\"publicKey\":\"$hostpub\",\"endpoint\":\"192.0.2.2:51820\",\"allowedIps\":[\"10.99.0.10/32\",\"10.98.0.0/24\"]}]}" >/dev/null \
  || fail "PUT /v1/peers"
api PUT /v1/egress '{"targets":["10.96.0.5"]}' >/dev/null || fail "PUT /v1/egress"
for _ in $(seq 50); do nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null 2>&1 && break; sleep 0.2; done
nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null || fail "no path host -> gateway -> pod"
[ -s "$T/root/var/lib/sbx/peers.json" ] || fail "the peer set was not saved"
[ "$(nsx sgw sysctl -n net.ipv4.conf.wg0.rp_filter)/$(nsx sgw sysctl -n net.ipv4.conf.priv0.rp_filter)" = 2/2 ] \
  || fail "rp_filter not loose on wg0 and priv0"
ok "first start: peers and egress pushed, host -> gateway -> pod works, rp_filter loose on wg0 and priv0"
rules=$(nsx sgw ip -4 rule show | sed 's/[[:space:]]*$//')
want=$'9:\tfrom all fwmark 0x200/0xf00 lookup 2004\n100:\tfrom all lookup local\n999:\tfrom all to 10.96.0.0/16 iif wg0 lookup 51822\n1000:\tfrom all iif wg0 lookup main suppress_prefixlength 16\n1001:\tfrom all iif wg0 lookup 51822\n32500:\tfrom all fwmark 0x40/0x60 lookup 180\n32766:\tfrom all lookup main\n32767:\tfrom all lookup default'
[ "$rules" = "$want" ] || fail "ip rules after the first start: $rules"
# replies to the private network leave over it, not by KubeSpan's rule (main's /16 is suppressed for wg0)
nsx sgw ip -4 route show table 51822 | grep -q '^10.97.0.0/16 via 10.97.0.1 dev priv0' \
  || fail "controlCidr not in table 51822: $(nsx sgw ip -4 route show table 51822)"
nsx shost bash -c 'exec 3<>/dev/tcp/10.99.0.1/7443' 2>/dev/null || fail "the host does not reach the API on 10.99.0.1:7443 over wg0"
ok "Talos's rules: sandboxd's at 999/1000/1001 only (moved from 99/100), the local rule at 100 kept; host -> wgAddress:7443 over wg0"
before=$(snapshot); hs=$(handshake)
[ -n "$hs" ] && [ "$hs" != 0 ] || fail "no handshake with the host"

# ---- 1. kill -9 under traffic, then a second start -----------------------------------------------------------------
traffic 14 kill
sleep 3
[ "$(readlink "/proc/$gw/exe")" = "$bin" ] || fail "pid $gw is not sandboxd"
kill -9 "$gw"; exit_code >/dev/null
t_kill=$(date +%s.%N)
nsx sgw bash -c 'exec 3<>/dev/tcp/10.99.0.1/7443' 2>/dev/null && fail ":7443 still answers after the kill"
sleep 2
nsx sgw ip link show wg0 >/dev/null || fail "wg0 went with the process"
start_gw "$T/host.json"
wait_api >/dev/null
down=$(python3 -c "import sys; print(f'{float(sys.argv[2])-float(sys.argv[1]):.2f}')" "$t_kill" "$(date +%s.%N)")
check_traffic kill
after=$(snapshot)
[ "$before" = "$after" ] || fail "the second start changed the data plane: $(diff <(echo "$before") <(echo "$after"))"
[ "$(handshake)" = "$hs" ] || fail "the gateway's handshake with the host changed ($hs -> $(handshake)): wg0 was re-made"
ok "kill -9: wg0 adopted (same ifindex, handshake $hs unchanged), nft/rules/routes/peers unchanged; :7443 back $down s after the kill (2 s of it before the restart)"
grep -q "saved peer set applied" "$T/sandboxd-2.log" || fail "the second start did not apply the saved peer set"

# ---- 3. the MSS clamp -----------------------------------------------------------------------------------------------
mss=$(tail -1 "$T/mss.out")
[ -n "$mss" ] && [ "$mss" -le 1280 ] || fail "the pod saw MSS $mss for a sandbox on a 1420-byte wg0 (want <= 1280)"
ok "the egress pod's connection from a sandbox has MSS $mss (clamped to gatewayMss 1280; unclamped it would be 1368)"

# ---- 2. a host.json missing a field ---------------------------------------------------------------------------------
traffic 8 badconfig
sleep 1
running || fail "sandboxd (start $n_start) is not running"
kill -TERM "$gw"; exit_code >/dev/null
start_gw "$T/host-bad.json"
for _ in $(seq 50); do running || break; sleep 0.1; done
if running; then fail "sandboxd started with a host.json without podCidr"; fi
code=$(exit_code)
[ "$code" != 0 ] || fail "sandboxd exited 0 on a bad host.json"
grep -q podCidr "$T/sandboxd-$n_start.log" || fail "the refusal does not name podCidr: $(cat "$T/sandboxd-$n_start.log")"
check_traffic badconfig
after=$(snapshot)
[ "$before" = "$after" ] || fail "a refused host.json changed the data plane: $(diff <(echo "$before") <(echo "$after"))"
[ "$(handshake)" = "$hs" ] || fail "the handshake changed after a refused start"
ok "a host.json without podCidr: refused (exit $code, before touching anything), data plane unchanged under traffic"

# ---- 4. wg0 gone (a node reboot): made again with the saved peers, before any push ----------------------------------
ip netns exec sgw ip link del wg0
start_gw "$T/host.json"
wait_api >/dev/null
# the host's session died with the old wg0: the gateway's own packet starts the new handshake at once
for _ in $(seq 50); do
  nsx sgw ping -q -c1 -W1 10.98.0.5 >/dev/null 2>&1 && nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null 2>&1 && break
  sleep 0.2
done
nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null || fail "no path after wg0 was re-made from the saved state"
# (whole outputs, not grep -q on a pipe: pipefail would see the writer's SIGPIPE)
grep -qx "$hostpub" <<< "$(nsx sgw wg show wg0 peers)" || fail "the saved peer is not on the new wg0"
grep -q "10.96.0.5" <<< "$(nsx sgw nft list table ip sbxhost)" || fail "the saved egress target is not in the table"
ok "wg0 re-made after it was gone: the saved peer and egress target applied with no push, host -> pod works"
echo "PASS"
