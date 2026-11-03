# ghcr.io/de0ch/sandboxd:<sha12> (platform 5.2): sandboxd in gateway mode as the main process of hermit's gateway
# DaemonSet, a host-network privileged pod on one cluster node (hermit spec 14.2; crates/sandboxd/docs/HOST.md
# "Gateway"). It holds the static sandboxd binary and the commands it runs on a gateway: ip (iproute2), wg, nft,
# sysctl (procps), and crane, tar and zstd for the image service (:7444). No installer and nothing written on the node
# but the host network namespace's wg0, nft table `ip sbxhost`, rules and routes (kept across the pod's restarts) and
# the state directory, which the pod mounts from the node:
#   /var/lib/sbx     hostPath /var/lib/hermit-gateway (saved peer set and egress targets, the image cache)
#   /etc/sbx/host.json            the deployment's host.json (ConfigMap), SBX_CONFIG
#   the TLS files and wg.key      where host.json's `tls` and `wgKeyFile` name them (Secret)
# Its context needs only dist/sandboxd (the static musl binary, cargo build --profile sandboxd); ci.yml's `images` job (images/build.sh)
# builds it from that:
#   docker build -f images/sandboxd/Dockerfile --build-arg REVISION=<40 hex> <context>
FROM alpine:3.22@sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f

ARG REVISION=dev
ARG CRANE_VERSION=0.20.6
ARG CRANE_SHA256=c1d593d01551f2c9a3df5ca0a0be4385a839bd9b86d4a76e18d7b17d16559127

ADD --checksum=sha256:${CRANE_SHA256} \
    https://github.com/google/go-containerregistry/releases/download/v${CRANE_VERSION}/go-containerregistry_Linux_x86_64.tar.gz \
    /tmp/crane.tgz
RUN apk add --no-cache iproute2 nftables wireguard-tools-wg procps-ng tar zstd \
 && tar -xzf /tmp/crane.tgz -C /usr/local/bin crane && rm /tmp/crane.tgz
COPY --chmod=755 dist/sandboxd /usr/local/bin/sandboxd

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="sandboxd: hermit's sandbox host agent, run here as the fabric's gateway"
ENV SBX_CONFIG=/etc/sbx/host.json
USER root
ENTRYPOINT ["/usr/local/bin/sandboxd"]
