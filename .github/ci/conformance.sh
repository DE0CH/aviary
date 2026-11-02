#!/usr/bin/env bash
# The conformance suites as binaries with the contract's own durations (platform 8, WP-09):
#   aviary-conformance gateway  against test-gateway (the reference gateway), its drain last (SIGTERM),
#   aviary-conformance model    against mock-model.
# (aviary-conformance hermit runs against fake-hermit in-process: testing/conformance/tests/hermit.rs.)
# Usage: conformance.sh <dir with the static binaries> <output dir for JUnit XML and logs>
set -euo pipefail
bin=$(cd "${1:?binaries dir}" && pwd)
out=${2:?output dir}
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"; jobs -p | xargs -r kill 2>/dev/null || true' EXIT

# throwaway credentials, generated per run (platform 4.7)
# a token may start with "-" (base64url), so a flag takes it as --flag=value
tok() { head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n'; }
GATEWAY_TOKEN_MAGPIE=$(tok)
GATEWAY_TOKEN_HERMIT=$(tok)
GATEWAY_TOKEN_TOOLS=$(tok)
GATEWAY_MODEL_KEY=$(tok)
GATEWAY_ACCEPT_PIGEON=$(tok)
GATEWAY_ACCEPT_HERMIT=$(tok)
export GATEWAY_TOKEN_MAGPIE GATEWAY_TOKEN_HERMIT GATEWAY_TOKEN_TOOLS GATEWAY_MODEL_KEY GATEWAY_ACCEPT_PIGEON GATEWAY_ACCEPT_HERMIT
cat > "$work/tokens.json" <<JSON
{"gatewayToMagpie": "$GATEWAY_TOKEN_MAGPIE", "gatewayToHermit": "$GATEWAY_TOKEN_HERMIT",
 "gatewayToTools": "$GATEWAY_TOKEN_TOOLS", "gatewayToModel": "$GATEWAY_MODEL_KEY",
 "pigeonToGateway": "$GATEWAY_ACCEPT_PIGEON", "hermitToGateway": "$GATEWAY_ACCEPT_HERMIT"}
JSON
echo "1:$(tok)" > "$work/signing-key"

fakes=127.0.0.1:18080
hermit_fakes=127.0.0.1:18081 # hermit alone: its outage closes this port, so connects are refused
"$bin/test-gateway" --proxy-listen 127.0.0.1:13128 --api-listen 127.0.0.1:18082 --ops-listen 127.0.0.1:19090 \
  --magpie "http://$fakes" --hermit "http://$hermit_fakes" --tools "http://$fakes" --model "http://$fakes" \
  --self-host 192.0.2.10 --self-host egress > "$out/test-gateway.log" 2>&1 &
gw=$!
# scrape its metrics until it exits: the last scrape is from inside the drain
( while kill -0 "$gw" 2>/dev/null; do
    curl -sf http://127.0.0.1:19090/metrics > "$work/metrics.tmp" && mv "$work/metrics.tmp" "$out/test-gateway.metrics"
    sleep 0.5
  done ) &

status=0
"$bin/aviary-conformance" --junit "$out/gateway.xml" gateway \
  --proxy http://127.0.0.1:13128 --api http://127.0.0.1:18082 --sandbox-ip 127.0.0.1 \
  --serve-fakes "$fakes" --serve-hermit-fakes "$hermit_fakes" --tokens "$work/tokens.json" --signing-key "@$work/signing-key" \
  --sigterm "kill -TERM $gw" --self-host 192.0.2.10 --self-host egress || status=1

# the drained gateway exits 0 with log `drained`
gw_exit=0
timeout 30 tail --pid="$gw" -f /dev/null || true
wait "$gw" || gw_exit=$?
if [ "$gw_exit" -ne 0 ] || ! grep -q '"drained"' "$out/test-gateway.log"; then
  echo "test-gateway did not drain cleanly (exit $gw_exit)"; status=1
fi
# nothing was cut at a drain bound: the log says cut=[], and <gw>_drain_cut_total{kind} is exported with no cut
if ! grep '"gateway drained"' "$out/test-gateway.log" | grep -q '"cut":"\[\]"'; then
  echo "test-gateway's drain cut requests: $(grep '"gateway drained"' "$out/test-gateway.log")"; status=1
fi
if [ "$(grep -cE '^test_gateway_drain_cut_total\{kind="[A-Za-z_]+"\} 0$' "$out/test-gateway.metrics" 2>/dev/null)" != 3 ]; then
  echo "test-gateway does not export test_gateway_drain_cut_total{kind} at 0 for its three kinds:"
  grep drain_cut "$out/test-gateway.metrics" || true; status=1
fi
# the internet request the suite sent was refused and logged
if ! grep '"internet request refused"' "$out/test-gateway.log" | grep -q 'unlisted.invalid'; then
  echo "test-gateway did not log the refused internet request"; status=1
fi

"$bin/mock-model" --listen 127.0.0.1:18090 > "$out/mock-model.log" 2>&1 &
mm=$!
for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/18090) 2>/dev/null && break; sleep 0.1; done
"$bin/aviary-conformance" --junit "$out/model.xml" model --target http://127.0.0.1:18090 --token="$(tok)" || status=1
kill "$mm" 2>/dev/null || true
exit "$status"
