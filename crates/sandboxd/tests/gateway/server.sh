#!/usr/bin/env bash
# The gateway on a plain server (fabric/host/gateway-setup.sh; crates/sandboxd/docs/HOST.md "Gateway on a plain
# server"). Runs as root on a throwaway Ubuntu 24.04 machine with systemd (ci.yml job `sandboxd-soak`; any test box):
# THIS machine becomes the gateway, set up by the real script with the real sandboxd binary, and two network
# namespaces stand for what is around it. At the end the machine is as it was, apart from the installed packages.
#
#   (root)  the gateway: sbxt-pub 192.0.2.1 (the public side), sbxt-priv 10.97.0.6 (the private network)
#   shost   an AWS host: wg0 peered with the gateway, a sandbox address 10.98.0.5 on a dummy device
#   spod    the private network's router 10.97.0.1, a pod 10.96.0.5 (hermit's side), the egress load balancer
#           10.97.0.50:3128 and the database's 10.97.0.51:5432 (echo servers that say which address called them)
#
# Checks:
#   1. the script refuses a host.json that is not a gateway's, and one sandboxd refuses, and installs nothing then
#   2. after it: sandboxd.service is enabled and active, the API answers a pod address over the private network, and
#      the script made nothing a gateway does not need (no JuiceFS or checkpoint units, no runsc, no crane)
#   3. with hermit's pushes: sandbox -> pod and pod -> sandbox work, and both the egress VIP and a host's own
#      connection into the private network reach the load balancer from the gateway's private address (what makes a
#      balancer's answers come back)
#   4. a second run of the script (an upgrade) under traffic loses no packet and keeps the TCP connection
#   5. a reboot (wg0, the nft table, sandboxd's rules and the pod route gone, the unit started as at boot): all of it
#      is back with the saved peers and egress target, with no push
#
# Usage: server.sh <sandboxd binary>
set -euo pipefail
bin=$(readlink -f "${1:?usage: server.sh <sandboxd binary>}")
here=$(cd "$(dirname "$0")" && pwd)
fabric=$(readlink -f "$here/../../../../fabric")
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
[ -d /run/systemd/system ] || { echo "needs systemd" >&2; exit 2; }
[ ! -e /etc/sbx ] && ! ip link show wg0 >/dev/null 2>&1 || { echo "this machine already has /etc/sbx or wg0: not a throwaway" >&2; exit 2; }
export DEBIAN_FRONTEND=noninteractive
export NEEDRESTART_SUSPEND=1
command -v wg >/dev/null && command -v nft >/dev/null && command -v jq >/dev/null \
  || { apt-get -qq update; apt-get -qq install -y --no-install-recommends wireguard-tools nftables jq >/dev/null; }
for t in ip wg nft openssl curl python3 ping jq; do command -v $t >/dev/null || { echo "missing $t" >&2; exit 2; }; done
modprobe wireguard 2>/dev/null || true

T=$(mktemp -d /tmp/sbx-gw-server.XXXXXX)
pids=()
cleanup() {
  set +e
  for p in "${pids[@]}"; do kill -9 "$p" 2>/dev/null; done
  systemctl disable --now sandboxd.service >/dev/null 2>&1
  rm -f /etc/systemd/system/sandboxd.service /usr/local/sbin/sbx-gateway-routes /usr/local/bin/sandboxd \
    /etc/systemd/networkd.conf.d/50-sbx.conf
  systemctl daemon-reload
  ip link del wg0 2>/dev/null; nft delete table ip sbxhost 2>/dev/null
  for p in 999 1000 1001; do ip rule del priority $p 2>/dev/null; done
  ip route flush table 51822 2>/dev/null; ip route del 10.96.0.0/16 2>/dev/null
  for n in shost spod; do ip netns pids "$n" 2>/dev/null | xargs -r kill -9; ip netns del "$n" 2>/dev/null; done
  ip link del sbxt-pub 2>/dev/null; ip link del sbxt-priv 2>/dev/null
  rm -rf /etc/sbx /var/lib/sbx /var/log/sbx "$T"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; journalctl -u sandboxd -n 30 --no-pager >&2 2>/dev/null; exit 1; }
ok() { echo "ok: $*"; }
nsx() { local n=$1; shift; ip netns exec "$n" "$@"; }

# ---- what is around the gateway ------------------------------------------------------------------------------------
for n in shost spod; do ip netns del "$n" 2>/dev/null || true; ip netns add "$n"; nsx "$n" ip link set lo up; done
ip link add sbxt-pub type veth peer name eth0 netns shost
ip link add sbxt-priv type veth peer name priv0 netns spod
ip addr add 192.0.2.1/24 dev sbxt-pub; ip link set sbxt-pub up
# as a cloud's private network hands it out: the server's own /32, the network through its router
ip addr add 10.97.0.6/32 dev sbxt-priv; ip link set sbxt-priv up
ip route add 10.97.0.1 dev sbxt-priv; ip route add 10.97.0.0/16 via 10.97.0.1 dev sbxt-priv
nsx shost ip addr add 192.0.2.2/24 dev eth0; nsx shost ip link set eth0 up
nsx spod ip addr add 10.97.0.1/16 dev priv0; nsx spod ip link set priv0 up
nsx spod ip link add pod0 type dummy; nsx spod ip link set pod0 up
nsx spod ip addr add 10.96.0.5/32 dev pod0; nsx spod ip addr add 10.97.0.50/32 dev pod0; nsx spod ip addr add 10.97.0.51/32 dev pod0
nsx spod ip route add 10.98.0.0/15 via 10.97.0.6    # the private network's route to the gateway: sandboxes and hosts

bash "$fabric/host/make-certs.sh" "$T/pki" >/dev/null
umask 077
wg genkey > "$T/gw.key"; wg genkey > "$T/host.key"
gwpub=$(wg pubkey < "$T/gw.key"); hostpub=$(wg pubkey < "$T/host.key")
cat > "$T/host.json" <<JSON
{"name":"gw-test","role":"hub","gateway":true,"wgAddress":"10.99.0.1","hostCidr":"10.99.0.0/24","wgPort":51821,
 "controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","egressPorts":[[3128,3128]],
 "podCidr":"10.96.0.0/16","privateGateway":"10.97.0.1","reserveMemory":0,"gatewayMss":1280,
 "limits":{"cgroups":false}}
JSON
mkdir -p "$T/fabric/bin"; cp -r "$fabric/host" "$T/fabric/host"; install -m755 "$bin" "$T/fabric/bin/sandboxd"
setup() { SBX_HOST_JSON=$1 bash "$T/fabric/host/gateway-setup.sh"; }

nsx shost ip link add wg0 type wireguard
nsx shost wg set wg0 private-key "$T/host.key" listen-port 51820 \
  peer "$gwpub" endpoint 192.0.2.1:51821 allowed-ips 10.99.0.0/24,10.99.1.1/32,10.97.0.0/16,10.96.0.0/16 persistent-keepalive 5
nsx shost ip addr add 10.99.0.10/24 dev wg0; nsx shost ip link set wg0 mtu 1420 up
nsx shost ip route add 10.99.1.1/32 dev wg0; nsx shost ip route add 10.96.0.0/16 dev wg0; nsx shost ip route add 10.97.0.0/16 dev wg0
nsx shost ip link add sbx0 type dummy; nsx shost ip addr add 10.98.0.5/32 dev sbx0; nsx shost ip link set sbx0 up

api() { # api METHOD PATH [BODY]: hermit, from a pod address over the private network
  nsx spod curl -sS --fail-with-body --max-time 10 --interface 10.96.0.5 --resolve sandboxd:7443:10.99.0.1 \
    --cacert "$T/pki/ca.crt" --cert "$T/pki/hermit.crt" --key "$T/pki/hermit.key" -X "$1" \
    -H 'content-type: application/json' ${3:+--data "$3"} "https://sandboxd:7443$2"
}
cat > "$T/echo-server.py" <<'PY'
import socket, threading
def serve(c, peer):
    f = c.makefile("rwb")
    f.write(b"%s\n" % peer.encode()); f.flush()
    for line in f:
        f.write(line); f.flush()
def listen(addr, port):
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((addr, port))
    srv.listen(16)
    while True:
        c, a = srv.accept()
        threading.Thread(target=serve, args=(c, a[0]), daemon=True).start()
threading.Thread(target=listen, args=("10.97.0.51", 5432), daemon=True).start()
listen("10.97.0.50", 3128)
PY
cat > "$T/tcp-client.py" <<'PY'
import socket, sys, time
s = socket.socket()
s.bind(("10.98.0.5", 0))
s.settimeout(5)  # a stall of 5 s anywhere fails
s.connect(("10.99.1.1", 3128))
f = s.makefile("rwb")
peer = f.readline().decode().strip()
end, n = time.time() + float(sys.argv[1]), 0
while time.time() < end:
    m = b"%d\n" % n
    f.write(m); f.flush()
    if f.readline() != m:
        sys.exit("echo mismatch at %d" % n)
    n += 1
    time.sleep(0.02)
print(peer, n)
PY
nsx spod python3 "$T/echo-server.py" > "$T/echo.log" 2>&1 &
pids+=("$!")
traffic() { # traffic <seconds> <tag>
  local s=$1 tag=$2
  nsx shost ping -q -I 10.98.0.5 -i 0.01 -c $((s * 80)) -w $((s + 20)) 10.96.0.5 > "$T/ping-$tag.out" 2>&1 &
  ping_pid=$!; pids+=("$ping_pid")
  nsx shost python3 "$T/tcp-client.py" "$s" > "$T/tcp-$tag.out" 2>&1 &
  tcp_pid=$!; pids+=("$tcp_pid")
}
check_traffic() { # check_traffic <tag>
  local tag=$1 sent recv
  wait "$ping_pid" || true
  wait "$tcp_pid" || fail "the TCP connection through the VIP broke ($tag): $(cat "$T/tcp-$tag.out")"
  read -r sent recv < <(awk '/packets transmitted/ {print $1, $4}' "$T/ping-$tag.out")
  [ -n "$sent" ] && [ "$sent" -gt 0 ] || fail "no ping summary ($tag): $(cat "$T/ping-$tag.out")"
  [ "$sent" = "$recv" ] || fail "$((sent - recv)) of $sent packets lost ($tag)"
  ok "$tag: $sent of $sent ICMP packets through the gateway, TCP through the VIP unbroken ($(cut -d' ' -f2 "$T/tcp-$tag.out") lines)"
}
paths() { # the three paths of a working gateway
  for _ in $(seq 50); do nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null 2>&1 && break; sleep 0.2; done
  nsx shost ping -q -c1 -W1 -I 10.98.0.5 10.96.0.5 >/dev/null || fail "no path sandbox -> gateway -> pod ($1)"
  nsx spod ping -q -c1 -W1 -I 10.96.0.5 10.98.0.5 >/dev/null || fail "no path pod -> gateway -> sandbox ($1)"
  nsx shost python3 "$T/tcp-client.py" 0.2 > "$T/tcp-$1.out" 2>&1 || fail "the egress VIP does not answer ($1): $(cat "$T/tcp-$1.out")"
  [ "$(cut -d' ' -f1 "$T/tcp-$1.out")" = 10.97.0.6 ] \
    || fail "the load balancer saw $(cut -d' ' -f1 "$T/tcp-$1.out") ($1): want the gateway's private address 10.97.0.6"
  # the host's own connection into the private network: its JuiceFS metadata, to the balancer in front of the database
  local seen
  seen=$(nsx shost python3 -c 'import socket; s = socket.socket(); s.bind(("10.99.0.10", 0)); s.settimeout(5); s.connect(("10.97.0.51", 5432)); print(s.makefile().readline().strip())') \
    || fail "the host does not reach the private network ($1)"
  [ "$seen" = 10.97.0.6 ] || fail "the private network saw the host as $seen ($1): want the gateway's private address 10.97.0.6"
}

# ---- 1. refusals ----------------------------------------------------------------------------------------------------
mkdir -p /etc/sbx/tls
cp "$T/pki/ca.crt" /etc/sbx/tls/ca.crt; cp "$T/pki/gateway.crt" /etc/sbx/tls/host.crt; cp "$T/pki/gateway.key" /etc/sbx/tls/host.key
cp "$T/gw.key" /etc/sbx/wg.key
jq '.gateway = false' "$T/host.json" > "$T/host-nogw.json"
jq 'del(.podCidr)' "$T/host.json" > "$T/host-bad.json"
if setup "$T/host-nogw.json" > "$T/setup-nogw.out" 2>&1; then fail "the script took a host.json that is not a gateway's"; fi
if setup "$T/host-bad.json" > "$T/setup-bad.out" 2>&1; then fail "the script took a host.json without podCidr"; fi
grep -q podCidr "$T/setup-bad.out" || fail "the refusal does not name podCidr: $(cat "$T/setup-bad.out")"
[ ! -e /etc/sbx/host.json ] && [ ! -e /etc/systemd/system/sandboxd.service ] && ! ip link show wg0 >/dev/null 2>&1 \
  || fail "a refused host.json left something installed"
ok "refused: a host.json that is not a gateway's, and one without podCidr; nothing installed"

# ---- 2. the setup ---------------------------------------------------------------------------------------------------
setup "$T/host.json" > "$T/setup-1.out" 2>&1 || fail "gateway-setup.sh: $(cat "$T/setup-1.out")"
grep -q "^gateway ready: .* wg $gwpub," "$T/setup-1.out" || fail "the script's last line: $(tail -1 "$T/setup-1.out")"
[ "$(systemctl is-enabled sandboxd)/$(systemctl is-active sandboxd)" = enabled/active ] || fail "sandboxd.service is not enabled and active"
for u in sbx-jfs.service run-sbx-ckpt.mount sbx-userdata.service; do
  [ ! -e "/etc/systemd/system/$u" ] || fail "the script made $u"
done
[ ! -e /usr/local/bin/runsc ] && [ ! -e /usr/local/bin/juicefs ] || fail "the script installed runsc or juicefs"
api GET /v1/host > "$T/host.out" || fail "the API does not answer a pod address over the private network: $(cat "$T/host.out")"
ok "set up: sandboxd.service enabled and active, the API answers a pod; no JuiceFS, checkpoint or user-data unit, no runsc"

# ---- 3. hermit's pushes, the three paths ----------------------------------------------------------------------------
api PUT /v1/peers "{\"peers\":[{\"publicKey\":\"$hostpub\",\"endpoint\":\"192.0.2.2:51820\",\"allowedIps\":[\"10.99.0.10/32\",\"10.98.0.0/24\"]}]}" >/dev/null \
  || fail "PUT /v1/peers"
api PUT /v1/egress '{"targets":["10.97.0.50"]}' >/dev/null || fail "PUT /v1/egress"
paths pushed
ok "pushed: sandbox -> pod, pod -> sandbox, and the egress VIP reaches the load balancer from 10.97.0.6"

# ---- 4. a second run under traffic ----------------------------------------------------------------------------------
hs() { wg show wg0 latest-handshakes | awk -v k="$hostpub" '$1 == k {print $2}'; }
ifindex=$(cat /sys/class/net/wg0/ifindex)
traffic 10 rerun
sleep 2
setup "$T/host.json" > "$T/setup-2.out" 2>&1 || fail "the second run: $(cat "$T/setup-2.out")"
check_traffic rerun
[ "$(cat /sys/class/net/wg0/ifindex)" = "$ifindex" ] || fail "the second run re-made wg0"
ok "a second run of the script: wg0 kept, no packet lost"

# ---- 5. a reboot ----------------------------------------------------------------------------------------------------
systemctl stop sandboxd
ip link del wg0; nft delete table ip sbxhost
for p in 999 1000 1001; do ip rule del priority $p 2>/dev/null || true; done
ip route flush table 51822; ip route del 10.96.0.0/16
systemctl start sandboxd
for _ in $(seq 40); do (exec 3<>/dev/tcp/10.99.0.1/7443) 2>/dev/null && break; sleep 0.5; done
# the host's session died with the old wg0: the gateway's own packet starts the new handshake
for _ in $(seq 50); do ping -q -c1 -W1 10.98.0.5 >/dev/null 2>&1 && break; sleep 0.2; done
paths reboot
ip route show 10.96.0.0/16 | grep -q 'via 10.97.0.1' || fail "the pod route is not back: $(ip route show 10.96.0.0/16)"
ok "after a reboot: wg0, the rules, the saved peer and egress target are back with no push; all three paths work"
echo "PASS"
