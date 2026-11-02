#!/bin/bash
# runs ON a wp19 host: $1 = a|b. Installs the fabric host setup (setup.sh hub on a, burst on b), the hand-made
# python image with mapcount.py, host.json, and starts sandboxd.
set -euo pipefail
side=$1; cd /root/wp19
export GV=precopy-20261102.1
apt-get -qq update >/dev/null; apt-get -qq install -y jq rsync >/dev/null
mkdir -p /etc/sbx/tls; install -m600 pki/ca.crt pki/host.crt pki/host.key /etc/sbx/tls/
if [ "$side" = a ]; then
  cat > /root/wp19/host.json <<J
{"name":"wp19-a","role":"hub","wgAddress":"10.99.0.1","hostCidr":"10.99.0.0/24","wgPort":51821,"controlCidr":"10.97.0.0/16",
 "sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","privateGateway":"10.77.0.1","reserveMemory":1073741824}
J
fi
mkdir -p fabric/bin; cp -f bin/sandboxd fabric/bin/sandboxd
# the image (before sandboxd starts, so b still has egress through a)
if ! command -v crane >/dev/null; then curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.20.6/go-containerregistry_Linux_x86_64.tar.gz | tar -xz -C /usr/local/bin crane; fi
cat > /root/wp19/img-config.json <<'J'
{"architecture":"amd64","os":"linux","config":{"Env":["PATH=/usr/local/bin:/usr/bin:/bin","LANG=C.UTF-8"],"Entrypoint":["/bin/sh","-c","mkdir -p /tmp/mc && exec python3 /opt/mapcount.py /tmp/mc/counter /tmp/mc/log.jsonl"]}}
J
D=$(sha256sum /root/wp19/img-config.json | cut -c1-64); dir=/var/lib/sbx/images/$D
if [ ! -f "$dir/.done" ]; then
  rm -rf "$dir"; mkdir -p "$dir/rootfs"; crane export python:3.12-slim - | tar -x -C "$dir/rootfs"
  install -m755 mapcount.py "$dir/rootfs/opt/mapcount.py"; cp img-config.json "$dir/config.json"; touch "$dir/.done"
fi
echo "$D" > /root/wp19/digest
if [ "$side" = a ]; then
  SBX_HOST_JSON=/root/wp19/host.json bash fabric/host/setup.sh hub 2>&1 | tail -2
else
  bash fabric/host/setup.sh burst 2>&1 | tail -1
  (umask 077; wg genkey > /etc/sbx/wg.key)
  hubkey=$(cat /root/wp19/hub.pub)
  cat > /etc/sbx/host.json <<J
{"name":"wp19-b","role":"burst","wgAddress":"10.99.0.10","hostCidr":"10.99.0.0/24","wgPort":51821,"controlCidr":"10.97.0.0/16",
 "sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","imageSource":"http://10.99.0.1:7444","reserveMemory":1073741824,
 "hub":{"publicKey":"$hubkey","endpoint":"10.77.0.2:51821","allowedIps":["10.99.0.0/24","10.97.0.0/16","10.99.1.1/32"]},
 "tls":{"ca":"/etc/sbx/tls/ca.crt","cert":"/etc/sbx/tls/host.crt","key":"/etc/sbx/tls/host.key"},"wgKeyFile":"/etc/sbx/wg.key"}
J
  chmod 600 /etc/sbx/host.json
  systemctl start run-sbx-ckpt.mount; systemctl restart sandboxd; sleep 2; systemctl is-active sandboxd; wg show wg0 public-key
fi
