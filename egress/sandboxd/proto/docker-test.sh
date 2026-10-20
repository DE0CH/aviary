#!/usr/bin/env bash
# Docker inside a gVisor sandbox (root on a sandbox host): sandbox NAME at IP from docker:29-dind, behind the same
# egress lock as any sandbox (only EGRESS:3128). dockerd pulls through the proxy; `docker run hello-world` and a
# `docker build` run inside. Follows gvisor.dev/docs/tutorials/docker-in-gvisor: Docker 29 needs --net-raw,
# --allow-packet-socket-write and a tmpfs /var/lib/docker; iptables off, so inner containers use --network=host.
# The capabilities are the sandbox's own (gVisor's), not the host's. Usage: docker-test.sh NAME IP EGRESS_IP CTRL_IP
set -euo pipefail
name=$1 ip=$2 egress=$3 ctrl=$4
here=$(cd "$(dirname "$0")" && pwd)
img=docker:29-dind; dir=/var/lib/sbx/images/dind
if [ ! -f $dir/.done ]; then mkdir -p $dir/rootfs; crane export $img - | tar -x -C $dir/rootfs; crane config $img > $dir/config.json; touch $dir/.done; fi
"$here/sbx.sh" rm "$name" >/dev/null 2>&1 || true
"$here/sbx.sh" net "$name" "$ip" "$egress" "$ctrl"
b=/var/lib/sbx/bundles/$name; mkdir -p $b
proxy=http://sbx:test@$egress:3128
all='["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FOWNER","CAP_FSETID","CAP_KILL","CAP_SETGID","CAP_SETUID","CAP_SETPCAP","CAP_NET_BIND_SERVICE","CAP_NET_RAW","CAP_NET_ADMIN","CAP_SYS_CHROOT","CAP_MKNOD","CAP_AUDIT_WRITE","CAP_SETFCAP","CAP_SYS_ADMIN","CAP_SYS_PTRACE","CAP_SYS_RESOURCE","CAP_IPC_LOCK","CAP_SYS_NICE"]'
jq -n --arg root $dir/rootfs --arg ns /var/run/netns/sbx-$name --arg proxy $proxy --argjson caps "$all" --arg name "$name" '{
  ociVersion: "1.0.0",
  process: { user: {uid: 0, gid: 0}, cwd: "/",
    args: ["sh", "-c", "mkdir -p /etc/docker && echo {\\\"iptables\\\":false,\\\"ip6tables\\\":false,\\\"bridge\\\":\\\"none\\\"} > /etc/docker/daemon.json && exec dockerd >/var/log/dockerd.log 2>&1"],
    env: ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "HTTP_PROXY=\($proxy)", "HTTPS_PROXY=\($proxy)", "NO_PROXY="],
    capabilities: {bounding: $caps, effective: $caps, permitted: $caps, inheritable: [], ambient: []} },
  root: {path: $root, readonly: false}, hostname: "dind",
  mounts: [
    {destination: "/proc", type: "proc", source: "proc"},
    {destination: "/dev", type: "tmpfs", source: "tmpfs", options: ["nosuid","strictatime","mode=755","size=65536k"]},
    {destination: "/sys", type: "sysfs", source: "sysfs", options: ["nosuid","noexec","nodev"]},
    {destination: "/sys/fs/cgroup", type: "cgroup", source: "cgroup", options: ["nosuid","noexec","nodev"]},
    {destination: "/tmp", type: "tmpfs", source: "tmpfs"},
    {destination: "/var/lib/docker", type: "tmpfs", source: "tmpfs"}
  ],
  linux: { namespaces: [{type: "pid"}, {type: "ipc"}, {type: "uts"}, {type: "mount"}, {type: "network", path: $ns}],
    cgroupsPath: "/sbx/\($name)", resources: {memory: {limit: 3221225472}, pids: {limit: 4096}} }
}' > $b/config.json
SBX_EXTRA="--net-raw --allow-packet-socket-write" "$here/sbx.sh" run "$name"
x() { SBX_EXTRA="--net-raw --allow-packet-socket-write" "$here/sbx.sh" exec-root "$name" "$@"; }
for i in $(seq 60); do x docker info >/dev/null 2>&1 && break; sleep 1; done
x docker version --format 'docker {{.Server.Version}} in gVisor'
x docker run --rm --network=host hello-world | grep -m1 'Hello from Docker'
x sh -c 'mkdir -p /tmp/b && printf "FROM alpine:3.22\nRUN echo built-inside-gvisor > /msg\nCMD cat /msg\n" > /tmp/b/Dockerfile && docker build -q --network=host -t t /tmp/b >/dev/null && docker run --rm --network=host t'
