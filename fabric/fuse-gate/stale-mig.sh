#!/usr/bin/env bash
# WP-57: live-migrate a sandbox that keeps zeroing and freeing memory (sandbox/stale.py) and report whether any page
# it zeroed or freed came back with old bytes on the target. Run on the source host, like migrate.sh.
# Usage: stale-mig.sh NAME TARGET_IP [MIB [DIRTY_MIB]]   -> /root/out/stale-NAME/ (stale.jsonl, receive.json, result.json)
#   DIRTY_MIB (default 0): a second process rewrites that much memory throughout, as the hold does in migrate.sh (the
#   last live round about that large, so the program's frees have that round's duration to land after it)
set -uo pipefail
cd "$(dirname "$0")" || exit 1
name=$1; dst=$2; mib=${3:-64}; dirty=${4:-0}
out=/root/out/stale-$name; rm -rf "$out"; mkdir -p "$out"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
./gate.sh rm "$name"; : > "/var/log/gate/$name.log"
./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; mkdir -p /var/log/gate; : > /var/log/gate/$name.log; ./gate.sh bundle $name"
./gate.sh exec "$name" --root sh -c "echo 'python3 /media/stale.py /tmp/stale.jsonl $mib' > /run/jobs/stale.tmp && mv /run/jobs/stale.tmp /run/jobs/stale.sh"
[ "$dirty" != 0 ] && ./gate.sh exec "$name" --root sh -c "echo 'python3 /media/stale.py --dirty $dirty' > /run/jobs/dirty.tmp && mv /run/jobs/dirty.tmp /run/jobs/dirty.sh"
sleep 6
"${SSH[@]}" "cd /root/gate && ./gate.sh receive $name 7000" > "$out/receive.json" &
rj=$!
for _ in $(seq 100); do "${SSH[@]}" "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
./gate.sh send "$name" "$dst" 7000 > "$out/send.txt" 2>&1
wait $rj
./gate.sh rm "$name"
sleep 6
"${SSH[@]}" "cd /root/gate && ./gate.sh exec $name cat /tmp/stale.jsonl" > "$out/stale.jsonl"
"${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root sh -c 'cat /srv/log/exits.log 2>/dev/null'" > "$out/exits.log"
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; rm -rf /run/gate/ckpt/in-$name"
faults=$(grep -c '"fault"' "$out/stale.jsonl")
alive_after=$(python3 -c "import json,sys; r=[json.loads(l) for l in open(sys.argv[1]) if 'alive' in l]; print(r[-1]['i'] if r else 0)" "$out/stale.jsonl")
echo "{\"name\":\"$name\",\"mib\":$mib,\"dirty_mib\":$dirty,\"runsc\":\"$(runsc --version | head -1 | awk '{print $3}')\",\"faults\":$faults,\"last_iteration\":$alive_after,\"receive\":$(cat "$out/receive.json" || echo null)}" | tee "$out/result.json"
