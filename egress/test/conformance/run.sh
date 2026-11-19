#!/usr/bin/env bash
# chat.yml `conformance` (aviary platform spec 17.3): the egress image under test against aviary's conformance suites.
#   gateway: egress in a container whose fixed names (magpie, tools, upstream-model) and sandbox route
#            (conformance-route, platform 3.11.8) are the suite's fakes on this host's :80, deny-by-default internet; hermit is the suite's own hermit listener (:8081, through a DNAT of
#            hermit's address 192.0.2.80:80), so the outage's first phase closes it and egress's connects are refused
#            (ECONNREFUSED), not reset; the drain last (SIGTERM), then egress must have exited 0 with log `drained` and
#            nothing cut; the refused internet requests must be in egress_log; /version's builtAt is the image's.
#   model:   egress again, its model upstream mock-model, hermit's two routes from fake-hermit.ts; Messages API calls
#            through the proxy with a sandbox token.
# Usage: run.sh <image> <dir with aviary-conformance and mock-model> <output dir for JUnit XML and logs>
set -euo pipefail
img=${1:?image}
bin=$(cd "${2:?binaries dir}" && pwd)
out=$(mkdir -p "${3:?output dir}" && cd "$3" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
chat=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
mkdir -m 777 "$work/hermit"
asroot() { if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi; }
HERMIT_IP=192.0.2.80
dnat=()
cleanup() {
  [ ${#dnat[@]} = 0 ] || asroot iptables -t nat -D PREROUTING "${dnat[@]}" 2>/dev/null || true
  docker rm -f conf-egress conf-egress-model conf-hermit conf-pg >/dev/null 2>&1 || true
  jobs -p | xargs -r kill 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
# the fakes listen on :80 of this host
if [ "$(cat /proc/sys/net/ipv4/ip_unprivileged_port_start)" -gt 80 ]; then asroot sysctl -qw net.ipv4.ip_unprivileged_port_start=80; fi

# throwaway credentials, generated per run (passed as --flag=value: a base64url token may start with "-")
tok() { head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n'; }
MAGPIE=$(tok) HERMIT=$(tok) TOOLS=$(tok) MODEL=$(tok) PIGEON=$(tok) HERMIT_IN=$(tok) ADMIN=$(tok) PGPW=$(tok) ROUTE=$(tok)
cat > "$work/tokens.json" <<JSON
{"gatewayToMagpie": "$MAGPIE", "gatewayToHermit": "$HERMIT", "gatewayToTools": "$TOOLS", "gatewayToModel": "$MODEL",
 "pigeonToGateway": "$PIGEON", "hermitToGateway": "$HERMIT_IN", "gatewayToRoute": "$ROUTE"}
JSON
echo "1:$(tok)" > "$work/signing-key"

docker run -d --name conf-pg -e POSTGRES_USER=egress -e POSTGRES_PASSWORD="$PGPW" -e POSTGRES_DB=egress postgres:17 >/dev/null
pg=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' conf-pg)
for _ in $(seq 60); do docker exec conf-pg pg_isready -q -U egress -d egress && break; sleep 1; done
gw=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
hosts=(--add-host magpie:host-gateway --add-host tools:host-gateway --add-host upstream-model:host-gateway
  --add-host conformance-route:host-gateway)
# hermit at its own address, DNAT'ed to the suite's hermit listener on this host (the bridge's gateway, :8081)
dnat=(-i docker0 -d "$HERMIT_IP" -p tcp --dport 80 -j DNAT --to-destination "$gw:8081")
asroot iptables -t nat -I PREROUTING "${dnat[@]}"
start_egress() { # <name> <egress.json> <database> <hermit's address>
  # postgres:17's entrypoint answers pg_isready from its init-time server, then restarts it: retry until the database
  # is there (a create during the restart failed silently and egress never started)
  local _
  for _ in $(seq 30); do
    docker exec conf-pg psql -q -U egress -d egress -c "create database $3" >/dev/null 2>&1 || true
    docker exec conf-pg psql -tA -U egress -d egress -c "select 1 from pg_database where datname = '$3'" 2>/dev/null | grep -q 1 && break
    sleep 1
  done
  docker run -d --name "$1" "${hosts[@]}" --add-host "hermit:$4" -v "$2:/etc/egress/egress.json:ro" \
    -e EGRESS_DATABASE_URL="postgres://egress:$PGPW@$pg:5432/$3" \
    -e EGRESS_TOKEN_MAGPIE="$MAGPIE" -e EGRESS_TOKEN_HERMIT="$HERMIT" -e EGRESS_TOKEN_TOOLS="$TOOLS" -e EGRESS_TOKEN_ROUTE="$ROUTE" \
    -e OPENROUTER_API_KEY="$MODEL" -e EGRESS_ACCEPT_PIGEON="$PIGEON" -e EGRESS_ACCEPT_HERMIT="$HERMIT_IN" \
    -e EGRESS_ACCEPT_ADMIN="$ADMIN" "$img" egress >/dev/null
  docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$1"
}

status=0
ip=$(start_egress conf-egress "$here/egress-gateway.json" egress_gateway "$HERMIT_IP")
# /version: builtAt is the image's CHAT_BUILT_AT (RFC 3339), null only for an image built without it
want=$(docker image inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$img" | sed -n 's/^CHAT_BUILT_AT=//p')
for _ in $(seq 60); do curl -fs "http://$ip:9090/healthz" >/dev/null && break; sleep 1; done
got=$(curl -fs "http://$ip:9090/version" | python3 -c 'import json,sys; print(json.load(sys.stdin)["builtAt"] or "")')
if [ "$got" != "$want" ]; then echo "/version builtAt: got '$got', want '$want'"; status=1; fi
"$bin/aviary-conformance" --junit "$out/gateway.xml" gateway \
  --proxy "http://$ip:3128" --api "http://$ip:8082" --sandbox-ip "$gw" --serve-fakes 0.0.0.0:80 \
  --serve-hermit-fakes 0.0.0.0:8081 \
  --tokens "$work/tokens.json" --signing-key "@$work/signing-key" --sigterm "docker kill -s TERM conf-egress" \
  --self-host 192.0.2.10 --self-host egress --route conformance-route || status=1
code=$(timeout 700 docker wait conf-egress || echo timeout)
docker logs conf-egress > "$out/egress-gateway.log" 2>&1 || true
if [ "$code" != 0 ] || ! grep -q '"at":"drained"' "$out/egress-gateway.log"; then
  echo "egress did not drain cleanly (exit $code)"; status=1
fi
if ! grep '"at":"drained"' "$out/egress-gateway.log" | grep -q '"cut":{"model":0,"tunnel":0,"fixed":0,"tools":0}'; then
  echo "egress's drain cut work: $(grep '"at":"drained"' "$out/egress-gateway.log" || true)"; status=1
fi
# the refused internet requests (an unlisted host, plain and CONNECT) are in egress_log
denied=$(docker exec conf-pg psql -tA -U egress -d egress_gateway -c "select count(*) from egress_log where host = 'unlisted.invalid' and decision like 'deny%'")
if [ "${denied:-0}" -lt 2 ]; then echo "the refused internet requests were not logged ($denied rows)"; status=1; fi

"$bin/mock-model" --listen 0.0.0.0:18090 > "$out/mock-model.log" 2>&1 &
docker run -d --name conf-hermit --network host -v "$chat/test:/app/test:ro" -v "$work/hermit:/work" --entrypoint node "$img" \
  --disable-warning=ExperimentalWarning test/conformance/fake-hermit.ts 80 "$HERMIT" /work/sandbox-token >/dev/null
for _ in $(seq 50); do [ -s "$work/hermit/sandbox-token" ] && break; sleep 0.2; done
ip=$(start_egress conf-egress-model "$here/egress-model.json" egress_model host-gateway)
for _ in $(seq 60); do curl -fs "http://$ip:9090/readyz" >/dev/null && break; sleep 1; done
"$bin/aviary-conformance" --junit "$out/model.xml" model --target "http://$ip:8082" --token="$PIGEON" \
  --proxy "http://$ip:3128" --sandbox-token="$(cat "$work/hermit/sandbox-token")" || status=1
docker logs conf-egress-model > "$out/egress-model.log" 2>&1 || true
docker logs conf-hermit > "$out/fake-hermit.log" 2>&1 || true
exit "$status"
