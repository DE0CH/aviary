# YYC Drive: the server (server/) and, at /srv/edge, the edge (edge/: the Worker, with this repository's page as its
# assets) for the deploy to take out and hand to Cloudflare. One image, one commit for both.
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
# ffmpeg draws the previews (a frame of a video, a picture scaled down)
RUN apt-get -o Acquire::Retries=5 update && apt-get -o Acquire::Retries=5 install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/*
COPY --from=server /src/target/release/yyc-drive /usr/local/bin/yyc-drive
COPY edge/wrangler.jsonc edge/package.json /srv/edge/
COPY edge/src /srv/edge/src
COPY --from=page /src/dist /srv/edge/assets
COPY edge/_headers /srv/edge/assets/_headers
ENV DRIVE_LISTEN=0.0.0.0:8080
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yyc-drive"]
