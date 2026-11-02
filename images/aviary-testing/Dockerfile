# ghcr.io/de0ch/aviary-testing:<sha12> (platform 5.2): every testing/* binary in one image, for the harness and for
# other repositories' conformance jobs (jarvis3's egress runs `aviary-conformance gateway` and `model` from it,
# platform 17.3). The container's command names the binary (`aviary-conformance gateway …`, `test-gateway`,
# `mock-model …`); PATH has /usr/local/bin. Its context needs only the static musl binaries in dist/; ci.yml's `build`
# job builds it from a context of those:
#   docker build -f images/aviary-testing/Dockerfile --build-arg REVISION=<40 hex> <context>
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

ARG REVISION=dev

COPY --chmod=755 dist/aviary-conformance dist/test-gateway dist/mock-model dist/fake-tools dist/fake-access \
     dist/fake-sandboxd /usr/local/bin/

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="aviary's test binaries: conformance suites, reference gateway, fakes"
USER 65532:65532
