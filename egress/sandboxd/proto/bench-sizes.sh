#!/usr/bin/env bash
# For each workload size: fresh sandbox s1 on host A, workload running 15 s, then bench-local.sh (in-place
# checkpoint + restore) with the given restore flags. Shows how the pause scales with memory. Run from the operator
# machine. Usage: bench-sizes.sh "SIZES_MIB" [restore flags...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
A=$(col host-a 3) E=$(col egress 3) Ap=$(col host-a 4) Ep=$(col egress 4) TOKEN=$(cat "$STATE/token")
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
sizes=$1; shift
scp -q "${o[@]}" "$here/bench-local.sh" "$here/sbx.sh" ubuntu@$A:
for s in $sizes; do
  ssh "${o[@]}" ubuntu@$A "sudo ./sbx.sh rm s1 >/dev/null 2>&1; sudo rm -rf /run/sbx/ckpt/* /var/log/sbx/debug; sudo ./sbx.sh net s1 10.200.0.10 $Ep $Ep && sudo ./sbx.sh bundle s1 10.200.0.10 $Ep $TOKEN && sudo SBX_DEBUG=${SBX_DEBUG:-} ./sbx.sh run s1"
  ssh "${o[@]}" ubuntu@$E "sudo ip route replace 10.200.0.10/32 via $Ap; until curl -sf -m 1 http://10.200.0.10:7070/healthz >/dev/null; do sleep 0.2; done
    [ $s -gt 0 ] && ./start-workload.sh 10.200.0.10 $TOKEN $s 64 >/dev/null; sleep 15"
  echo "== size ${s} MiB, restore flags: $*"
  ssh "${o[@]}" ubuntu@$A "sudo ./bench-local.sh s1 $* | grep -E 'checkpoint_ms|Kernel load took|Overall|restore walltime|async page loading ended'"
done
