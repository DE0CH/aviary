#!/usr/bin/env bash
# Is a FUSE request resent only on a checkpoint? A held 1 MiB append is interrupted by a signal (SA_RESTART, then
# without it): the FUSE ops (INTERRUPT, a second WRITE with a new unique) and magpie's calls are recorded.
# Usage: sigresend.sh NAME
set -euo pipefail
cd "$(dirname "$0")"
name=$1; out=/root/out/sig-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
job() { r sh -c "echo '$2' > /run/jobs/$1.tmp && mv /run/jobs/$1.tmp /run/jobs/$1.sh"; }
for mode in restart eintr; do
  f=/home/sandbox/.claude/sig-$mode.jsonl
  x sh -c "echo start > $f"
  r touch /run/magpie-ctl/hold
  job "w$mode" "python3 /media/sigwrite.py $f $mode > /tmp/sig-$mode.json 2>&1"
  sleep 2
  r sh -c "kill -USR1 \$(cat /tmp/sigwrite.pid)"
  sleep 2
  r rm /run/magpie-ctl/hold
  for _ in $(seq 100); do r test -s /tmp/sig-$mode.json && break; sleep 0.1; done
  r cat /tmp/sig-$mode.json > "$out/$mode.json"
  r sh -c "grep -P '\\tsig-$mode.jsonl\\t|\\tINTERRUPT\\t' /srv/supervisor/ops.tsv | grep -P '\\t(WRITE|INTERRUPT)\\t'" > "$out/$mode.ops.tsv"
  r sh -c "grep 'sig-$mode.jsonl' /srv/magpie/calls.log | grep -v HOLDING" > "$out/$mode.calls.log"
  r rm -f /tmp/sigwrite.pid
  echo "$mode: $(cat "$out/$mode.json")"; cut -f2,5,8 "$out/$mode.ops.tsv"; cut -d' ' -f2-5 "$out/$mode.calls.log"
done
./gate.sh rm "$name"
