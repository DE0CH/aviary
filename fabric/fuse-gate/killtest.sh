#!/usr/bin/env bash
# Gate item 4: SIGKILL the supervisor at each point of the request journal (read returned, magpie call sent, magpie
# answered, reply written) during a 1 MiB WRITE and during a READ; the keeper holds /dev/fuse meanwhile.
# Usage: killtest.sh NAME [JOURNAL]   (JOURNAL /mnt/fuse-journal = the host-backed scratch dir instead of tmpfs)
set -euo pipefail
cd "$(dirname "$0")"
name=$1; journal=${2:-/srv/supervisor/fuse-journal}; out=/root/out/kill-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name" "$journal"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
spid() { r sh -c "for p in /proc/[0-9]*; do c=\$(tr '\\0' ' ' < \$p/cmdline 2>/dev/null); case \"\$c\" in */media/fuse-gate\ serve*) echo \${p#/proc/};; esac; done"; }
restart_with() {  # $1 = die-at spec or "": kill -9 the running supervisor; the loop restarts it (with the flag)
  [ -n "$1" ] && r sh -c "echo $1 > /run/gate-die-at"
  local p; p=$(spid); r kill -9 "$p"
  for _ in $(seq 50); do n=$(spid); [ -n "$n" ] && [ "$n" != "$p" ] && break; sleep 0.1; done
  [ -z "$1" ] || for _ in $(seq 50); do r test -f /run/gate-die-at || break; sleep 0.1; done
}
# keeper-only hold: the supervisor is SIGSTOPped... no: killed, and the mount must still answer after the restart
x sh -c "echo before > ~/.claude/keeper.txt"
p=$(spid); r kill -9 "$p"; sleep 0.5
x sh -c "cat ~/.claude/keeper.txt; echo after >> ~/.claude/keeper.txt; cat ~/.claude/keeper.txt" > "$out/keeper.txt"
# a 4 MiB pattern file for the READ cases
x python3 -c "
import os
fd=os.open('/home/sandbox/.claude/read.bin', os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o600)
for i in range(4): os.write(fd, bytes([97+i])*(1<<20))
os.close(fd)"
for point in read sent answered replied; do
  restart_with "$point:WRITE:2"
  x python3 /media/killtest.py write "/home/sandbox/.claude/w-$point.jsonl" 3 1048576 > "$out/write-$point.json"
  # READ: the cache is dropped first (so READ makes a magpie call: `sent`/`answered` happen)
  r sh -c "rm -f /srv/supervisor/cache/*"
  # the first READ after the drop fetches the whole file (sent/answered happen there); later READs are cache hits
  n=2; case $point in sent|answered) n=1 ;; esac
  restart_with "$point:READ:$n"
  x python3 /media/killtest.py read /home/sandbox/.claude/read.bin 1048576 > "$out/read-$point.json"
done
r cat /srv/log/supervisor.log > "$out/supervisor.log"
r cat /srv/supervisor/ops.tsv > "$out/ops.tsv"
r cat /srv/magpie/calls.log > "$out/calls.log"
for f in "$out"/write-*.json "$out"/read-*.json; do echo "$(basename "$f"): $(jq -c '{exact, slow: [(.writes // .reads)[] | select(.ms > 50)]}' "$f")"; done
grep -c '^fault-injection' "$out/supervisor.log" | sed 's/^/kills: /'
grep '^replayed' "$out/supervisor.log" | sed 's/^/  /'
cut -f8 "$out/ops.tsv" | sort | uniq -d | grep -c . | sed 's/^/unique values seen more than once in ops.tsv (replays): /'
cat "$out/keeper.txt"
