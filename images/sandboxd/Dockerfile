# ghcr.io/de0ch/sandboxd:<sha12> (platform 5.2): the installer of hermit's gateway DaemonSet (hermit spec 14.2). Its
# initContainer copies /opt/hermit-gateway (sandboxd, crane, gateway.sh) to the node and runs gateway.sh there; the
# main container sleeps. Built from the repository root after the `build` job: dist/sandboxd is the static musl
# binary (cargo build --profile sandboxd).
#   docker build -f images/sandboxd/Dockerfile --build-arg REVISION=<40 hex> .
FROM busybox:1.37.0@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec

ARG REVISION=dev
ARG CRANE_VERSION=0.20.6
ARG CRANE_SHA256=c1d593d01551f2c9a3df5ca0a0be4385a839bd9b86d4a76e18d7b17d16559127

ADD --checksum=sha256:${CRANE_SHA256} \
    https://github.com/google/go-containerregistry/releases/download/v${CRANE_VERSION}/go-containerregistry_Linux_x86_64.tar.gz \
    /tmp/crane.tgz
RUN mkdir -p /opt/hermit-gateway && tar -xzf /tmp/crane.tgz -C /opt/hermit-gateway crane && rm /tmp/crane.tgz \
 && printf '#!/bin/sh\nexec sleep 2147483647\n' > /bin/sleep-forever && chmod 755 /bin/sleep-forever
COPY --chmod=755 dist/sandboxd fabric/host/gateway.sh /opt/hermit-gateway/

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="sandboxd and the gateway installer (hermit's fabric)"
USER root
CMD ["/bin/sleep-forever"]
