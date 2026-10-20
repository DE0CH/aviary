#!/usr/bin/env bash
# Moves sandbox NAME (address IP) from host SRC to host DST: what the controller does (docs/SANDBOX-VM.md,
# "Migration"). Run on the controller, which reaches both hosts by SSH (key $KEY) and owns the /32 route to the
# sandbox. MODE:
#   lazy  (default) stop-and-save on SRC into its tmpfs; DST restores at once with --background, reading pages
#         on demand (and in the background) from SRC over NFS; SRC is removed when DST has every page
#   copy  stop-and-save on SRC, copy the whole image to DST, restore (classic stop-and-copy, for comparison)
# Usage: migrate.sh NAME IP SRC DST EGRESS_IP TOKEN [MODE]   (SRC/DST are private IPs)
set -euo pipefail
name=$1 ip=$2 src=$3 dst=$4 egress=$5 token=$6 mode=${7:-lazy}
KEY=${KEY:-$HOME/key}
ssh_() { local h=$1; shift; ssh -i "$KEY" -o StrictHostKeyChecking=no -o ControlMaster=auto -o ControlPath="/tmp/cm-%h" -o ControlPersist=10m "ubuntu@$h" "$@"; }
ms() { date +%s%3N; }
img=/run/sbx/ckpt/$name

# prepare the target before anything stops: network (same address, same MACs), bundle, the source's images over NFS
ssh_ "$dst" "sudo ./sbx.sh rm $name >/dev/null 2>&1; sudo ./sbx.sh net $name $ip $egress $egress && sudo ./sbx.sh bundle $name $ip $egress $token
  sudo mkdir -p /mnt/from-$src && (mountpoint -q /mnt/from-$src || sudo mount -t nfs4 -o ro,nconnect=8,rsize=1048576 $src:/run/sbx/ckpt /mnt/from-$src)"
ssh_ "$src" true

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
sudo ip route replace "$ip/32" via "$dst"                              # the sandbox's traffic now goes to DST
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
