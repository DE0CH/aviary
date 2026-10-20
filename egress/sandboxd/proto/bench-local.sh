#!/usr/bin/env bash
# On one host (root): checkpoints the running sandbox NAME into the checkpoint tmpfs and restores it in place,
# with gVisor's debug timelines, to break the pause down. Usage: bench-local.sh NAME [restore flags, e.g. --background]
# [-- checkpoint flags]. Prints: checkpoint ms, restore ms, the sentry's own phase timings.
set -euo pipefail
name=$1; shift; rflags=(); cflags=()
while [ $# -gt 0 ] && [ "$1" != -- ]; do rflags+=("$1"); shift; done; [ "${1:-}" = -- ] && { shift; cflags=("$@"); }
R=(runsc --root /run/sbx/runsc --platform=systrap --network=sandbox --overlay2=root:self,size=5g --host-uds=none --host-fifo=none --allow-connected-on-save)  # = sbx.sh
ms() { date +%s%3N; }
d=/tmp/sbx-bench; rm -rf $d; mkdir -p $d; img=/run/sbx/ckpt/bench-$name; rm -rf $img
t0=$(ms); "${R[@]}" --debug --debug-log=$d/ckpt- checkpoint --image-path $img "${cflags[@]}" "$name"; t1=$(ms)
"${R[@]}" delete -force "$name" 2>/dev/null || true   # the saved sandbox may still be exiting
t2=$(ms); "${R[@]}" --debug --debug-log=$d/rst- restore --bundle /var/lib/sbx/bundles/$name --image-path $img --detach "${rflags[@]}" "$name" </dev/null &>>/var/log/sbx/$name.log; t3=$(ms)
echo "checkpoint_ms=$((t1-t0)) restore_ms=$((t3-t2)) pages_bytes=$(stat -c %s $img/pages.img 2>/dev/null || echo 0) state_bytes=$(stat -c %s $img/checkpoint.img)"
grep -h -E "took \[|Timeline Restore/PagesFileLoader/async page loading ended|Timeline Restore ended|save.*took|Save took|saved|Timeline Save|Timeline Checkpoint" $d/ckpt- $d/rst- \
  | grep -v -E "seccomp|CPUID" | sed -E 's/ \(unix nanos.*//; s/^[IDW][0-9]+ //; s/ +[0-9]+ [a-z_]+\.go:[0-9]+\]//' | head -30
