#!/usr/bin/env bash
# Shape B of docs/SANDBOX-VM.md, prototype: one Cloud Hypervisor microVM per sandbox (needs /dev/kvm: bare metal or a
# nested-virtualisation VM such as EC2 m7i with NestedVirtualization=enabled), live-migrated with CH's pre-copy.
# Run as root on a host.
#   vm.sh setup IMAGE            CH v54 + ch-remote + CH's kernel; the sandbox image as a read-only ext4 root disk
#                                (built on ONE host and copied byte-identical to the others: the guest caches blocks)
#   vm.sh net NAME IP EGRESS CTRL tap + /32 route + the same egress lock as sbx.sh
#   vm.sh run NAME IP EGRESS TOKEN [MEM_MiB=2048] [CPUS=2]
#   vm.sh receive NAME PORT      empty VMM waiting for a migration (after `net` on this host)
#   vm.sh send NAME DST:PORT [send-migration options]
#   vm.sh rm NAME
set -euo pipefail
cmd=$1; shift
D=/var/lib/sbx/vm; CHV=v54.0; KREL=ch-release-v7.2.8-20261009
GW_MAC=ee:ee:ee:ee:ee:ee
mac_of() { printf '02:5b:%02x:%02x:%02x:%02x' ${1//./ }; }
case $cmd in
setup)
  image=$1; mkdir -p $D
  [ -x /usr/local/bin/cloud-hypervisor ] || { curl -fsSL -o /usr/local/bin/cloud-hypervisor https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/$CHV/cloud-hypervisor-static
    curl -fsSL -o /usr/local/bin/ch-remote https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/$CHV/ch-remote-static
    chmod +x /usr/local/bin/cloud-hypervisor /usr/local/bin/ch-remote; }
  [ -f $D/vmlinux ] || curl -fsSL -o $D/vmlinux https://github.com/cloud-hypervisor/linux/releases/download/$KREL/vmlinux-x86_64
  if [ ! -f $D/rootfs.ext4 ]; then
    r=$(mktemp -d); crane export "$image" - | tar -x -C $r
    # what the guest needs on top of the container image: iproute2 for its network, and an init
    cp /etc/resolv.conf $r/etc/resolv.conf
    chroot $r sh -c 'apt-get -qq update && apt-get -qq install -y --no-install-recommends iproute2 >/dev/null && rm -rf /var/lib/apt/lists/*'
    cat > $r/sbin/sbx-init <<'INIT'
#!/bin/sh
# PID 1 of a sandbox microVM: mounts, network from the kernel command line (sbx.ip, sbx.egress, sbx.token), then the
# sandbox agent as uid 1000 under tini. The root disk is read-only and shared; everything writable is tmpfs, so it
# lives in guest memory and moves with the VM.
mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
mkdir -p /dev/pts /dev/shm; mount -t devpts devpts /dev/pts; mount -t tmpfs tmpfs /dev/shm
mount -t tmpfs tmpfs /tmp; mount -t tmpfs tmpfs /run
mount -t tmpfs -o uid=1000,gid=1000 tmpfs /work; mount -t tmpfs -o uid=1000,gid=1000 tmpfs /home/sandbox
for kv in $(cat /proc/cmdline); do case $kv in sbx.*=*) k=${kv%%=*}; export "SBX_${k#sbx.}=${kv#*=}";; esac; done
ip link set lo up; ip link set eth0 up; ip addr add "$SBX_ip/32" dev eth0
ip route add 169.254.1.1 dev eth0; ip route add default via 169.254.1.1
p="http://sbx:$SBX_token@$SBX_egress:3128"
exec /usr/bin/tini -s -- setpriv --reuid 1000 --regid 1000 --clear-groups env -i \
  PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/opt/py/bin:/usr/local/bin:/usr/bin:/bin \
  HOME=/home/sandbox VIRTUAL_ENV=/opt/py JARVIS_WORK_DIR=/work PORT=7070 PYTHONUNBUFFERED=1 LANG=C.UTF-8 \
  JARVIS_EXEC_TOKEN="$SBX_token" HTTP_PROXY="$p" HTTPS_PROXY="$p" http_proxy="$p" https_proxy="$p" NO_PROXY= \
  sh -c 'cd /work && exec node --import /opt/agent/ipv4-listen.mjs /opt/agent/agent.mjs'
INIT
    chmod +x $r/sbin/sbx-init
    # CH's guest kernel has no IPv6; the agent listens on "::", so map that to 0.0.0.0 (preloaded, image untouched)
    cat > $r/opt/agent/ipv4-listen.mjs <<'JS'
import net from "node:net";
const listen = net.Server.prototype.listen;
net.Server.prototype.listen = function (...a) { if (a[1] === "::") a[1] = "0.0.0.0"; return listen.apply(this, a); };
JS
    mkfs.ext4 -q -F -L sbxroot -d $r $D/rootfs.ext4 3G; rm -rf $r
  fi
  ls -la $D ;;
net)
  name=$1 ip=$2 egress=$3 ctrl=$4; tap=t$name
  ip link del $tap 2>/dev/null || true
  ip tuntap add $tap mode tap; ip link set $tap address $GW_MAC up
  sysctl -qw net.ipv4.ip_forward=1 "net.ipv4.conf.$tap.proxy_arp=1" "net.ipv4.conf.$tap.rp_filter=0" "net.ipv4.neigh.$tap.proxy_delay=0"
  ip route replace $ip/32 dev $tap
  nft -f - <<NFT
table inet sbx_$tap
delete table inet sbx_$tap
table inet sbx_$tap {
  chain forward {
    type filter hook forward priority 0; policy accept;
    iifname "$tap" ip saddr $ip ip daddr $egress tcp dport 3128 accept
    oifname "$tap" ip saddr $egress tcp sport 3128 ip daddr $ip accept
    oifname "$tap" ip saddr $ctrl ip daddr $ip tcp dport 7070 accept
    iifname "$tap" ip saddr $ip ip daddr $ctrl tcp sport 7070 accept
    iifname "$tap" counter drop
    oifname "$tap" counter drop
  }
  chain input { type filter hook input priority 0; policy accept; iifname "$tap" counter drop; }
}
NFT
  ;;
run)
  name=$1 ip=$2 egress=$3 token=$4 mem=${5:-2048} cpus=${6:-2}; mkdir -p /var/log/sbx /run/sbx
  setsid cloud-hypervisor --api-socket /run/sbx/$name.sock --kernel $D/vmlinux \
    --disk path=$D/rootfs.ext4,readonly=on,image_type=raw --cpus boot=$cpus --memory size=${mem}M \
    --net tap=t$name,mac=$(mac_of $ip) --serial file=/var/log/sbx/$name.serial --console off \
    --cmdline "console=ttyS0 root=/dev/vda ro init=/sbin/sbx-init quiet sbx.ip=$ip sbx.egress=$egress sbx.token=$token" \
    --log-file /var/log/sbx/$name.ch.log -v </dev/null &>/dev/null & ;;
receive)
  name=$1 port=$2; mkdir -p /var/log/sbx /run/sbx; rm -f /run/sbx/$name.sock
  setsid cloud-hypervisor --api-socket /run/sbx/$name.sock --log-file /var/log/sbx/$name.ch.log -v </dev/null &>/dev/null &
  until [ -S /run/sbx/$name.sock ]; do sleep 0.05; done
  setsid ch-remote --api-socket /run/sbx/$name.sock receive-migration receiver_url=tcp:0.0.0.0:$port </dev/null &>/var/log/sbx/$name.recv.log & ;;
send)
  name=$1 dst=$2; shift 2; opts=$(IFS=,; echo "$*")
  ch-remote --api-socket /run/sbx/$name.sock send-migration "destination_url=tcp:$dst${opts:+,$opts}" ;;
rm)
  name=$1; pkill -f "api-socket /run/sbx/$name.sock" || true; ip link del t$name 2>/dev/null || true
  nft delete table inet sbx_t$name 2>/dev/null || true; rm -f /run/sbx/$name.sock ;;
*) echo "unknown $cmd" >&2; exit 2 ;;
esac
