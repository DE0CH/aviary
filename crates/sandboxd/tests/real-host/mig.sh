#!/bin/bash
# runs ON wp19-a (the hub): one live migration a -> b through sandboxd's API (as hermit's migrate task calls it) of a
# sandbox whose first process is mapcount.py (a counter written through a MAP_SHARED mapping of a scratch file and
# checked every iteration); optional FILL_MIB of random data in the scratch first. Prints one JSON line.
# Usage: mig.sh NAME N [FILL_MIB]   (N: the sandbox address's last octet)
set -uo pipefail
name=$1; n=$2; fill=${3:-0}
P=/root/wp19/pki; D=$(cat /root/wp19/digest)
api() { local ip=$1 m=$2 p=$3; shift 3; curl -sS -m 1000 --cacert $P/ca.crt --cert $P/hermit.crt --key $P/hermit.key \
  --resolve sandboxd:7443:"$ip" -X "$m" -H 'content-type: application/json' "https://sandboxd:7443$p" "$@"; }
B() { ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/wp19 root@10.77.0.3 "$@"; }
spec="{\"name\":\"$name\",\"ip\":\"10.98.0.$n\",\"image\":\"$D\",\"env\":{},\"cpu\":1,\"memory\":2147483648,\"cpuFeatures\":\"\"}"
c=$(api 10.99.0.1 POST /v1/sandboxes -d "$spec")
echo "$c" | grep -q '"state":"running"' || { echo "{\"name\":\"$name\",\"create\":$c}"; exit 1; }
sleep 3
if [ "$fill" -gt 0 ]; then
  runsc --root /run/sbx/runsc exec "$name" sh -c "dd if=/dev/urandom of=/tmp/fill bs=1M count=$fill status=none && sync" || echo "fill failed" >&2
fi
sleep 2
s=$(api 10.99.0.1 GET /v1/sandboxes/"$name"/spec)
rx=$(api 10.99.0.10 POST /v1/sandboxes/"$name"/receive -d "$s")
to=$(echo "$rx" | jq -r '"\(.address):\(.port)"')
t0=$(date +%s%3N)
send=$(api 10.99.0.1 POST /v1/sandboxes/"$name"/send -d "{\"to\":\"$to\",\"downtimeMs\":100,\"maxRounds\":20}")
t1=$(date +%s%3N)
res=$(api 10.99.0.10 GET "/v1/sandboxes/$name/receive?wait=120000")
sleep 3
log=$(B "runsc --root /run/sbx/runsc exec $name cat /tmp/mc/log.jsonl")
faults=$(printf '%s\n' "$log" | grep -c '"fault"')
alive=$(printf '%s\n' "$log" | grep '"alive"' | tail -1)
api 10.99.0.10 DELETE /v1/sandboxes/"$name" >/dev/null
echo "{\"name\":\"$name\",\"fill_mib\":$fill,\"binary\":\"$(cat /root/wp19/which)\",\"faults\":$faults,\"send_ms\":$((t1 - t0)),\"send\":${send:-null},\"receive\":${res:-null},\"last_alive\":${alive:-null}}"
[ "$faults" = 0 ] || printf '%s\n' "$log" | grep '"fault"' | head -3 >&2
