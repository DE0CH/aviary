# ghcr.io/de0ch/hermit:<sha12> (hermit spec 8.5, platform 5.2): the static musl `hermit` binary and the `juicefs`
# 1.4.1 CLI (sha256-checked here; hermit runs `juicefs quota`, `clone` and `rmr` for workspaces, hermit 15.5) on
# distroless static, uid 65532, read-only root. Its context needs only dist/hermit (the static musl binary of ci.yml's
# `build` job); ci.yml builds it from a context of that one file:
#   docker build -f images/hermit/Dockerfile --build-arg REVISION=<40 hex> <context>
# linux/amd64 and linux/arm64 (ci.yml builds each natively): BuildKit's TARGETARCH picks the juicefs stage, each with
# the digest the release's own checksums.txt lists
FROM busybox:1.37.0@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec AS juicefs-amd64
ARG JUICEFS_VERSION=1.4.1
ADD --checksum=sha256:01ee09a21a9351a465e09906f113845e1c6a19bea70f530e5e2b2125b2dd3b82 \
    https://github.com/juicedata/juicefs/releases/download/v${JUICEFS_VERSION}/juicefs-${JUICEFS_VERSION}-linux-amd64.tar.gz \
    /tmp/juicefs.tgz

FROM busybox:1.37.0@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec AS juicefs-arm64
ARG JUICEFS_VERSION=1.4.1
ADD --checksum=sha256:1015ade83a7a93180a29f6c93ee5780a3eda52522331934ef2ef0cc0921995fd \
    https://github.com/juicedata/juicefs/releases/download/v${JUICEFS_VERSION}/juicefs-${JUICEFS_VERSION}-linux-arm64.tar.gz \
    /tmp/juicefs.tgz

FROM juicefs-${TARGETARCH} AS juicefs
RUN mkdir -p /out && tar -xzf /tmp/juicefs.tgz -C /out juicefs && chmod 755 /out/juicefs

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

ARG REVISION=dev

COPY --from=juicefs /out/juicefs /usr/local/bin/juicefs
COPY --chmod=755 dist/hermit /usr/local/bin/hermit

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="hermit: sandboxes, people and workspaces"
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/hermit"]
CMD ["serve"]
