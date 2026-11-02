#!/usr/bin/env bash
# Supervisor 22 items 6 and 10 on the real build, in one sandbox (fake magpie on loopback inside it; the cluster path
# is not in this rig): append throughput through the real mount (bench.py: 1,000 x 4 KiB and 50 x 1 MiB, one
# O_APPEND write per line, as Claude Code) at 0, 5 and 20 ms injected per cc write; then Claude Code launched by the
# real supervisor runs c.json (Read, Glob, 40 chunks 20 ms apart, five turns) at 0, 50 and 500 ms per cc write and
# once with every cc write held for 30 s from 1 s in (msgs.py over the cc-tap stdout log); then the transcript grown
# to 87 MiB (grow87.py) and resumed by a new Claude Code process, first from a cold cache (the supervisor killed and
# its content cache emptied), then warm. Results: /root/out/rlat-NAME/.
# Usage: rig-latency.sh NAME   (LAYOUT=scratch by default)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; out=/root/out/rlat-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; mkdir -p /var/log/gate; : > "/var/log/gate/$name.log"
# fake magpie keeps every byte and every call body in memory (about 2 x the 400 MiB written here), inside the
# sandbox, where the supervisor's memory guard counts it against the descriptor's memoryBytes
RIG=1 RIG_CC_TAP=1 GATE_MEM=$((12 << 30)) RIG_SANDBOX_MEMORY=$((10 << 30)) LAYOUT=${LAYOUT:-scratch} ./gate.sh bundle "$name"; ./gate.sh run "$name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
ctl() { r python3 /media/rig/rigctl.py "$@"; }
lat() { ctl post "/latency/cc_write?ms=$1" > /dev/null; }
now_us() { echo $(( $(date +%s%N) / 1000 )); }
kill_cc() { ctl kill-cc >> "$out/kill-cc.log"; sleep 1; }
ctl wait-mount 60 || { echo "FAIL: no mount"; exit 1; }
cid=$(r cat /run/rig/cid); tr=/home/sandbox/.claude/projects/-workspace/$cid.jsonl
r sh -c "mkdir -p /tmp/s && python3 /media/rig/mksessions.py /tmp/s"
for ms in 0 5 20; do
  lat "$ms"
  x python3 /media/bench.py "/home/sandbox/.claude/projects/bench-$ms" 1000 4096 > "$out/bench-4k-$ms.json"
  x python3 /media/bench.py "/home/sandbox/.claude/projects/bench-$ms" 50 1048576 > "$out/bench-1m-$ms.json"
done
lat 0
x sh -c "mkdir -p /workspace/work && printf 'line1\nline two\n' > /workspace/work/b.txt"
r python3 /media/rig/rigsession.py /tmp/s/r0.json /tmp/out-r0 > "$out/seed.summary.json"
for ms in 0 50 500; do
  lat "$ms"; s=$(now_us)
  r python3 /media/rig/rigsession.py /tmp/s/c.json "/tmp/c-$ms" > "$out/c-$ms.summary.json"
  r sh -c "python3 /media/rig/msgs.py /srv/model/chunks.log /tmp/cc-tap/stdout.jsonl $s $(now_us)" > "$out/msgs-$ms.json"
done
lat 0
s=$(now_us)
( sleep 1; ctl post /hold/cc_write > /dev/null; sleep 30; ctl post /release > /dev/null ) &
r python3 /media/rig/rigsession.py /tmp/s/c.json /tmp/c-hold > "$out/c-hold.summary.json"
wait
r sh -c "python3 /media/rig/msgs.py /srv/model/chunks.log /tmp/cc-tap/stdout.jsonl $s $(now_us)" > "$out/msgs-hold.json"
# the 87 MiB resume
kill_cc
x python3 /media/grow87.py "$tr" 87 > "$out/grow.json"
for run in cold warm; do
  kill_cc
  if [ $run = cold ]; then
    r sh -c "rm -rf /var/lib/supervisor/cache/*"; ctl kill-supervisor > /dev/null; sleep 0.5
    ctl wait-mount 60
    r sh -c "grep -c 'fuse connection' /srv/log/supervisor.log" > "$out/cold-supervisor.txt"
  fi
  r python3 /media/rig/rigsession.py /tmp/s/r.json "/tmp/r-$run" > "$out/r87-$run.summary.json"
done
r sh -c "grep -H VmHWM /proc/[0-9]*/status; cat /srv/log/exits.log 2>/dev/null" > "$out/memory.txt" 2>/dev/null
r sh -c "cat /srv/log/supervisor.log" > "$out/supervisor.log"
r sh -c "cat /tmp/cc-tap/stdout.jsonl" > "$out/cc-stdout.jsonl"
r sh -c "cat /srv/model/chunks.log" > "$out/model.log"
r python3 /media/rig/rigctl.py get /calls/turn_state > "$out/calls-turn_state.json"
# what the cold resume fetched from magpie (the cache was empty): every cc_file call, with its range
r python3 /media/rig/rigctl.py get /calls/cc_file > "$out/calls-cc_file.json"
for f in "$out"/bench-*.json; do echo "$(basename "$f") $(jq -c '{n, size, MBps, p50_ms, p99_ms, max_ms}' "$f")"; done
for v in 0 50 500 hold; do echo "c-$v $(jq -c '[.turns[] | [.state, ((.end_us-.start_us)/1000|floor)]]' "$out/c-$v.summary.json") $(cat "$out/msgs-$v.json")"; done
for run in cold warm; do echo "r87-$run $(jq -c '[.turns[] | [.state, ((.end_us-.start_us)/1000|floor)]]' "$out/r87-$run.summary.json")"; done
cat "$out/grow.json"
echo "cc_file calls: $(jq -c '[.[] | {path: .query.path, offset: .query.offset, length: .query.length}]' "$out/calls-cc_file.json" | cut -c1-600)"
./gate.sh rm "$name"
