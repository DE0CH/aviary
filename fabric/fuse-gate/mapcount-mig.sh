#!/usr/bin/env bash
# WP-57: live-migrate a sandbox (scratch layout, gate.sh send-scratch / receive-relay, as migrate.sh LAYOUT=scratch)
# whose process writes a counter through a MAP_SHARED mapping of a scratch file (sandbox/mapcount.py), and report
# whether the mapping on the target lost writes made on the source. FINAL_QUICK=1: sandboxd's quick-check final
# delta; otherwise --checksum (gate.sh). Usage: mapcount-mig.sh NAME TARGET_IP -> /root/out/mapcount-NAME/
set -uo pipefail
cd "$(dirname "$0")" || exit 1
name=$1; dst=$2
out=/root/out/mapcount-$name; rm -rf "$out"; mkdir -p "$out"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; mkdir -p /var/log/gate; : > "/var/log/gate/$name.log"
LAYOUT=scratch ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; rm -rf /var/lib/gate/scratch/$name; mkdir -p /var/log/gate; : > /var/log/gate/$name.log; LAYOUT=scratch ./gate.sh bundle $name"
./gate.sh exec "$name" --root sh -c "mkdir -p /mnt/mc && chmod 777 /mnt/mc && echo 'python3 /media/mapcount.py /mnt/mc/counter /tmp/mapcount.jsonl' > /run/jobs/mc.tmp && mv /run/jobs/mc.tmp /run/jobs/mc.sh"
sleep 4
"${SSH[@]}" "cd /root/gate && ./gate.sh receive-relay $name 7000" > "$out/receive.json" &
rj=$!
for _ in $(seq 100); do "${SSH[@]}" "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
./gate.sh send-scratch "$name" "$dst" 7000 > "$out/send.json" 2> "$out/send.txt"
wait $rj
./gate.sh rm "$name"
sleep 3
"${SSH[@]}" "cd /root/gate && ./gate.sh exec $name cat /tmp/mapcount.jsonl" > "$out/mapcount.jsonl"
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; rm -rf /run/gate/ckpt/in-$name"
faults=$(grep -c '"fault"' "$out/mapcount.jsonl")
echo "{\"name\":\"$name\",\"final_quick\":\"${FINAL_QUICK:-}\",\"faults\":$faults,\"send\":$(cat "$out/send.json" || echo null)}" | tee "$out/result.json"
