#!/usr/bin/env bash
# legacy-in-a-box (WP-54, DESIGN 9.9, decision 13): the switch window on one machine. The new stack is testing/e2e's
# harness (k3d aviary-e2e: pigeon, magpie, hermit, the gateway, mock-model, fake-access); the old stack is a second
# k3d cluster (aviary-old, namespace jarvis: two ui pods of the A1 image, the web role and sandbox agent stand-in, the
# mock model, k8s/old.yaml) with its Postgres beside it; both on the harness's Docker network and the same JuiceFS
# mount. bridge.mjs stands in for the load balancers B1 (ui-legacy -> the old ui) and B2 (the old ui's pass-on ->
# pigeon); the new side reaches the old Postgres (B3) and the old API (B7) on the network. The importer's hand-over
# runs in the new cluster (k8s/new.yaml).
#
#   run.sh up <release sha12> <ui image>   e2e up with that release's images, then the old cluster, bridges, import
#   run.sh smoke                            a real old turn through the bridge, then the bulk import sees it
#   run.sh personas <claudecodeui dir> <persona build dir>
#                                           the edge stand-in on the persona build, then the persona drivers
#                                           (claudecodeui jarvis/test/persona) once clean (must pass) and once per
#                                           seeded defect (must fail on that defect's check)
#   run.sh gate | owner <id> | native-turns <id>   the runner's steps the drivers call (control.mjs)
#   run.sh counters [--check] | seeds              the zero counters; a seeded defect per counter
#   run.sh logs <dir> | down
#
# Not done yet (each a later step; this job is nightly/manual only until the whole rig passes): seeded defects for the
# zero counters beyond the in-flight close and the cut old turn; the personas beyond `draft` (a send in flight at
# the move, an upload after T0 on an unmoved tab, a file attached at move - 2 s to a pigeon-born conversation, an ask,
# view-as, a hold, an idle warm old sandbox, the A1 gated rollout, a bridge-node kill); iOS (out of scope).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
e2e=$here/../e2e
work=${BOX_WORK:-/tmp/aviary-box}
export E2E_WORK=${E2E_WORK:-/tmp/aviary-e2e}
net=aviary-e2e
subnet=172.30.0.0/24
old_pg_ip=172.30.0.20
bridge_ip=172.30.0.21
edge_ip=172.30.0.22
jfs=/mnt/aviary/jfs
old=aviary-old
k3s_image=rancher/k3s:v1.36.5-k3s1
pg_image=ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie@sha256:ed6923f9ba2399cf9070379a353c710c29ced8a294be73986a2c3dae7e599ce3
team=access.aviary-e2e.svc.cluster.local
aud=e2e0000000000000000000000000000000000000000000000000000000000e2e
SWITCH=box-switch-token
CC=box-cc-token

log() { echo "box: $*" >&2; }
die() { log "$*"; exit 1; }
wait_for() { # wait_for <seconds> <what> <command…>
  local until=$((SECONDS + $1)) what=$2; shift 2
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || die "timed out waiting for $what"
    sleep 2
  done
}
knew() { kubectl --kubeconfig "$E2E_WORK/kubeconfig" "$@"; }
kold() { kubectl --kubeconfig "$work/kubeconfig-old" "$@"; }
secret_value() { knew -n aviary-e2e get secret "$1" -o "jsonpath={.data.$2}" | base64 -d; }

old_pg() {
  local pw=$1 dir=$work/old-pg
  mkdir -p "$dir"
  openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=aviary-old-pg -keyout "$dir/server.key" -out "$dir/server.crt" 2>/dev/null
  printf '%s\n' "$pw" > "$dir/pw"
  chmod 0755 "$dir" && chmod 0644 "$dir"/*
  docker run -d --name aviary-old-pg --network "$net" --ip "$old_pg_ip" -v "$dir:/ci:ro" "$pg_image" bash -ec '
    bin=$(ls -d /usr/lib/postgresql/*/bin)
    "$bin/initdb" -D /tmp/pgdata -U postgres --pwfile=/ci/pw -A scram-sha-256 >/dev/null
    cp /ci/server.crt /ci/server.key /tmp/pgdata/ && chmod 0600 /tmp/pgdata/server.key
    printf "local all all trust\nhost all all all scram-sha-256\nhostssl all all all scram-sha-256\n" > /tmp/pgdata/pg_hba.conf
    exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c ssl=on -c fsync=off -c max_connections=200
  ' >/dev/null
  wait_for 60 "the old Postgres" docker exec aviary-old-pg pg_isready -q -h 127.0.0.1
  docker exec aviary-old-pg psql -q -h /var/run/postgresql -U postgres -c "create database cloudcli" -c "create database jarvis" >/dev/null
  docker exec aviary-old-pg psql -q -h /var/run/postgresql -U postgres -d jarvis -c \
    "create table ops_events (id bigserial primary key, at timestamptz not null default now(), kind text not null, ok boolean, ms integer, sandbox_id uuid, conversation_id uuid, host text, detail jsonb)" >/dev/null
}

render() { # render <template> <out> KEY=VALUE…: each __KEY__ replaced; a value starting with @ is a file, indented 4
  python3 - "$@" <<'PY'
import sys
src, out, *kv = sys.argv[1:]
s = open(src).read()
for item in kv:
    k, v = item.split("=", 1)
    if v.startswith("@"):
        v = "\n".join("    " + line for line in open(v[1:]).read().splitlines())
    s = s.replace(f"__{k}__", v)
open(out, "w").write(s)
PY
}

# each ui pod on a NodePort of its own (30090, 30091): the runner's move, admit and report calls reach every pod, as
# cutover's do through pods/proxy (one Service would pin a kept-alive client to one pod); again after every rollout
label_pods() {
  local i=0 p
  # the Deployment's current pods: running and not terminating (a rollout's old pods are both for a while)
  wait_for 120 "two current ui pods" sh -c "[ \"\$(kubectl --kubeconfig '$work/kubeconfig-old' -n jarvis get pods -l app=ui -o json | jq '[.items[] | select(.metadata.deletionTimestamp == null and .status.phase == \"Running\")] | length')\" = 2 ]"
  for p in $(kold -n jarvis get pods -l app=ui -o json | jq -r '.items[] | select(.metadata.deletionTimestamp == null and .status.phase == "Running") | "pod/" + .metadata.name'); do
    kold -n jarvis label --overwrite "$p" "ui-pod=p$i" >/dev/null 2>&1 || continue
    kold -n jarvis apply -f - >/dev/null <<YAML
apiVersion: v1
kind: Service
metadata: {name: ui-p$i, namespace: jarvis}
spec:
  type: NodePort
  selector: {app: ui, ui-pod: p$i}
  ports: [{port: 8080, targetPort: 8080, nodePort: $((30090 + i))}]
YAML
    i=$((i + 1))
  done
}

follow_ui_logs() { # the old ui pods' log into $work/ui.log, restarted after a rollout (new pods)
  pkill -f "kubeconfig-old -n jarvis logs" 2>/dev/null || true
  kubectl --kubeconfig "$work/kubeconfig-old" -n jarvis logs -f -l app=ui --prefix --max-log-requests 6 --tail=-1 >> "$work/ui.log" 2>&1 &
}

up() {
  local s=${1:?release sha12} ui=${2:?ui image}
  mkdir -p "$work"
  # the new cluster, unchanged (kept when it is already up: BOX_KEEP_NEW=1, a rerun of the old side)
  if [ "${BOX_KEEP_NEW:-}" != 1 ]; then
    # BOX_MAGPIE: another magpie image (a fix not yet in a release)
    timeout 1800 "$e2e/run.sh" up "pigeon:$s" "${BOX_MAGPIE:-magpie:$s}" "hermit:$s" "aviary-testing:$s" "hermit-sandbox:$s"
  fi
  docker rm -f -v aviary-edge aviary-bridge aviary-old-pg >/dev/null 2>&1 || true
  k3d cluster delete "$old" >/dev/null 2>&1 || true
  local pw; pw=$(openssl rand -hex 16)
  old_pg "$pw"
  local cloudcli_db="postgres://postgres:$pw@$old_pg_ip:5432/cloudcli?sslmode=require"
  local jarvis_db="postgres://postgres:$pw@$old_pg_ip:5432/jarvis?sslmode=require"
  # (the old stack's Node clients take sslmode=no-verify for the stand-in's self-signed certificate; the importer's
  # require does not check it)
  # the old cluster: its own CIDRs, the same network and JuiceFS mount
  log "old cluster"
  timeout 600 k3d cluster create "$old" --image "$k3s_image" --network "$net" --no-lb --wait --timeout 300s \
    --k3s-arg "--disable=traefik@server:*" --k3s-arg "--cluster-cidr=10.60.0.0/16@server:*" \
    --k3s-arg "--service-cidr=10.61.0.0/16@server:*" \
    --k3s-arg "--kubelet-arg=eviction-hard=memory.available<100Mi,nodefs.available<1%,imagefs.available<1%@server:*" \
    --volume "/mnt/aviary:/mnt/aviary:rshared@server:0" --kubeconfig-update-default=false >/dev/null
  k3d kubeconfig get "$old" > "$work/kubeconfig-old"
  local old_ip new_ip
  old_ip=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$net\").IPAddress}}" "k3d-$old-server-0")
  new_ip=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$net\").IPAddress}}" "k3d-aviary-e2e-server-0")
  docker image inspect "$ui" >/dev/null 2>&1 || docker pull -q "$ui" >/dev/null
  timeout 600 k3d image import -c "$old" "$ui" >/dev/null
  # Access: the old ui trusts fake-access's first key (kid k1), as the old stack trusts the same Access team
  docker run --rm -v "$E2E_WORK/ca:/ca:ro" -v "$work:/w" --user 0 --entrypoint node "$ui" -e '
    const { createPublicKey } = require("node:crypto");
    const fs = require("node:fs");
    const jwk = createPublicKey(fs.readFileSync("/ca/k1.pem")).export({ format: "jwk" });
    fs.writeFileSync("/w/jwks.json", JSON.stringify({ keys: [{ ...jwk, kid: "k1", alg: "RS256", use: "sig" }] }));'
  cat > "$work/config.json" <<JSON
{"product": "legacy-in-a-box", "access": {"teamDomain": "$team", "audience": "$aud", "jwksFile": "/etc/jarvis/jwks.json"},
 "users": ["@example.org", "@yyc.example"], "admins": ["admin@example.org"],
 "models": [{"id": "claude-sonnet-5-5", "label": "Sonnet 5.5", "contextWindow": 200000}],
 "defaultModel": "claude-sonnet-5-5", "titleModel": "claude-sonnet-5-5"}
JSON
  local lu; lu=$(secret_value pigeon-env PIGEON_TOKEN_UI)
  render "$here/k8s/old.yaml" "$work/old.yaml" "CONFIG_JSON=$(tr -d '\n' < "$work/config.json")" \
    "JWKS_JSON=$(cat "$work/jwks.json")" "FAKE_WEB=@$here/../lu-rig/old/fake-web.mjs" \
    "MOCK_MODEL=@$here/../lu-rig/old/mock-model.mjs" "UI_IMAGE=$ui" "CC=$CC" "SWITCH=$SWITCH" "LU=$lu" \
    "PASS_ON=http://$bridge_ip:8081" "CLOUDCLI_DB=${cloudcli_db%require}no-verify" "JARVIS_DB=${jarvis_db%require}no-verify" "JFS=$jfs"
  kold apply -f "$work/old.yaml" >/dev/null
  for d in model web ui; do
    timeout 400 kubectl --kubeconfig "$work/kubeconfig-old" -n jarvis rollout status "deploy/$d" --timeout=390s >/dev/null \
      || { kold -n jarvis get pods -o wide >&2; die "old deployment $d did not become ready"; }
  done
  label_pods
  # the bridges B1 and B2
  docker run -d --name aviary-bridge --network "$net" --ip "$bridge_ip" -v "$here:/box:ro" --entrypoint node "$ui" \
    /box/bridge.mjs --listen 8080 --to "$old_ip:30080" --listen 8081 --to "$new_ip:30081" >/dev/null
  # the new cluster's side: bridge Services, the importer
  wait_for 60 "the importer's token" sh -c "[ -n \"\$(kubectl --kubeconfig '$work/kubeconfig-old' -n jarvis get secret magpie-import-token -o jsonpath='{.data.token}')\" ]"
  local token
  token=$(kold -n jarvis get secret magpie-import-token -o jsonpath='{.data.token}' | base64 -d)
  kold -n jarvis get secret magpie-import-token -o jsonpath='{.data.ca\.crt}' | base64 -d > "$work/old-ca.crt"
  render "$here/k8s/new.yaml" "$work/new.yaml" "BRIDGE_IP=$bridge_ip" "SUBNET=$subnet" "CLOUDCLI_DB=$cloudcli_db" \
    "JARVIS_DB=$jarvis_db" "SWITCH=$SWITCH" "OLD_API=$old_ip" "KUBE_TOKEN=$token" \
    "ACCEPT_MAGPIE=$(secret_value magpie-env MAGPIE_TOKEN_IMPORT)" "KUBE_CA=@$work/old-ca.crt" "JFS=$jfs"
  knew apply -f "$work/new.yaml" >/dev/null
  cat > "$work/env" <<ENV
OLD_UI=http://$old_ip:30080
BRIDGE=http://$bridge_ip
NEW_PIGEON=http://$new_ip:30081
OLD_API=https://$old_ip:6443
OLD_PODS=http://$old_ip:30090,http://$old_ip:30091
SWITCH=$SWITCH
UI_IMAGE=$ui
ENV
  log "up (the importer's hand-over waits for the bulk import: run.sh smoke)"
}

# a real turn on the old stack as staff@example.org, through the bridge (B1, what pigeon's LU uses), then the bulk
# import (`magpie import run`, a Job) imports it
smoke() {
  # shellcheck disable=SC1091
  . "$work/env"
  docker run --rm --network "$net" -v "$here/../lu-rig:/rig:ro" -v "$E2E_WORK/ca:/ca:ro" --user 0 --entrypoint node \
    -e "RIG_ISS=https://$team" -e "RIG_AUD=$aud" -e RIG_KID=k1 -e RIG_EMAIL=staff@example.org \
    "$UI_IMAGE" /rig/smoke.mjs "$BRIDGE:8080" /ca/k1.pem | tee "$work/smoke.json"
  import_job
  local id
  id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sessionId"])' "$work/smoke.json")
  docker exec aviary-e2e-pg psql -q -t -h /var/run/postgresql -U postgres -d magpie -c \
    "select id, migrating from conversations where id = '$id'" | grep -q "$id" || die "conversation $id not imported"
  log "smoke: $id ran on the old stack and is imported (legacy)"
}

# a conversation of `email` on the old stack with `prompt` as its first message, imported (the bulk import's catch-up)
legacy_conversation() { # legacy_conversation <email> <prompt>: prints its id
  # shellcheck disable=SC1091
  . "$work/env"
  docker run --rm --network "$net" -v "$here/../lu-rig:/rig:ro" -v "$E2E_WORK/ca:/ca:ro" --user 0 --entrypoint node \
    -e "RIG_ISS=https://$team" -e "RIG_AUD=$aud" -e RIG_KID=k1 -e "RIG_EMAIL=$1" -e "RIG_PROMPT=$2" \
    "$UI_IMAGE" /rig/smoke.mjs "$BRIDGE:8080" /ca/k1.pem > "$work/conv.json" || { cat "$work/conv.json" >&2; die "the old turn failed"; }
  import_job >&2
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sessionId"])' "$work/conv.json"
}

import_job() {
  knew -n aviary-e2e delete job box-import --ignore-not-found --wait=true >/dev/null
  knew apply -f - >/dev/null <<YAML
apiVersion: batch/v1
kind: Job
metadata: {name: box-import, namespace: aviary-e2e}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: 600
  template:
    metadata: {labels: {app: import}}
    spec:
      serviceAccountName: plain
      restartPolicy: Never
      securityContext: {runAsNonRoot: true, runAsUser: 1000, runAsGroup: 1000}
      containers:
        - name: run
          image: aviary-e2e/magpie:e2e
          imagePullPolicy: Never
          args: [import, run]
          envFrom: [{secretRef: {name: magpie-env}}, {secretRef: {name: magpie-import}}]
          env: [{name: POD_NAME, valueFrom: {fieldRef: {fieldPath: metadata.name}}}]
          volumeMounts: [{name: jfs, mountPath: /jfs, readOnly: true, mountPropagation: HostToContainer}]
      volumes: [{name: jfs, hostPath: {path: $jfs, type: Directory}}]
YAML
  local until=$((SECONDS + 600)) phase=
  while [ "$SECONDS" -lt "$until" ]; do
    phase=$(knew -n aviary-e2e get job box-import -o jsonpath='{.status.conditions[?(@.status=="True")].type}' 2>/dev/null || true)
    case $phase in *Complete*|*Failed*) break ;; esac
    sleep 2
  done
  case $phase in *Complete*) ;; *) knew -n aviary-e2e logs job/box-import --tail=50 >&2; die "the bulk import failed (${phase:-timed out})" ;; esac
  knew -n aviary-e2e logs job/box-import --tail=5
}

# the runner's gate (DESIGN 9.2): `magpie import allow-handover` as a Job, which reads every ui pod's switch report
# through the old API (B7) and sets import_control.handover_allowed_at only when none has a non-LU socket
gate() {
  knew -n aviary-e2e delete job box-gate --ignore-not-found --wait=true >/dev/null
  knew apply -f - >/dev/null <<YAML
apiVersion: batch/v1
kind: Job
metadata: {name: box-gate, namespace: aviary-e2e}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: 300
  template:
    metadata: {labels: {app: import}}
    spec:
      serviceAccountName: plain
      restartPolicy: Never
      securityContext: {runAsNonRoot: true, runAsUser: 1000, runAsGroup: 1000}
      containers:
        - name: gate
          image: aviary-e2e/magpie:e2e
          imagePullPolicy: Never
          args: [import, allow-handover]
          envFrom: [{secretRef: {name: magpie-env}}, {secretRef: {name: magpie-import}}]
          env: [{name: POD_NAME, valueFrom: {fieldRef: {fieldPath: metadata.name}}}]
          volumeMounts:
            - {name: jfs, mountPath: /jfs, readOnly: true, mountPropagation: HostToContainer}
            - {name: legacy-kube-ca, mountPath: /etc/legacy-kube-ca, readOnly: true}
      volumes:
        - {name: jfs, hostPath: {path: $jfs, type: Directory}}
        - {name: legacy-kube-ca, configMap: {name: legacy-kube-ca}}
YAML
  local until=$((SECONDS + 300)) phase=
  while [ "$SECONDS" -lt "$until" ]; do
    phase=$(knew -n aviary-e2e get job box-gate -o jsonpath='{.status.conditions[?(@.status=="True")].type}' 2>/dev/null || true)
    case $phase in *Complete*|*Failed*) break ;; esac
    sleep 2
  done
  knew -n aviary-e2e logs job/box-gate --tail=20
  case $phase in *Complete*) ;; *) die "the gate was not set (${phase:-timed out})" ;; esac
}

# the zero counters of DESIGN 9.9 (testing/switch/zero-counters.txt), summed over every pod that exports them: pigeon,
# magpie and the importer through the API's pod proxy, the old router's report per pod, and the old ui pods' log
# (old_turn_cut_total outlives a drained pod only there: personas keeps $work/ui.log). Prints `<series> <value>`;
# --check exits 1 when any is not 0.
counters() {
  local list=$here/../switch/zero-counters.txt out=$work/counters.txt
  : > "$out"
  local series who state p v total
  while read -r series who state; do
    case $series in ''|'#'*) continue ;; esac
    [ "$state" = exported ] || continue
    case $series in *_seconds_count) continue ;; esac # a histogram: not a zero counter
    total=0
    case $who in
      pigeon|magpie)
        local sel; [ "$who" = pigeon ] && sel=app=pigeon || sel="app in (magpie,import)"
        # the running pods only (the box's Jobs carry app=import too)
        for p in $(knew -n aviary-e2e get pods -l "$sel" --field-selector=status.phase=Running -o jsonpath='{.items[*].metadata.name}'); do
          v=$( (knew get --raw "/api/v1/namespaces/aviary-e2e/pods/$p:9090/proxy/metrics" 2>/dev/null || true) \
            | awk -v s="$series" 'index($0, s " ") == 1 {print $2}' | head -1)
          total=$(python3 -c "print(int(float('${v:-0}')) + $total)")
        done ;;
      router)
        # shellcheck disable=SC1091
        . "$work/env"
        local u
        for u in ${OLD_PODS//,/ }; do
          v=$( (curl -sf -H "x-jarvis-switch-token: $SWITCH" "$u/__jarvis/sockets" || echo '{}') | jq -r ".$series // 0")
          total=$((total + ${v:-0}))
        done
        if [ "$series" = counters.oldTurnCutTotal ] && [ -f "$work/ui.log" ]; then
          v=$(grep -c "old_turn_cut_total" "$work/ui.log" || true)
          [ "${v:-0}" -gt "$total" ] && total=$v
        fi ;;
      *) continue ;;
    esac
    echo "$series $total" | tee -a "$out"
  done < "$list"
  if [ "${1:-}" = --check ] && awk '$2 != 0 {bad = 1} END {exit !bad}' "$out"; then
    die "zero counters not 0: $(awk '$2 != 0' "$out" | tr '\n' ' ')"
  fi
}

magpie_sql() { docker exec aviary-e2e-pg psql -q -t -A -h /var/run/postgresql -U postgres -d magpie -c "$1"; }

owner() { # owner <conversation>: legacy | owned
  local m; m=$(magpie_sql "select migrating from conversations where id = '${1:?id}'")
  case $m in t) echo legacy ;; f) echo owned ;; *) die "no conversation $1" ;; esac
}

native_turns() { magpie_sql "select count(*) from turns where conversation_id = '${1:?id}' and origin = 'person'"; }

# every old ui pod: the arming undone (between persona runs)
admit_all() {
  local p
  for p in $(kold -n jarvis get pods -l app=ui -o name); do
    kold -n jarvis exec "$p" -c ui -- node -e "fetch('http://127.0.0.1:8080/__jarvis/admit',{method:'POST',headers:{'x-jarvis-switch-token':'$SWITCH'}}).then(r=>process.exit(r.ok?0:1))"
  done
}

personas() {
  local ccui=${1:?claudecodeui dir} web=${2:?persona build dir}
  ccui=$(cd "$ccui" && pwd); web=$(cd "$web" && pwd)
  # shellcheck disable=SC1091
  . "$work/env"
  local old_ip new_ip
  old_ip=${OLD_UI#http://}; old_ip=${old_ip%:*}
  new_ip=${NEW_PIGEON#http://}; new_ip=${new_ip%:*}
  docker rm -f aviary-edge >/dev/null 2>&1 || true
  docker run -d --name aviary-edge --network "$net" --ip "$edge_ip" -v "$here:/box:ro" -v "$web:/web:ro" --entrypoint node \
    "$UI_IMAGE" /box/edge.mjs --port 8443 --app /web --old "$old_ip:30080" --new "$new_ip:30081" >/dev/null
  wait_for 30 "the edge" curl -sf "http://$edge_ip:8443/__edge/stats"
  # Playwright (the version paseo's app pins) and its Chromium, outside the checkout
  local pw=$work/playwright
  if [ ! -d "$pw/node_modules/playwright" ]; then
    local v; v=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print((d.get("devDependencies",{}).get("playwright") or "1.56.1").lstrip("^~"))' "$ccui/paseo/packages/app/package.json")
    mkdir -p "$pw" && npm --prefix "$pw" install --no-save --no-audit --no-fund "playwright@$v" >/dev/null
    (cd "$pw" && npx playwright install --with-deps chromium >/dev/null)
  fi
  local lu; lu=$(secret_value pigeon-env PIGEON_TOKEN_UI)
  local results=$work/personas; mkdir -p "$results"
  # the old ui pods' log, kept (old_turn_cut_total of a drained pod)
  follow_ui_logs
  # the drivers' control (control.mjs: gate, owner, native-turns), on the host
  pkill -f "legacy-in-a-box/control.mjs" 2>/dev/null || true
  node "$here/control.mjs" "$here/run.sh" 18299 > "$work/control.log" 2>&1 &
  wait_for 20 "the control" curl -s -o /dev/null -X POST http://127.0.0.1:18299/none
  local fail=0 defect tag id out
  # the defect runs first: the clean run goes on to the gate, after which conversations are released once idle
  for defect in lost-draft second-reconnect ""; do
    tag="persona-$(openssl rand -hex 4)"
    # the previous run's move armed the old router (its non-LU HTTP goes to pigeon): undone first
    admit_all
    id=$(legacy_conversation staff@example.org "$tag first message")
    curl -sf -X POST "http://$edge_ip:8443/__edge/reset" >/dev/null
    # beside its node_modules: an ES module resolves playwright from its own folder
    cp "$ccui/jarvis/test/persona/personas.mjs" "$pw/personas.mjs"
    out=$(node "$pw/personas.mjs" --edge "http://$edge_ip:8443" \
      --old-ui "$OLD_UI" --old-pods "$OLD_PODS" --switch-token "$SWITCH" --control http://127.0.0.1:18299 --lu-token "$lu" --key "$E2E_WORK/ca/k1.pem" --kid k1 \
      --iss "https://$team" --aud "$aud" --email staff@example.org --title "$tag" --conversation "$id" \
      ${defect:+--defect "$defect"} --out "$results" | tail -1) || true
    echo "$out" | tee -a "$results/results.ndjson"
    case $defect in
      "") echo "$out" | jq -e '.ok' >/dev/null || { log "the clean persona run failed"; fail=1; } ;;
      lost-draft) echo "$out" | jq -e '[.checks[] | select(.name == "draft kept across the move")][0].ok == false' >/dev/null \
        || { log "the seeded lost draft was not caught"; fail=1; } ;;
      second-reconnect) echo "$out" | jq -e '[.checks[] | select(.name == "one reconnect per move")][0].ok == false' >/dev/null \
        || { log "the seeded second reconnect was not caught"; fail=1; } ;;
    esac
  done
  # the clean run's switch left every zero counter at 0 (the seeded defects above are client- and edge-side: none
  # of them moves a counter)
  counters --check || fail=1
  [ "$fail" = 0 ] || die "personas: see $results"
  log "personas: clean run passed, both seeded defects caught"
}

# a seeded defect per zero counter (WP-54 acceptance): each must move its counter, which counters --check reports
seeds() {
  # shellcheck disable=SC1091
  . "$work/env"
  local fail=0 first v
  first=${OLD_PODS%%,*}
  # every current pod on its NodePort, unarmed: an armed router refuses the seed's own upgrade (503 before the 101,
  # as it should)
  label_pods
  admit_all
  value() { awk -v s="$1" '$1 == s {print $2}' "$work/counters.txt"; }
  # switch_inflight_closes_total: a client that sends a request after the move's close frame
  docker run --rm --network "$net" -v "$here:/box:ro" -v "$E2E_WORK/ca:/ca:ro" --user 0 --entrypoint node "$UI_IMAGE" \
    /box/seed-inflight.mjs "$first" "$SWITCH" /ca/k1.pem "https://$team" "$aud" seed@example.org || fail=1
  counters >/dev/null
  v=$(value counters.inflightClosesTotal)
  if [ "${v:-0}" -gt 0 ]; then log "seed in-flight close: counted ($v)"; else log "seed in-flight close: not counted"; fail=1; fi
  # old_turn_cut_total: an old pod drained (deadline 5 s) while its process runs a long turn
  kold -n jarvis set env deploy/ui JARVIS_UI_DRAIN_SECONDS=5 >/dev/null
  timeout 300 kubectl --kubeconfig "$work/kubeconfig-old" -n jarvis rollout status deploy/ui --timeout=290s >/dev/null
  label_pods; follow_ui_logs; sleep 3
  admit_all # the in-flight seed's move armed it (the old turn below is started through the old path)
  docker run -d --rm --name aviary-seed-turn --network "$net" -v "$here/../lu-rig:/rig:ro" -v "$E2E_WORK/ca:/ca:ro" --user 0 \
    --entrypoint node -e "RIG_ISS=https://$team" -e "RIG_AUD=$aud" -e RIG_KID=k1 -e RIG_EMAIL=seed@example.org \
    -e "RIG_PROMPT=[[slow 120]] cut me" "$UI_IMAGE" /rig/smoke.mjs "$BRIDGE:8080" /ca/k1.pem >/dev/null
  sleep 15
  # each pod's own log stream to its end (the drain's line is the last thing a pod writes)
  local lp
  for lp in $(kold -n jarvis get pods -l app=ui -o name); do
    kubectl --kubeconfig "$work/kubeconfig-old" -n jarvis logs -f "$lp" --tail=5 --prefix >> "$work/ui.log" 2>&1 &
  done
  sleep 2
  kold -n jarvis delete pods -l app=ui --grace-period=60 --wait=true >/dev/null 2>&1 || true
  docker rm -f aviary-seed-turn >/dev/null 2>&1 || true
  sleep 3
  counters >/dev/null
  v=$(value counters.oldTurnCutTotal)
  if [ "${v:-0}" -gt 0 ]; then log "seed cut old turn: counted ($v)"; else log "seed cut old turn: not counted"; fail=1; fi
  kold -n jarvis set env deploy/ui JARVIS_UI_DRAIN_SECONDS=60 >/dev/null
  timeout 300 kubectl --kubeconfig "$work/kubeconfig-old" -n jarvis rollout status deploy/ui --timeout=290s >/dev/null
  label_pods; follow_ui_logs
  [ "$fail" = 0 ] || die "seeds: a seeded defect was not counted"
  log "seeds: each seeded defect moved its counter"
}

logs() {
  local out; out=$(mkdir -p "$1" && cd "$1" && pwd)
  "$e2e/run.sh" artifacts "$out/new" || true
  mkdir -p "$out/old"
  kold -n jarvis get pods -o wide > "$out/old/pods.txt" 2>&1 || true
  local p
  for p in $(kold -n jarvis get pods -o name 2>/dev/null); do kold -n jarvis logs "$p" --all-containers --tail=-1 > "$out/old/$(basename "$p").log" 2>&1 || true; done
  docker logs aviary-bridge > "$out/bridge.log" 2>&1 || true
  docker logs aviary-edge > "$out/edge.log" 2>&1 || true
  cp -r "$work/personas" "$out/" 2>/dev/null || true
  docker logs aviary-old-pg > "$out/old-pg.log" 2>&1 || true
}

down() {
  docker rm -f -v aviary-edge aviary-bridge aviary-old-pg >/dev/null 2>&1 || true
  k3d cluster delete "$old" >/dev/null 2>&1 || true
  "$e2e/run.sh" down || true
}

case ${1:-} in
  up) shift; up "$@" ;;
  smoke) smoke ;;
  personas) shift; personas "$@" ;;
  gate) gate ;;
  owner) shift; owner "$@" ;;
  native-turns) shift; native_turns "$@" ;;
  counters) shift; counters "$@" ;;
  seeds) seeds ;;
  logs) shift; logs "$@" ;;
  down) down ;;
  *) die "usage: run.sh up <release sha12> <ui image> | smoke | personas <claudecodeui dir> <build dir> | logs <dir> | down" ;;
esac
