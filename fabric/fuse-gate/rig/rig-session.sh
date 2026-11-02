#!/usr/bin/env bash
# Supervisor 22 items 2 and 9 on the real build: Claude Code 2.1.304 launched by the real supervisor (argv,
# environment and managed files of 6.1-6.4; the turns come as magpie's start_turn commands, rigsession.py) with its
# transcript on the real mount: session a (tools, a foreground subagent, an MCP call through the gateway, five
# failure-injected turns, growth past 5 MiB, /compact with CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1), then two torn tails,
# each followed by a resume in a new process (Claude Code killed while idle, the transcript cut, a start_turn with
# resume): (b) the final byte cut off, (b2) a line cut 100 bytes before its end. The tombstone trigger runs in a
# sandbox of its own (rig-tomb.sh: one conversation per sandbox). Results: /root/out/rsess-NAME/. STRACE=1: runsc --strace
# for the syscall inventory (item 9; strace-inventory.py over /var/log/gate/strace-NAME/).
# Usage: rig-session.sh NAME   (LAYOUT=scratch by default, as in production)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; out=/root/out/rsess-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; mkdir -p /var/log/gate; : > "/var/log/gate/$name.log"
# the stdout tap (cc-tap) records Claude Code's stream-json, except in the strace run (no extra processes there)
tap=1; [ -n "${STRACE:-}" ] && tap=
RIG=1 RIG_CC_TAP=$tap LAYOUT=${LAYOUT:-scratch} ./gate.sh bundle "$name"
if [ -n "${STRACE:-}" ]; then
  rm -rf "/var/log/gate/strace-$name"; mkdir -p "/var/log/gate/strace-$name"
  ./gate.sh run "$name" --strace --debug --debug-log="/var/log/gate/strace-$name/"
else
  ./gate.sh run "$name"
fi
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
r python3 /media/rig/rigctl.py wait-mount 60 || { echo "FAIL: no mount"; exit 1; }
cid=$(r cat /run/rig/cid)
r sh -c "mkdir -p /tmp/s && python3 /media/rig/mksessions.py /tmp/s"
tr=/home/sandbox/.claude/projects/-workspace/$cid.jsonl
# Claude Code killed while idle (a crash): the next start_turn spawns a new process with --resume
kill_cc() { r python3 /media/rig/rigctl.py kill-cc >> "$out/kill-cc.log"; sleep 1; }
t0=$(date +%s%N)
r python3 /media/rig/rigsession.py /tmp/s/a.json /tmp/out-a > "$out/a.summary.json"
t1=$(date +%s%N)
r sh -c "ls -la /etc/claude-code; cat /etc/claude-code/managed-settings.json /etc/claude-code/managed-mcp.json; echo; cat /etc/claude-code/CLAUDE.md" > "$out/managed.txt" 2>&1
r sh -c "for p in /proc/[0-9]*; do c=\$(tr '\0' ' ' < \$p/cmdline 2>/dev/null); case \"\$c\" in *claude*) echo \"\${p#/proc/} uid=\$(awk '/^Uid/{print \$2}' \$p/status) cwd=\$(readlink \$p/cwd): \$c\"; tr '\0' '\n' < \$p/environ | sort | sed 's/sbx:[^@]*@/sbx:<token>@/';; esac; done" > "$out/claude-process.txt"
kill_cc
# (b) the last byte off: a complete JSON line without its newline
torn=$(x sh -c "s=\$(stat -c %s $tr) && truncate -s \$((s-1)) $tr && echo \$((s-1))")
echo "{\"cut\":\"final byte\",\"torn_at\":$torn}" > "$out/torn-b.json"
r python3 /media/rig/rigsession.py /tmp/s/b.json /tmp/out-b > "$out/b.summary.json"
x python3 -c "f=open('$tr','rb'); f.seek($torn); print(repr(f.read(1)))" > "$out/torn-b.byte-at-cut.txt"
kill_cc
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
r python3 /media/rig/rigsession.py /tmp/s/b2.json /tmp/out-b2 > "$out/b2.summary.json"
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
echo "{\"session_a_ms\":$(( (t1-t0)/1000000 )),\"resumes_ms\":$(( (t2-t1)/1000000 )),\"conversation\":\"$cid\"}" > "$out/times.json"
r python3 /media/rig/rigreport.py > "$out/report.json"
for route in cc_write cc_rename cc_truncate cc_setattr cc_unlink cc_mkdir turn_state; do
  r python3 /media/rig/rigctl.py get "/calls/$route" > "$out/calls-$route.json"
done
r sh -c "find /home/sandbox/.claude -printf '%y %m %s %p\n' | sort -k4" > "$out/tree.txt"
r sh -c "cat /srv/log/supervisor.log" > "$out/supervisor.log"
r sh -c "cat /srv/model/chunks.log" > "$out/model.log"
r sh -c "cat /srv/log/exits.log 2>/dev/null" > "$out/exits.txt"
r sh -c "cat /tmp/cc-tap/stdout.jsonl 2>/dev/null" > "$out/cc-stdout.jsonl"
r tar -C /home/sandbox -cf - .claude/projects 2>/dev/null | tar -C "$out" -xf -
jq -c '[.turns[] | [.state, .error.code // null]]' "$out"/a.summary.json "$out"/b.summary.json "$out"/b2.summary.json
echo "done: $out"
./gate.sh rm "$name"
