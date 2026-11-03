#!/usr/bin/env bash
# The harness's tools on a hosted runner (testing/e2e/README.md): k3d 5.8.3 and juicefs 1.4.1, sha256-checked.
# kubectl, docker and jq are the runner's own. Used by ci.yml `e2e` and nightly.yml's harness jobs.
set -euo pipefail
curl -sSfL -o /tmp/k3d https://github.com/k3d-io/k3d/releases/download/v5.8.3/k3d-linux-amd64
echo "dbaa79a76ace7f4ca230a1ff41dc7d8a5036a8ad0309e9c54f9bf3836dbe853e  /tmp/k3d" | sha256sum -c -
sudo install -m 755 /tmp/k3d /usr/local/bin/k3d
curl -sSfL -o /tmp/juicefs.tgz https://github.com/juicedata/juicefs/releases/download/v1.4.1/juicefs-1.4.1-linux-amd64.tar.gz
echo "01ee09a21a9351a465e09906f113845e1c6a19bea70f530e5e2b2125b2dd3b82  /tmp/juicefs.tgz" | sha256sum -c -
sudo tar -xzf /tmp/juicefs.tgz -C /usr/local/bin juicefs
k3d version && juicefs version && kubectl version --client
