#!/bin/bash
# runs ON a wp19 host (as the hub, a): hermit 13.2 item 6 on a real runsc: a sandbox (memory limit 512 MiB) whose
# process allocates past its limit, so the host memcg OOM-kills in /sbx/<name>; then how long until GET /v1/sandboxes
# lists it alive:false, and what is left in its cgroup. Usage: oom.sh NAME N
set -uo pipefail
name=$1; n=$2; P=/root/wp19/pki; D=$(cat /root/wp19/digest)
api() { curl -sS -m 60 --cacert $P/ca.crt --cert $P/hermit.crt --key $P/hermit.key --resolve sandboxd:7443:10.99.0.1 -X "$1" -H 'content-type: application/json' "https://sandboxd:7443$2" "${@:3}"; }
api POST /v1/sandboxes -d "{\"name\":\"$name\",\"ip\":\"10.98.0.$n\",\"image\":\"$D\",\"env\":{},\"cpu\":1,\"memory\":536870912,\"cpuFeatures\":\"\"}" | jq -c '{state,alive}'
sleep 3; cg=/sys/fs/cgroup/sbx/$name
# shellcheck disable=SC2013 # one pid per line
echo "before: $(tr '\n' ' ' < "$cg"/cgroup.events) procs: $(wc -l < "$cg"/cgroup.procs) $(for p in $(cat "$cg"/cgroup.procs); do tr '\0' ' ' < /proc/"$p"/cmdline | cut -c1-40; echo -n '; '; done)"
t0=$(date +%s%3N)
timeout 60 runsc --root /run/sbx/runsc exec "$name" python3 -c "
b=[]
while True: b.append(bytearray(64<<20))" >/dev/null 2>&1; echo "exec exit $? after $(( $(date +%s%3N) - t0 )) ms"
grep -E "oom_kill |oom " "$cg"/memory.events 2>/dev/null | tr '\n' ' '; echo
for _ in $(seq 1 30); do
  a=$(api GET /v1/sandboxes | jq -r --arg n "$name" '.sandboxes[] | select(.name == $n) | .alive')
  ev=$(tr '\n' ' ' < "$cg"/cgroup.events 2>/dev/null || echo gone)
  echo "t+$(( $(date +%s%3N) - t0 )) ms: alive=$a cgroup.events: $ev procs: $(wc -l < "$cg"/cgroup.procs 2>/dev/null)"
  [ "$a" = false ] && break; sleep 1
done
echo "runsc state: $(runsc --root /run/sbx/runsc state "$name" 2>&1 | jq -r .status 2>/dev/null)"
journalctl -k --since "-2min" --no-pager | grep -iE "oom|killed process" | tail -3 | cut -c1-200
api DELETE /v1/sandboxes/"$name"; echo
