# YYC Drive: the page (this repository's build) and the server (server/) in one image.
# Build from the repository's root: docker build -f server/Dockerfile .
FROM node:22-bookworm AS page
WORKDIR /src
RUN npm install -g pnpm@11.28.11
COPY . .
RUN pnpm install --frozen-lockfile \
 && NODE_OPTIONS=--max-old-space-size=4096 pnpm build \
 && mkdir -p dist/themes && cp -r server/theme dist/themes/opencloud

FROM rust:1.99-bookworm AS server
WORKDIR /src
COPY server/Cargo.toml server/Cargo.lock ./
COPY server/src src
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=server /src/target/release/yyc-drive /usr/local/bin/yyc-drive
COPY --from=page /src/dist /srv/web
ENV DRIVE_WEB=/srv/web DRIVE_LISTEN=0.0.0.0:8080
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yyc-drive"]
