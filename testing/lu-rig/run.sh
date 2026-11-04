#!/usr/bin/env bash
# The LU rig (WP-54, DESIGN 9.9): the old stack's ui image of the A1 commit (router with the switch module + CloudCLI)
# running real Claude Code turns, beside a stand-in of its web role and sandbox agent (old/fake-web.mjs) and a mock
# model (old/mock-model.mjs), with Postgres. pigeon's LU cases then run against it from the host
# (crates/pigeon/tests/lu_rig.rs, ignored unless AVIARY_TEST_LU_RIG_UI is set).
#
#   run.sh up <ui image>   start it, then run.sh smoke; prints the env the cases need (also in $LU_RIG_WORK/env)
#   run.sh smoke           one real old turn through the ui (smoke.mjs)
#   run.sh test            cargo test -p pigeon --test lu_rig -- --ignored (with that env)
#   run.sh logs <dir>      every container's log, the stand-in's calls and the model's requests
#   run.sh down
#
# Access: the ui trusts one RSA key (access.jwksFile) that run.sh makes; the cases' pigeon harness signs with the same
# key (AVIARY_TEST_LU_RIG_KEYS: its team one.example.org, key t1-a). Ports on 127.0.0.1: ui 18180, stand-in 17170, model 18181,
# Postgres 15532. Needs docker, openssl, curl, jq.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
work=${LU_RIG_WORK:-/tmp/aviary-lu-rig}
net=aviary-lu-rig
pg_image=ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie@sha256:ed6923f9ba2399cf9070379a353c710c29ced8a294be73986a2c3dae7e599ce3
LU_PAIR=lu-pair-token          # pigeon's harness (crates/pigeon/tests/common/mod.rs LU_PAIR)
SWITCH=lu-rig-switch-token
CC=cc-rig-token

log() { echo "lu-rig: $*" >&2; }
die() { log "$*"; exit 1; }
wait_for() { # wait_for <seconds> <what> <command…>
  local until=$((SECONDS + $1)) what=$2; shift 2
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || die "timed out waiting for $what"
    sleep 1
  done
}

up() {
  local ui=${1:?ui image}
  down >/dev/null 2>&1 || true
  rm -rf "$work"; mkdir -p "$work/keys" "$work/etc" "$work/jfs/ws" "$work/jfs/users" "$work/home" "$work/sbxhome"
  chmod -R 0777 "$work/jfs" "$work/home" "$work/sbxhome" "$work/etc"
  # the Access key both sides use (pigeon's harness: team one.example.org, aud-one, kid t1-a)
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$work/keys/t1-a.pem" 2>/dev/null
  chmod 0644 "$work/keys/t1-a.pem"
  docker network create "$net" >/dev/null
  docker run --rm -v "$work:/w" --entrypoint node "$ui" -e '
    const { createPublicKey } = require("node:crypto");
    const fs = require("node:fs");
    const jwk = createPublicKey(fs.readFileSync("/w/keys/t1-a.pem")).export({ format: "jwk" });
    fs.writeFileSync("/w/etc/jwks.json", JSON.stringify({ keys: [{ ...jwk, kid: "t1-a", alg: "RS256", use: "sig" }] }));
    fs.writeFileSync("/w/etc/config.json", JSON.stringify({
      product: "LU rig",
      access: { teamDomain: "one.example.org", audience: "aud-one", jwksFile: "/etc/jarvis/jwks.json" },
      users: ["@example.org", "admin@example.com", "admin2@example.com"],
      admins: ["admin@example.com", "admin2@example.com"],
      models: [{ id: "claude-sonnet-5-5", label: "Sonnet 5.5", contextWindow: 200000 }],
      defaultModel: "claude-sonnet-5-5", titleModel: "claude-sonnet-5-5",
    }, null, 2));'
  # Postgres (database cloudcli as in the old stack)
  docker run -d --name lu-pg --network "$net" -p 127.0.0.1:15532:5432 "$pg_image" bash -ec '
    bin=$(ls -d /usr/lib/postgresql/*/bin)
    "$bin/initdb" -D /tmp/pgdata -U postgres -A trust >/dev/null
    printf "local all all trust\nhost all all all trust\n" > /tmp/pgdata/pg_hba.conf
    exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c fsync=off -c max_connections=200
  ' >/dev/null
  wait_for 60 postgres docker exec lu-pg pg_isready -q -h 127.0.0.1
  docker exec lu-pg psql -q -h /var/run/postgresql -U postgres -c "create database cloudcli" >/dev/null
  # the model and the web role + agent (root: a mount namespace per process; Claude Code itself runs as 1000)
  docker run -d --name lu-model --network "$net" -p 127.0.0.1:18181:18080 -v "$here/old:/rig:ro" -e PORT=18080 \
    --entrypoint node "$ui" /rig/mock-model.mjs >/dev/null
  docker run -d --name lu-old --network "$net" -p 127.0.0.1:17170:17070 --privileged --user 0 \
    -v "$here/old:/rig:ro" -v "$work/jfs:/jfs" -v "$work/sbxhome:/home/sandbox" \
    -e PORT=17070 -e JARVIS_CC_TOKEN=$CC -e MOCK_URL=http://lu-model:18080 -e SELF_URL=http://lu-old:17070 \
    --entrypoint sh "$ui" -c 'mkdir -p /workspace && exec node /rig/fake-web.mjs' >/dev/null
  # the ui, as jarvis/ui.yaml runs it, with the switch module on
  docker run -d --name lu-ui --network "$net" -p 127.0.0.1:18180:8080 \
    --add-host host.docker.internal:host-gateway \
    -v "$work/etc:/etc/jarvis:ro" -v "$work/jfs:/jfs" -v "$work/home:/var/lib/cloudcli/home" \
    -e PORT=8080 -e JARVIS_CONFIG=/etc/jarvis/config.json -e JARVIS_WEB_URL=http://lu-old:17070 \
    -e JARVIS_JFS_ROOT=/jfs -e JARVIS_UI_HOME_ROOT=/var/lib/cloudcli/home -e JARVIS_UI_MAX_USERS=8 \
    -e JARVIS_UI_IDLE_MINUTES=30 -e JARVIS_UI_DRAIN_SECONDS=900 \
    -e DATABASE_URL=postgres://postgres@lu-pg:5432/cloudcli -e JWT_SECRET=lu-rig-jwt -e JARVIS_CC_TOKEN=$CC \
    -e JARVIS_SENT_FILES_TOKEN=lu-rig-sent \
    -e JARVIS_UI_SWITCH_TOKEN=$SWITCH -e JARVIS_UI_LU_TOKEN=$LU_PAIR \
    -e JARVIS_UI_PASS_ON_URL=http://host.docker.internal:18190 -e JARVIS_UI_PURGE=off \
    "$ui" >/dev/null
  wait_for 120 "the ui" curl -sf http://127.0.0.1:18180/healthz
  wait_for 30 "the stand-in" curl -sf http://127.0.0.1:17170/healthz
  cat > "$work/env" <<ENV
export AVIARY_TEST_LU_RIG_UI=http://127.0.0.1:18180
export AVIARY_TEST_LU_RIG_OLD=http://127.0.0.1:17170
export AVIARY_TEST_LU_RIG_MODEL=http://127.0.0.1:18181
export AVIARY_TEST_LU_RIG_SWITCH_TOKEN=$SWITCH
export AVIARY_TEST_LU_RIG_SWITCH_URL=http://127.0.0.1:18180/__jarvis
export AVIARY_TEST_LU_RIG_KEYS=$work/keys
export AVIARY_TEST_LU_RIG_JFS=$work/jfs
ENV
  smoke >&2 || die "the smoke turn failed (run.sh logs <dir>)"
  log "up: $(docker inspect -f '{{.Config.Image}}' lu-ui)"
  cat "$work/env"
}

smoke() {
  local ui
  ui=$(docker inspect -f '{{.Config.Image}}' lu-ui)
  docker run --rm --network host -v "$here:/rig:ro" -v "$work/keys:/keys:ro" --entrypoint node "$ui" \
    /rig/smoke.mjs http://127.0.0.1:18180 /keys/t1-a.pem
}

test_() {
  [ -f "$work/env" ] || die "not up"
  # shellcheck disable=SC1091
  . "$work/env"
  cd "$here/../.."
  cargo test --locked -p pigeon --test lu_rig -- --ignored --test-threads=1 "$@"
}

logs() {
  local out=${1:?dir}
  mkdir -p "$out"
  local c
  for c in lu-ui lu-old lu-model lu-pg; do docker logs "$c" > "$out/$c.log" 2>&1 || true; done
  curl -s http://127.0.0.1:17170/__rig/calls > "$out/old-calls.json" || true
  curl -s http://127.0.0.1:18181/__log > "$out/model-requests.json" || true
}

down() {
  docker rm -f -v lu-ui lu-old lu-model lu-pg >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  if [ -d "$work" ]; then
    docker run --rm --user 0 -v "$work:/w" --entrypoint rm "$pg_image" -rf /w/jfs /w/home /w/sbxhome >/dev/null 2>&1 || true
  fi
}

case ${1:-} in
  up) shift; up "$@" ;;
  smoke) smoke ;;
  test) shift; test_ "$@" ;;
  logs) shift; logs "$@" ;;
  down) down ;;
  *) die "usage: run.sh up <ui image> | smoke | test | logs <dir> | down" ;;
esac
