#!/bin/bash
# runs ON wp19-a: hermit 13.2 item 6 on a real runsc: SIGKILL the sandbox's sentry (the runsc-sandbox process, as a
# memcg OOM kill of the sentry would), then how long until GET /v1/sandboxes lists it alive:false. Usage: NAME N
set -uo pipefail
name=$1; n=$2; P=/root/wp19/pki; D=$(cat /root/wp19/digest)
api() { curl -sS -m 60 --cacert $P/ca.crt --cert $P/hermit.crt --key $P/hermit.key --resolve sandboxd:7443:10.99.0.1 -X "$1" -H 'content-type: application/json' "https://sandboxd:7443$2" "${@:3}"; }
api POST /v1/sandboxes -d "{\"name\":\"$name\",\"ip\":\"10.98.0.$n\",\"image\":\"$D\",\"env\":{},\"cpu\":1,\"memory\":536870912,\"cpuFeatures\":\"\"}" | jq -c '{state,alive}'
sleep 12; cg=/sys/fs/cgroup/sbx/$name
# shellcheck disable=SC2013 # one pid per line
for p in $(cat "$cg"/cgroup.procs); do tr '\0' ' ' < /proc/"$p"/cmdline | grep -q '^runsc-sandbox' && kill -9 "$p" && echo "killed sentry $p"; done
t0=$(date +%s%3N)
for _ in $(seq 1 30); do
  a=$(api GET /v1/sandboxes | jq -r --arg n "$name" '.sandboxes[] | select(.name == $n) | .alive')
  echo "t+$(( $(date +%s%3N) - t0 )) ms: alive=$a procs=$(wc -l < "$cg"/cgroup.procs 2>/dev/null) $(tr '\n' ' ' < "$cg"/cgroup.events 2>/dev/null || echo cgroup-gone)"
  [ "$a" = false ] && break; sleep 0.5
done
api DELETE /v1/sandboxes/"$name"; echo
