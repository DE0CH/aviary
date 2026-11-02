#!/usr/bin/env bash
# Gate items 6 and 10 in one sandbox: append throughput (1,000 x 4 KiB, 50 x 1 MiB) and a cold read of an 87 MiB
# file, each at injected magpie latencies; then the token-gap session (session-c: Read, Glob, 40 chunks 20 ms apart,
# five turns) at 0, 50 and 500 ms per mutating call, and once with every call held for 30 s mid-session.
# Usage: latency.sh NAME
set -euo pipefail
cd "$(dirname "$0")"
name=$1; out=/root/out/lat-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
lat() { r sh -c "echo $1 > /run/magpie-ctl/latency_ms"; }
spid() { r sh -c "for p in /proc/[0-9]*; do c=\$(tr '\\0' ' ' < \$p/cmdline 2>/dev/null); case \"\$c\" in */media/fuse-gate\ serve*) echo \${p#/proc/};; esac; done"; }
x sh -c "mkdir -p /tmp/s && python3 /media/mkscripts.py /tmp/s && mkdir -p /home/sandbox/work && printf 'line1\nline two\n' > /home/sandbox/work/b.txt"
for ms in 0 5 20; do
  lat $ms
  x python3 /media/bench.py /home/sandbox/.claude/bench-$ms 1000 4096 > "$out/bench-4k-$ms.json"
  x python3 /media/bench.py /home/sandbox/.claude/bench-$ms 50 1048576 > "$out/bench-1m-$ms.json"
done
lat 0
# cold read of an 87 MiB file: written through the mount, cache dropped, supervisor restarted
x python3 -c "
import os
fd=os.open('/home/sandbox/.claude/big87.jsonl', os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o600)
for i in range(87): os.write(fd, b'z'*((1<<20)-1)+b'\n')
os.close(fd)"
for warm in cold warm; do
  if [ $warm = cold ]; then r sh -c "rm -f /srv/supervisor/cache/*"; p=$(spid); r kill -9 "$p"; sleep 1; fi
  x python3 -c "
import time, json
t=time.time(); n=0
with open('/home/sandbox/.claude/big87.jsonl','rb') as f:
    while True:
        b=f.read(1<<20)
        if not b: break
        n+=len(b)
print(json.dumps({'$warm': True, 'bytes': n, 's': round(time.time()-t,3)}))" > "$out/read87-$warm.json"
done
for ms in 0 50 500; do
  lat $ms
  x python3 /media/drive.py /tmp/s/session-c.json /tmp/c-$ms > "$out/c-$ms.summary.json"
  r sh -c "python3 /media/gaps.py /srv/model/chunks.log /tmp/c-$ms/stdout.jsonl" > "$out/gaps-$ms.json"
done
lat 0
# a 30 s hold of every mutating call, starting 1 s into the session
( sleep 1; r touch /run/magpie-ctl/hold; sleep 30; r rm -f /run/magpie-ctl/hold ) &
x python3 /media/drive.py /tmp/s/session-c.json /tmp/c-hold > "$out/c-hold.summary.json"
wait
r sh -c "python3 /media/gaps.py /srv/model/chunks.log /tmp/c-hold/stdout.jsonl" > "$out/gaps-hold.json"
# the same hold with CLAUDE_CODE_EAGER_FLUSH=1: 2.1.304 then waits for its transcript drain before the result
# without the 10 s cap ("transcript drain still running after 10000ms; the result goes out without it")
x sh -c "python3 -c \"import json; s=json.load(open('/tmp/s/session-c.json')); s['env']={'CLAUDE_CODE_EAGER_FLUSH': '1'}; json.dump(s, open('/tmp/s/session-ce.json','w'))\""
( sleep 1; r touch /run/magpie-ctl/hold; sleep 30; r rm -f /run/magpie-ctl/hold ) &
x python3 /media/drive.py /tmp/s/session-ce.json /tmp/c-hold-eager > "$out/c-hold-eager.summary.json"
wait
# when each held transcript append landed (ops.tsv: start + duration) against each turn's result
r cat /srv/supervisor/ops.tsv > "$out/ops.tsv"
r cat /srv/magpie/calls.log > "$out/calls.log"
for v in hold hold-eager; do
  python3 - "$out/c-$v.summary.json" "$out/ops.tsv" > "$out/order-$v.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
w = []
for l in open(sys.argv[2]):
    f = l.rstrip("\n").split("\t")
    if len(f) >= 8 and f[1] == "WRITE" and f[3].startswith("projects/") and f[3].count("/") == 2 and f[3].endswith(".jsonl"):
        w.append((int(f[0]), int(f[0]) + int(f[6])))
t0 = s["turns"][0]["start_us"]
turns = []
for t in s["turns"]:
    during = [e for b, e in w if t["start_us"] <= b <= t["end_us"]]
    pend = [e for b, e in w if b <= t["end_us"] < e]
    turns.append({"start_s": round((t["start_us"] - t0) / 1e6, 3), "result_s": round((t["end_us"] - t0) / 1e6, 3),
                  "result": t["result"], "appends_started": len(during),
                  "appends_still_pending_at_result": len(pend),
                  "last_pending_landed_s": round((max(pend) - t0) / 1e6, 3) if pend else None})
print(json.dumps({"turns": turns}))
PY
done
for f in "$out"/bench-*.json "$out"/read87-*.json; do echo "$(basename "$f") $(cat "$f")"; done
for v in hold hold-eager; do echo "order-$v $(cat "$out/order-$v.json")"; done
for v in 0 50 500 hold; do echo "c-$v $(jq -c '{turns:[.turns[] | [.result, ((.end_us-.start_us)/1000|floor)]]}' "$out/c-$v.summary.json") $(cat "$out/gaps-$v.json")"; done
