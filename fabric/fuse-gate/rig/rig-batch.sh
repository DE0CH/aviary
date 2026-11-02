#!/usr/bin/env bash
# rig-mig.sh repeated: N rounds of every OP (write create mkdir rename) in every LAYOUTS (default "tmpfs scratch"),
# every run's outputs kept.  Usage: rig-batch.sh PREFIX N [TARGET_IP]   -> /root/out/rig-batch-PREFIX.log
# (LOOPBACK=1 as in rig-mig.sh)
pre=$1; n=$2; dst=${3:-::1}
cd "$(dirname "$0")" || exit 1
log=/root/out/rig-batch-$pre.log; : > "$log"
for i in $(seq -w 1 "$n"); do
  for layout in ${LAYOUTS:-tmpfs scratch}; do
    for op in ${OPS:-write create mkdir rename}; do
      id=$pre$i-${layout:0:1}-$op
      LAYOUT=$layout ./rig-mig.sh "$id" "$op" "$dst" > "/root/out/rig-run-$id.txt" 2>&1; st=$?
      echo "$id layout=$layout op=$op loopback=${LOOPBACK:-0} exit=$st resend=$(grep -c . "/root/out/rig-$id/resend.txt" 2>/dev/null) $(tail -n 1 "/root/out/rig-run-$id.txt")" >> "$log"
    done
  done
done
echo "done" >> "$log"
