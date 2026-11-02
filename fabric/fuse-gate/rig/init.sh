#!/bin/sh
# The first process of a RIG=1 sandbox (gate.sh bundle): the gate's scripted mock model (`fuse-gate model`, chunk
# send times in /srv/model/chunks.log), fuse-rig's world (fake hermit, fake magpie and fake-tools behind the reference
# gateway, on loopback, Claude Code's model behind its `model` name), the job runner, then the real supervisor-keeper
# exec'd as PID 1 with the world's proxy URL as HTTP_PROXY; the keeper starts the real /usr/local/bin/supervisor,
# which mounts ~/.claude and, on a start_turn, launches Claude Code.
# Logs: /srv/log/{model,world,supervisor,exits}.log (the keeper's stdout and stderr are the supervisor's console).
set -u
mkdir -p /run/rig /srv/log /srv/model /run/jobs /home/sandbox/.claude && chmod 1777 /run/jobs && chown 1000:1000 /home/sandbox
( /media/fuse-gate model 127.0.0.1:7200 /srv/model/chunks.log 2>>/srv/log/model.log
  echo "model exited status $? at $(date +%s.%N)" >>/srv/log/exits.log ) &
( /media/rig/fuse-rig world /run/rig http://127.0.0.1:7200 2>>/srv/log/world.log
  echo "world exited status $? at $(date +%s.%N)" >>/srv/log/exits.log ) &
for _ in $(seq 400); do [ -f /run/rig/proxy ] && break; sleep 0.05; done
# jobs: /run/jobs/NAME.sh run as uid 1000, children of the sandbox's own processes (not of a host `runsc exec`)
( while true; do
    for f in /run/jobs/*.sh; do
      [ -f "$f" ] || continue
      mv "$f" "$f.started"
      ( setpriv --reuid=1000 --regid=1000 --clear-groups env HOME=/home/sandbox sh "$f.started" </dev/null >"$f.out" 2>&1
        st=$?; [ $st = 0 ] || echo "job $(basename "$f") exited status $st at $(date +%s.%N)" >>/srv/log/exits.log ) &
    done
    sleep 0.1
  done ) &
HTTP_PROXY=$(cat /run/rig/proxy)
export HTTP_PROXY
exec /usr/local/bin/supervisor-keeper >>/srv/log/supervisor.log 2>&1
