#!/usr/bin/env bash
# Gate item 3: live-migrate a sandbox (pre-copy, checkpoint --migrate-to / restore --migrate-listen) to another host
# while Claude Code is mid-turn and writes are blocked in the supervisor (the fake magpie holds every mutating call).
# Run on the source host; the target is reached by SSH as root (same gate.sh and /var/lib/gate on both).
# Usage: migrate.sh NAME TARGET_IP [FILL_COUNT FILL_MB]   (FILL: content cache filled first, e.g. 64 87 = 64 x 4 MiB
#        tool results plus an 87 MiB transcript-sized file, see fill.py)
# LAYOUT=tmpfs (default): supervisor state, cache, journal and the fake store on the sandbox's /srv tmpfs (they move
#   as memory pages); LAYOUT=scratch: on the host-backed scratch dir, moved the way sandboxd moves scratch (rsync
#   rounds while running, the final delta while stopped, behind `sandboxd migrate-relay`: gate.sh send-scratch).
# HOLD=0: no hold, no blocked append, no probe (a control: migration with nothing held in the supervisor).
# HOLD=2: the matched control: the same 3 x 1 MiB append process and the probe as HOLD=1, but the fake magpie does
#   not hold (the append completes before the checkpoint; nothing is held in the supervisor across it).
# HOLD=3: HOLD=2 plus a process that rewrites DIRTY_MIB (default 125) MiB of its memory throughout (stale.py --dirty),
#   so the last memory round is about as large as with the hold (WP-57: separates "requests held across the
#   checkpoint" from "a final round with about 125 MB more pages").
# MODE=local: Claude Code's HOME on the sandbox's /tmp (no FUSE in its path; a control). RESTORE_DEBUG=1: the target
#   restores with runsc --debug and the faults it logs are collected (debug-faults.txt).
# GATE_RUNSC_EXTRA: extra runsc flags on both hosts (an experiment, e.g. --systrap-disable-syscall-patching).
# Every output lands in /root/out/mig-NAME (both hosts' runsc logs included). Exit 1 when the run fails: no
# summary from the driver within 120 s of the release, a turn that is not `success`, the blocked append not exact,
# a transcript that differs from magpie's copy, a repeated `unique`, an in-sandbox process exit (exits.log: the
# keeper, the supervisor, the fakes, a job exiting non-zero), Claude Code's own non-zero exit, or a fault in the
# supervisor's log (a panicked thread, a request read incompletely: SHORTREAD).
set -uo pipefail
cd "$(dirname "$0")" || exit 1
name=$1; dst=$2; fc=${3:-0}; fm=${4:-0}; layout=${LAYOUT:-tmpfs}; hold=${HOLD:-1}; mode=${MODE:-fuse}
out=/root/out/mig-$name; rm -rf "$out"; mkdir -p "$out"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
ms() { date +%s%3N; }
fail=()
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; : > "/var/log/gate/$name.log"
LAYOUT=$layout ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
"${SSH[@]}" "cd /root/gate && GATE_RUNSC_EXTRA='${GATE_RUNSC_EXTRA:-}' ./gate.sh rm $name; rm -rf /var/lib/gate/scratch/$name; mkdir -p /var/log/gate; : > /var/log/gate/$name.log; LAYOUT=$layout ./gate.sh bundle $name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
if [ "$fc" != 0 ]; then
  x python3 /media/fill.py "$fc" "$fm" > "$out/fill.txt"
  r sh -c "du -sm /srv/supervisor/cache/ /srv/magpie/d/" >> "$out/fill.txt"
fi
# Claude Code mid-turn: a tool call, then an 8 s streamed answer whose transcript write will block
job() { r sh -c "echo '$2' > /run/jobs/$1.tmp && mv /run/jobs/$1.tmp /run/jobs/$1.sh"; }
if [ "$mode" = local ]; then
  x sh -c "mkdir -p /tmp/lh && python3 -c \"import json; s=json.load(open('/media/session-m.json')); s['env']={'HOME': '/tmp/lh'}; json.dump(s, open('/tmp/sm.json','w'))\""
  job drive "cd /home/sandbox && python3 /media/drive.py /tmp/sm.json /tmp/out-m"
else
  job drive "cd /home/sandbox && python3 /media/drive.py /media/session-m.json /tmp/out-m"
fi
# the directly blocked append goes to an existing file (a held CREATE would hold the mount's lock: GATE.md)
x sh -c "echo start > ~/.claude/blocked.jsonl; echo probe > ~/.claude/keeper-probe.txt; cat ~/.claude/keeper-probe.txt >/dev/null"
sleep 2.5
if [ "$hold" != 0 ]; then
  [ "$hold" = 1 ] && r touch /run/magpie-ctl/hold
  [ "$hold" = 3 ] && job dirty "python3 /media/stale.py --dirty ${DIRTY_MIB:-125}"
  job blocked "python3 /media/killtest.py append-existing /home/sandbox/.claude/blocked.jsonl 3 1048576 > /tmp/blocked.json"
fi
sleep 8
r sh -c "grep -c HOLDING /srv/magpie/calls.log; grep HOLDING /srv/magpie/calls.log | tail -3" > "$out/held-before.txt"
if [ "$layout" = scratch ]; then
  "${SSH[@]}" "cd /root/gate && RESTORE_DEBUG=${RESTORE_DEBUG:-} GATE_RUNSC_EXTRA='${GATE_RUNSC_EXTRA:-}' ./gate.sh receive-relay $name 7000" > "$out/receive.json" &
else
  "${SSH[@]}" "cd /root/gate && RESTORE_DEBUG=${RESTORE_DEBUG:-} GATE_RUNSC_EXTRA='${GATE_RUNSC_EXTRA:-}' ./gate.sh receive $name 7000" > "$out/receive.json" &
fi
rj=$!
for _ in $(seq 100); do "${SSH[@]}" "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
t0=$(ms)
if [ "$layout" = scratch ]; then
  ./gate.sh send-scratch "$name" "$dst" 7000 > "$out/send.json" 2> "$out/send.txt"
else
  ./gate.sh send "$name" "$dst" 7000 > "$out/send.txt" 2>&1
fi
t1=$(ms)
wait $rj
t2=$(ms)
./gate.sh rm "$name"
y() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name $*"; }
yr() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root $*"; }
yr sh -c "'grep -c HOLDING /srv/magpie/calls.log'" > "$out/held-after-move.txt"
# a probe on the target while the hold is still on: how long a stat and a cached read of other files wait
yj() { "${SSH[@]}" "cd /root/gate && ./gate.sh exec $name --root sh -c \"echo '$2' > /run/jobs/$1.tmp && mv /run/jobs/$1.tmp /run/jobs/$1.sh\""; }
[ "$hold" != 0 ] && yj probe 'python3 /media/probe.py > /tmp/probe.json'
sleep 3
yr sh -c "'ps -eo pid,stat,pcpu,comm; grep -c HOLDING /srv/magpie/calls.log; date +%s.%N'" >> "$out/held-after-move.txt"
t3=$(ms)
yr rm -f /run/magpie-ctl/hold
yr sh -c "'for i in \$(seq 1200); do [ -f /tmp/out-m/summary.json ] && { [ $hold = 0 ] || [ -f /tmp/blocked.json ]; } && break; sleep 0.1; done'"
t4=$(ms)
y cat /tmp/out-m/summary.json > "$out/drive.json" 2>/dev/null || fail+=("no driver summary within 120 s of the release")
y cat /tmp/out-m/stdout.jsonl > "$out/drive-stdout.jsonl" 2>/dev/null
y cat /tmp/out-m/stderr.log > "$out/drive-stderr.log" 2>/dev/null
if [ "$hold" != 0 ]; then
  y cat /tmp/blocked.json > "$out/blocked.json" 2>/dev/null || fail+=("the blocked append never returned")
  y cat /tmp/probe.json > "$out/probe.json"
fi
yr cat /srv/magpie/calls.log > "$out/calls.log"
yr cat /srv/supervisor/ops.tsv > "$out/ops.tsv"
yr python3 /media/report.py /home/sandbox/.claude /srv/magpie/d /srv/supervisor/ops.tsv > "$out/report.json"
yr sh -c "'cat /srv/log/supervisor.log'" > "$out/supervisor.log"
yr sh -c "'cat /srv/log/exits.log 2>/dev/null'" > "$out/exits.log"
yr sh -c "'cat /srv/log/model.log /srv/log/magpie.log /srv/log/keeper.log'" > "$out/fakes.log"
yr sh -c "'cat /srv/model/chunks.log'" > "$out/model-chunks.log"
yr sh -c "'for f in /run/jobs/*.out; do echo == \$f; cat \$f; done'" > "$out/jobs.out"
cp "/var/log/gate/$name.log" "$out/runsc-source.log"
"${SSH[@]}" "cat /var/log/gate/$name.log" > "$out/runsc-target.log"
"${SSH[@]}" "cat /var/log/gate/$name.restore.out" > "$out/runsc-restore.out"
[ "$layout" = scratch ] && "${SSH[@]}" "cat /var/log/gate/$name.relay.log /var/log/gate/$name.rsync.log 2>/dev/null" > "$out/relay.log"
if [ -n "${RESTORE_DEBUG:-}" ]; then
  "${SSH[@]}" "grep -h -i -E 'unhandled user fault|SIGSEGV|signal 11|fatal|panic|exited with' /var/log/gate/dbg-$name/*boot* 2>/dev/null | head -n 200" > "$out/debug-faults.txt"
  "${SSH[@]}" "du -sh /var/log/gate/dbg-$name" >> "$out/debug-faults.txt"
fi
echo "{\"layout\":\"$layout\",\"runsc_extra\":\"${GATE_RUNSC_EXTRA:-}\",\"hold\":$hold,\"mode\":\"$mode\",\"send_ms\":$((t1-t0)),\"total_ms\":$((t2-t0)),\"release_to_turn_done_ms\":$((t4-t3)),\"migrate_start_us\":${t0}000}" > "$out/times.json"
# checks
if [ -s "$out/drive.json" ]; then
  jq -e '(.turns | length) == 2 and all(.turns[]; .result == "success")' "$out/drive.json" >/dev/null || fail+=("a turn did not end success: $(jq -c '[.turns[].result]' "$out/drive.json")")
fi
[ -s "$out/blocked.json" ] && { jq -e .exact "$out/blocked.json" >/dev/null || fail+=("the blocked append is not exact"); }
[ "$mode" = local ] || jq -e '[.files[] | select(.path|endswith(".jsonl")) | .equal_to_magpie] | all' "$out/report.json" >/dev/null || fail+=("a transcript differs from magpie's copy")
python3 ./uniques.py "$out/ops.tsv" "${t0}000" > "$out/uniques.json" || fail+=("unique check failed: $(cat "$out/uniques.json")")
[ "$layout" = scratch ] && { d=$(jq -r '.files_differing_after_final // ""' "$out/send.json" 2>/dev/null); [ -n "$d" ] && fail+=("scratch files differ from the source after the final delta: $d"); }
[ -s "$out/exits.log" ] && fail+=("an in-sandbox process exited: $(tr '\n' ';' < "$out/exits.log")")
# a thread that panicked or a request read incompletely does not end a process; the supervisor's log says so
grep -E 'panicked|short read|exited status' "$out/supervisor.log" > "$out/supervisor-faults.txt" && fail+=("the supervisor logged a fault: $(head -n 3 "$out/supervisor-faults.txt" | tr '\n' ';')")
# Claude Code's own exit (the driver records it; a signal is a negative number)
[ -s "$out/drive.json" ] && { jq -e '.exit == 0' "$out/drive.json" >/dev/null || fail+=("Claude Code exited $(jq -c .exit "$out/drive.json")"); }
printf '%s\n' "${fail[@]}" > "$out/failures.txt"
cat "$out/times.json" "$out/receive.json"; echo; [ -s "$out/send.json" ] && cat "$out/send.json"
jq -c '{turns: [.turns[].result], exit}' "$out/drive.json" 2>/dev/null; jq -c '{exact, writes}' "$out/blocked.json" 2>/dev/null; cat "$out/probe.json"
grep -P '\tINTERRUPT\t' "$out/ops.tsv" | cut -f2,5
jq -c '{uniques, replayed_lines, repeat_count, boundaries}' "$out/uniques.json"
"${SSH[@]}" "cd /root/gate && ./gate.sh rm $name; rm -rf /run/gate/ckpt/in-$name"
if [ -s "$out/failures.txt" ] && [ "$(wc -c < "$out/failures.txt")" -gt 1 ]; then echo "FAIL: $(tr '\n' ';' < "$out/failures.txt")"; exit 1; fi
echo PASS
