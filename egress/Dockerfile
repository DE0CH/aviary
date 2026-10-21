# The chat runtime image (docs/CHAT.md): roles web, egress and tools (JARVIS_ROLE), the staff UI, and the claw binary
# the web role runs per turn. Build context: chat/. Build args CLAW_URL and CLAW_SHA256 come from chat/claw.json.

FROM node:22-trixie-slim AS ui
WORKDIR /src/ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui ./
RUN npm run build

FROM node:22-trixie-slim AS claw
ARG CLAW_URL
ARG CLAW_SHA256
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
 && test -n "$CLAW_URL" && test -n "$CLAW_SHA256" \
 && curl -fsSL -o /claw "$CLAW_URL" \
 && echo "$CLAW_SHA256  /claw" | sha256sum -c - \
 && chmod 755 /claw

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
COPY --from=ui /src/ui-dist /app/ui-dist
COPY --from=claw /claw /usr/local/bin/claw
RUN chmod 755 src/bin/* && mkdir -p /var/lib/jarvis/work /var/cache/jarvis-egress && chown -R 1000:1000 /var/lib/jarvis /var/cache/jarvis-egress

ARG CHAT_COMMIT=unknown
ARG CLAW_URL
LABEL org.opencontainers.image.source="https://github.com/DE0CH/jarvis3" \
      org.opencontainers.image.description="Jarvis 3 chat runtime (web, egress, tools)" \
      jarvis3.chat-commit="${CHAT_COMMIT}" \
      jarvis3.claw="${CLAW_URL}"
ENV NODE_ENV=production \
    JARVIS_COMMIT=${CHAT_COMMIT} \
    PORT=8080 \
    JARVIS_CONFIG=/etc/jarvis/config.json \
    JARVIS_UI_DIR=/app/ui-dist \
    JARVIS_BRAND_DIR=/etc/jarvis/brand \
    JARVIS_WORK_ROOT=/var/lib/jarvis/work \
    CLAW_PATH=/usr/local/bin/claw
USER 1000:1000
EXPOSE 8080 3128
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["node", "--disable-warning=ExperimentalWarning", "src/main.ts"]
