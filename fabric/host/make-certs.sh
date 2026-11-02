#!/usr/bin/env bash
# Makes the PKI between hermit and sandboxd (crates/sandboxd/docs/HOST.md, "Security"), into DIR (mode 700):
#   ca.crt / ca.key           the CA "hermit fabric CA" (EC P-256, 10 years). ca.key only signs: keep it offline (in
#                             the deployment's secret store only)
#   gateway.crt / .key        the gateway's server certificate (CN and SAN "sandboxd")
#   host.crt / .key           the AWS hosts' shared server certificate (CN and SAN "sandboxd"; each new host gets it
#                             in its user data; WireGuard already tells hosts apart)
#   hermit.crt / .key         hermit's client certificate (CN hermit, clientAuth: sandboxd accepts only it)
# Leaf certificates last 3 years. Prints only file names.
# Usage: make-certs.sh DIR [leaf...]   (leaf: gateway host hermit; default all; an existing CA is reused)
set -euo pipefail
dir=${1:?dir}; shift
leaves=${*:-gateway host hermit}
umask 077; mkdir -p "$dir"; cd "$dir"
if [ ! -s ca.key ]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout ca.key -out ca.crt -days 3650 \
    -subj "/CN=hermit fabric CA" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
fi
for leaf in $leaves; do
  case $leaf in
    gateway|host) cn=sandboxd; ext="subjectAltName=DNS:sandboxd
extendedKeyUsage=serverAuth" ;;
    hermit) cn=hermit; ext="extendedKeyUsage=clientAuth" ;;
    *) echo "unknown leaf $leaf" >&2; exit 2 ;;
  esac
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$leaf.key" -out "$leaf.csr" -subj "/CN=$cn" 2>/dev/null
  printf '%s\nbasicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\n' "$ext" > "$leaf.ext"
  openssl x509 -req -in "$leaf.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 1095 -extfile "$leaf.ext" -out "$leaf.crt" 2>/dev/null
  rm -f "$leaf.csr" "$leaf.ext"
  echo "$dir/$leaf.crt"
done
