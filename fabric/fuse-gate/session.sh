#!/usr/bin/env bash
# Gate items 2 and 9: the scripted Claude Code session (mkscripts.py) in a fresh sandbox, then a torn tail and a
# resume; results under /root/out/NAME. MODE fuse (~/.claude on the supervisor's mount) or local (HOME on tmpfs, the
# reference). STRACE=1: the sandbox runs with runsc --strace (syscall inventory).
# Usage: session.sh NAME [fuse|local]
set -euo pipefail
cd "$(dirname "$0")"
name=$1; mode=${2:-fuse}; out=/root/out/$name
rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"
./gate.sh bundle "$name"
if [ -n "${STRACE:-}" ]; then
  rm -rf "/var/log/gate/strace-$name"; mkdir -p "/var/log/gate/strace-$name"
  ./gate.sh run "$name" --strace --debug --debug-log="/var/log/gate/strace-$name/"
else
  ./gate.sh run "$name"
fi
sleep 1
x() { ./gate.sh exec "$name" "$@"; }
home=/home/sandbox
if [ "$mode" = local ]; then home=/tmp/lh; x mkdir -p $home; fi
x sh -c "mkdir -p /tmp/s && python3 /media/mkscripts.py /tmp/s"
if [ "$mode" = local ]; then
  x sh -c "cd /tmp/s && for f in session-*.json; do python3 -c \"import json,sys; s=json.load(open('\$f')); s.setdefault('env',{})['HOME']='$home'; json.dump(s,open('\$f','w'))\"; done"
fi
t0=$(date +%s%N)
x python3 /media/drive.py /tmp/s/session-a.json /tmp/out-a > "$out/a.summary.json"
t1=$(date +%s%N)
sid=$(jq -r .session_id "$out/a.summary.json")
tr=$(x sh -c "ls $home/.claude/projects/*/$sid.jsonl")
x python3 /media/tombstone.py "$tr" > "$out/tombstone.json"
torn=$(x sh -c "s=\$(stat -c %s $tr) && truncate -s \$((s-1)) $tr && echo \$((s-1))")
echo "{\"torn_at\":$torn}" > "$out/torn.json"
x sh -c "sed -i 's/@SID/$sid/' /tmp/s/session-b.json"
x python3 /media/drive.py /tmp/s/session-b.json /tmp/out-b > "$out/b.summary.json"
x python3 -c "import sys; f=open('$tr','rb'); f.seek($torn); print(repr(f.read(1)))" > "$out/byte-at-torn.txt"
x python3 /media/tombstone.py "$tr" > "$out/tombstone2.json" || true
t2=$(date +%s%N)
echo "{\"session_a_ms\":$(( (t1-t0)/1000000 )),\"session_b_ms\":$(( (t2-t1)/1000000 )),\"sid\":\"$sid\"}" > "$out/times.json"
if [ "$mode" = fuse ]; then
  ./gate.sh exec "$name" --root python3 /media/report.py $home/.claude /srv/magpie/d /srv/supervisor/ops.tsv > "$out/report.json"
fi
./gate.sh exec "$name" --root sh -c "find $home/.claude -printf '%y %m %s %p\n' | sort -k4" > "$out/tree.txt"
./gate.sh exec "$name" --root tar -C / -cf - tmp/out-a tmp/out-b srv/supervisor/ops.tsv srv/magpie/calls.log srv/model/chunks.log srv/log 2>/dev/null | tar -C "$out" -xf -
./gate.sh exec "$name" --root tar -C $home -cf - .claude/projects 2>/dev/null | tar -C "$out" -xf -
echo "done: $out"
