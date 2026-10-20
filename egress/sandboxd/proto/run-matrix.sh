#!/usr/bin/env bash
# The measured matrix: for each workload size, a fresh sandbox on A, then A→B lazy and B→A copy. Results in
# $STATE/results-<time>.txt. Usage: run-matrix.sh "SIZES_MIB" [RATE_MIB_S=64]
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}; export STATE
out=$STATE/results-$(date -u +%Y%m%dT%H%M%S).txt
for s in $1; do
  "$here/bringup.sh" >/dev/null 2>&1
  { echo "=== size $s MiB, rate ${2:-64} MiB/s: A->B lazy"; "$here/run-test.sh" a b lazy "$s" "${2:-64}" --fresh-workload
    echo "=== size $s MiB: B->A copy"; "$here/run-test.sh" b a copy; } 2>&1 | grep -v -E 'Warning|^\s*$' | tee -a "$out"
done
echo "results: $out"
