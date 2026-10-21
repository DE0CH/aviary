#!/usr/bin/env bash
# After hosts-up.sh: prepares hosts A and B, and C if state/hosts.txt has one (host-setup.sh), starts the egress stand-in on the controller VM, copies
# the scripts, and starts sandbox s1 (10.200.0.10) on host A with the controller's /32 route pointing at A.
# Usage: bringup.sh [image]   (STATE as for hosts-up.sh)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); STATE=${STATE:-$here/state}
IMAGE=${1:-ghcr.io/de0ch/jarvis3-sandbox:288ec849eab9}
col() { awk -v n="yyc-sbx-$1" '$1==n{print $'"$2"'}' "$STATE/hosts.txt"; }
A=$(col host-a 3) B=$(col host-b 3) C=$(col host-c 3) E=$(col egress 3) Ap=$(col host-a 4) Ep=$(col egress 4)
o=(-i "$STATE/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
until ssh "${o[@]}" ubuntu@$A true && ssh "${o[@]}" ubuntu@$B true && ssh "${o[@]}" ubuntu@$E true && { [ -z "$C" ] || ssh "${o[@]}" ubuntu@$C true; }; do sleep 3; done
[ -s "$STATE/token" ] || openssl rand -hex 16 > "$STATE/token"; TOKEN=$(cat "$STATE/token")
for h in $A $B $C; do
  scp -q "${o[@]}" "$here/host-setup.sh" "$here/sbx.sh" ubuntu@$h: 
  ssh "${o[@]}" ubuntu@$h "sudo ./host-setup.sh $IMAGE ${GVISOR:-}" 2>&1 | tail -1 &
done
scp -q "${o[@]}" "$here"/{egress-standin.py,probe-agent.sh,migrate.sh,start-workload.sh,check-workload.sh,workload.py} "$STATE/key" ubuntu@$E:
ssh "${o[@]}" ubuntu@$E "sudo apt-get -qq update >/dev/null 2>&1; sudo apt-get -qq install -y jq bc wireguard-tools >/dev/null 2>&1
  sudo sysctl -qw net.ipv4.ip_forward=1; systemctl is-active -q sbx-egress || sudo systemd-run -q --unit sbx-egress python3 \$HOME/egress-standin.py /var/log/sbx-heartbeat.log"
wait
# the CPU features common to all hosts: every sandbox gets exactly these (sbx.sh cpu_features), so it can move to any host
common=""
for h in $A $B $C; do
  f=$(ssh "${o[@]}" ubuntu@$h "runsc cpu-features" | tr ',' '\n' | sort)
  if [ -z "$common" ]; then common=$f; else common=$(comm -12 <(echo "$common") <(echo "$f")); fi
done
common=$(echo "$common" | paste -sd,)
for h in $A $B $C; do ssh "${o[@]}" ubuntu@$h "sudo mkdir -p /var/lib/sbx && echo $common | sudo tee /var/lib/sbx/cpu-features >/dev/null"; done
echo "$(echo "$common" | tr ',' '\n' | wc -l) CPU features common to all hosts" >&2
for h in $B $C; do ssh "${o[@]}" ubuntu@$h "sudo ./sbx.sh rm s1 >/dev/null 2>&1; sudo rm -rf /run/sbx/ckpt/*"; done
ssh "${o[@]}" ubuntu@$A "sudo ./sbx.sh rm s1 >/dev/null 2>&1; sudo rm -rf /run/sbx/ckpt/*; sudo ./sbx.sh net s1 10.200.0.10 $Ep $Ep && sudo ./sbx.sh bundle s1 10.200.0.10 $Ep $TOKEN && sudo ./sbx.sh run s1"
ssh "${o[@]}" ubuntu@$E "sudo ip route replace 10.200.0.10/32 via $Ap ${ROUTE_OPTS:-}; until curl -sf -m 1 http://10.200.0.10:7070/healthz; do sleep 0.2; done; echo"
