# ghcr.io/de0ch/pigeon:<sha12> (platform 5.2, pigeon spec 3.4): the static musl binary on distroless, uid 65532, for a
# read-only root filesystem. The app bundle is not in this image: the pod mounts it at /srv/app (pigeon spec 17.1).
# Its context needs only dist/pigeon; ci.yml's `build` job builds it from a context of that file:
#   docker build -f images/pigeon/Dockerfile --build-arg REVISION=<40 hex> <context>
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

ARG REVISION=dev

COPY --chmod=755 dist/pigeon /usr/local/bin/pigeon

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="pigeon: the app-facing gateway of aviary"
USER 65532:65532
EXPOSE 8080 9090
ENTRYPOINT ["/usr/local/bin/pigeon"]
CMD ["serve"]
