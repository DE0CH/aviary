# ghcr.io/de0ch/magpie:<sha12> (platform 5.2, magpie 18.1): the glibc `magpie` binary of ci.yml's `build` job (built
# on Debian trixie, .github/ci/build-magpie.sh) on debian:trixie-slim with libheif1 and its HEVC decoder plugin
# libheif-plugin-libde265 (without the plugin libheif reads no HEIC and every HEIC upload answers 415), uid 10001.
# Trixie, not platform 5.2's bookworm: libheif-rs needs libheif 1.17 or later and bookworm has 1.15 (README "Where
# this differs"). Its context needs only dist/magpie; ci.yml builds it and decodes a HEIC photo with it:
#   docker build -f images/magpie/Dockerfile --build-arg REVISION=<40 hex> <context>
FROM debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4

ARG REVISION=dev

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libheif1 libheif-plugin-libde265 \
 && rm -rf /var/lib/apt/lists/*

COPY --chmod=755 dist/magpie /usr/local/bin/magpie

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="magpie: conversations, timelines and blobs of aviary"
USER 10001:10001
EXPOSE 8080 9090
ENTRYPOINT ["/usr/local/bin/magpie"]
CMD ["serve"]
