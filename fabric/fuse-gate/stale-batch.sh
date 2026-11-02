#!/usr/bin/env bash
# WP-57: N runs of stale-mig.sh -> /root/out/stale-batch-PREFIX.log (one result line per run, then "done")
# Usage: stale-batch.sh TARGET_IP PREFIX N [MIB [DIRTY_MIB]]
dst=$1; pre=$2; n=$3; mib=${4:-32}; dirty=${5:-128}
cd "$(dirname "$0")" || exit 1
log=/root/out/stale-batch-$pre.log; : > "$log"
for i in $(seq -w 1 "$n"); do
  timeout 180 ./stale-mig.sh "$pre$i" "$dst" "$mib" "$dirty" 2>/dev/null | tail -n 1 >> "$log"
done
echo done >> "$log"
