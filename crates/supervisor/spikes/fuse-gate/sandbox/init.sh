#!/bin/sh
# The sandbox's first process (root inside the sandbox): keeper, fake magpie, mock model, then the supervisor in a
# restart loop (the keeper's job in production). State lives on the /srv tmpfs; /mnt is a host-backed scratch dir.
# /run/gate-die-at (POINT:OPCODE:NTH), when present at a supervisor start, is passed as --die-at once.
set -u
S=/srv
# GATE_LAYOUT=scratch: supervisor state (cache, journal, ops log, node ids) and the fake magpie's store on the
# host-backed scratch dir /mnt (moved by rsync on a migration, as sandboxd moves scratch); the paths under /srv stay
if [ "${GATE_LAYOUT:-tmpfs}" = scratch ]; then
  mkdir -p /mnt/supervisor /mnt/magpie && ln -sfn /mnt/supervisor $S/supervisor && ln -sfn /mnt/magpie $S/magpie
fi
mkdir -p $S/supervisor $S/magpie/d $S/model $S/log /run/magpie-ctl /home/sandbox/.claude
chown 1000:1000 /home/sandbox
[ -d /media/seed ] && cp -a /media/seed/. $S/magpie/d/ 2>/dev/null
/media/fuse-gate keeper /run/keeper.sock 2>>$S/log/keeper.log &
# the fakes are restarted if they exit, and every exit is logged with its status (a crash would show here)
( while true; do /media/fuse-gate magpie 127.0.0.1:7100 $S/magpie /run/magpie-ctl 2>>$S/log/magpie.log
    echo "magpie exited status $? at $(date +%s.%N)" >>$S/log/exits.log; sleep 0.2; done ) &
( while true; do /media/fuse-gate model 127.0.0.1:7200 $S/model/chunks.log 2>>$S/log/model.log
    echo "model exited status $? at $(date +%s.%N)" >>$S/log/exits.log; sleep 0.2; done ) &
sleep 0.3
# jobs: /run/jobs/NAME.sh files run as uid 1000, children of this process (processes started by the host's
# `runsc exec` keep host stdio and are not what production runs; Claude Code is a child of the supervisor there)
mkdir -p /run/jobs && chmod 1777 /run/jobs
( while true; do
    for f in /run/jobs/*.sh; do
      [ -f "$f" ] || continue
      mv "$f" "$f.started"
      setpriv --reuid=1000 --regid=1000 --clear-groups env HOME=/home/sandbox sh "$f.started" </dev/null >"$f.out" 2>&1 &
    done
    sleep 0.1
  done ) &
J=${GATE_JOURNAL:-$S/supervisor/fuse-journal}
while true; do
  # /run/gate-hold-restart: no new supervisor while it exists (the keeper-only test holds the connection alone)
  while [ -f /run/gate-hold-restart ]; do sleep 0.05; done
  extra=""
  if [ -f /run/gate-die-at ]; then extra="--die-at $(cat /run/gate-die-at)"; rm -f /run/gate-die-at; fi
  # shellcheck disable=SC2086
  /media/fuse-gate serve /home/sandbox/.claude 127.0.0.1:7100 $S/supervisor "$J" /run/keeper.sock $extra 2>>$S/log/supervisor.log
  echo "supervisor exited status $? at $(date +%s.%N)" >>$S/log/supervisor.log
  sleep 0.05
done
