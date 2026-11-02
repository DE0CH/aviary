#!/usr/bin/env bash
# Host side of the FUSE gate (crates/supervisor/GATE.md): one sandbox under the production runsc flags of jarvis3
# chat/sandboxd/daemon/src/runsc.rs (FLAGS) and its bundle shape (root first process with the Docker capability set,
# HOME=/home/sandbox), with the spike from crates/supervisor/spikes/fuse-gate bound read-only at /media.
#   gate.sh bundle NAME [JOURNAL]   OCI bundle (+ netns gate-NAME with only lo); JOURNAL default /srv/... (tmpfs).
#                                   LAYOUT=scratch: supervisor state (cache, journal, ops log) and the fake magpie's
#                                   store live on /mnt, the host-backed scratch dir, as in production (cache and journal
#                                   on the scratch disk; magpie's store is remote in production, here it just rides along)
#   gate.sh run NAME [extra runsc flags]   start detached (extra flags only for the strace run)
#   gate.sh exec NAME [--root] CMD...      run in the sandbox (uid 1000 unless --root)
#   gate.sh send NAME HOST PORT [DOWNTIME] live-migrate to a `receive` on HOST (pre-copy, checkpoint --migrate-to)
#   gate.sh receive NAME PORT              restore NAME from a pre-copy stream (restore --migrate-listen)
#   gate.sh send-scratch NAME HOST PORT [DOWNTIME]  sandboxd's migration (crates/sandboxd sandboxes.rs send_locked):
#                                   rsync rounds of the scratch dir while it runs, checkpoint --migrate-to, then the
#                                   final rsync delta while it is stopped, then SBXDONE to the destination's relay
#   gate.sh receive-relay NAME PORT        restore behind `sandboxd migrate-relay` (it serves rsync into the scratch
#                                   dir and holds the stream's End until SBXDONE), as sandboxd's receive
#   gate.sh rm NAME
set -euo pipefail
ROOT=/run/gate/runsc
RUNSC=(runsc --root "$ROOT" --platform=systrap --network=sandbox --overlay2=none --host-uds=none --host-fifo=none
       --net-raw --allow-packet-socket-write --allow-suid)
# GATE_RUNSC_EXTRA: extra global flags for an experiment (e.g. --systrap-disable-syscall-patching); never production's
# shellcheck disable=SC2206
[ -n "${GATE_RUNSC_EXTRA:-}" ] && RUNSC+=($GATE_RUNSC_EXTRA)
cmd=$1; name=${2:-}
b=/var/lib/gate/bundles/$name
caps='["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FOWNER","CAP_FSETID","CAP_KILL","CAP_SETGID","CAP_SETUID","CAP_SETPCAP","CAP_NET_BIND_SERVICE","CAP_NET_RAW","CAP_NET_ADMIN","CAP_SYS_CHROOT","CAP_MKNOD","CAP_AUDIT_WRITE","CAP_SETFCAP","CAP_SYS_ADMIN","CAP_SYS_PTRACE","CAP_SYS_RESOURCE","CAP_IPC_LOCK","CAP_SYS_NICE"]'
case $cmd in
bundle)
  journal=${3:-/srv/supervisor/fuse-journal}
  layout=${LAYOUT:-tmpfs}
  [ "$layout" = scratch ] && journal=${3:-/mnt/supervisor/fuse-journal}
  img=/var/lib/gate/image
  mkdir -p "$b" /var/lib/gate/scratch/"$name"
  ip netns list | grep -qw "gate-$name" || { ip netns add "gate-$name"; ip -n "gate-$name" link set lo up; }
  jq -n --arg root "$(readlink -f $img)/rootfs" --arg ns "/var/run/netns/gate-$name" --arg name "$name" \
        --argjson caps "$caps" --argjson env "$(jq '.config.Env' $img/config.json)" --arg j "$journal" \
        --arg scratch "/var/lib/gate/scratch/$name" --arg layout "$layout" '{
    ociVersion: "1.0.0",
    process: { user: {uid: 0, gid: 0}, cwd: "/",
      args: ["/usr/bin/tini", "--", "/media/init.sh"],
      env: ($env + ["GATE_JOURNAL=\($j)", "GATE_LAYOUT=\($layout)"]),
      capabilities: {bounding: $caps, effective: $caps, permitted: $caps, inheritable: [], ambient: []},
      rlimits: [{type: "RLIMIT_NOFILE", hard: 65536, soft: 65536}] },
    root: { path: $root, readonly: true },
    hostname: "sandbox",
    mounts: [
      {destination: "/proc", type: "proc", source: "proc"},
      {destination: "/dev", type: "tmpfs", source: "tmpfs", options: ["nosuid","strictatime","mode=755","size=65536k"]},
      {destination: "/dev/pts", type: "devpts", source: "devpts", options: ["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620"]},
      {destination: "/dev/shm", type: "tmpfs", source: "shm", options: ["nosuid","noexec","nodev","mode=1777","size=65536k"]},
      {destination: "/sys", type: "sysfs", source: "sysfs", options: ["nosuid","noexec","nodev","ro"]},
      {destination: "/run", type: "tmpfs", source: "tmpfs", options: ["nosuid","nodev","mode=755"]},
      {destination: "/tmp", type: "tmpfs", source: "tmpfs", options: ["nosuid","nodev","mode=1777"]},
      {destination: "/home/sandbox", type: "tmpfs", source: "tmpfs", options: ["nosuid","nodev","mode=755"]},
      {destination: "/srv", type: "tmpfs", source: "tmpfs", options: ["nosuid","nodev","mode=755"]},
      {destination: "/mnt", type: "bind", source: $scratch, options: ["rbind","rw"]},
      {destination: "/media", type: "bind", source: "/var/lib/gate/bin", options: ["rbind","ro"]}
    ],
    linux: {
      namespaces: [{type: "pid"}, {type: "ipc"}, {type: "uts"}, {type: "mount"}, {type: "network", path: $ns}],
      cgroupsPath: "/gate/\($name)",
      resources: { memory: {limit: 4294967296}, pids: {limit: 4096} }
    }
  }' > "$b/config.json"
  ;;
run) shift 2; mkdir -p /var/log/gate; "${RUNSC[@]}" "$@" run --bundle "$b" --detach "$name" </dev/null &>>"/var/log/gate/$name.log" ;;
exec)
  shift 2
  if [ "${1:-}" = --root ]; then shift; "${RUNSC[@]}" exec --cwd / "$name" "$@"
  else "${RUNSC[@]}" exec --user 1000:1000 --cwd /home/sandbox "$name" "$@"; fi ;;
send) host=$3 port=$4; "${RUNSC[@]}" checkpoint --migrate-to "[$host]:$port" --migrate-downtime "${5:-100ms}" --migrate-max-rounds 20 "$name" ;;
receive)
  port=$3; dir=/run/gate/ckpt/in-$name; rm -rf "$dir"; mkdir -p /var/log/gate
  # RESTORE_DEBUG=1: the restored sandbox logs at debug level (a task's fatal fault shows as "Unhandled user fault")
  dbg=(); [ -n "${RESTORE_DEBUG:-}" ] && { rm -rf "/var/log/gate/dbg-$name"; dbg=(--debug "--debug-log=/var/log/gate/dbg-$name/"); }
  "${RUNSC[@]}" "${dbg[@]}" restore --migrate-listen "[::]:$port" --image-path "$dir" --background --bundle "$b" --detach "$name" \
    </dev/null >"/var/log/gate/$name.restore.out" 2>>"/var/log/gate/$name.log"
  grep -m1 '^{"from"' "/var/log/gate/$name.restore.out" || cat "/var/log/gate/$name.restore.out" ;;
send-scratch)
  host=$3 port=$4; sd=/var/lib/gate/scratch/$name; url="rsync://$host:$port/scratch/"
  ms() { date +%s%3N; }
  rs() { rsync -aHAX --numeric-ids --delete --exclude=/lost+found --stats --timeout=120 --contimeout=10 "$sd/" "$url"; }
  stat_of() { awk -F': ' '/^Number of regular files transferred:/{gsub(/,/,"",$2); f=$2} /^Total transferred file size:/{gsub(/[,a-z ]/,"",$2); b=$2} END{printf "{\"files\":%d,\"bytes\":%d", f, b}'; }
  live="["
  for i in 1 2 3; do  # SCRATCH_LIVE_ROUNDS; stop once a round is small (< 32 MiB and < 2,000 files)
    t=$(ms); o=$(rs) || { c=$?; [ $c = 23 ] || [ $c = 24 ] || exit $c; }; st=$(echo "$o" | stat_of)
    live="$live$([ $i = 1 ] || echo ,)$st,\"ms\":$(( $(ms) - t ))}"
    b=${st#*\"bytes\":}; b=${b%%[!0-9]*}; f=${st#*\"files\":}; f=${f%%[!0-9]*}
    [ "$b" -lt $((32 << 20)) ] && [ "$f" -lt 2000 ] && break
  done
  live="$live]"
  t1=$(ms)
  "${RUNSC[@]}" checkpoint --migrate-to "[$host]:$port" --migrate-downtime "${5:-100ms}" --migrate-max-rounds 20 "$name" >&2
  t2=$(ms)
  "${RUNSC[@]}" delete -force "$name" 2>/dev/null & dp=$!
  o=$(rs); fin=$(echo "$o" | stat_of); t3=$(ms)
  wait $dp || true
  ok=$(python3 -c "
import socket,sys
c=socket.create_connection(('$host',$port),10); c.settimeout(30); c.sendall(b'SBXDONE!')
print(c.recv(2).decode())")
  t4=$(ms)
  echo "{\"live_rounds\":$live,\"checkpoint_ms\":$((t2-t1)),\"final_rsync\":$fin,\"ms\":$((t3-t2))},\"done_reply\":\"$ok\",\"stopped_until_done_ms\":$((t4-t2))}"
  ;;
receive-relay)
  port=$3; dir=/run/gate/ckpt/in-$name; rm -rf "$dir"; mkdir -p /var/log/gate
  dbg=(); [ -n "${RESTORE_DEBUG:-}" ] && { rm -rf "/var/log/gate/dbg-$name"; dbg=(--debug "--debug-log=/var/log/gate/dbg-$name/"); }
  conf=/var/lib/gate/bundles/$name/rsyncd.conf
  printf 'use chroot = yes\nnumeric ids = yes\nuid = 0\ngid = 0\ntimeout = 300\nreverse lookup = no\nlog file = /var/log/gate/%s.rsync.log\n[scratch]\npath = /var/lib/gate/scratch/%s\nread only = no\nlist = no\nopen noatime = yes\n' "$name" "$name" > "$conf"
  "${RUNSC[@]}" "${dbg[@]}" restore --migrate-listen "127.0.0.1:$((port + 1000))" --image-path "$dir" --background --bundle "$b" --detach "$name" \
    </dev/null >"/var/log/gate/$name.restore.out" 2>>"/var/log/gate/$name.log" & rp=$!
  for _ in $(seq 100); do ss -ltn | grep -q "127.0.0.1:$((port + 1000)) " && break; sleep 0.05; done
  sandboxd migrate-relay "0.0.0.0:$port" "127.0.0.1:$((port + 1000))" "$conf" 900 2>>"/var/log/gate/$name.relay.log"; rc=$?
  wait $rp; echo "relay exit $rc" >> "/var/log/gate/$name.relay.log"
  grep -m1 '^{"from"' "/var/log/gate/$name.restore.out" || cat "/var/log/gate/$name.restore.out" ;;
rm)
  "${RUNSC[@]}" kill "$name" KILL 2>/dev/null || true
  "${RUNSC[@]}" delete -force "$name" 2>/dev/null || true
  ;;
*) echo "unknown command $cmd" >&2; exit 2 ;;
esac
