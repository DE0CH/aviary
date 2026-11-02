#!/usr/bin/env bash
# Gate items 2 and 9: the scripted Claude Code session (mkscripts.py) in a fresh sandbox, then two torn tails, each
# followed by a resume: (b) the final byte (the last line's "\n") cut off, (b2) a line cut 100 bytes before its end,
# leaving a partial JSON object (a crash mid-write). For each resume the FUSE ops on the transcript are recorded
# (`torn-*.ops.tsv`). Results under /root/out/NAME. MODE fuse (~/.claude on the supervisor's mount) or local (HOME on
# tmpfs, the reference). STRACE=1: the sandbox runs with runsc --strace (syscall inventory).
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
r() { ./gate.sh exec "$name" --root "$@"; }
home=/home/sandbox
if [ "$mode" = local ]; then home=/tmp/lh; x mkdir -p $home; fi
sid=11111111-2222-4333-8444-555555555555
x sh -c "mkdir -p /tmp/s && python3 /media/mkscripts.py /tmp/s"
if [ "$mode" = local ]; then
  x sh -c "cd /tmp/s && for f in session-*.json; do python3 -c \"import json,sys; s=json.load(open('\$f')); s.setdefault('env',{})['HOME']='$home'; json.dump(s,open('\$f','w'))\"; done"
fi
# the relocation's destination is occupied (Claude Code sets the occupant aside as <sid>.jsonl.superseded-<ms>)
x sh -c "mkdir -p /home/sandbox/work $home/.claude/projects/-home-sandbox-work && echo '{\"occupant\":true}' > $home/.claude/projects/-home-sandbox-work/$sid.jsonl"
oplines() { if [ "$mode" = fuse ]; then r sh -c "wc -l < /srv/supervisor/ops.tsv"; else echo 0; fi; }
opsince() {  # $1 = line count before: the FUSE ops on the transcript since then
  [ "$mode" = fuse ] || return 0
  r sh -c "tail -n +$(( $1 + 1 )) /srv/supervisor/ops.tsv | grep -P '\tprojects/-home-sandbox-work/$sid\.jsonl\t' | grep -v -P '\t(LOOKUP|GETATTR)\t'"
}
t0=$(date +%s%N)
x python3 /media/drive.py /tmp/s/session-a.json /tmp/out-a > "$out/a.summary.json"
t1=$(date +%s%N)
tr=$home/.claude/projects/-home-sandbox-work/$sid.jsonl
x python3 /media/tombstone.py "$tr" > "$out/tombstone-replay.json"
# (b) the last byte off: a complete JSON line without its newline
torn=$(x sh -c "s=\$(stat -c %s $tr) && truncate -s \$((s-1)) $tr && echo \$((s-1))")
echo "{\"cut\":\"final byte\",\"torn_at\":$torn}" > "$out/torn-b.json"
n=$(oplines)
x python3 /media/drive.py /tmp/s/session-b.json /tmp/out-b > "$out/b.summary.json"
opsince "$n" > "$out/torn-b.ops.tsv"
x python3 -c "f=open('$tr','rb'); f.seek($torn); print(repr(f.read(1)))" > "$out/torn-b.byte-at-cut.txt"
# (b2) a line torn mid-way: the file ends 100 bytes before the end of its last line of at least 2 KiB
x python3 - "$tr" > "$out/torn-b2.json" <<'PY'
import json, os, sys
p = sys.argv[1]
data = open(p, "rb").read()
end, cut, line = len(data), None, None
while end > 0:
    start = data.rfind(b"\n", 0, end - 1) + 1
    if end - start >= 2048:
        cut, line = end - 1 - 100, data[start:end]
        break
    end = start
os.truncate(p, cut)
print(json.dumps({"cut": "mid-line", "size_before": len(data), "torn_at": cut, "line_start": start,
                  "line_len": len(line), "tail_after_cut": data[cut - 60:cut].decode(errors="replace")}))
PY
cut2=$(jq .torn_at "$out/torn-b2.json")
n=$(oplines)
x python3 /media/drive.py /tmp/s/session-b2.json /tmp/out-b2 > "$out/b2.summary.json"
opsince "$n" > "$out/torn-b2.ops.tsv"
x python3 - "$tr" "$cut2" > "$out/torn-b2.after.json" <<'PY'
import json, sys
p, cut = sys.argv[1], int(sys.argv[2])
data = open(p, "rb").read()
bad = []
for i, l in enumerate(data.split(b"\n")):
    if l.strip():
        try:
            json.loads(l)
        except Exception:
            bad.append({"line": i, "len": len(l), "head": l[:80].decode(errors="replace")})
print(json.dumps({"size": len(data), "bytes_from_cut": data[cut:cut + 120].decode(errors="replace"),
                  "unparseable_lines": bad}))
PY
t2=$(date +%s%N)
echo "{\"session_a_ms\":$(( (t1-t0)/1000000 )),\"resumes_ms\":$(( (t2-t1)/1000000 )),\"sid\":\"$sid\"}" > "$out/times.json"
if [ "$mode" = fuse ]; then
  r python3 /media/report.py $home/.claude /srv/magpie/d /srv/supervisor/ops.tsv > "$out/report.json"
fi
r sh -c "find $home/.claude -printf '%y %m %s %p\n' | sort -k4" > "$out/tree.txt"
r tar -C / -cf - tmp/out-a tmp/out-b tmp/out-b2 srv/supervisor/ops.tsv srv/magpie/calls.log srv/model/chunks.log srv/log 2>/dev/null | tar -C "$out" -xf -
r tar -C $home -cf - .claude/projects 2>/dev/null | tar -C "$out" -xf -
jq -c '[.turns[] | [.result, .is_error]]' "$out"/a.summary.json "$out"/b.summary.json "$out"/b2.summary.json
echo "done: $out"
