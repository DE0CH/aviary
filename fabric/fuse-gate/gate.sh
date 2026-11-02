#!/usr/bin/env bash
# Host side of the FUSE gate (crates/supervisor/GATE.md): one sandbox under the production runsc flags of jarvis3
# chat/sandboxd/daemon/src/runsc.rs (FLAGS) and its bundle shape (root first process with the Docker capability set,
# HOME=/home/sandbox), with the spike from crates/supervisor/spikes/fuse-gate bound read-only at /media.
#   gate.sh bundle NAME [JOURNAL]   OCI bundle (+ netns gate-NAME with only lo); JOURNAL default /srv/... (tmpfs)
#   gate.sh run NAME [extra runsc flags]   start detached (extra flags only for the strace run)
#   gate.sh exec NAME [--root] CMD...      run in the sandbox (uid 1000 unless --root)
#   gate.sh send NAME HOST PORT [DOWNTIME] live-migrate to a `receive` on HOST (pre-copy, checkpoint --migrate-to)
#   gate.sh receive NAME PORT              restore NAME from a pre-copy stream (restore --migrate-listen)
#   gate.sh rm NAME
set -euo pipefail
ROOT=/run/gate/runsc
RUNSC=(runsc --root "$ROOT" --platform=systrap --network=sandbox --overlay2=none --host-uds=none --host-fifo=none
       --net-raw --allow-packet-socket-write --allow-suid)
cmd=$1; name=${2:-}
b=/var/lib/gate/bundles/$name
caps='["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FOWNER","CAP_FSETID","CAP_KILL","CAP_SETGID","CAP_SETUID","CAP_SETPCAP","CAP_NET_BIND_SERVICE","CAP_NET_RAW","CAP_NET_ADMIN","CAP_SYS_CHROOT","CAP_MKNOD","CAP_AUDIT_WRITE","CAP_SETFCAP","CAP_SYS_ADMIN","CAP_SYS_PTRACE","CAP_SYS_RESOURCE","CAP_IPC_LOCK","CAP_SYS_NICE"]'
case $cmd in
bundle)
  journal=${3:-/srv/supervisor/fuse-journal}
  img=/var/lib/gate/image
  mkdir -p "$b" /var/lib/gate/scratch/"$name"
  ip netns list | grep -qw "gate-$name" || { ip netns add "gate-$name"; ip -n "gate-$name" link set lo up; }
  jq -n --arg root "$(readlink -f $img)/rootfs" --arg ns "/var/run/netns/gate-$name" --arg name "$name" \
        --argjson caps "$caps" --argjson env "$(jq '.config.Env' $img/config.json)" --arg j "$journal" \
        --arg scratch "/var/lib/gate/scratch/$name" '{
    ociVersion: "1.0.0",
    process: { user: {uid: 0, gid: 0}, cwd: "/",
      args: ["/usr/bin/tini", "--", "/media/init.sh"],
      env: ($env + ["GATE_JOURNAL=\($j)"]),
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
  "${RUNSC[@]}" restore --migrate-listen "[::]:$port" --image-path "$dir" --background --bundle "$b" --detach "$name" \
    </dev/null >"/var/log/gate/$name.restore.out" 2>>"/var/log/gate/$name.log"
  grep -m1 '^{"from"' "/var/log/gate/$name.restore.out" || cat "/var/log/gate/$name.restore.out" ;;
rm)
  "${RUNSC[@]}" kill "$name" KILL 2>/dev/null || true
  "${RUNSC[@]}" delete -force "$name" 2>/dev/null || true
  ;;
*) echo "unknown command $cmd" >&2; exit 2 ;;
esac
