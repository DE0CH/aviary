#!/usr/bin/env bash
# One measured migration of sandbox s1 with the workload running (after bringup.sh). Prints the controller's
# timings, the agent probe's failures, the heartbeat gaps seen by the egress side, and the in-sandbox check.
# Usage: run-test.sh FROM(a|b) TO(a|b) [lazy|copy] [SIZE_MIB] [RATE_MIB_S] [--fresh-workload]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
from=$1 to=$2 mode=${3:-lazy} size=${4:-1024} rate=${5:-64} fresh=${6:-}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
E=$(col egress 3) Ep=$(col egress 4) S=$(col host-$from 4) D=$(col host-$to 4); TOKEN=$(cat "$STATE/token")
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
ssh "${o[@]}" ubuntu@$E "set -e
  [ -n '$fresh' ] && { ./start-workload.sh 10.200.0.10 $TOKEN $size $rate >/dev/null; sleep 20; }
  : > /tmp/probe.log; mark=\$(wc -l < /var/log/sbx-heartbeat.log)
  nohup ./probe-agent.sh 10.200.0.10 /tmp/probe.log >/dev/null 2>&1 & probe=\$!; sleep 2
  date +%s.%N | sed 's/^/start /'
  KEY=\$HOME/key ./migrate.sh s1 10.200.0.10 $S $D $Ep $TOKEN $mode
  sleep 3; kill \$probe || true
  echo '--- agent probe (failures or > 100 ms)'; cat /tmp/probe.log
  echo '--- heartbeat gaps seen outside'; tail -n +\$((mark+1)) /var/log/sbx-heartbeat.log
  echo '--- inside the sandbox'; ./check-workload.sh 10.200.0.10 $TOKEN"
