# ghcr.io/de0ch/hermit-jarvis (aviary/README.md): hermit-jarvis alone, as uid 65532, on distroless (glibc, CA
# certificates). Built by .github/workflows/aviary.yml; DE0CH/services pins it (its chat-pin workflow).
FROM rust:1.99.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin hermit-jarvis && install -D target/release/hermit-jarvis /out/hermit-jarvis

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/hermit-jarvis /usr/local/bin/hermit-jarvis
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/hermit-jarvis"]
