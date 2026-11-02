#!/usr/bin/env bash
# Supervisor spec 22 item 6: Claude Code resumes an 87 MiB transcript from a cold cache (the supervisor restarted with
# its content cache emptied, so the first READ fetches the whole file from the fake magpie). A short real session
# first, its transcript grown to 87 MiB with valid chained lines (grow87.py), then `claude --resume` with one turn.
# Usage: resume87.sh NAME
set -euo pipefail
cd "$(dirname "$0")"
name=$1; out=/root/out/r87-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
spid() { r sh -c "for p in /proc/[0-9]*; do c=\$(tr '\\0' ' ' < \$p/cmdline 2>/dev/null); case \"\$c\" in */media/fuse-gate\ serve*) echo \${p#/proc/};; esac; done"; }
sid=22222222-3333-4444-8555-666666666666
x sh -c "cat > /tmp/r0.json" <<J
{"args": ["--session-id", "$sid"], "turns": ["Seed.\nGATE: tool Bash {\"command\":\"echo seed\",\"description\":\"seed\"}\nGATE: text 3 0"]}
J
x python3 /media/drive.py /tmp/r0.json /tmp/out-r0 > "$out/seed.summary.json"
tr=/home/sandbox/.claude/projects/-home-sandbox/$sid.jsonl
x python3 /media/grow87.py "$tr" 87 > "$out/grow.json"
for run in cold warm; do
  if [ $run = cold ]; then
    r sh -c "rm -f /srv/supervisor/cache/*"; p=$(spid); r kill -9 "$p"; sleep 1
    r sh -c "grep 'supervisor up' /srv/log/supervisor.log | tail -n 1" > "$out/cold-supervisor.txt"
  fi
  x sh -c "cat > /tmp/r-$run.json" <<J
{"resume": "$sid", "turns": ["Resumed ($run cache).\nGATE: text 3 0"]}
J
  x python3 /media/drive.py /tmp/r-$run.json /tmp/out-r-$run > "$out/$run.summary.json"
done
r sh -c "grep -P '\tREAD\t' /srv/supervisor/ops.tsv | grep '$sid.jsonl' | awk -F'\t' '{n++; b+=0} END{print n\" READs of the transcript\"}'" > "$out/reads.txt"
r cat /srv/magpie/calls.log | grep "$sid.jsonl" | grep -v '/write' > "$out/magpie-reads.log" || true
for run in cold warm; do
  jq -c --arg run $run '{run: $run, total_ms, turns: [.turns[] | {result, turn_ms: ((.end_us - .start_us)/1000|floor)}]}' "$out/$run.summary.json"
done
cat "$out/grow.json" "$out/cold-supervisor.txt" "$out/reads.txt"
