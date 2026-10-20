#!/usr/bin/env bash
# The host side of one sandbox (what sandboxd will do; docs/SANDBOX-VM.md): its network namespace, OCI bundle,
# gVisor run / checkpoint / restore. Run as root on a sandbox host.
#   sbx.sh net NAME IP EGRESS_IP CTRL_IP   netns + veth, sandbox address IP/32, nftables: out only to EGRESS:3128,
#                                          in only from CTRL to :7070
#   sbx.sh bundle NAME IP EGRESS_IP TOKEN  OCI bundle from the host's current image (uid 1000, no capabilities)
#   sbx.sh run NAME                        start it (detached)
#   sbx.sh checkpoint NAME DIR [flags]     stop-and-save into DIR (pages uncompressed, for lazy restore)
#   sbx.sh restore NAME DIR [flags]        start it from DIR (on the target host, after `net` and `bundle`)
#   sbx.sh rm NAME                         delete the container and its network
set -euo pipefail
ROOT=/run/sbx/runsc
# Flags must be identical on both sides of a migration. systrap: no KVM. --network=sandbox: gVisor's own TCP/IP
# stack (netstack) owns the sandbox's connections, so they are part of the checkpoint and move with it.
# --allow-connected-on-save: keeps established TCP connections across a save. Without it they still survive the
# sandbox's FIRST migration but are aborted on every later one: the restored netstack loses its
# allowLiveTCPMigration setting (a `state:"nosave"` field nothing sets again after a restore; gVisor 20261012.0).
RUNSC=(runsc --root "$ROOT" --platform=systrap --network=sandbox --overlay2=root:self,size=5g
       --host-uds=none --host-fifo=none --allow-connected-on-save)
# SBX_DEBUG=1: gVisor debug logs with the sentry's own phase timelines under /var/log/sbx/debug/
# SBX_EXTRA: more runsc flags for this sandbox (e.g. Docker inside needs --net-raw --allow-packet-socket-write)
RUNSC+=(${SBX_EXTRA:-})
[ -n "${SBX_DEBUG:-}" ] && RUNSC+=(--debug --debug-log=/var/log/sbx/debug/)
cmd=$1; name=$2; ns=sbx-$name; veth=v${name:0:12}
GW_MAC=ee:ee:ee:ee:ee:ee   # host side of every sandbox veth, the same on every host, so a moved sandbox's ARP entry stays valid
mac_of() { printf '02:5b:%02x:%02x:%02x:%02x' ${1//./ }; }   # sandbox side: derived from its IP, same on every host

case $cmd in
net)
  ip=$3 egress=$4 ctrl=$5
  ip netns add "$ns"
  ip link add "$veth" type veth peer name eth0 netns "$ns"
  ip link set "$veth" address $GW_MAC up
  ip -n "$ns" link set eth0 address "$(mac_of "$ip")" up
  ip -n "$ns" link set lo up
  ip -n "$ns" addr add "$ip/32" dev eth0
  ip -n "$ns" route add 169.254.1.1 dev eth0 scope link
  ip -n "$ns" route add default via 169.254.1.1 dev eth0
  # proxy ARP answers the sandbox's ARP for its gateway; proxy_delay=0, or Linux holds each answer up to 0.8 s
  # (the default 80), which a restored sandbox pays on its first packets
  sysctl -qw "net.ipv4.conf.$veth.proxy_arp=1" "net.ipv4.conf.$veth.rp_filter=0" "net.ipv4.neigh.$veth.proxy_delay=0"
  ip route replace "$ip/32" dev "$veth"
  # the egress lock, one table per sandbox: stateless, so a connection that arrives mid-stream on a new host (after a
  # migration) passes without conntrack history. The sandbox may only talk to EGRESS:3128 and answer CTRL on :7070,
  # and can't reach the host itself.
  nft -f - <<NFT
table inet sbx_$veth
delete table inet sbx_$veth
table inet sbx_$veth {
  chain forward {
    type filter hook forward priority 0; policy accept;
    iifname "$veth" ip saddr $ip ip daddr $egress tcp dport 3128 accept
    oifname "$veth" ip saddr $egress tcp sport 3128 ip daddr $ip accept
    oifname "$veth" ip saddr $ctrl ip daddr $ip tcp dport 7070 accept
    iifname "$veth" ip saddr $ip ip daddr $ctrl tcp sport 7070 accept
    iifname "$veth" counter drop
    oifname "$veth" counter drop
  }
  chain input {
    type filter hook input priority 0; policy accept;
    iifname "$veth" counter drop
  }
}
NFT
  ;;
bundle)
  ip=$3 egress=$4 token=$5
  img=/var/lib/sbx/images/current; b=/var/lib/sbx/bundles/$name; mkdir -p "$b"
  proxy="http://sbx:$token@$egress:3128"
  jq -n --arg root "$(readlink -f $img)/rootfs" --arg ns "/var/run/netns/$ns" --arg name "$name" --arg proxy "$proxy" \
        --arg token "$token" --argjson env "$(jq '.config.Env' $img/config.json)" '{
    ociVersion: "1.0.0",
    process: { user: {uid: 1000, gid: 1000}, cwd: "/work", noNewPrivileges: true,
      args: ["/usr/bin/tini", "--", "node", "/opt/agent/agent.mjs"],
      env: ($env + ["JARVIS_EXEC_TOKEN=\($token)", "HTTP_PROXY=\($proxy)", "HTTPS_PROXY=\($proxy)",
                    "http_proxy=\($proxy)", "https_proxy=\($proxy)", "NO_PROXY="]),
      capabilities: {bounding: [], effective: [], inheritable: [], permitted: [], ambient: []},
      rlimits: [{type: "RLIMIT_NOFILE", hard: 65536, soft: 65536}] },
    root: { path: $root, readonly: false },
    hostname: "sandbox",
    mounts: [
      {destination: "/proc", type: "proc", source: "proc"},
      {destination: "/dev", type: "tmpfs", source: "tmpfs", options: ["nosuid","strictatime","mode=755","size=65536k"]},
      {destination: "/dev/pts", type: "devpts", source: "devpts", options: ["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620"]},
      {destination: "/dev/shm", type: "tmpfs", source: "shm", options: ["nosuid","noexec","nodev","mode=1777","size=65536k"]},
      {destination: "/sys", type: "sysfs", source: "sysfs", options: ["nosuid","noexec","nodev","ro"]},
      {destination: "/tmp", type: "tmpfs", source: "tmpfs", options: ["nosuid","nodev","mode=1777"]}
    ],
    linux: {
      namespaces: [{type: "pid"}, {type: "ipc"}, {type: "uts"}, {type: "mount"}, {type: "network", path: $ns}],
      cgroupsPath: "/sbx/\($name)",
      resources: { memory: {limit: 3221225472}, cpu: {quota: 200000, period: 100000}, pids: {limit: 1024} }
    }
  }' > "$b/config.json"
  ;;
run)        mkdir -p /var/log/sbx; "${RUNSC[@]}" run --bundle "/var/lib/sbx/bundles/$name" --detach "$name" </dev/null &>>"/var/log/sbx/$name.log" ;;
checkpoint) dir=$3; shift 3; rm -rf "$dir"; mkdir -p "$dir"; "${RUNSC[@]}" checkpoint --image-path "$dir" "$@" "$name" ;;
restore)    dir=$3; shift 3; mkdir -p /var/log/sbx; "${RUNSC[@]}" restore --bundle "/var/lib/sbx/bundles/$name" --image-path "$dir" --detach "$@" "$name" </dev/null &>>"/var/log/sbx/$name.log" ;;
exec)       shift 2; "${RUNSC[@]}" exec --user 1000:1000 --cwd /work "$name" "$@" ;;
exec-root)  shift 2; "${RUNSC[@]}" exec "$name" "$@" ;;
rm)
  "${RUNSC[@]}" kill "$name" KILL 2>/dev/null || true
  "${RUNSC[@]}" delete -force "$name" 2>/dev/null || true
  nft delete table inet "sbx_$veth" 2>/dev/null || true
  ip route del "$(ip route | awk -v v="$veth" '$3==v{print $1; exit}')" 2>/dev/null || true
  ip netns del "$ns" 2>/dev/null || true
  ;;
*) echo "unknown command $cmd" >&2; exit 2 ;;
esac
