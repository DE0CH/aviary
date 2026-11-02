#!/usr/bin/env bash
# Gate item 3: live-migrate a sandbox (pre-copy, checkpoint --migrate-to / restore --migrate-listen) to another host
# while Claude Code is mid-turn and writes are blocked in the supervisor (the fake magpie holds every mutating call).
# Run on the source host; the target is reached by SSH as root (same gate.sh and /var/lib/gate on both).
# Usage: migrate.sh NAME TARGET_IPV6 [FILL_COUNT FILL_MB]   (FILL: content cache filled first, e.g. 64 87)
set -euo pipefail
cd "$(dirname "$0")"
name=$1; dst=$2; fc=${3:-0}; fm=${4:-0}; out=/root/out/mig-$name; rm -rf "$out"; mkdir -p "$out"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
ms() { date +%s%3N; }
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; ./gate.sh bundle $name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
if [ "$fc" != 0 ]; then
  x python3 /media/fill.py "$fc" "$fm" > "$out/fill.txt"
  r sh -c "du -sm /srv/supervisor/cache /srv/magpie/d" >> "$out/fill.txt"
fi
# Claude Code mid-turn: a tool call, then an 8 s streamed answer whose transcript write will block
job() { r sh -c "echo '$2' > /run/jobs/$1.tmp && mv /run/jobs/$1.tmp /run/jobs/$1.sh"; }
job drive "cd /home/sandbox && python3 /media/drive.py /media/session-m.json /tmp/out-m"
# the directly blocked append goes to an existing file (a held CREATE would hold its directory's lock, and every
# path walk through that directory waits behind it: recorded in GATE.md)
x sh -c "echo start > ~/.claude/blocked.jsonl; echo probe > ~/.claude/keeper-probe.txt; cat ~/.claude/keeper-probe.txt >/dev/null"
sleep 2.5
r touch /run/magpie-ctl/hold
# and a direct 3 x 1 MiB append that blocks in the supervisor
job blocked "python3 /media/killtest.py append-existing /home/sandbox/.claude/blocked.jsonl 3 1048576 > /tmp/blocked.json"
sleep 8
r sh -c "grep -c HOLDING /srv/magpie/calls.log; grep HOLDING /srv/magpie/calls.log | tail -3" > "$out/held-before.txt"
"${SSH[@]}" "cd /root/gate && ./gate.sh receive $name 7000" > "$out/receive.json" &
rj=$!
for _ in $(seq 100); do "${SSH[@]}" "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
t0=$(ms)
./gate.sh send "$name" "$dst" 7000 > "$out/send.txt" 2>&1
t1=$(ms)
wait $rj
t2=$(ms)
./gate.sh rm "$name"
y() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name $*"; }
yr() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root $*"; }
yr sh -c "'grep -c HOLDING /srv/magpie/calls.log'" > "$out/held-after-move.txt"
# a probe on the target while the hold is still on: how long a stat and a cached read of other files wait
yj() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root sh -c \"echo '$2' > /run/jobs/$1.tmp && mv /run/jobs/$1.tmp /run/jobs/$1.sh\""; }
yj probe 'python3 /media/probe.py > /tmp/probe.json'
sleep 3
yr sh -c "'ps -eo pid,stat,pcpu,comm; grep -c HOLDING /srv/magpie/calls.log; date +%s.%N'" >> "$out/held-after-move.txt"
t3=$(ms)
yr rm /run/magpie-ctl/hold
yr sh -c "'for i in \$(seq 600); do [ -f /tmp/out-m/summary.json ] && break; sleep 0.1; done'"
t4=$(ms)
y cat /tmp/out-m/summary.json > "$out/drive.json"
y cat /tmp/blocked.json > "$out/blocked.json"
y cat /tmp/probe.json > "$out/probe.json"
yr cat /srv/magpie/calls.log > "$out/calls.log"
yr cat /srv/supervisor/ops.tsv > "$out/ops.tsv"
yr python3 /media/report.py /home/sandbox/.claude /srv/magpie/d /srv/supervisor/ops.tsv > "$out/report.json"
yr cat /srv/log/supervisor.log > "$out/supervisor.log"
echo "{\"send_ms\":$((t1-t0)),\"total_ms\":$((t2-t0)),\"release_to_turn_done_ms\":$((t4-t3))}" > "$out/times.json"
cat "$out/times.json" "$out/receive.json"; echo; cat "$out/held-before.txt" "$out/held-after-move.txt"
jq -c '{turns: [.turns[].result], exit}' "$out/drive.json"; jq -c '{exact, writes}' "$out/blocked.json"; cat "$out/probe.json"
grep -c '/write blocked.jsonl' "$out/calls.log" | sed 's/^/magpie writes to blocked.jsonl (3 appends + 1 create-time echo, plus resends): /'
grep -P '\tINTERRUPT\t' "$out/ops.tsv" | cut -f2,5
jq -c '[.files[] | select(.path|endswith(".jsonl")) | {path, equal_to_magpie, bad_lines}]' "$out/report.json"
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name"
