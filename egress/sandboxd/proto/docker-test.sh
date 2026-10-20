#!/usr/bin/env bash
# Docker inside a gVisor sandbox (root on a sandbox host): sandbox NAME at IP from docker:29-dind (sbx.sh
# bundle-dind), behind the same egress lock as any sandbox (only EGRESS:3128). dockerd pulls through the proxy;
# `docker run hello-world` and a `docker build` run inside, then PULL (default python:3.12-slim) is pulled so the
# sandbox holds a realistic image when it migrates. Follows gvisor.dev/docs/tutorials/docker-in-gvisor: Docker 29
# needs --net-raw, --allow-packet-socket-write and a tmpfs /var/lib/docker; iptables off, so inner containers use
# --network=host. The capabilities are the sandbox's own (gVisor's), not the host's.
# Usage: docker-test.sh NAME IP EGRESS_IP CTRL_IP   (env: PULL=image)
set -euo pipefail
name=$1 ip=$2 egress=$3 ctrl=$4
here=$(cd "$(dirname "$0")" && pwd)
"$here/sbx.sh" rm "$name" >/dev/null 2>&1 || true
"$here/sbx.sh" net "$name" "$ip" "$egress" "$ctrl"
"$here/sbx.sh" bundle-dind "$name" "$ip" "$egress"
t0=$(date +%s%3N); "$here/sbx.sh" run "$name"
x() { "$here/sbx.sh" exec-root "$name" "$@"; }
for i in $(seq 60); do x docker info >/dev/null 2>&1 && break; sleep 1; done
echo "dockerd answering after $(( $(date +%s%3N) - t0 )) ms"
x docker version --format 'docker {{.Server.Version}} in gVisor'
x docker run --rm --network=host hello-world | grep -m1 'Hello from Docker'
x sh -c 'mkdir -p /tmp/b && printf "FROM alpine:3.22\nRUN echo built-inside-gvisor > /msg\nCMD cat /msg\n" > /tmp/b/Dockerfile && docker build -q --network=host -t t /tmp/b >/dev/null && docker run --rm --network=host t'
t1=$(date +%s%3N); x docker pull -q "${PULL:-python:3.12-slim}" >/dev/null; echo "pulled ${PULL:-python:3.12-slim} in $(( $(date +%s%3N) - t1 )) ms"
x sh -c 'docker images --format "{{.Repository}}:{{.Tag}} {{.Size}}"; du -sh /var/lib/docker | sed "s/^/\/var\/lib\/docker: /"; grep -E "^(MemTotal|MemFree|MemAvailable|Cached):" /proc/meminfo'
echo "sandbox memory (host cgroup): $(cat /sys/fs/cgroup/sbx/$name/memory.current 2>/dev/null || cat /sys/fs/cgroup/memory/sbx/$name/memory.usage_in_bytes 2>/dev/null) bytes"
