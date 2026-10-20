#!/usr/bin/env bash
# One measured microVM migration (shape B) with the workload running: CH's own downtime figure, the controller's
# timings, the agent probe and the heartbeat gaps seen from outside. Usage: vm-run-test.sh FROM TO [send options]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
E=$(col egress 3) Ep=$(col egress 4) S=$(col host-$1 4) D=$(col host-$2 4); opts=${3:-downtime_ms=100,connections=4}
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
scp -q "${o[@]}" "$here/vm-migrate.sh" ubuntu@$E:
ssh "${o[@]}" ubuntu@$E "mark=\$(wc -l < /var/log/sbx-heartbeat.log); : > /tmp/probe-vm.log
  nohup ./probe-agent.sh 10.200.0.30 /tmp/probe-vm.log >/dev/null 2>&1 & p=\$!; sleep 1
  KEY=\$HOME/key timeout 300 ./vm-migrate.sh vm1 10.200.0.30 $S $D $Ep $opts | grep -a -E 'migration_total|Migration completed|iteration'
  sleep 3; kill \$p; echo '--- agent probe'; cat /tmp/probe-vm.log; echo '--- heartbeat gaps seen outside'; tail -n +\$((mark+1)) /var/log/sbx-heartbeat.log"
