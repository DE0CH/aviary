#!/usr/bin/env bash
# Makes the PKI between the controller (web role) and sandboxd (docs/SANDBOX-VM.md, "Security"), into DIR (mode 700):
#   ca.crt / ca.key           the CA (10 years). Keep ca.key offline (e.g. in an encrypted file), it only signs.
#   hub.crt / hub.key         the hub's server certificate (name "sandboxd")
#   aws.crt / aws.key         the AWS burst hosts' server certificate (name "sandboxd"; each new host gets it in its
#                             user data; WireGuard already tells hosts apart)
#   controller.crt / .key     the controller's client certificate (CN sandbox-controller: sandboxd accepts only it)
# Leaf certificates last 3 years. Prints only file names.
# Usage: make-certs.sh DIR [leaf...]   (leaf: hub aws controller; default all; an existing CA is reused)
set -euo pipefail
dir=${1:?dir}; shift
leaves=${*:-hub aws controller}
umask 077; mkdir -p "$dir"; cd "$dir"
if [ ! -s ca.key ]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout ca.key -out ca.crt -days 3650 \
    -subj "/CN=YYC AI sandboxd CA" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
fi
for leaf in $leaves; do
  case $leaf in
    hub|aws) cn=sandboxd; ext="subjectAltName=DNS:sandboxd
extendedKeyUsage=serverAuth" ;;
    controller) cn=sandbox-controller; ext="extendedKeyUsage=clientAuth" ;;
    *) echo "unknown leaf $leaf" >&2; exit 2 ;;
  esac
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$leaf.key" -out "$leaf.csr" -subj "/CN=$cn" 2>/dev/null
  printf '%s\nbasicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\n' "$ext" > "$leaf.ext"
  openssl x509 -req -in "$leaf.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 1095 -extfile "$leaf.ext" -out "$leaf.crt" 2>/dev/null
  rm -f "$leaf.csr" "$leaf.ext"
  echo "$dir/$leaf.crt"
done
