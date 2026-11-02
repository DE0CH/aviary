#!/usr/bin/env bash
# Gate item 1: /dev/fuse in a running sandbox, the supervisor's mount (root in the sandbox), uid 1000 I/O on it.
# Usage: mount-check.sh NAME   -> /root/out/mount-NAME/
set -euo pipefail
cd "$(dirname "$0")"
name=$1; out=/root/out/mount-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
{
  echo "host: $(uname -r) $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2) $(runsc --version | head -1)"
  echo "dev: $(r ls -l /dev/fuse)"
  echo "mount: $(r grep ' /home/sandbox/.claude ' /proc/mounts)"
  echo "supervisor: $(r sh -c 'grep -E "^mounted|supervisor up" /srv/log/supervisor.log | head -n 2' | tr '\n' ' ')"
  echo "INIT: $(r sh -c "grep -P '\tINIT\t' /srv/supervisor/ops.tsv | cut -f5")"
  echo "id: $(x id)"
  x sh -c 'mkdir -p ~/.claude/d1/d2 && printf "hello\n" > ~/.claude/d1/d2/f.txt && printf "more\n" >> ~/.claude/d1/d2/f.txt && cat ~/.claude/d1/d2/f.txt | tr "\n" "|" && echo && ls -la ~/.claude/d1/d2 && stat -c "%U:%G %u:%g %a %s %i %n" ~/.claude/d1 ~/.claude/d1/d2 ~/.claude/d1/d2/f.txt'
  echo "magpie copy: $(r cat /srv/magpie/d/d1/d2/f.txt | tr '\n' '|')"
} 2>&1 | cat > "$out/item1.txt"   # through a pipe: runsc exec reopens a file stdout at offset 0
cat "$out/item1.txt"
./gate.sh rm "$name"
