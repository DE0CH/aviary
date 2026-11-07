#!/usr/bin/env bash
# Generates one run's throwaway secrets and the files that name run-specific values (platform 7.1): pair tokens,
# hermit's token keys, the fabric PKI (fabric/host/make-certs.sh), the harness CA with fake-access's and the stand-in
# EC2's certificates, the Access key pair, hermit.json and its files, and the Kubernetes objects that carry them.
#
#   gen.sh <dir> <sandbox image digest sha256:…> <docker subnet> <postgres address> <s3 address>
#
# Reads PG_PASSWORD, S3_ACCESS_KEY, S3_SECRET_KEY and JFS_META_PASSWORD from <dir>/infra.env (run.sh writes it).
# Writes <dir>/k8s/*.yaml (applied after testing/e2e/k8s) and <dir>/driver.env (the driver's tokens). Nothing here is
# a real credential: every value is new per run and dies with the cluster.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mkdir -p "$1" && cd "$1" && pwd)
digest=${2:?sandbox image digest}
subnet=${3:?docker subnet}
pg=${4:?postgres address}
s3=${5:?s3 address}
# shellcheck source=/dev/null
. "$dir/infra.env"
umask 077
out=$dir/k8s
mkdir -p "$out"
ns=aviary-e2e
region=ap-east-1

tok() { openssl rand -hex 24; }
b64url32() { openssl rand 32 | base64 -w0 | tr '+/' '-_' | tr -d '='; }
b64() { base64 -w0 < "$1"; }

# --- pair tokens (platform 3.7: <CALLER>_TOKEN_<CALLEE> = <CALLEE>_ACCEPT_<CALLER>)
pigeon_magpie=$(tok); pigeon_hermit=$(tok); pigeon_model=$(tok)
magpie_hermit=$(tok); magpie_import=$(tok)
hermit_magpie=$(tok); hermit_egress=$(tok)
gateway_magpie=$(tok); gateway_hermit=$(tok); gateway_tools=$(tok); model_key=$(tok)
admin_magpie=$(tok); admin_hermit=$(tok); monitor_hermit=$(tok); tools_hermit=$(tok)

# --- PKI: the fabric's (hermit <-> sandboxd) and the harness CA (fake-access, EC2's name)
"$here/../../fabric/host/make-certs.sh" "$dir/fabric" >/dev/null
ca=$dir/ca
mkdir -p "$ca"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$ca/ca.key" -out "$ca/ca.crt" \
  -days 2 -subj "/CN=aviary-e2e CA" -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
leaf() { # leaf <name> <dns name>
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$ca/$1.key.ec" -out "$ca/$1.csr" \
    -subj "/CN=$2" 2>/dev/null
  openssl pkcs8 -topk8 -nocrypt -in "$ca/$1.key.ec" -out "$ca/$1.key"
  printf 'subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' "$2" > "$ca/$1.ext"
  openssl x509 -req -in "$ca/$1.csr" -CA "$ca/ca.crt" -CAkey "$ca/ca.key" -CAcreateserial -days 2 \
    -extfile "$ca/$1.ext" -out "$ca/$1.crt" 2>/dev/null
}
leaf access "access.$ns.svc.cluster.local"
leaf ec2 "ec2.$region.amazonaws.com"
for k in k1 k2; do
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$ca/$k.pem" 2>/dev/null
done
# the fabric's server keys in PKCS#8 (rustls reads either; one form throughout)
for l in gateway host hermit; do
  openssl pkcs8 -topk8 -nocrypt -in "$dir/fabric/$l.key" -out "$dir/fabric/$l.p8"
done

pg_url() { echo "postgres://$1:$PG_PASSWORD@$pg:5432/$1?sslmode=require"; }
jfs_meta="postgres://jfs@$pg:5432/jfs?sslmode=require"

# --- hermit.json (testing/e2e/hermit.json with this run's image)
jq --arg image "docker.io/aviary-e2e/hermit-sandbox:e2e@$digest" '.sandbox.image = $image' \
  "$here/hermit.json" > "$dir/hermit.json"

# --- the gateway host's config (hermit's user-data shape, sandboxd role hub)
gw_key=$(openssl rand 32 | base64 -w0)
jq -n --arg key "$gw_key" --rawfile ca "$dir/fabric/ca.crt" --rawfile cert "$dir/fabric/gateway.crt" \
  --rawfile k "$dir/fabric/gateway.p8" '{
  host: {name: "hermit-gw", role: "hub", gateway: true, wgAddress: "10.52.0.1", reserveMemory: 0},
  wgPrivateKey: $key, tls: {ca: $ca, cert: $cert, key: $k}}' > "$dir/gateway-host.json"

secret_env() { # secret_env <name> KEY=VALUE…
  local name=$1; shift
  printf 'apiVersion: v1\nkind: Secret\nmetadata: {name: %s, namespace: %s}\ntype: Opaque\nstringData:\n' "$name" "$ns"
  for kv in "$@"; do
    printf '  %s: %s\n' "${kv%%=*}" "$(jq -Rn --arg v "${kv#*=}" '$v')"
  done
  echo ---
}
secret_files() { # secret_files <name> KEY=PATH…
  local name=$1; shift
  printf 'apiVersion: v1\nkind: Secret\nmetadata: {name: %s, namespace: %s}\ntype: Opaque\ndata:\n' "$name" "$ns"
  for kv in "$@"; do
    printf '  %s: %s\n' "${kv%%=*}" "$(b64 "${kv#*=}")"
  done
  echo ---
}

{
  secret_env pigeon-env \
    "PIGEON_TOKEN_MAGPIE=$pigeon_magpie" "PIGEON_TOKEN_HERMIT=$pigeon_hermit" "PIGEON_TOKEN_MODEL=$pigeon_model" \
    "PIGEON_VIEW_AS_KEYS=$(b64url32)" "PIGEON_BLOB_KEYS=$(b64url32)"
  secret_env magpie-env \
    "MAGPIE_DATABASE_URL=$(pg_url magpie)" \
    "MAGPIE_S3_ENDPOINT=http://$s3:9000" "MAGPIE_S3_REGION=us-east-1" "MAGPIE_S3_BUCKET=magpie" \
    "MAGPIE_S3_ACCESS_KEY_ID=$S3_ACCESS_KEY" "MAGPIE_S3_SECRET_ACCESS_KEY=$S3_SECRET_KEY" \
    "MAGPIE_ACCEPT_PIGEON=$pigeon_magpie" "MAGPIE_ACCEPT_GATEWAY=$gateway_magpie" \
    "MAGPIE_ACCEPT_HERMIT=$hermit_magpie" "MAGPIE_ACCEPT_ADMIN=$admin_magpie" \
    "MAGPIE_TOKEN_HERMIT=$magpie_hermit" "MAGPIE_TOKEN_IMPORT=$magpie_import"
  secret_env hermit-env \
    "HERMIT_DATABASE_URL=$(pg_url hermit)" "HERMIT_TOKEN_KEYS=1:$(b64url32)" \
    "HERMIT_ACCEPT_PIGEON=$pigeon_hermit" "HERMIT_ACCEPT_MAGPIE=$magpie_hermit" \
    "HERMIT_ACCEPT_GATEWAY=$gateway_hermit" "HERMIT_ACCEPT_TOOLS=$tools_hermit" \
    "HERMIT_ACCEPT_MONITOR=$monitor_hermit" "HERMIT_ACCEPT_ADMIN=$admin_hermit" \
    "HERMIT_TOKEN_MAGPIE=$hermit_magpie" "HERMIT_TOKEN_EGRESS=$hermit_egress" \
    "HERMIT_FABRIC_CA=$(cat "$dir/fabric/ca.crt")" \
    "HERMIT_FABRIC_CLIENT_CERT=$(cat "$dir/fabric/hermit.crt")" "HERMIT_FABRIC_CLIENT_KEY=$(cat "$dir/fabric/hermit.p8")" \
    "HERMIT_FABRIC_HOST_CERT=$(cat "$dir/fabric/host.crt")" "HERMIT_FABRIC_HOST_KEY=$(cat "$dir/fabric/host.p8")" \
    "HERMIT_AWS_ACCESS_KEY_ID=AKIAE2E0000000000000" "HERMIT_AWS_SECRET_ACCESS_KEY=$(tok)" \
    "HERMIT_JUICEFS_META_URL=$jfs_meta" "HERMIT_JUICEFS_META_PASSWORD=$JFS_META_PASSWORD"
  secret_env gateway-env \
    "GATEWAY_TOKEN_MAGPIE=$gateway_magpie" "GATEWAY_TOKEN_HERMIT=$gateway_hermit" \
    "GATEWAY_TOKEN_TOOLS=$gateway_tools" "GATEWAY_MODEL_KEY=$model_key" \
    "GATEWAY_ACCEPT_PIGEON=$pigeon_model" "GATEWAY_ACCEPT_HERMIT=$hermit_egress"
  secret_env tools-token "token=$gateway_tools"
  secret_files harness-ca "ca.crt=$ca/ca.crt"
  secret_files access-keys "k1.pem=$ca/k1.pem" "k2.pem=$ca/k2.pem" "tls.crt=$ca/access.crt" "tls.key=$ca/access.key"
  secret_files fake-ec2-tls "tls.crt=$ca/ec2.crt" "tls.key=$ca/ec2.key"
  secret_files fabric-gateway "host.json=$dir/gateway-host.json"
  secret_env driver-env \
    "E2E_MAGPIE_ADMIN=$admin_magpie" "E2E_HERMIT_ADMIN=$admin_hermit" "E2E_HERMIT_MONITOR=$monitor_hermit" \
    "E2E_MAGPIE_PIGEON=$pigeon_magpie" "E2E_HERMIT_MAGPIE=$magpie_hermit"
  secret_files driver-ca "ca.crt=$ca/ca.crt"
} > "$out/secrets.yaml"

{
  printf 'apiVersion: v1\nkind: ConfigMap\nmetadata: {name: hermit-config, namespace: %s}\ndata:\n' "$ns"
  for f in "$dir/hermit.json" "$here/hermit/system-prompt.md" "$here/hermit/sandbox-rules.md" "$here/hermit/title-system.md"; do
    printf '  %s: %s\n' "$(basename "$f")" "$(jq -Rs . < "$f")"
  done
  echo ---
  printf 'apiVersion: v1\nkind: ConfigMap\nmetadata: {name: hermit-catalogue, namespace: %s}\ndata:\n' "$ns"
  printf '  catalogue.json: %s\n' "$(jq -Rs . < "$here/hermit/catalogue.json")"
} > "$out/config.yaml"

# Postgres and S3 run beside the cluster on its Docker network; the API server is the node's :6443
cat > "$out/external.yaml" <<YAML
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: external, namespace: $ns}
spec:
  podSelector: {matchExpressions: [{key: app, operator: In, values: [magpie, hermit]}]}
  policyTypes: [Egress]
  egress:
    - to: [{ipBlock: {cidr: $pg/32}}]
      ports: [{port: 5432}]
    - to: [{ipBlock: {cidr: $s3/32}}]
      ports: [{port: 9000}]
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: api-server, namespace: $ns}
spec:
  podSelector: {matchExpressions: [{key: app, operator: In, values: [hermit, fake-ec2, fabric-gateway, fake-host, driver]}]}
  policyTypes: [Egress]
  egress:
    - to: [{ipBlock: {cidr: $subnet}}]
      ports: [{port: 6443}]
---
apiVersion: v1
kind: ConfigMap
metadata: {name: coredns-custom, namespace: kube-system}
data:
  # hermit's EC2 client calls EC2's regional name; in the harness that name is the stand-in
  ec2.override: |
    rewrite name exact ec2.$region.amazonaws.com fake-ec2.$ns.svc.cluster.local
YAML

cat > "$dir/driver.env" <<ENV
E2E_MAGPIE_ADMIN=$admin_magpie
E2E_HERMIT_ADMIN=$admin_hermit
E2E_HERMIT_MONITOR=$monitor_hermit
E2E_MAGPIE_PIGEON=$pigeon_magpie
E2E_HERMIT_MAGPIE=$magpie_hermit
ENV
echo "$out"
