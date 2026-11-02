#!/usr/bin/env bash
# Control for the processes lost after a restore (GATE.md item 3): migrate a sandbox (pre-copy, as migrate.sh, no
# FUSE traffic) while copyout.py streams data into a process's pages through read(2) and checks them; stale pages
# after the restore = pages written by the sentry that the pre-copy did not re-send. Usage: copyout-mig.sh NAME TARGET_IP
set -uo pipefail
cd "$(dirname "$0")" || exit 1
name=$1; dst=$2; out=/root/out/co-$name; rm -rf "$out"; mkdir -p "$out"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; mkdir -p /var/log/gate; ./gate.sh bundle $name"
r() { ./gate.sh exec "$name" --root "$@"; }
r sh -c "echo 'python3 /media/copyout.py 20 /tmp/co.log' > /run/jobs/co.tmp && mv /run/jobs/co.tmp /run/jobs/co.sh"
sleep 6
"${SSH[@]}" "cd /root/gate && ./gate.sh receive $name 7000" > "$out/receive.json" &
rj=$!
for _ in $(seq 100); do "${SSH[@]}" "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
t0=$(date +%s.%N)
./gate.sh send "$name" "$dst" 7000 > "$out/send.txt" 2>&1
wait $rj
./gate.sh rm "$name"
"${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root sh -c 'for i in \$(seq 300); do [ -f /run/jobs/co.sh.out ] && ! pgrep -f copyout.py >/dev/null && break; sleep 0.1; done; cat /tmp/co.log'" > "$out/co.log"
"${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root sh -c 'cat /run/jobs/co.sh.out /srv/log/exits.log 2>/dev/null'" > "$out/job.out"
echo "{\"migrate_start\":$t0}" > "$out/times.json"
python3 - "$out/co.log" "$t0" <<'PY'
import json, sys
t0 = float(sys.argv[2]); rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
before = [r for r in rows if r["t"] < t0]; after = [r for r in rows if r["t"] >= t0]
print(json.dumps({"checks_before": len(before), "stale_before": max([r["stale_pages"] for r in before] or [0]),
                  "checks_after": len(after), "stale_after_max": max([r["stale_pages"] for r in after] or [0]),
                  "first_after": after[0] if after else None}))
PY
cat "$out/receive.json"; echo
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name"
