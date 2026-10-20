#!/usr/bin/env bash
# Moves sandbox NAME (address IP) from host SRC to host DST: what the controller does (docs/SANDBOX-VM.md,
# "Migration"). Run on the controller, which reaches both hosts by SSH (key $KEY) and owns the /32 route to the
# sandbox. MODE:
#   precopy (default) live migration with the forked runsc: DST runs a receiver; SRC copies the sandbox's memory to
#         it while the sandbox keeps running, re-sends pages dirtied meanwhile, then stops it only to send the last
#         dirty pages and its kernel state (target: $DOWNTIME, default 100ms); DST restores at once
#   lazy  stop-and-save on SRC into its tmpfs; DST restores at once with --background, reading pages
#         on demand (and in the background) from SRC over NFS; SRC is removed when DST has every page
#   copy  stop-and-save on SRC, copy the whole image to DST, restore (classic stop-and-copy, for comparison)
# SRC/DST are the addresses the hosts reach each other by (private IPs in one VPC, overlay IPs across clouds).
# ROUTE_VIA: how the controller routes the sandbox's /32 to DST (default "via DST"; e.g. "dev wg0" for a host behind
# WireGuard, whose peer then also gets the /32 in its AllowedIPs: WG_PEER=<DST's public key>). ROUTE_OPTS: more
# `ip route` options, e.g. "rto_min 20ms" (egress then retransmits what was lost during the switch after 20 ms
# instead of Linux's 200 ms minimum).
# BUNDLE: the sbx.sh bundle command for the target (default bundle; bundle-dind for a Docker sandbox), NOHEALTH=1:
# don't wait for the agent's /healthz (sandboxes without the agent).
# Usage: migrate.sh NAME IP SRC DST EGRESS_IP TOKEN [MODE]
set -euo pipefail
name=$1 ip=$2 src=$3 dst=$4 egress=$5 token=$6 mode=${7:-precopy}
KEY=${KEY:-$HOME/key}
ssh_() { local h=$1; shift; ssh -i "$KEY" -o StrictHostKeyChecking=no -o ControlMaster=auto -o ControlPath="/tmp/cm-%h" -o ControlPersist=10m "ubuntu@$h" "$@"; }
ms() { date +%s%3N; }
img=/run/sbx/ckpt/$name

route() {  # point the sandbox's /32 at DST
  if [ -n "${WG_PEER:-}" ]; then sudo wg set wg0 peer "$WG_PEER" allowed-ips "$(sudo wg show wg0 allowed-ips | awk -v p="$WG_PEER" '$1==p{for(i=2;i<=NF;i++) if($i!="(none)") printf "%s,", $i}')$ip/32"; fi
  sudo ip route replace "$ip/32" ${ROUTE_VIA:-via $dst} ${ROUTE_OPTS:-}
}

# prepare the target before anything stops: network (same address, same MACs), bundle, the source's images over NFS
ssh_ "$dst" "sudo ./sbx.sh rm $name >/dev/null 2>&1; sudo ./sbx.sh net $name $ip $egress $egress && sudo ./sbx.sh ${BUNDLE:-bundle} $name $ip $egress $token
  [ $mode = precopy ] || { sudo mkdir -p /mnt/from-$src && (mountpoint -q /mnt/from-$src || sudo mount -t nfs4 -o ro,nconnect=8,rsize=1048576 $src:/run/sbx/ckpt /mnt/from-$src); }"
ssh_ "$src" true

if [ "$mode" = precopy ]; then
  # DST: create the sandbox while the stream arrives into its tmpfs, restore when it ends (lazily: pages still load
  # after it runs)
  out=$(mktemp)
  ssh_ "$dst" "sudo ./sbx.sh receive-restore $name 7000" > "$out" &
  dstjob=$!
  until ssh_ "$dst" "ss -ltn | grep -q ':7000 '"; do :; done
  t0=$(ms)
  ssh_ "$src" "sudo ./sbx.sh send $name $dst:7000 ${DOWNTIME:-100ms}"   # returns when the final save is sent
  t1=$(ms)
  wait $dstjob                                                          # DST restored
  t2=$(ms)
  route
  t3=$(ms)
  [ -n "${NOHEALTH:-}" ] || until curl -sf -m 0.2 "http://$ip:7070/healthz" >/dev/null; do :; done
  t4=$(ms)
  r=$(cat "$out"); rm -f "$out"
  final=$(jq -r .final_ms <<<"$r"); restore=$(jq -r .restore_ms <<<"$r")
  # stopped -> healthy, estimated: the source stops right after its last live round, which DST sees as the last
  # Round record; from there DST took final_ms to get the rest, restore_ms to restore, then route + first answer
  echo "{\"mode\":\"precopy\",\"migration_total_ms\":$((t2-t0)),\"send_returned_ms\":$((t1-t0)),\"dst\":$r,\"route_ms\":$((t3-t2)),\"first_health_ms\":$((t4-t3)),\"stopped_to_healthy_est_ms\":$(jq -n "$final + $restore + $((t4-t2))")}"
  ssh_ "$src" "sudo ./sbx.sh rm $name"
  ssh_ "$dst" "sudo rm -rf /run/sbx/ckpt/in-$name"
  exit 0
fi

t0=$(ms)
ssh_ "$src" "sudo ./sbx.sh checkpoint $name $img"                 # the sandbox stops here
t1=$(ms)
if [ "$mode" = copy ]; then
  ssh_ "$dst" "sudo rm -rf /run/sbx/ckpt/in-$name && sudo cp -r /mnt/from-$src/$name /run/sbx/ckpt/in-$name"
  t2=$(ms); ssh_ "$dst" "sudo ./sbx.sh restore $name /run/sbx/ckpt/in-$name"
else
  t2=$t1; ssh_ "$dst" "sudo ./sbx.sh restore $name /mnt/from-$src/$name --background"
fi
t3=$(ms)
route                                                                  # the sandbox's traffic now goes to DST
t4=$(ms)
until curl -sf -m 0.2 "http://$ip:7070/healthz" >/dev/null; do :; done
t5=$(ms)
bytes=$(ssh_ "$src" "sudo du -sb $img | cut -f1")
echo "{\"mode\":\"$mode\",\"checkpoint_ms\":$((t1-t0)),\"copy_ms\":$((t2-t1)),\"restore_ms\":$((t3-t2)),\"route_ms\":$((t4-t3)),\"first_health_ms\":$((t5-t4)),\"stopped_to_healthy_ms\":$((t5-t0)),\"image_bytes\":$bytes}"
# the source keeps serving pages until the target has loaded all of them; then it goes
if [ "$mode" = lazy ]; then
  # done when the source's NFS server has stopped sending (its read-bytes counter holds still for 2 s)
  prev=-1; while :; do cur=$(ssh_ "$src" "awk '/^io/{print \$2}' /proc/net/rpc/nfsd"); [ "$cur" = "$prev" ] && break; prev=$cur; sleep 2; done
  t6=$(( $(ms) - 2000 ))
  echo "{\"background_load_done_after_ms\":$(( t6 - t3 )),\"nfs_bytes_read\":$cur}"
fi
ssh_ "$src" "sudo ./sbx.sh rm $name; sudo rm -rf $img /run/sbx/ckpt/in-$name"
