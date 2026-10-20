#!/usr/bin/env bash
# Live-migrates microVM NAME (address IP) from SRC to DST with Cloud Hypervisor's pre-copy (vm.sh), from the
# controller, then moves the /32 route. Usage: vm-migrate.sh NAME IP SRC DST EGRESS_IP [send-migration options...]
set -euo pipefail
name=$1 ip=$2 src=$3 dst=$4 egress=$5; shift 5
KEY=${KEY:-$HOME/key}
ssh_() { local h=$1; shift; ssh -i "$KEY" -o StrictHostKeyChecking=no -o ControlMaster=auto -o ControlPath="/tmp/cm-%h" -o ControlPersist=10m "ubuntu@$h" "$@"; }
ms() { date +%s%3N; }
ssh_ "$dst" "sudo ./vm.sh rm $name >/dev/null 2>&1; sudo ./vm.sh net $name $ip $egress $egress && sudo ./vm.sh receive $name 6000"; sleep 0.5
tx0=$(ssh_ "$src" "cat /sys/class/net/\$(ip route show default | awk '{print \$5}')/statistics/tx_bytes")
t0=$(ms)
ssh_ "$src" "sudo ./vm.sh send $name $dst:6000 $*"     # returns at once; CH migrates in the background
# the VM runs on DST when DST's receive-migration call returns: then the route moves
ssh_ "$dst" "while pgrep -f '[r]eceive-migration receiver_url' >/dev/null; do sleep 0.002; done"
t1=$(ms)
tx1=$(ssh_ "$src" "cat /sys/class/net/\$(ip route show default | awk '{print \$5}')/statistics/tx_bytes")
sudo ip route replace "$ip/32" via "$dst"
until curl -sf -m 0.2 "http://$ip:7070/healthz" >/dev/null; do :; done
t2=$(ms)
echo "{\"migration_total_ms\":$((t1-t0)),\"route_to_healthy_ms\":$((t2-t1)),\"src_tx_bytes\":$((tx1-tx0))}"
ssh_ "$dst" "sudo grep -a -E 'event = (resum|migration)' /var/log/sbx/$name.ch.log | tail -6" || true
ssh_ "$src" "sudo grep -a -i -E 'Migration completed|iteration|dirty|pausing|event = (paus|snapshot)' /var/log/sbx/$name.ch.log | grep -v MSR | tail -25; sudo ./vm.sh rm $name >/dev/null 2>&1" || true
