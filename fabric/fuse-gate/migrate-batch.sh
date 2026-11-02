#!/usr/bin/env bash
# Gate item 3, repeated: N migrations per configuration, every run's outputs kept (failed ones included).
# Usage: migrate-batch.sh TARGET_IP PREFIX N [LAYOUT [FILL_COUNT FILL_MB]]   -> /root/out/batch-PREFIX.log
# HOLDS="1 2": the runs alternate between these HOLD values (run i takes the ((i-1) mod k)-th), so a configuration
#   and its control share the same hosts and the same hour; otherwise every run uses HOLD (default 1).
dst=$1; pre=$2; n=$3; layout=${4:-tmpfs}; fc=${5:-0}; fm=${6:-0}
cd "$(dirname "$0")" || exit 1
log=/root/out/batch-$pre.log; : > "$log"
read -r -a holds <<< "${HOLDS:-${HOLD:-1}}"
for i in $(seq -w 1 "$n"); do
  h=${holds[$(( (10#$i - 1) % ${#holds[@]} ))]}
  HOLD=$h LAYOUT=$layout ./migrate.sh "$pre$i" "$dst" "$fc" "$fm" > "/root/out/run-$pre$i.txt" 2>&1
  echo "$pre$i $layout fill=$fc/$fm hold=$h mode=${MODE:-fuse} debug=${RESTORE_DEBUG:-0}${COMPARE:+ compare=$COMPARE}${FINAL_QUICK:+ final_quick=$FINAL_QUICK} exit=$? $(tail -n 1 "/root/out/run-$pre$i.txt")" >> "$log"
done
echo "done" >> "$log"
