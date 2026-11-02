#!/usr/bin/env bash
# Supervisor 22 items 1 and 5 on the real build (RIG=1): /dev/fuse in a running sandbox, the real supervisor's mount
# (root in the sandbox, started by the real keeper as PID 1), INIT 7.31, uid 1000 I/O on both scopes and the result in
# magpie (item1.txt); flock and fcntl locks between two uid 1000 processes on a magpie-scope file, a local-scope file
# and /tmp (locks.txt).   Usage: rig-mount.sh NAME   -> /root/out/rmount-NAME/
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; out=/root/out/rmount-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; RIG=1 LAYOUT=${LAYOUT:-scratch} ./gate.sh bundle "$name"; ./gate.sh run "$name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
r python3 /media/rig/rigctl.py wait-mount 30
{
  echo "host: $(uname -r) $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2) $(runsc --version | head -1)"
  echo "pid 1: $(r cat /proc/1/cmdline | tr '\0' ' ')"
  echo "dev: $(r ls -l /dev/fuse)"
  echo "mount: $(r grep ' /home/sandbox/.claude ' /proc/mounts)"
  echo "supervisor: $(r sh -c 'grep -E "\"(mounted ~/.claude|fuse connection|fuse mounted)\"" /srv/log/supervisor.log' | tr '\n' ' ')"
  echo "id: $(x id)"
  x sh -c 'mkdir -p ~/.claude/projects/d1/d2 && printf "hello\n" > ~/.claude/projects/d1/d2/f.txt && printf "more\n" >> ~/.claude/projects/d1/d2/f.txt && cat ~/.claude/projects/d1/d2/f.txt | tr "\n" "|" && echo && ls -la ~/.claude/projects/d1/d2 && stat -c "%U:%G %u:%g %a %s %i %n" ~/.claude/projects/d1 ~/.claude/projects/d1/d2 ~/.claude/projects/d1/d2/f.txt'
  echo "magpie copy: $(r python3 /media/rig/rigctl.py get '/data?path=projects/d1/d2/f.txt' | tr '\n' '|') $(r python3 /media/rig/rigctl.py get '/node?path=projects/d1/d2/f.txt')"
  x sh -c 'mkdir -p ~/.claude/sessions && printf "local\n" > ~/.claude/sessions/s.txt && cat ~/.claude/sessions/s.txt && ls -la ~/.claude'
  echo "local scope in magpie: $(r python3 /media/rig/rigctl.py get '/node?path=sessions/s.txt' || echo absent)"
  echo "local backing file: $(r cat /var/lib/supervisor/local/sessions/s.txt)"
  echo "refused: $(x sh -c 'printf x > ~/.claude/settings.json' 2>&1 | tr '\n' ' ') (exit $?)"
} 2>&1 | cat > "$out/item1.txt"
cat "$out/item1.txt"
{
  echo "magpie scope: $(x python3 /media/rig/locks.py /home/sandbox/.claude/projects/lockfile)"
  echo "local scope:  $(x python3 /media/rig/locks.py /home/sandbox/.claude/sessions/lockfile)"
  echo "tmp:          $(x python3 /media/rig/locks.py /tmp/lockfile)"
} 2>&1 | cat > "$out/locks.txt"
cat "$out/locks.txt"
r cat /srv/log/supervisor.log > "$out/supervisor.log"
./gate.sh rm "$name"
