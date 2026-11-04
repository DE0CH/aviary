#!/usr/bin/env bash
# The end-to-end harness (platform 7): the real pigeon, magpie and hermit images, the real sandbox image as Pods made
# by fake-sandboxd, the reference gateway, mock-model, fake-tools and fake-access, in one k3d cluster on this machine,
# with Postgres, S3 and a JuiceFS mount beside it. The same on a GitHub runner (ci.yml `e2e`) and a laptop.
#
#   run.sh up <pigeon image> <magpie image> <hermit image> <aviary-testing image> <hermit-sandbox image>
#   run.sh test [scenario…]     the driver Job (testing/e2e/src: every scenario of 7.8 it has, or the named ones)
#   run.sh artifacts <dir>      every Pod's logs, events, admin dumps, mock-model's requests, the driver's frames
#   run.sh down                 the cluster, the containers and the mount
#
# Needs docker, k3d, kubectl, jq, openssl and juicefs (1.4.1) on PATH, and sudo for the FUSE mount. Every bound is a
# timeout: nothing waits for ever. The work directory is E2E_WORK (default /tmp/aviary-e2e). E2E_PRELOAD=1 copies the
# cluster's own images (Calico, k3s's) from the local Docker into the node, for a machine whose containers cannot pull.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
work=${E2E_WORK:-/tmp/aviary-e2e}
cluster=aviary-e2e
ns=aviary-e2e
net=aviary-e2e
subnet=172.30.0.0/24
pg_ip=172.30.0.10
s3_ip=172.30.0.11
# the host ports of Postgres and S3 (the JuiceFS mounts use them); E2E_PG_PORT/E2E_S3_PORT on a host whose own
# services hold the defaults (a shared build machine)
pg_port=${E2E_PG_PORT:-15432}
s3_port=${E2E_S3_PORT:-19000}
# both JuiceFS mounts live under one folder the node shares (rshared), as in YYC: the hosts' mount (sandboxes'
# /workspace, root-squash=1000:1000) and hermit's own (all-squash=1000:1000: everything hermit makes is the sandbox
# user's, hermit spec 15.4)
mnt=/mnt/aviary
jfs=$mnt/jfs
jfs_hermit=$mnt/jfs-hermit
k3s_image=rancher/k3s:v1.36.5-k3s1
pg_image=ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie@sha256:ed6923f9ba2399cf9070379a353c710c29ced8a294be73986a2c3dae7e599ce3
s3_image=versity/versitygw:v1.9.0@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7
calico_version=v3.30.3
calico_sha256=9382d2b27a76f40c170454b408653e6d71e2205ef0aef069e942bb690e7381d0
export KUBECONFIG=$work/kubeconfig

log() { echo "e2e: $*" >&2; }
die() { log "$*"; exit 1; }
k() { kubectl -n "$ns" "$@"; }

wait_for() { # wait_for <seconds> <what> <command…>
  local until=$((SECONDS + $1)) what=$2; shift 2
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || die "timed out waiting for $what"
    sleep 1
  done
}

node_import() { # node_import <image…>: into the node's containerd (k3d image import, or the copy of E2E_PRELOAD)
  if [ "${E2E_PRELOAD:-}" = 1 ]; then
    local i
    for i in "$@"; do
      docker image inspect "$i" >/dev/null 2>&1 || docker pull -q --platform linux/amd64 "$i" >/dev/null
      if ! docker save --platform linux/amd64 "$i" 2>/dev/null | docker exec -i "k3d-$cluster-server-0" ctr -n k8s.io images import --platform linux/amd64 - >/dev/null 2>&1; then
        # an image Docker keeps without its amd64 manifest's layers: the registry's copy (crane), by a registry
        # name of the same image when $i is a local tag (aviary-e2e/<name>:e2e of a release's ghcr.io image)
        local src=$i ref
        ref=$(docker image inspect -f '{{range .RepoTags}}{{println .}}{{end}}' "$i" 2>/dev/null | grep -m1 -E '^(ghcr\.io|docker\.io|quay\.io)/' || true)
        [ -n "$ref" ] && src=$ref
        crane pull --platform linux/amd64 "$src" "$work/preload.tar"
        docker exec -i "k3d-$cluster-server-0" ctr -n k8s.io images import - < "$work/preload.tar" >/dev/null
        rm -f "$work/preload.tar"
        if [ "$src" != "$i" ]; then
          local want=$i
          case $want in */*/*) ;; */*) want=docker.io/$want ;; *) want=docker.io/library/$want ;; esac
          docker exec "k3d-$cluster-server-0" ctr -n k8s.io images tag --force "$src" "$want" >/dev/null
        fi
      fi
    done
  else
    timeout 600 k3d image import -c "$cluster" "$@" >/dev/null
  fi
}

infra() {
  mkdir -p "$work"
  docker network inspect "$net" >/dev/null 2>&1 || docker network create --subnet "$subnet" --gateway 172.30.0.1 "$net" >/dev/null
  local pg_password s3_access s3_secret jfs_password
  pg_password=$(openssl rand -hex 16); s3_access=$(openssl rand -hex 10); s3_secret=$(openssl rand -hex 20)
  jfs_password=$pg_password
  umask 077
  cat > "$work/infra.env" <<ENV
PG_PASSWORD=$pg_password
S3_ACCESS_KEY=$s3_access
S3_SECRET_KEY=$s3_secret
JFS_META_PASSWORD=$jfs_password
ENV
  umask 022
  # Postgres (the production image, TLS on, scram): databases and roles magpie, hermit, jfs
  local pgdir=$work/pg
  mkdir -p "$pgdir"
  openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=aviary-e2e-postgres \
    -keyout "$pgdir/server.key" -out "$pgdir/server.crt" 2>/dev/null
  printf '%s\n' "$pg_password" > "$pgdir/pw"
  chmod 0755 "$pgdir" && chmod 0644 "$pgdir"/*
  docker run -d --name aviary-e2e-pg --network "$net" --ip "$pg_ip" -p "127.0.0.1:${pg_port}:5432" -v "$pgdir:/ci:ro" \
    "$pg_image" bash -ec '
    bin=$(ls -d /usr/lib/postgresql/*/bin)
    "$bin/initdb" -D /tmp/pgdata -U postgres --pwfile=/ci/pw -A scram-sha-256 >/dev/null
    cp /ci/server.crt /ci/server.key /tmp/pgdata/ && chmod 0600 /tmp/pgdata/server.key
    printf "local all all trust\nhostssl all all all scram-sha-256\n" > /tmp/pgdata/pg_hba.conf
    exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c ssl=on -c fsync=off -c max_connections=300
  ' >/dev/null
  wait_for 60 postgres docker exec aviary-e2e-pg pg_isready -q -h 127.0.0.1
  local db
  for db in magpie hermit jfs; do
    docker exec aviary-e2e-pg psql -q -h /var/run/postgresql -U postgres -v ON_ERROR_STOP=1 \
      -c "create role $db login password '$pg_password'" -c "create database $db owner $db" >/dev/null
  done
  # S3 (versitygw over a directory; a bucket is a directory)
  local s3dir=$work/s3
  mkdir -p "$s3dir/magpie" "$s3dir/jfs"
  chmod -R 0777 "$s3dir"
  docker run -d --name aviary-e2e-s3 --network "$net" --ip "$s3_ip" -p "127.0.0.1:${s3_port}:9000" -v "$s3dir:/data" \
    "$s3_image" --access "$s3_access" --secret "$s3_secret" --region us-east-1 --port :9000 posix /data >/dev/null
  wait_for 60 s3 curl -s -o /dev/null "http://127.0.0.1:${s3_port}/"
  # JuiceFS: meta in Postgres jfs, data in bucket jfs, mounted where the node and hermit see /workspace folders
  META_PASSWORD=$jfs_password timeout 120 juicefs format --storage s3 --bucket "http://127.0.0.1:${s3_port}/jfs" \
    --access-key "$s3_access" --secret-key "$s3_secret" \
    "postgres://jfs@127.0.0.1:${pg_port}/jfs?sslmode=require" aviary-e2e >"$work/juicefs-format.log" 2>&1 \
    || { cat "$work/juicefs-format.log" >&2; die "juicefs format failed"; }
  sudo mkdir -p "$mnt"
  mountpoint -q "$mnt" || sudo mount --bind "$mnt" "$mnt"
  sudo mount --make-rshared "$mnt"
  sudo mkdir -p "$jfs" "$jfs_hermit"
  sudo env META_PASSWORD="$jfs_password" timeout 120 juicefs mount -d --log "$work/juicefs.log" \
    --root-squash 1000:1000 "postgres://jfs@127.0.0.1:${pg_port}/jfs?sslmode=require" "$jfs" >/dev/null
  sudo env META_PASSWORD="$jfs_password" timeout 120 juicefs mount -d --log "$work/juicefs-hermit.log" \
    --all-squash 1000:1000 "postgres://jfs@127.0.0.1:${pg_port}/jfs?sslmode=require" "$jfs_hermit" >/dev/null
  wait_for 30 "the JuiceFS mount" mountpoint -q "$jfs"
  wait_for 30 "hermit's JuiceFS mount" mountpoint -q "$jfs_hermit"
  # the top folder is the sandbox user's (YYC's one-time jfs-owner Job, hermit spec 15.4)
  sudo mkdir -p "$jfs/ws"
  sudo chown 1000:1000 "$jfs/ws"
}

cluster() {
  log "cluster"
  # kubelet's eviction at 1% free: its default (10-15%) tainted a hosted runner's node with disk-pressure once the
  # images were imported, leaving every Pod Pending (nightly e2e-chaos, 2026-11-03); a test node evicts nothing
  timeout 600 k3d cluster create "$cluster" --image "$k3s_image" --network "$net" --no-lb --wait --timeout 300s \
    --k3s-arg "--flannel-backend=none@server:*" --k3s-arg "--disable-network-policy@server:*" \
    --k3s-arg "--disable=traefik@server:*" --k3s-arg "--cluster-cidr=10.48.0.0/13@server:*" \
    --k3s-arg "--service-cidr=10.43.0.0/16@server:*" \
    --k3s-arg "--kubelet-arg=eviction-hard=memory.available<100Mi,nodefs.available<1%,imagefs.available<1%@server:*" \
    --k3s-arg "--kubelet-arg=eviction-minimum-reclaim=nodefs.available=0,imagefs.available=0@server:*" \
    --volume "$mnt:$mnt:rshared@server:0" --kubeconfig-update-default=false >/dev/null
  k3d kubeconfig get "$cluster" > "$KUBECONFIG"
  # Calico: NetworkPolicy, and Pods at the addresses hermit chose (cni.projectcalico.org/ipAddrs)
  local manifest=$work/calico.yaml
  if [ ! -s "$work/calico.orig.yaml" ]; then
    curl -sSfL -o "$work/calico.orig.yaml" \
      "https://raw.githubusercontent.com/projectcalico/calico/$calico_version/manifests/calico.yaml"
  fi
  echo "$calico_sha256  $work/calico.orig.yaml" | sha256sum -c - >/dev/null || die "calico.yaml checksum"
  sed -e 's|# - name: CALICO_IPV4POOL_CIDR|- name: CALICO_IPV4POOL_CIDR|' \
      -e 's|#   value: "192.168.0.0/16"|  value: "10.48.0.0/16"|' \
      -e '/name: CALICO_IPV4POOL_IPIP/{n;s|value: "Always"|value: "Never"|}' "$work/calico.orig.yaml" > "$manifest"
  if [ "${E2E_PRELOAD:-}" = 1 ]; then
    local images
    images=$(grep -o 'image: .*' "$manifest" | awk '{print $2}' | sort -u)
    # shellcheck disable=SC2086 # a list of image names
    node_import $images rancher/mirrored-pause:3.10.2 busybox:1.37.0 \
      rancher/mirrored-coredns-coredns:1.14.7 rancher/local-path-provisioner:v0.0.37 rancher/mirrored-metrics-server:v0.9.0
  fi
  kubectl apply -f "$manifest" >/dev/null
  timeout 300 kubectl -n kube-system rollout status ds/calico-node --timeout=290s >/dev/null
  kubectl apply -f - >/dev/null <<'YAML'
apiVersion: crd.projectcalico.org/v1
kind: IPPool
metadata: {name: fabric}
spec: {cidr: 10.52.0.0/16, ipipMode: Never, vxlanMode: Never, natOutgoing: false, nodeSelector: "!all()", blockSize: 26}
YAML
}

images() { # images <pigeon> <magpie> <hermit> <testing> <sandbox>
  log "images"
  docker tag "$1" aviary-e2e/pigeon:e2e
  docker tag "$2" aviary-e2e/magpie:e2e
  docker tag "$3" aviary-e2e/hermit:e2e
  docker tag "$4" aviary-e2e/testing:e2e
  docker tag "$5" aviary-e2e/hermit-sandbox:e2e
  node_import aviary-e2e/pigeon:e2e aviary-e2e/magpie:e2e aviary-e2e/hermit:e2e aviary-e2e/testing:e2e \
    aviary-e2e/hermit-sandbox:e2e
}

deploy() {
  log "deploy"
  local digest
  digest=$(docker image inspect -f '{{.Id}}' aviary-e2e/hermit-sandbox:e2e)
  "$here/gen.sh" "$work" "$digest" "$subnet" "$pg_ip" "$s3_ip" >/dev/null
  kubectl apply -f "$here/k8s/00-base.yaml" >/dev/null
  kubectl apply -f "$work/k8s/" >/dev/null
  kubectl -n kube-system rollout restart deploy/coredns >/dev/null
  kubectl apply -f "$here/k8s/10-services.yaml" -f "$here/k8s/20-network.yaml" >/dev/null
  local d
  for d in magpie hermit pigeon test-gateway mock-model fake-tools fake-access fake-ec2 fabric-gateway; do
    timeout 400 kubectl -n "$ns" rollout status "deploy/$d" --timeout=390s >/dev/null \
      || { k get pods -o wide >&2; die "deployment $d did not become ready"; }
  done
  log "up"
}

driver() { # the driver Job with the named scenarios (E2E_DRIVER_SECONDS bounds it, default 2400)
  k delete job driver --ignore-not-found --wait=true >/dev/null
  local deadline=${E2E_DRIVER_SECONDS:-2400}
  local args=
  if [ $# -gt 0 ]; then args=$(printf ', "%s"' "$@"); fi
  k apply -f - >/dev/null <<YAML
apiVersion: batch/v1
kind: Job
metadata: {name: driver, namespace: $ns}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: $deadline
  template:
    metadata: {labels: {app: driver}}
    spec:
      serviceAccountName: driver
      restartPolicy: Never
      containers:
        - name: driver
          image: aviary-e2e/testing:e2e
          imagePullPolicy: Never
          command: [aviary-e2e, --env-file, /etc/aviary-e2e/env/env$args]
          env:
            - {name: SSL_CERT_FILE, value: /etc/aviary-e2e/ca/ca.crt}
          volumeMounts:
            - {name: env, mountPath: /etc/aviary-e2e/env, readOnly: true}
            - {name: ca, mountPath: /etc/aviary-e2e/ca, readOnly: true}
            - {name: jfs, mountPath: $jfs, readOnly: true, mountPropagation: HostToContainer}
      volumes:
        - {name: env, secret: {secretName: driver-file}}
        - {name: ca, secret: {secretName: driver-ca}}
        - {name: jfs, hostPath: {path: $jfs, type: Directory}}
YAML
  local until=$((SECONDS + deadline + 50)) phase=
  while [ "$SECONDS" -lt "$until" ]; do
    phase=$(k get job driver -o jsonpath='{.status.conditions[?(@.status=="True")].type}' 2>/dev/null || true)
    case $phase in *Complete*|*Failed*) break ;; esac
    sleep 3
  done
  k logs job/driver --tail=-1 || true
  case $phase in *Complete*) log "driver passed" ;; *) die "driver: ${phase:-timed out}" ;; esac
}

artifacts() {
  local out
  out=$(mkdir -p "$1" && cd "$1" && pwd)
  k get pods -o wide > "$out/pods.txt" 2>&1 || true
  k get events --sort-by=.lastTimestamp > "$out/events.txt" 2>&1 || true
  kubectl get events -A --sort-by=.lastTimestamp > "$out/events-all.txt" 2>&1 || true
  local p
  for p in $(k get pods -o name 2>/dev/null); do
    k logs "$p" --all-containers --tail=-1 > "$out/$(basename "$p").log" 2>&1 || true
    k logs "$p" --all-containers --previous --tail=-1 > "$out/$(basename "$p").previous.log" 2>/dev/null || true
  done
  cp "$work/juicefs.log" "$work/juicefs-hermit.log" "$out/" 2>/dev/null || true
  docker logs aviary-e2e-pg > "$out/postgres.log" 2>&1 || true
  docker logs aviary-e2e-s3 > "$out/s3.log" 2>&1 || true
}

down() {
  k3d cluster delete "$cluster" >/dev/null 2>&1 || true
  docker rm -f -v aviary-e2e-pg aviary-e2e-s3 >/dev/null 2>&1 || true
  local m
  for m in "$jfs" "$jfs_hermit"; do
    if mountpoint -q "$m" 2>/dev/null; then sudo juicefs umount --force "$m" >/dev/null 2>&1 || sudo umount -l "$m" || true; fi
  done
  if mountpoint -q "$mnt" 2>/dev/null; then sudo umount -l "$mnt" || true; fi
  docker network rm "$net" >/dev/null 2>&1 || true
  sudo rm -rf "$work"
}

case ${1:-} in
  up)
    shift
    [ $# = 5 ] || die "usage: run.sh up <pigeon> <magpie> <hermit> <aviary-testing> <hermit-sandbox>"
    infra
    cluster
    images "$@"
    deploy
    # the driver's tokens as one file (the driver reads files, not its environment)
    k create secret generic driver-file --from-file=env="$work/driver.env" --dry-run=client -o yaml | k apply -f - >/dev/null
    ;;
  test) shift; driver "$@" ;;
  artifacts) artifacts "${2:?dir}" ;;
  down) down ;;
  *) die "usage: run.sh up|test|artifacts|down" ;;
esac
