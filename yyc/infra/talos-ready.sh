#!/bin/bash
# talos-ready.sh: read-only check that the new Talos cluster yyc-talos and the old k3s cluster `yyc` give YYC AI what
# DESIGN 15.2 N1-N12 require, including that each build input of 15.2.1 (I1-I9) is what was built (yyc-ai/refactor
# DESIGN.md 15.2, WORK-PACKAGES WP-59). It changes nothing on either cluster or in Hetzner: kubectl get/auth can-i,
# talosctl get, Hetzner GETs, a TLS handshake, and the Descheduler's own --dry-run.
#
# Each line: PASS, FAIL, or PENDING (a check this script does not make: it needs a probe pod, a GitHub run or the
# gateway image; the line says what makes it). Exit 1 on any FAIL. Output goes into the task record.
#
# Env:
#   KUBECONFIG_TALOS  admin kubeconfig of yyc-talos (default ~/talos/kubeconfig; through `talos.sh tunnel`)
#   TALOSCONFIG       its talosconfig (default ~/talos/talosconfig)
#   OLD_MASTER        IPv6 of yyc-master1 (the old cluster; `cluster.sh ip`), SECRETS its admin key dir (~/.yyc-cluster)
#   YYC_HETZNER_READ  optional: a read token of Hetzner project yyc, for the server labels, routes and firewall checks
#                     (run under `session-secret --keys YYC_HETZNER_READ --exec yyc-ai/infra/talos-ready.sh`)
# Prints no secret: only selected fields are read (never a Secret, a machine config or KubeSpan's keys).
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
KT=${KUBECONFIG_TALOS:-$HOME/talos/kubeconfig}
TC=${TALOSCONFIG:-$HOME/talos/talosconfig}
SECRETS=${SECRETS:-$HOME/.yyc-cluster}
POD_CIDR=10.210.0.0/16 SVC_CIDR=10.211.0.0/16
NEED_MI=13824 PER_NODE_MI=2688            # N12 (DESIGN 15.7)
OLD_PEAK_EXTRA_MI=$((3072 + 2048))         # N10: A1's surge plus the poller rollout slot

fails=0
pass() { echo "PASS     $1: $2"; }
fail() { echo "FAIL     $1: $2"; fails=$((fails + 1)); }
pend() { echo "PENDING  $1: $2"; }
check() { local id=$1 msg=$2; shift 2; if "$@"; then pass "$id" "$msg"; else fail "$id" "$msg"; fi; }

k() { kubectl --kubeconfig "$KT" --request-timeout=20s "$@"; }
t() { talosctl --talosconfig "$TC" "$@"; }
old() {
  [ -n "${OLD_MASTER:-}" ] || return 1
  ssh -i "$SECRETS/admin" -o UserKnownHostsFile="$SECRETS/known_hosts" -o StrictHostKeyChecking=yes -o ConnectTimeout=15 \
    -o BatchMode=yes "root@$OLD_MASTER" kubectl "$@"
}
hz() { curl -fsS -H "Authorization: Bearer $YYC_HETZNER_READ" "https://api.hetzner.cloud/v1/$1"; }
in_cidr() { python3 -c 'import ipaddress,sys; sys.exit(0 if ipaddress.ip_network(sys.argv[1]).subnet_of(ipaddress.ip_network(sys.argv[2])) else 1)' "$1" "$2"; }
quiet() { "$@" >/dev/null 2>&1; }

echo "talos-ready.sh $(date -u +%Y-%m-%dT%H:%M:%SZ)"
NODES=$(k get nodes -o json) || { fail N2 "the new API answers through the operator path ($KT)"; exit 1; }
CP_IPS=$(jq -r '.items[] | select(.metadata.labels["node-role.kubernetes.io/control-plane"] != null)
  | [.status.addresses[] | select(.type == "InternalIP") | .address][0]' <<< "$NODES")
CP1=$(head -1 <<< "$CP_IPS")

# ---- N2: the operator path ----
check N2 "kubectl through the operator path: /readyz" bash -c "kubectl --kubeconfig '$KT' get --raw /readyz >/dev/null"
check N2 "talosctl through the operator path: every control node answers" \
  bash -c "for n in $(tr '\n' ' ' <<< "$CP_IPS"); do talosctl --talosconfig '$TC' -n \$n version --short >/dev/null || exit 1; done"

# ---- N1 / I4: the deploy path ----
ep=$(k get endpointslices -n default -l kubernetes.io/service-name=kubernetes -o json \
  | jq -r '[.items[] | .ports[0].port as $p | .endpoints[] | .addresses[] + ":" + ($p|tostring)] | sort | join(" ")')
echo "INFO     N1: the kubernetes Service's endpoints (record them): $ep"
check N1 "the API's endpoints are the control nodes' addresses on 6443" \
  test "$ep" = "$(sed 's/$/:6443/' <<< "$CP_IPS" | sort | tr '\n' ' ' | sed 's/ $//')"
sans=$(echo | timeout 10 openssl s_client -connect 127.0.0.1:6443 -servername kubernetes 2>/dev/null \
  | openssl x509 -noout -ext subjectAltName 2>/dev/null | tr -d ' ' | tr ',' '\n')
check N1 "127.0.0.1 among the API server certificate's SANs (tunnel on 127.0.0.1:6443)" grep -qx 'IPAddress:127.0.0.1' <<< "$sans"
for p in $(k -n kube-system get pods -l component=kube-apiserver -o name); do
  a=$(k -n kube-system get "$p" -o json | jq -r '.spec.containers[0] | (.command // []) + (.args // []) | .[]')
  check N1 "${p#pod/}: --authentication-config set, no --oidc-* flag (I4)" \
    bash -c "grep -q -- '^--authentication-config=' <<< \"\$1\" && ! grep -q -- '^--oidc-' <<< \"\$1\"" _ "$a"
done
ca_live=$(k -n kube-system get configmap kube-root-ca.crt -o jsonpath='{.data.ca\.crt}')
check N1 "infra/yyc-talos-ca.pem is the cluster's CA" test "$ca_live" = "$(cat "$ROOT/infra/yyc-talos-ca.pem")"
check N1 "the deploy connector yyc-deploy/cloudflared is available (2 replicas)" \
  bash -c "[ \"\$(kubectl --kubeconfig '$KT' -n yyc-deploy get deploy cloudflared -o jsonpath='{.status.availableReplicas}' 2>/dev/null)\" = 2 ]"
# each API server's own JWT authenticator: GitHub's key set fetched (over that node's WARP) and GitHub tokens accepted
# (its /metrics, read on that node's own tunnel port: talos.sh tunnel forwards 6443-6445 to cp1-cp3)
for port in ${TALOS_API_PORTS:-6443 6444 6445}; do
  m=$(kubectl --kubeconfig "$KT" --request-timeout=20s --server "https://127.0.0.1:$port" get --raw /metrics 2>/dev/null \
    | grep -E '^apiserver_authentication_jwt_authenticator_(jwks_fetch_last_key_set_info|latency_seconds_count\{.*result="success")')
  ok=$(grep -c '^apiserver_authentication_jwt_authenticator_latency_seconds_count' <<< "$m")
  n=$(grep '^apiserver_authentication_jwt_authenticator_latency_seconds_count' <<< "$m" | awk '{s+=$2} END {print s+0}')
  check N1 "API server on :$port fetched GitHub's JWKS and accepted $n GitHub tokens (the JWKS over its node's WARP)" \
    bash -c "grep -q jwks_fetch_last_key_set_info <<< \"\$1\" && [ $ok -ge 1 ] && [ $n -ge 1 ]" _ "$m"
done
pend N1 "a plan-mode dispatch of yyc-ai.yml shows \`gha:yyc-ai\` (kubectl auth whoami) and its can-i --list matches deploy-workflows 12.3 (GitHub run; 2026-11-05: run 41960543987)"
if k auth can-i --list --as=gha:yyc-ai -n yyc-ai >/dev/null 2>&1; then
  echo "INFO     N1: kubectl auth can-i --list --as=gha:yyc-ai -n yyc-ai (compare with deploy-workflows 12.3):"
  k auth can-i --list --as=gha:yyc-ai -n yyc-ai | sed 's/^/           /'
fi

# ---- N3 / I2: Cilium ----
cc=$(k -n kube-system get configmap cilium-config -o json | jq -c '.data | {ipam, "routing-mode", "tunnel-protocol",
  "kube-proxy-replacement", "enable-ipv4-masquerade", "enable-bpf-masquerade", mtu, "enable-policy"}')
check N3 "Cilium as I2 (ipam kubernetes, VXLAN tunnel, kube-proxy kept, iptables masquerade, MTU 1320, default policy)" \
  test "$cc" = '{"ipam":"kubernetes","routing-mode":"tunnel","tunnel-protocol":"vxlan","kube-proxy-replacement":"false","enable-ipv4-masquerade":"true","enable-bpf-masquerade":"false","mtu":"1320","enable-policy":"default"}'
check N3 "kube-proxy runs on every node" \
  bash -c "[ \"\$(kubectl --kubeconfig '$KT' -n kube-system get ds kube-proxy -o jsonpath='{.status.numberReady}')\" = \"$(jq '.items | length' <<< "$NODES")\" ]"
pend N3 "probe pods in a default-deny namespace: deny/admit, ipBlock /32 to an old node, ipBlock <pod-cidr> admits no pod, toEntities kube-apiserver passes and a control node /32 does not, magpie-pg-ingress (standby streaming, app=import passes, unlabelled fails), readiness under default deny"

# ---- N4 / I1 ----
bad=$(jq -r '.items[] | "\(.metadata.name) \(.spec.podCIDR // "none")"' <<< "$NODES" | while read -r n c; do
  [ "$c" != none ] && in_cidr "$c" "$POD_CIDR" || echo "$n:$c"; done)
check N4 "every node's podCIDR inside $POD_CIDR" test -z "$bad"
svc_ip=$(k get svc kubernetes -n default -o jsonpath='{.spec.clusterIP}')
check N4 "Service CIDR $SVC_CIDR (kubernetes = $svc_ip)" in_cidr "$svc_ip/32" "$SVC_CIDR"
cm=$(k -n kube-system get pods -l component=kube-controller-manager -o json | jq -r '.items[0].spec.containers[0].command[]')
check N4 "controller-manager --cluster-cidr=$POD_CIDR, mask /24" \
  bash -c "grep -qx -- '--cluster-cidr=$POD_CIDR' <<< \"\$1\" && grep -qx -- '--node-cidr-mask-size-ipv4=24' <<< \"\$1\"" _ "$cm"
for c in 10.0.0.0/16 10.200.0.0/15 10.202.0.0/15 10.244.0.0/16 10.45.0.0/16; do
  check N4 "$POD_CIDR and $SVC_CIDR disjoint from $c" python3 -c 'import ipaddress as i,sys
a,b,c=(i.ip_network(x) for x in sys.argv[1:]); sys.exit(1 if a.overlaps(c) or b.overlaps(c) else 0)' "$POD_CIDR" "$SVC_CIDR" "$c"
done
check N4 "the cluster README names both CIDRs" grep -q "pods $POD_CIDR, services $SVC_CIDR" "$ROOT/news-stream/deploy/clusters/talos/README.md"
pend N4 "a probe pod's connection to pg-hosts arrives from its node's 10.0.0.x (masquerade)"

# ---- N5 / I5 ----
workers=$(jq -c '[.items[] | select(.metadata.labels["node-role.kubernetes.io/control-plane"] == null)
  | {n: .metadata.name, net: .metadata.labels["yyc.hetzner/network"], pool: .metadata.labels.pool,
     region: .metadata.labels["topology.kubernetes.io/region"], gw: .metadata.labels["hermit.yyc/gateway"],
     ip: ([.status.addresses[] | select(.type == "InternalIP") | .address][0]),
     ready: ([.status.conditions[] | select(.type == "Ready")][0].status == "True"),
     sched: ((.spec.unschedulable // false) | not)}]' <<< "$NODES")
echo "INFO     N5: workers $(jq -c 'map({n, net, pool, region, gw, ip})' <<< "$workers")"
yw=$(jq '[.[] | select(.net == "yyc" and .pool == "apps" and .ready and .sched)] | length' <<< "$workers")
check N5 "at least 3 schedulable workers labelled yyc.hetzner/network=yyc, pool=apps ($yw)" test "$yw" -ge 3
# no fsn1 count (DESIGN 15.2 N5, change 2026-11-05): the workers have no public IP, so their egress leaves through
# yyc-nat1 in fsn1 wherever they are (checked below with the Hetzner token)
check N5 "every node labelled yyc.hetzner/network=yyc has its address in 10.0.0.0/16" \
  bash -c "for ip in $(jq -r '.[] | select(.net == "yyc") | .ip' <<< "$workers" | tr '\n' ' '); do python3 -c 'import ipaddress,sys; sys.exit(0 if ipaddress.ip_address(sys.argv[1]) in ipaddress.ip_network(\"10.0.0.0/16\") else 1)' \$ip || exit 1; done"
check N5 "only workers carry the labels (no control node with yyc.hetzner/network)" \
  test "$(jq '[.items[] | select(.metadata.labels["node-role.kubernetes.io/control-plane"] != null and .metadata.labels["yyc.hetzner/network"] != null)] | length' <<< "$NODES")" = 0
if [ -n "${YYC_HETZNER_READ:-}" ]; then
  srv=$(hz 'servers?per_page=50')
  for n in $(jq -r '.items[].metadata.name' <<< "$NODES"); do
    check N5 "Hetzner server $n labelled yyc-cluster=talos" \
      test "$(jq -r --arg n "$n" '.servers[] | select(.name == $n) | .labels["yyc-cluster"] // ""' <<< "$srv")" = talos
  done
  nat=$(jq -r '.servers[] | select(.name == "yyc-nat1") | "\(.location.name) \([.private_net[].ip][0])"' <<< "$srv")
  def=$(hz 'networks?name=yyc' | jq -r '.networks[0].routes[] | select(.destination == "0.0.0.0/0") | .gateway')
  check N5 "egress leaves through yyc-nat1 in fsn1: network yyc routes 0.0.0.0/0 to it (yyc-nat1: ${nat:-none}; 0.0.0.0/0 via ${def:-none})" \
    test "$nat|$def" = "fsn1 10.0.0.30|10.0.0.30"
  if OLDN=$(old get nodes -o jsonpath='{.items[*].metadata.name}' 2>/dev/null); then
    for n in $OLDN; do
      check N5 "Hetzner server $n (old cluster) labelled yyc-cluster=k3s (T2's relabel)" \
        test "$(jq -r --arg n "$n" '.servers[] | select(.name == $n) | .labels["yyc-cluster"] // ""' <<< "$srv")" = k3s
    done
  fi
else
  pend N5 "Hetzner labels yyc-cluster=talos|k3s: set YYC_HETZNER_READ"
fi

# ---- N6: add-ons ----
cert=$(k get --raw /api/v1/namespaces/kube-system/services/sealed-secrets-controller:http/proxy/v1/cert.pem 2>/dev/null)
check N6 "the Sealed Secrets controller's cert is infra/sealed-secrets-talos.pem" test "$cert" = "$(cat "$ROOT/infra/sealed-secrets-talos.pem")"
newcnpg=$(k -n cnpg-system get deploy -o jsonpath='{.items[*].spec.template.spec.containers[*].image}' | tr ' ' '\n' | grep '/cloudnative-pg:' | sed 's/.*://')
oldcnpg=$(old -n cnpg-system get deploy -o jsonpath='{.items[*].spec.template.spec.containers[*].image}' 2>/dev/null | tr ' ' '\n' | grep '/cloudnative-pg:' | sed 's/.*://')
if [ -n "$oldcnpg" ]; then check N6 "CloudNativePG at the old cluster's version ($newcnpg vs $oldcnpg)" test "$newcnpg" = "$oldcnpg"
else pend N6 "CloudNativePG version vs the old cluster's: set OLD_MASTER (new: $newcnpg)"; fi
check N6 "StorageClass yyc-block: csi.hetzner.cloud, WaitForFirstConsumer" \
  test "$(k get sc yyc-block -o jsonpath='{.provisioner} {.volumeBindingMode}')" = "csi.hetzner.cloud WaitForFirstConsumer"
check N6 "the Hetzner cloud controller is available" \
  bash -c "[ \"\$(kubectl --kubeconfig '$KT' -n kube-system get deploy hcloud-cloud-controller-manager -o jsonpath='{.status.availableReplicas}')\" -ge 1 ]"
check N6 "the JuiceFS CSI node plugin runs on every network-yyc worker" \
  bash -c "[ \"\$(kubectl --kubeconfig '$KT' -n kube-system get ds juicefs-csi-node -o jsonpath='{.status.numberReady}' 2>/dev/null)\" = \"$(jq '[.[] | select(.net == "yyc")] | length' <<< "$workers")\" ] && [ \"$(jq '[.[] | select(.net == "yyc")] | length' <<< "$workers")\" -gt 0 ]"
pend N6 "a pod mounts volume yyc-ai read-only through the JuiceFS CSI on a Talos node and lists ws/ (needs pg reachable from the new nodes: B3, pg-ingress 5432 from 10.0.0.0/16); a yyc-block PVC attaches (done 2026-11-05 by hand; and the CSI mounted a throwaway volume on w1)"

# ---- N7 / I3, I6, I7 ----
gw=$(jq -c '[.[] | select(.gw == "true")]' <<< "$workers")
check N7 "exactly one gateway node (hermit.yyc/gateway=true), in fsn1 and network yyc" \
  test "$(jq '[.[] | select(.region == "fsn1" and .net == "yyc")] | length' <<< "$gw")/$(jq length <<< "$gw")" = "1/1"
ccm=$(k -n kube-system get deploy hcloud-cloud-controller-manager -o json | jq -c '.spec.template.spec.containers[0] |
  {routes: ([.env[]? | select(.name == "HCLOUD_NETWORK_ROUTES_ENABLED") | .value][0]),
   cidr: ([(.command // []) + (.args // []) | .[] | select(startswith("--cluster-cidr="))][0])}')
check N7 "cloud controller: routes on, --cluster-cidr=$POD_CIDR (I3)" test "$ccm" = "{\"routes\":\"true\",\"cidr\":\"--cluster-cidr=$POD_CIDR\"}"
# Control nodes and the gateway node have a public IP and their own WARP (warp0); workers have neither and reach the
# outside through network yyc's 0.0.0.0/0 to yyc-nat1 (news-stream/deploy/clusters/talos README "Workers without
# public IPs"). wg0 exists only on the gateway node (sandboxd's), whose two wg0 rules sit at 1000/1001: after the
# local table's rule (100 on Talos), before KubeSpan's 32500 and main (aviary crates/sandboxd/docs/HOST.md).
for row in $(jq -r '.items[] | [([.status.addresses[] | select(.type == "InternalIP") | .address][0]),
    (if .metadata.labels["hermit.yyc/gateway"] == "true" then "gw" elif .metadata.labels["node-role.kubernetes.io/control-plane"] != null then "cp" else "w" end)]
    | join("=")' <<< "$NODES"); do
  ip=${row%=*} role=${row#*=}
  ks=$(t -n "$ip" get kubespanconfig -o json 2>/dev/null | jq -c '.spec | {enabled, advertiseKubernetesNetworks, mtu}')
  check N7 "$ip KubeSpan on, no pod networks, MTU 1370 (I6)" test "$ks" = '{"enabled":true,"advertiseKubernetesNetworks":false,"mtu":1370}'
  links=$(t -n "$ip" get links -o json 2>/dev/null | jq -r '.metadata.id' | tr '\n' ' ')
  case $role in
    cp) check N7 "$ip (control node, public IP) has warp0 and no wg0 (I6)" bash -c "grep -qw warp0 <<< '$links' && ! grep -qw wg0 <<< '$links'" ;;
    gw) check N7 "$ip (gateway node, public IPv6) has warp0 (I6; its wg0 is sandboxd's)" grep -qw warp0 <<< "$links" ;;
    w) check N7 "$ip (worker, no public IP: out through yyc-nat1) has no wg0 (I6)" bash -c "! grep -qw wg0 <<< '$links'" ;;
  esac
  rules=$(t -n "$ip" get routingrulestatuses -o json 2>/dev/null | jq -r '[.spec.priority, (.spec.table|tostring)] | join(":")' | tr '\n' ' ')
  # sandboxd's own rules on the gateway node are not a clash
  others=$(tr ' ' '\n' <<< "$rules" | grep . | { if [ "$role" = gw ]; then grep -vxE '1000:main|1001:51822'; else cat; fi; } | tr '\n' ' ')
  check N7 "$ip: no routing table 51822 in use (I6)" bash -c "! grep -q ':51822\\b' <<< '$others'"
  taken=$(tr ' ' '\n' <<< "$others" | grep -E '^100[01]:' | tr '\n' ' ')
  if [ -n "$taken" ]; then
    fail N7 "$ip: rule priority 1000 or 1001 is taken (by: $taken); sandboxd's wg0 rules use them (I6)"
  else pass N7 "$ip: rule priorities 1000/1001 free for sandboxd (I6)"; fi
  local_prio=$(tr ' ' '\n' <<< "$rules" | awk -F: '$2 == "local" {print $1}' | sort -n | tail -1)
  ks_prio=$(tr ' ' '\n' <<< "$rules" | awk -F: '$2 == "180" {print $1}' | sort -n | head -1)
  check N7 "$ip: the local table's rule (${local_prio:-none}) before 1000 and KubeSpan's (${ks_prio:-none}) after 1001, so sandboxd's rules take effect (I6)" \
    bash -c "[ -n '$local_prio' ] && [ '$local_prio' -lt 1000 ] && [ '${ks_prio:-99999}' -gt 1001 ]"
done
if [ -n "${YYC_HETZNER_READ:-}" ]; then
  routes=$(hz 'networks?name=yyc' | jq -r '.networks[0].routes[] | "\(.destination) \(.gateway)"')
  check N7 "network yyc keeps 10.200.0.0/15" grep -q '^10.200.0.0/15 ' <<< "$routes"
  check N7 "network yyc has 10.202.0.0/15 (to the gateway node)" grep -q '^10.202.0.0/15 ' <<< "$routes"
  # one route per old node's pod /24 (the old cluster shrinks as its servers are rebuilt into Talos: no fixed count)
  if old_nodes=$(old get nodes -o json 2>/dev/null); then
    for line in $(jq -r '.items[] | "\(.spec.podCIDR)=\([.status.addresses[] | select(.type == "InternalIP") | .address][0])"' <<< "$old_nodes"); do
      check N7 "network yyc keeps the old node route ${line%=*} via ${line#*=}" grep -qx "${line%=*} ${line#*=}" <<< "$routes"
    done
  else
    pend N7 "network yyc keeps a route per old node's pod /24: set OLD_MASTER (and SECRETS)"
  fi
  for line in $(jq -r '.items[] | "\(.spec.podCIDR)=\([.status.addresses[] | select(.type == "InternalIP") | .address][0])"' <<< "$NODES"); do
    check N7 "route ${line%=*} via ${line#*=}" grep -qx "${line%=*} ${line#*=}" <<< "$routes"
  done
  gwn=$(jq -r '.[0].n // empty' <<< "$gw")
  if [ -n "$gwn" ]; then
    fwid=$(hz 'servers?per_page=50' | jq -r --arg n "$gwn" '.servers[] | select(.name == $n) | .public_net.firewalls[].id')
    ok=
    for f in $fwid; do hz "firewalls/$f" | jq -e '.firewall.rules[] | select(.direction == "in" and .protocol == "udp" and .port == "51821" and (.source_ips | index("::/0")))' >/dev/null && ok=1; done
    check N7 "the gateway server's Hetzner firewall lets udp 51821 in from ::/0" test -n "$ok"
  fi
else
  pend N7 "network yyc routes and the gateway's firewall rule: set YYC_HETZNER_READ"
fi
pend N7 "the hermit-gateway pod (WP-19's image) on the gateway node: wg0 and its nft table, VIP 10.202.1.1:3128 DNAT with source kept, 1 MiB each way, reply path on the private interface from 10.0.0.1, <gw>:30433, bridge NodePorts from 10.0.255.2, kill-and-reapply leaves wg0, nft, table 51822 and its rule unchanged"

# ---- N8 / I8 ----
WF=$ROOT/.github/workflows/talos-nodes.yml NW=$ROOT/news-stream/deploy/clusters/talos/node-work.sh
check N8 "the upgrade workflow and node-work.sh exist" bash -c "[ -f '$WF' ] && [ -x '$NW' ]"
dsus=$(k -n kube-system get cronjob descheduler -o jsonpath='{.spec.suspend}' 2>/dev/null)
if [ -z "$dsus" ]; then fail N8 "the Descheduler CronJob kube-system/descheduler exists"
else pass N8 "the Descheduler CronJob exists (suspend=$dsus)"; fi
pol=$(k -n kube-system get configmap descheduler-policy -o jsonpath='{.data.policy\.yaml}' 2>/dev/null)
check N8 "the live Descheduler policy (ConfigMap kube-system/descheduler-policy) exists" test -n "$pol"
[ -n "$pol" ] && check N8 "… on every plugin (evictableNamespaces.exclude)" python3 -c '
import sys, yaml
p = yaml.safe_load(sys.stdin.read())
ok = True
for prof in p["profiles"]:
    for pc in prof["pluginConfig"]:
        if pc["name"] == "DefaultEvictor": continue
        ex = ((pc.get("args") or {}).get("evictableNamespaces") or {}).get("exclude") or []
        ok = ok and {"db", "yyc-ai-fabric"} <= set(ex)
sys.exit(0 if ok else 1)' <<< "$pol"
if [ -n "$pol" ] && command -v crane >/dev/null; then
  W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
  img=$(k -n kube-system get cronjob descheduler -o jsonpath='{.spec.jobTemplate.spec.template.spec.containers[0].image}')
  if crane export --platform linux/amd64 "$img" - 2>/dev/null | tar -x -C "$W" bin/descheduler 2>/dev/null; then
    printf '%s\n' "$pol" > "$W/policy.yaml"
    "$W/bin/descheduler" --kubeconfig "$KT" --policy-config-file "$W/policy.yaml" --dry-run --v=3 > "$W/out" 2>&1
    hits=$(grep -E 'Evicted pod' "$W/out" | grep -E 'pod="(db|yyc-ai-fabric)/' || true)
    check N8 "descheduler --dry-run with the live policy evicts no pod in db or yyc-ai-fabric ($(grep -c 'Evicted pod' "$W/out") evictions in all)" test -z "$hits"
  else pend N8 "descheduler --dry-run: could not extract the binary from $img"; fi
fi

# ---- N9 ----
for ns in yyc-ai yyc-ai-fabric yyc-deploy; do
  check N9 "yyc-deploy/sealed-secrets-applier can create SealedSecrets in $ns" \
    bash -c "kubectl --kubeconfig '$KT' auth can-i create sealedsecrets.bitnami.com -n $ns --as=system:serviceaccount:yyc-deploy:sealed-secrets-applier >/dev/null 2>&1"
  check N9 "… and nothing else there (no Secrets, no Deployments)" \
    bash -c "! kubectl --kubeconfig '$KT' auth can-i get secrets -n $ns --as=system:serviceaccount:yyc-deploy:sealed-secrets-applier >/dev/null 2>&1 && ! kubectl --kubeconfig '$KT' auth can-i create deployments -n $ns --as=system:serviceaccount:yyc-deploy:sealed-secrets-applier >/dev/null 2>&1"
done

# ---- N10 / I9: the old cluster's footing ----
if ON=$(old get nodes -o json 2>/dev/null); then
  for n in yyc-master1 yyc-pool-db-worker1 yyc-pool-apps-worker2 yyc-pool-large-worker1 yyc-pool-large-worker2; do
    check N10 "old cluster keeps $n (Ready)" test "$(jq -r --arg n "$n" '.items[] | select(.metadata.name == $n) | [.status.conditions[] | select(.type == "Ready")][0].status' <<< "$ON")" = True
  done
  check N10 "old cluster keeps Flux" quiet old get ns flux-system
  OP=$(old get pods -A -o json --field-selector=status.phase!=Succeeded,status.phase!=Failed)
  room=$(python3 - "$OLD_PEAK_EXTRA_MI" <(echo "$ON") <(echo "$OP") <<'PY'
import json, sys
def mi(v):
    for s, f in (("Ki", 1/1024), ("Mi", 1), ("Gi", 1024), ("k", 1000/1048576), ("M", 1e6/1048576), ("G", 1e9/1048576)):
        if v.endswith(s): return float(v[:-len(s)]) * f
    return float(v) / 1048576
extra = int(sys.argv[1]); nodes = json.load(open(sys.argv[2])); pods = json.load(open(sys.argv[3]))
alloc = {n["metadata"]["name"]: mi(n["status"]["allocatable"]["memory"]) for n in nodes["items"]}
req = {k: 0.0 for k in alloc}
for p in pods["items"]:
    n = p["spec"].get("nodeName")
    if n in req:
        req[n] += sum(mi(c.get("resources", {}).get("requests", {}).get("memory", "0")) for c in p["spec"]["containers"])
free_large = [n for n in ("yyc-pool-large-worker1", "yyc-pool-large-worker2") if n in alloc and alloc[n] - req[n] >= 1536]
ta, tr = sum(alloc.values()), sum(req.values())
print(f"{int(ta)} {int(tr)} {int(tr + extra)} {len(free_large)}")
PY
)
  read -r ta tr peak fl <<< "$room"
  check N10 "old side's peak fits: requests $tr Mi + $OLD_PEAK_EXTRA_MI Mi = $peak Mi <= $ta Mi allocatable" test "$peak" -le "$ta"
  check N10 "two 1.5 GiB surge slots on two different 8 GB nodes ($fl of the large workers have 1536 Mi free)" test "$fl" -ge 2
else
  pend N10 "the old cluster's nodes, Flux and room: set OLD_MASTER (and SECRETS)"
fi

# ---- N11 ----
pend N11 "order T4 (yyc-mcp, capture route, monitor after A5), T5 after A6 and T4, T6 last: the builder's task record"

# ---- N12 ----
NP=$(k get pods -A -o json --field-selector=status.phase!=Succeeded,status.phase!=Failed)
room=$(python3 - <(echo "$NODES") <(echo "$NP") <<'PY'
import json, sys
def mi(v):
    for s, f in (("Ki", 1/1024), ("Mi", 1), ("Gi", 1024), ("k", 1000/1048576), ("M", 1e6/1048576), ("G", 1e9/1048576)):
        if v.endswith(s): return float(v[:-len(s)]) * f
    return float(v) / 1048576
nodes = json.load(open(sys.argv[1])); pods = json.load(open(sys.argv[2]))
use = {n["metadata"]["name"]: mi(n["status"]["allocatable"]["memory"]) for n in nodes["items"]
       if n["metadata"]["labels"].get("yyc.hetzner/network") == "yyc" and not n["spec"].get("unschedulable")}
req = {k: 0.0 for k in use}
for p in pods["items"]:
    n = p["spec"].get("nodeName")
    if n in req:
        req[n] += sum(mi(c.get("resources", {}).get("requests", {}).get("memory", "0")) for c in p["spec"]["containers"])
free = sorted((int(use[n] - req[n]) for n in use), reverse=True)
print(sum(free), " ".join(map(str, free)) or "-")
PY
)
total=${room%% *} each=${room#* }
check N12 "free memory on the nodes YYC AI may use: $total Mi >= $NEED_MI Mi (per node: $each)" test "${total:-0}" -ge "$NEED_MI"
check N12 "at least three of them with >= $PER_NODE_MI Mi free" \
  test "$(tr ' ' '\n' <<< "$each" | awk -v m="$PER_NODE_MI" '$1 >= m' | grep -c .)" -ge 3

echo "---"
[ "$fails" = 0 ] && echo "no FAIL (PENDING lines still need their own test)" || echo "$fails FAIL"
[ "$fails" = 0 ]
