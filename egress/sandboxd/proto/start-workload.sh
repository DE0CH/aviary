#!/usr/bin/env bash
# Starts the migration-test workload in a sandbox through its agent (run on the controller): a background process
# dirtying memory (SIZE MiB, RATE MiB/s), a heartbeat TCP connection through the egress proxy, a background counter.
# Usage: start-workload.sh IP TOKEN [SIZE_MIB=1024] [RATE_MIB_S=64]
set -euo pipefail
ip=$1 token=$2 size=${3:-1024} rate=${4:-64}
here=$(cd "$(dirname "$0")" && pwd)
ex() { curl -sf -m 30 -X POST -H "X-Jarvis-Exec-Token: $token" -H 'content-type: application/json' \
  -d "$(jq -n --arg c "$1" --argjson bg "${2:-false}" '{command: $c, background: $bg}')" "http://$ip:7070/exec"; echo; }
ex "echo $(base64 -w0 "$here/workload.py") | base64 -d > /work/workload.py"
ex "python3 /work/workload.py mem $size $rate" true
ex "python3 /work/workload.py heart 127.0.0.1 9000" true
ex 'while :; do date +%s.%N >> /work/counter.log; sleep 1; done' true
