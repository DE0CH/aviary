# The chat runtime image (docs/CHAT.md): roles web (sandbox manager for the CloudCLI UI, janitor, scaler), egress (the
# sandbox gateway) and tools (MCP), picked by the first argument (Kubernetes `args: [egress]`). Build context: chat/.

FROM node:22-trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --no-audit --no-fund && npm cache clean --force
COPY src ./src
# the sandbox egress allowlist (generated, egress-allowlist/generate.py); the egress role reads it at start
COPY egress-allowlist/allow.txt egress-allowlist/deny.txt ./egress-allowlist/
RUN mkdir -p /var/cache/jarvis-egress && chown -R 1000:1000 /var/cache/jarvis-egress

ARG CHAT_COMMIT=unknown
# RFC 3339 build time (egress's /version builtAt); empty = null
ARG CHAT_BUILT_AT=
LABEL org.opencontainers.image.source="https://github.com/DE0CH/jarvis3" \
      org.opencontainers.image.description="Jarvis 3 chat runtime (web, egress, tools)" \
      jarvis3.chat-commit="${CHAT_COMMIT}"
ENV NODE_ENV=production \
    PORT=8080 \
    JARVIS_CONFIG=/etc/jarvis/config.json \
    CHAT_COMMIT=${CHAT_COMMIT} \
    CHAT_BUILT_AT=${CHAT_BUILT_AT}
USER 1000:1000
EXPOSE 8080 3128 8082 9090
ENTRYPOINT ["/usr/bin/tini", "--", "node", "--disable-warning=ExperimentalWarning", "src/main.ts"]
