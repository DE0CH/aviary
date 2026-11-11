# ghcr.io/de0ch/herald:<sha12> (crates/herald/README.md): the static musl binary on distroless, uid 65532, for a
# read-only root filesystem. Its context needs only dist/herald; ci.yml's `images` job builds it with images/build.sh:
#   docker build -f images/herald/Dockerfile --build-arg REVISION=<40 hex> <context>
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

ARG REVISION=dev

COPY --chmod=755 dist/herald /usr/local/bin/herald

LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="herald: Telegram notification channels for aviary conversations"
USER 65532:65532
EXPOSE 8080 9090
ENTRYPOINT ["/usr/local/bin/herald"]
CMD ["serve"]
