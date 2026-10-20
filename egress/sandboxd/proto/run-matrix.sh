#!/usr/bin/env bash
# The measured matrix: for each workload size, a fresh sandbox on host a, then four pre-copy migrations in a row
# between a and $PEER (default b; c for the Hetzner host): a->peer, peer->a, a->peer, peer->a. Each prints the
# controller's timings, the heartbeat gaps the egress side saw, and the in-sandbox check (processes, memcheck,
# heartbeat connection, counter). Results in $STATE/results-<time>.txt.
# Usage: run-matrix.sh "SIZES_MIB" [RATE_MIB_S=64]   (env: PEER=b|c, DOWNTIME=100ms)
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}; export STATE
peer=${PEER:-b}
out=$STATE/results-$(date -u +%Y%m%dT%H%M%S).txt
for s in $1; do
  "$here/bringup.sh" >/dev/null 2>&1
  { echo "=== size $s MiB, rate ${2:-64} MiB/s: a->$peer"; "$here/run-test.sh" a $peer precopy "$s" "${2:-64}" --fresh-workload
    echo "=== size $s MiB: $peer->a"; "$here/run-test.sh" $peer a precopy
    echo "=== size $s MiB: a->$peer (3rd in a row)"; "$here/run-test.sh" a $peer precopy
    echo "=== size $s MiB: $peer->a (4th in a row)"; "$here/run-test.sh" $peer a precopy; } 2>&1 | grep -v -E 'Warning|^\s*$' | tee -a "$out"
done
echo "results: $out"
