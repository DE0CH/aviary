#!/usr/bin/env bash
# A Docker sandbox (docker-test.sh: dockerd running, hello-world + a built image + a pulled image) on host a,
# live-migrated with pre-copy to $PEER (default b; c for the Hetzner host) and back; after each move the pulled
# and built images are listed and run again. Run from the operator machine after bringup.sh (and overlay-up.sh for c).
# Usage: docker-migrate-test.sh   (env: PEER, PULL, DOWNTIME)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
peer=${PEER:-b}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
wg() { awk -v n="$1" '$1==n{print $'"$2"'}' "$STATE/wg.txt"; }
A=$(col host-a 3) Ap=$(col host-a 4) E=$(col egress 3) Ep=$(col egress 4) TOKEN=$(cat "$STATE/token")
P=$(col host-$peer 3) Pp=$(col host-$peer 4)
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
scp -q "${o[@]}" "$here/docker-test.sh" "$here/sbx.sh" ubuntu@$A:
scp -q "${o[@]}" "$here/sbx.sh" ubuntu@$P:
scp -q "${o[@]}" "$here/migrate.sh" ubuntu@$E:
ssh "${o[@]}" ubuntu@$E "sudo ip route replace 10.200.0.20/32 via $Ap"
echo "=== Docker sandbox d1 on a"
ssh "${o[@]}" ubuntu@$A "sudo PULL=${PULL:-python:3.12-slim} ./docker-test.sh d1 10.200.0.20 $Ep $Ep"
check() {  # host
  ssh "${o[@]}" ubuntu@$1 "sudo ./sbx.sh exec-root d1 sh -c 'docker images --format \"{{.Repository}}:{{.Tag}} {{.Size}}\"; docker run --rm --network=host t; docker run --rm --network=host ${PULL:-python:3.12-slim} python3 -c \"print(\\\"python in a pulled image, after migration\\\")\"'"
}
mig() {  # from to
  local s d route=""
  s=$(col host-$1 4) d=$(col host-$2 4)
  if [ "$1" = c ] || [ "$2" = c ]; then
    s=$(wg $1 2) d=$(wg $2 2)
    if [ "$2" = c ]; then route="ROUTE_VIA='dev wg0 src $Ep' WG_PEER=$(wg c 3)"; else route="ROUTE_VIA='via $(col host-$2 4)'"; fi
  fi
  ssh "${o[@]}" ubuntu@$E "KEY=\$HOME/key DOWNTIME=${DOWNTIME:-100ms} BUNDLE=bundle-dind NOHEALTH=1 $route ./migrate.sh d1 10.200.0.20 $s $d $Ep $TOKEN precopy"
}
echo "=== a -> $peer"; mig a $peer; check $P
echo "=== $peer -> a"; mig $peer a; check $A
ssh "${o[@]}" ubuntu@$A "sudo ./sbx.sh rm d1"
