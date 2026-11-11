# ghcr.io/de0ch/hermit-jarvis (aviary/README.md): hermit-jarvis alone, as uid 65532, on distroless (glibc, CA
# certificates). Built by .github/workflows/aviary.yml; DE0CH/services pins it (its chat-pin workflow).
FROM rust:1.99.1-bookworm AS build
WORKDIR /src
# the commit it is built from (aviary-core's `ver` in every log line and /version; `dev` when unset)
ARG AVIARY_REVISION=dev
ENV AVIARY_REVISION=${AVIARY_REVISION}
COPY . .
RUN cargo build --release --locked --bin hermit-jarvis && install -D target/release/hermit-jarvis /out/hermit-jarvis

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/hermit-jarvis /usr/local/bin/hermit-jarvis
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/hermit-jarvis"]
