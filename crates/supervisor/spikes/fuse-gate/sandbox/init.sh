#!/bin/sh
# The sandbox's first process (root inside the sandbox): keeper, fake magpie, mock model, then the supervisor in a
# restart loop (the keeper's job in production). State lives on the /srv tmpfs; /mnt is a host-backed scratch dir.
# /run/gate-die-at (POINT:OPCODE:NTH), when present at a supervisor start, is passed as --die-at once.
set -u
S=/srv
mkdir -p $S/supervisor $S/magpie/d $S/model $S/log /run/magpie-ctl /home/sandbox/.claude
chown 1000:1000 /home/sandbox
[ -d /media/seed ] && cp -a /media/seed/. $S/magpie/d/ 2>/dev/null
/media/fuse-gate keeper /run/keeper.sock 2>>$S/log/keeper.log &
/media/fuse-gate magpie 127.0.0.1:7100 $S/magpie /run/magpie-ctl 2>>$S/log/magpie.log &
/media/fuse-gate model 127.0.0.1:7200 $S/model/chunks.log 2>>$S/log/model.log &
sleep 0.3
J=${GATE_JOURNAL:-$S/supervisor/fuse-journal}
while true; do
  extra=""
  if [ -f /run/gate-die-at ]; then extra="--die-at $(cat /run/gate-die-at)"; rm -f /run/gate-die-at; fi
  # shellcheck disable=SC2086
  /media/fuse-gate serve /home/sandbox/.claude 127.0.0.1:7100 $S/supervisor "$J" /run/keeper.sock $extra 2>>$S/log/supervisor.log
  echo "supervisor exited status $? at $(date +%s.%N)" >>$S/log/supervisor.log
  sleep 0.05
done
