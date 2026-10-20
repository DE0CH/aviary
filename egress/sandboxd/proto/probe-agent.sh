#!/usr/bin/env bash
# Polls a sandbox agent's /healthz every 20 ms from the controller and logs each failure or slow answer (> 100 ms)
# with a timestamp: the agent's unavailability during a migration. Usage: probe-agent.sh IP LOGFILE
ip=$1; log=$2
while :; do
  t0=$(date +%s.%N)
  if out=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://$ip:7070/healthz"); then :; fi
  t1=$(date +%s.%N)
  ms=$(echo "($t1-$t0)*1000/1" | bc)
  if [ "$out" != 200 ] || [ "$ms" -gt 100 ]; then echo "$t0 code=$out ms=$ms" >> "$log"; fi
  sleep 0.02
done
