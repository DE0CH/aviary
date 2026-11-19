#!/bin/bash
# bridges.sh: the two cross-cluster bridges of YYC AI's switch window (yyc/refactor DESIGN 15.5, WP-60), from their
# definitions in bridges.json: one Debian server with no public IP (`server`, yyc-bridge1, project yyc) that carries
# each bridge's address (10.0.255.1, 10.0.255.2) as a Hetzner alias IP on network yyc and runs HAProxy in TCP mode.
# Prints no secret.
#
#   bridges.sh up [NAME|LINK]      create or correct the box and its HAProxy (idempotent): the server (cax11 fsn1,
#                                  Debian, no public IPv4 or IPv6, network yyc from creation, its label; cloud-init gives
#                                  it the default route via the network's gateway, which network yyc routes to yyc-nat1,
#                                  and yyc-nat1 as resolver), the alias IPs of the bridges it carries, HAProxy, and one
#                                  `listen` per service: the bridge's address:listen, round robin to every target
#                                  server's private IP at the NodePort, sourced from the bridge's address, TCP health
#                                  checks. Targets are read from the Hetzner API on every up (a server added to or
#                                  removed from a cluster needs an up; check says so). Services the box carries that
#                                  are not in bridges.json are left (and named).
#   bridges.sh check [NAME|LINK]   exit 0 when the box is as defined and every bridge target with a callee pod is
#                                  healthy: for each service, each server that runs a ready callee pod is a target and
#                                  UP in HAProxy on that service, and at least one target is
#   bridges.sh down NAME|LINK|all  a bridge (NAME: its services and its alias IP), one link's service (LINK; the bridge
#                                  goes with its last service), or the whole box (all; also when its last bridge goes).
#                                  Billing stops when the server is deleted.
#   bridges.sh render              print the HAProxy config `up` would write for every bridge in bridges.json
#   NAME: yyc-bridge-old | yyc-bridge-new; LINK: B1 B2 B4 B5 B6.
#
# Env: YYC_HETZNER_API (project yyc, read & write; check and render need read only). The box is reached by SSH with
# $SECRETS/admin (default ~/.yyc-cluster; hcloud key yyc-cluster-admin) through `server.jump` (yyc-master1). check reads
# the callee pods: on the old cluster through $BRIDGES_OLD_KUBECTL (an argv prefix; default: $CUTOVER_OLD kubectl, else
# news-stream/deploy/cluster/cluster.sh ssh kubectl, which needs SECRETS), on the new one with $BRIDGES_NEW_KUBECONFIG
# (default $CUTOVER_NEW_KUBECONFIG). Tests only (yyc/tests/test_bridges.py): HCLOUD_ENDPOINT (a fake API),
# BRIDGES_SSH (an argv prefix run as `$BRIDGES_SSH <ip> <command>` instead of ssh), BRIDGES_BOX_ROOT (a folder the box's
# paths live under), BRIDGES_DEF (another bridges.json).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DEF=${BRIDGES_DEF:-$HERE/bridges.json}
API=${HCLOUD_ENDPOINT:-https://api.hetzner.cloud/v1}
KEYS=${SECRETS:-$HOME/.yyc-cluster}
R=${BRIDGES_BOX_ROOT:-}
CFG=$R/etc/haproxy/haproxy.cfg STATE=$R/etc/yyc-bridge/state.json ADDRS=$R/etc/yyc-bridge/addresses
SOCK=$R/run/haproxy/admin.sock STATS=$R/usr/local/sbin/yyc-bridge-stats
MARK='#--yyc-bridge--'
: "${YYC_HETZNER_API:?YYC_HETZNER_API (store yyc) is not set}"

say() { echo "bridges: $*" >&2; }
die() { say "$*"; exit 1; }
g() { jq -r "$1" "$DEF"; }
SERVER=$(g .server.name)

# the Hetzner API; the token goes to curl on stdin (a config line), never in argv
h() {
  local method=$1 path=$2 body=${3:-}
  local out code
  out=$(mktemp)
  code=$(printf 'header = "Authorization: Bearer %s"\n' "$YYC_HETZNER_API" |
    curl -sS -K - -o "$out" -w '%{http_code}' -X "$method" -H 'Content-Type: application/json' \
      ${body:+--data "$body"} "$API$path") || { rm -f "$out"; die "$method $path: no answer"; }
  if [ "${code:0:1}" != 2 ]; then
    say "$method $path: HTTP $code $(jq -c '.error // .' "$out" 2>/dev/null || head -c 300 "$out")"
    rm -f "$out"; return 1
  fi
  cat "$out"; rm -f "$out"
}

# waits for every action in the JSON on stdin (`action` and `next_actions`) to succeed
wait_action() {
  local id status i
  for id in $(jq -r '[.action.id?, .next_actions[]?.id] | map(select(. != null))[]'); do
    for i in $(seq 1 180); do
      status=$(h GET "/actions/$id" | jq -r .action.status)
      case "$status" in
        success) continue 2 ;;
        error) die "action $id failed: $(h GET "/actions/$id" | jq -c .action.error)" ;;
      esac
      sleep 1
    done
    die "action $id still $status after 180 s"
  done
}

# the bridges (and, for a LINK, the one service) an argument names
select_bridges() {
  local arg=${1:-}
  if [ -z "$arg" ] || [ "$arg" = all ]; then jq -c '.bridges[]' "$DEF"; return; fi
  local d
  d=$(jq -c --arg a "$arg" '.bridges[] | select(.name == $a or any(.services[]; .link == $a))' "$DEF")
  [ -n "$d" ] || die "no bridge or link named $arg in $DEF"
  echo "$d"
}
link_of() { case "${1:-}" in B[0-9]*) echo "$1" ;; *) echo "" ;; esac; }

network_id() {
  local id
  id=$(h GET "/networks?name=$(g .network)" | jq -r '.networks[0].id // empty')
  [ -n "$id" ] || die "network $(g .network) not found"
  echo "$id"
}
server_get() { h GET "/servers?name=$SERVER" | jq -c '.servers[0] // empty'; }
priv() { jq -c --argjson n "$NET" '[.private_net[] | select(.network == $n)][0] // empty' <<<"$1"; }

# the box over SSH through the jump host (yyc-master1), with the cluster admin key
box() {
  if [ -n "${BRIDGES_SSH:-}" ]; then
    # shellcheck disable=SC2086
    $BRIDGES_SSH "$BOX_IP" "$@"
  else
    ssh -i "$KEYS/admin" -o UserKnownHostsFile="$KEYS/known_hosts" -o StrictHostKeyChecking=accept-new \
      -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=20 \
      -o "ProxyCommand=ssh -i $KEYS/admin -o UserKnownHostsFile=$KEYS/known_hosts -o BatchMode=yes -W %h:%p $(g .server.jump)" \
      "root@$BOX_IP" "$@"
  fi
}
forget_host() { [ -n "${BRIDGES_SSH:-}" ] || ssh-keygen -q -f "$KEYS/known_hosts" -R "$1" >/dev/null 2>&1 || true; }

# cloud-init: network yyc is the box's only link. A unit (at boot and every minute) adds the default route via the
# network's gateway (network yyc routes 0.0.0.0/0 to yyc-nat1), the bridge addresses listed in /etc/yyc-bridge/addresses
# (up writes them; Hetzner delivers an alias IP to the server, the OS must carry it) and the resolver.
user_data() {
  sed -e "s/@GW@/$(g .server.gateway)/g" -e "s/@DNS@/$(g .server.resolver)/g" <<'EOF'
#cloud-config
write_files:
  - path: /usr/local/sbin/yyc-bridge-net
    permissions: '0755'
    content: |
      #!/bin/sh
      # yyc-bridge1's network (yyc/infra/bridges.sh): default route via network yyc, bridge addresses, resolver
      set -eu
      dev=""
      for i in $(seq 1 60); do
        dev=$(ip -o -4 addr show scope global | awk '$4 ~ /^10\.0\./ && $4 !~ /^10\.0\.255\./ {print $2; exit}')
        [ -n "$dev" ] && break
        sleep 1
      done
      [ -n "$dev" ] || { echo "yyc-bridge-net: no address in network yyc" >&2; exit 1; }
      ip route replace default via @GW@ dev "$dev" onlink
      want=" $(cat /etc/yyc-bridge/addresses 2>/dev/null | tr '\n' ' ') "
      for a in $want; do ip addr replace "$a/32" dev "$dev"; done
      for a in $(ip -o -4 addr show dev "$dev" | awk '{print $4}' | sed -n 's#^\(10\.0\.255\.[0-9]*\)/32$#\1#p'); do
        case "$want" in *" $a "*) ;; *) ip addr del "$a/32" dev "$dev" ;; esac
      done
      if [ -L /etc/resolv.conf ] || [ "$(cat /etc/resolv.conf 2>/dev/null)" != "nameserver @DNS@" ]; then
        rm -f /etc/resolv.conf
        echo "nameserver @DNS@" > /etc/resolv.conf
      fi
  - path: /etc/systemd/system/yyc-bridge-net.service
    content: |
      [Unit]
      Description=yyc-bridge1 network: default route, bridge addresses, resolver (yyc/infra/bridges.sh)
      After=network.target
      Before=haproxy.service
      [Service]
      Type=oneshot
      ExecStart=/usr/local/sbin/yyc-bridge-net
      [Install]
      WantedBy=multi-user.target
  - path: /etc/systemd/system/yyc-bridge-net.timer
    content: |
      [Timer]
      OnUnitActiveSec=1min
      [Install]
      WantedBy=timers.target
  - path: /etc/sysctl.d/90-yyc-bridge.conf
    content: |
      net.ipv4.ip_nonlocal_bind = 1
  - path: /etc/dhcp/dhclient-enter-hooks.d/yyc-bridge-resolver
    content: |
      make_resolv_conf() { :; }
runcmd:
  - [sysctl, -q, --system]
  - [systemctl, daemon-reload]
  - [systemctl, enable, --now, yyc-bridge-net.service, yyc-bridge-net.timer]
EOF
}

# the servers a label selector names
servers_by() { h GET "/servers?label_selector=$(jq -rn --arg s "$1" '$s|@uri')&per_page=50"; }
# a bridge's targets: [{name, ip}] of the servers matching `targets` minus `exclude`, by private IP on network yyc
targets_of() {
  local d=$1 inc exc='{"servers":[]}' ex
  inc=$(servers_by "$(jq -r .targets <<<"$d")")
  ex=$(jq -r '.exclude // empty' <<<"$d")
  [ -z "$ex" ] || exc=$(servers_by "$ex")
  jq -r --argjson n "$NET" '.servers[] | select(all(.private_net[]; .network != $n)) | .name' <<<"$inc" |
    while read -r s; do say "$s matches $(jq -r .targets <<<"$d") but is not on network $(g .network): no target"; done
  jq -c --argjson n "$NET" --argjson exc "$exc" '[.servers[] | select(.id as $i | [$exc.servers[].id] | index($i) | not)
    | {name, ip: ([.private_net[] | select(.network == $n) | .ip][0])} | select(.ip != null)] | sort_by(.name)' <<<"$inc"
}
targets_map() { # $1 state -> {bridge name: targets}
  local d m='{}'
  while read -r d; do
    [ -n "$d" ] || continue
    m=$(jq -c --arg b "$(jq -r .name <<<"$d")" --argjson t "$(targets_of "$d")" '.[$b] = $t' <<<"$m")
  done < <(jq -c '.bridges[]' <<<"$1")
  echo "$m"
}

# the HAProxy config for a state ({bridges: [bridge with the services the box carries]}) and its targets
render() {
  jq -r --argjson t "$2" --argjson hc "$(g .healthCheck)" --arg r "$R" --arg sock "$SOCK" --arg box "$SERVER" '
"# \($box): HAProxy for the cross-cluster bridges, written by yyc/infra/bridges.sh up from bridges.json
# and the targets in the Hetzner API. Do not edit: the next up replaces it.
global
" + (if $r == "" then "  user haproxy\n  group haproxy\n" else "" end) + "  log /dev/log local0 notice
  stats socket \($sock) mode 600 level admin
  maxconn 4096

defaults
  mode tcp
  log global
  option dontlognull
  balance roundrobin
  # the health check timeout; a connection to a target gets the same, and one that fails goes to another target
  timeout connect \($hc.timeout)s
  timeout check \($hc.timeout)s
  retries 2
  option redispatch
  # long-lived connections (LU sockets) idle up to an hour
  timeout client 1h
  timeout server 1h
" + ([.bridges[] | . as $b | .services[] | "
# \($b.name) \(.link): \($b.ip):\(.listen) -> \($b.targets)\(if $b.exclude then " minus " + $b.exclude else "" end) at :\(.nodePort) (callee \(.callee.selector) in \(.callee.namespace))
listen \(.link)
  bind \($b.ip):\(.listen)
  source \($b.ip)
  default-server check inter \($hc.interval)s fall \($hc.retries + 1) rise 2
" + ([$t[$b.name][]? as $s | "  server \($s.name) \($s.ip):\(.nodePort)\n"] | join(""))] | join(""))' <<<"$1"
}

# the lines of one `listen` section of a config file
section() { awk -v l="listen $1" '$0 == l {on = 1; next} /^listen / {on = 0} on && /^  server /' "$2"; }
head_of() { awk '/^# [^ ]+ B[0-9]+: |^listen /{exit} {print}' "$1"; }

# state, config and HAProxy's stats from the box, into files in $1 (state.json, haproxy.cfg, stats.csv)
read_box() {
  local out
  out=$(box "cat $STATE 2>/dev/null || echo '{\"bridges\":[]}'; echo '$MARK'; cat $CFG 2>/dev/null; echo '$MARK';
    python3 $STATS $SOCK 2>/dev/null || true") || return 1
  awk -v m="$MARK" -v d="$1" 'BEGIN {f[0] = d "/state.json"; f[1] = d "/haproxy.cfg"; f[2] = d "/stats.csv"; n = 0;
    for (i = 0; i < 3; i++) printf "" > f[i]} $0 == m {n++; next} {print > f[n]}' <<<"$out"
}
# HAProxy's `show stat` CSV -> {proxy: {server: status}}
stats_json() {
  jq -Rn '[inputs | select(length > 0)] as $l | if ($l | length) == 0 then {} else
    ($l[0] | ltrimstr("# ") | split(",")) as $h | ($h | index("status")) as $st
    | reduce ($l[1:][] | split(",")) as $r ({}; .[$r[0]][$r[1]] = $r[$st]) end' "$1"
}

wait_ssh() {
  local i
  for i in $(seq 1 60); do
    box true 2>/dev/null && return 0
    sleep 5
  done
  die "$SERVER ($BOX_IP): no SSH after 300 s"
}

# HAProxy and the stats reader on the box (idempotent)
setup_box() {
  box "R='$R' bash -s" <<'EOF'
set -euo pipefail
if command -v cloud-init >/dev/null 2>&1; then cloud-init status --wait >/dev/null || true; fi
if ! command -v haproxy >/dev/null 2>&1; then
  # the box leaves through yyc-nat1's WARP over IPv4: Hetzner's mirror answers only Hetzner addresses, so Debian's
  # own (debian.sources) serves it
  rm -f /etc/apt/sources.list.d/hetzner-mirror.sources /etc/apt/sources.list.d/hetzner-security-mirror.sources
  echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99yyc-bridge-ipv4
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq haproxy >/dev/null
fi
mkdir -p "$R/etc/yyc-bridge" "$R/etc/haproxy" "$R/run/haproxy" "$R/usr/local/sbin"
cat > "$R/usr/local/sbin/yyc-bridge-stats.new" <<'P'
#!/usr/bin/env python3
# HAProxy's `show stat` CSV from its stats socket (yyc/infra/bridges.sh check)
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(5)
s.connect(sys.argv[1])
s.sendall(b"show stat\n")
out = b""
while True:
    d = s.recv(65536)
    if not d:
        break
    out += d
sys.stdout.write(out.decode())
P
chmod 755 "$R/usr/local/sbin/yyc-bridge-stats.new"
mv "$R/usr/local/sbin/yyc-bridge-stats.new" "$R/usr/local/sbin/yyc-bridge-stats"
EOF
}

# the bridge addresses: Hetzner's alias IPs and the box's own list, to exactly the bridges in $1 (a state)
set_addresses() {
  local st=$1 srv want have id
  srv=$(server_get); id=$(jq -r .id <<<"$srv")
  want=$(jq -c '[.bridges[].ip] | unique' <<<"$st")
  have=$(jq -c '.alias_ips // [] | sort' <<<"$(priv "$srv")")
  jq -r '.[]' <<<"$want" | box "cat > $ADDRS.new && mv $ADDRS.new $ADDRS && systemctl restart yyc-bridge-net.service"
  if [ "$want" != "$have" ]; then
    say "$SERVER: alias IPs $have -> $want"
    h POST "/servers/$id/actions/change_alias_ips" "$(jq -nc --argjson n "$NET" --argjson a "$want" \
      '{network: $n, alias_ips: $a}')" | wait_action
  fi
}

# HAProxy's config and the box's state to $1 (a state); reloads HAProxy only when the config changed
apply() {
  local st=$1 tmp
  tmp=$(mktemp -d)
  render "$st" "$(targets_map "$st")" >"$tmp/cfg"
  box "cat $CFG 2>/dev/null || true" >"$tmp/old"
  if ! cmp -s "$tmp/old" "$tmp/cfg"; then
    if grep -q 'written by yyc/infra/bridges.sh' "$tmp/old"; then
      say "$SERVER: HAProxy config changed, reloading:"
      diff "$tmp/old" "$tmp/cfg" | grep '^[<>] ' | grep -v '^[<>] #' >&2 || true
    else
      say "$SERVER: HAProxy config written (it had another), reloading"
    fi
    box "cat > $CFG.new && haproxy -c -q -f $CFG.new >&2 && mv $CFG.new $CFG && systemctl enable -q haproxy &&
      systemctl reload-or-restart haproxy && systemctl is-active -q haproxy" <"$tmp/cfg" ||
      { rm -rf "$tmp"; die "$SERVER: HAProxy did not take the new config"; }
  fi
  jq . <<<"$st" | box "cat > $STATE.new && mv $STATE.new $STATE"
  rm -rf "$tmp"
}

create_server() {
  say "$SERVER: creating ($(g .server.type), $(g .server.location), $(g .server.image), no public IP, network $(g .network))"
  h POST /servers "$(jq -nc --arg n "$SERVER" --arg t "$(g .server.type)" --arg l "$(g .server.location)" \
    --arg i "$(g .server.image)" --arg k "$(g .server.sshKey)" --argjson lab "$(g .server.labels)" \
    --argjson net "$NET" --arg ud "$(user_data)" \
    '{name: $n, server_type: $t, location: $l, image: $i, ssh_keys: [$k], networks: [$net], labels: $lab,
      public_net: {enable_ipv4: false, enable_ipv6: false}, user_data: $ud, start_after_create: true}')" | wait_action
}

# RED lines for a server that is not as bridges.json defines it (no output: as defined)
shape() {
  jq -r --argjson d "$(g .server)" --argjson n "$NET" '
    (if .public_net.ipv4 != null or .public_net.ipv6 != null then "has a public IP" else empty end),
    (if .server_type.name != $d.type then "is \(.server_type.name), not \($d.type)" else empty end),
    (if .location.name != $d.location then "is in \(.location.name), not \($d.location)" else empty end),
    (if any(.private_net[]; .network == $n) | not then "is not on network yyc" else empty end),
    (if .status != "running" then "is \(.status)" else empty end)' <<<"$1"
}

up_box() {
  local arg=$1 only srv id cur new d sel
  only=$(link_of "$arg")
  NET=$(network_id)
  srv=$(server_get)
  if [ -z "$srv" ]; then
    create_server
    srv=$(server_get)
    forget_host "$(jq -r .ip <<<"$(priv "$srv")")"
  fi
  id=$(jq -r .id <<<"$srv")
  local bad
  bad=$(shape "$srv" | grep -v '^is starting$\|^is initializing$' || true)
  [ -z "$bad" ] || die "$SERVER $(paste -sd, - <<<"$bad"): bridges.sh down all, then up"
  if [ "$(jq -cS '.labels' <<<"$srv")" != "$(jq -cS .server.labels "$DEF")" ]; then
    say "$SERVER: labels -> $(jq -c .server.labels "$DEF")"
    h PUT "/servers/$id" "$(jq -c '{labels: .server.labels}' "$DEF")" >/dev/null
  fi
  BOX_IP=$(jq -r .ip <<<"$(priv "$srv")")
  wait_ssh
  setup_box
  local tmp; tmp=$(mktemp -d)
  read_box "$tmp" || die "$SERVER: cannot read its state"
  cur=$(cat "$tmp/state.json"); rm -rf "$tmp"
  sel=$(select_bridges "$arg" | jq -sc --arg o "$only" 'map(.services |= map(select($o == "" or .link == $o)))')
  new=$(jq -c --argjson sel "$sel" 'reduce $sel[] as $d (.;
    if any(.bridges[]; .name == $d.name) then
      .bridges |= map(if .name == $d.name then ($d | del(.services)) + {services: ((.services | map(select(.link as $l
        | [$d.services[].link] | index($l) | not))) + $d.services)} else . end)
    else .bridges += [$d] end) | .bridges |= (sort_by(.name) | map(.services |= sort_by(.link)))' <<<"$cur")
  set_addresses "$new"
  apply "$new"
  jq -r --slurpfile def "$DEF" '.bridges[] | .name as $b | .services[] | .link as $l
    | select([$def[0].bridges[].services[].link] | index($l) | not) | "\($b) \($l)"' <<<"$new" |
    while read -r x; do say "$x is not in bridges.json (left; bridges.sh down it)"; done
  while read -r d; do say "$(jq -r .name <<<"$d"): up at $(jq -r .ip <<<"$d") on $SERVER ($BOX_IP)"; done < <(select_bridges "$arg")
}

kube_old() {
  if [ -n "${BRIDGES_OLD_KUBECTL:-}" ]; then
    # shellcheck disable=SC2086
    $BRIDGES_OLD_KUBECTL "$@"
  elif [ -n "${CUTOVER_OLD:-}" ]; then
    # shellcheck disable=SC2086
    $CUTOVER_OLD kubectl "$@"
  else
    "$ROOT/news-stream/deploy/cluster/cluster.sh" ssh kubectl "$@"
  fi
}
kube_new() {
  local kc=${BRIDGES_NEW_KUBECONFIG:-${CUTOVER_NEW_KUBECONFIG:-}}
  [ -n "$kc" ] || die "no kubeconfig for the new cluster (BRIDGES_NEW_KUBECONFIG or CUTOVER_NEW_KUBECONFIG)"
  KUBECONFIG=$kc kubectl "$@"
}

# the nodes that run a ready callee pod of a service
callee_nodes() { # $1 cluster, $2 namespace, $3 selector
  local out
  # stdin closed: an ssh-based kubectl would read the caller's loop input
  if [ "$1" = old ]; then out=$(kube_old get pods -n "$2" -l "$3" -o json </dev/null)
  else out=$(kube_new get pods -n "$2" -l "$3" -o json </dev/null); fi
  jq -r '.items[] | select(.metadata.deletionTimestamp == null)
    | select(any(.status.conditions[]?; .type == "Ready" and .status == "True")) | .spec.nodeName' <<<"$out" | sort -u
}

check_box() {
  local arg=$1 only srv bad=0 tmp d
  only=$(link_of "$arg")
  NET=$(network_id)
  srv=$(server_get)
  if [ -z "$srv" ]; then
    while read -r d; do echo "RED $(jq -r .name <<<"$d"): $SERVER does not exist"; done < <(select_bridges "$arg")
    return 1
  fi
  local s
  while read -r s; do [ -z "$s" ] || { echo "RED $SERVER: $s"; bad=1; }; done < <(shape "$srv")
  BOX_IP=$(jq -r '.ip // empty' <<<"$(priv "$srv")")
  [ -n "$BOX_IP" ] || return 1
  tmp=$(mktemp -d)
  if ! read_box "$tmp"; then echo "RED $SERVER: no SSH to $BOX_IP"; rm -rf "$tmp"; return 1; fi
  local st health aliases
  st=$(cat "$tmp/state.json")
  health=$(stats_json "$tmp/stats.csv")
  aliases=$(jq -c '.alias_ips // []' <<<"$(priv "$srv")")
  [ "$health" != "{}" ] || { echo "RED $SERVER: HAProxy's stats do not answer (not running?)"; bad=1; }
  # the config's head (global, defaults) as up would write it
  render '{"bridges":[]}' '{}' >"$tmp/head.cfg"
  [ "$(head_of "$tmp/haproxy.cfg")" = "$(head_of "$tmp/head.cfg")" ] ||
    { echo "RED $SERVER: HAProxy's global settings are not as defined (bridges.sh up)"; bad=1; }
  while read -r d; do
    local name ip cluster sdef tg rendered link port np nodes node stt ok_any sbad
    name=$(jq -r .name <<<"$d"); ip=$(jq -r .ip <<<"$d"); cluster=$(jq -r .cluster <<<"$d")
    jq -e --arg i "$ip" 'index($i)' <<<"$aliases" >/dev/null ||
      { echo "RED $name: $ip is no alias IP of $SERVER"; bad=1; }
    tg=$(targets_of "$d")
    [ "$tg" != "[]" ] || { echo "RED $name: $(jq -r .targets <<<"$d") matches no target server"; bad=1; }
    render "$(jq -c --argjson d "$d" '{bridges: [$d]}' <<<'{}')" "$(jq -nc --arg b "$name" --argjson t "$tg" '{($b): $t}')" >"$tmp/want.cfg"
    while read -r sdef; do
      link=$(jq -r .link <<<"$sdef"); port=$(jq .listen <<<"$sdef"); np=$(jq .nodePort <<<"$sdef"); sbad=0
      [ -n "$only" ] && [ "$link" != "$only" ] && continue
      if ! jq -e --arg n "$name" --argjson s "$sdef" --argjson d "$d" 'any(.bridges[]; .name == $n and .ip == $d.ip and
          .targets == $d.targets and .exclude == $d.exclude and any(.services[]; . == $s))' <<<"$st" >/dev/null; then
        echo "RED $name $link: not up on $SERVER as defined (bridges.sh up $link)"; bad=1; continue
      fi
      if [ "$(jq -r --arg l "$link" '.[$l].FRONTEND // ""' <<<"$health")" != OPEN ]; then
        echo "RED $name $link: HAProxy does not listen on $ip:$port"; bad=1; continue
      fi
      [ "$(section "$link" "$tmp/haproxy.cfg")" = "$(section "$link" "$tmp/want.cfg")" ] ||
        { echo "RED $name $link: HAProxy's targets differ from the Hetzner API's (a server added or removed: bridges.sh up)"; sbad=1; }
      if ! nodes=$(callee_nodes "$cluster" "$(jq -r .callee.namespace <<<"$sdef")" "$(jq -r .callee.selector <<<"$sdef")"); then
        echo "RED $name $link: cannot read the callee pods on the $cluster cluster"; bad=1; continue
      fi
      if [ -z "$nodes" ]; then
        echo "RED $name $link: no ready callee pod ($(jq -r .callee.selector <<<"$sdef") in $(jq -r .callee.namespace <<<"$sdef"))"; bad=1; continue
      fi
      ok_any=$(jq -r --arg l "$link" '[.[$l] // {} | to_entries[] | select(.key != "FRONTEND" and .key != "BACKEND")
        | select(.value | startswith("UP"))] | length' <<<"$health")
      for node in $nodes; do
        stt=$(jq -r --arg l "$link" --arg n "$node" '.[$l][$n] // "absent"' <<<"$health")
        case "$stt" in
          UP*) ;;
          absent) echo "RED $name $link: $node runs a callee pod but is no target (its Hetzner label? then bridges.sh up)"; sbad=1 ;;
          *) echo "RED $name $link: $node runs a callee pod but is $stt on :$port"; sbad=1 ;;
        esac
      done
      [ "$ok_any" -gt 0 ] || { echo "RED $name $link: no healthy target on :$port"; sbad=1; }
      [ $sbad = 0 ] || { bad=1; continue; }
      echo "ok  $name $link: $ip:$port -> :$np, healthy on $(echo "$nodes" | paste -sd, -)"
    done < <(jq -c '.services[]' <<<"$d")
  done < <(select_bridges "$arg")
  rm -rf "$tmp"
  return $bad
}

delete_server() {
  local srv=$1
  say "$SERVER: deleting"
  h DELETE "/servers/$(jq -r .id <<<"$srv")" | wait_action
  [ -z "$(server_get)" ] || die "$SERVER still listed after its delete"
  forget_host "$(jq -r '.ip // empty' <<<"$(priv "$srv")")"
  say "$SERVER: deleted (billing stops)"
}

down_box() {
  local arg=$1 only srv tmp cur new
  only=$(link_of "$arg")
  NET=$(network_id)
  srv=$(server_get)
  [ -n "$srv" ] || { say "$SERVER: already gone"; return 0; }
  if [ "$arg" = all ]; then delete_server "$srv"; return 0; fi
  select_bridges "$arg" >/dev/null
  BOX_IP=$(jq -r .ip <<<"$(priv "$srv")")
  tmp=$(mktemp -d)
  read_box "$tmp" || die "$SERVER: cannot read its state"
  cur=$(cat "$tmp/state.json"); rm -rf "$tmp"
  new=$(jq -c --arg a "$arg" --arg o "$only" '.bridges |= map(if $o != "" then .services |= map(select(.link != $o))
    elif .name == $a then .services = [] else . end | select(.services | length > 0))' <<<"$cur")
  if [ "$new" = "$cur" ]; then say "$arg: not on $SERVER"; return 0; fi
  if [ "$(jq '.bridges | length' <<<"$new")" = 0 ]; then
    say "$arg: $SERVER carries nothing else"
    delete_server "$srv"; return 0
  fi
  say "$SERVER: removing $arg"
  apply "$new"
  set_addresses "$new"
  say "$SERVER: now carries $(jq -r '[.bridges[] | .services[].link] | join(" ")' <<<"$new")"
}

cmd=${1:-}; arg=${2:-}
case "$cmd" in
  up) up_box "$arg" ;;
  check)
    rc=0
    check_box "$arg" || rc=1
    [ $rc = 0 ] && echo "bridges: green" || echo "bridges: red"
    exit $rc ;;
  down)
    [ -n "$arg" ] || die "down needs NAME, LINK or all"
    down_box "$arg" ;;
  render)
    NET=$(network_id)
    st=$(jq -c '{bridges}' "$DEF")
    render "$st" "$(targets_map "$st")" ;;
  *) sed -n '2,32p' "$0"; exit 2 ;;
esac
