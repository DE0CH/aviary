# The sandbox image (docs/CHAT.md, "Sandbox lifecycle"): where a conversation's bash commands run, as uid 1000 in
# /work, inside a gVisor pod whose only way out is the egress proxy. It holds no secrets. Build context: chat/sandbox/.
FROM node:22-trixie-slim

# tools for news and PR work: documents, PDFs, spreadsheets, charts, CJK fonts for rendering Chinese text
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates tini bash curl wget git jq unzip zip xz-utils file less procps \
      python3 python3-venv python3-pip \
      poppler-utils pandoc fonts-noto-cjk fonts-noto-core fontconfig \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man

# A virtualenv owned by the sandbox user, first on PATH: `pip install x` works without root or
# --break-system-packages, and installs carry over while the sandbox lives.
ENV VIRTUAL_ENV=/opt/py PATH=/opt/py/bin:$PATH
RUN python3 -m venv /opt/py \
 && /opt/py/bin/pip install --no-cache-dir \
      pandas openpyxl xlsxwriter python-docx python-pptx matplotlib pypdf pdfplumber \
      beautifulsoup4 lxml requests httpx Pillow markdown tabulate python-dateutil chardet opencc-python-reimplemented \
 && find /opt/py -name '__pycache__' -prune -exec rm -rf {} + \
 && chown -R 1000:1000 /opt/py

# Docker (docs/SANDBOX-VM.md, "Docker in sandboxes"): on our own hosts (provider "vm") dockerd starts on the first
# docker command (sbx-init.mjs + the wrapper /usr/local/bin/docker); elsewhere the wrapper says Docker isn't available.
RUN install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo $VERSION_CODENAME) stable" > /etc/apt/sources.list.d/docker.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends docker-ce docker-ce-cli containerd.io docker-buildx-plugin \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man \
 && mkdir -p /var/lib/docker
COPY docker-wrapper.sh /usr/local/bin/docker

# uid 1000 is the image's `node` user; the sandbox's home is /home/sandbox (not /home/user: E2B's template builder
# makes that for its own user, mode 700), the workspace /work
RUN mkdir -p /home/sandbox /work /opt/agent \
 && chown 1000:1000 /home/sandbox /work \
 && usermod -d /home/sandbox node
COPY agent.mjs tar.mjs sbx-init.mjs /opt/agent/

ENV HOME=/home/sandbox \
    JARVIS_WORK_DIR=/work \
    PORT=7070 \
    NPM_CONFIG_PREFIX=/home/sandbox/.npm-global \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1 \
    MPLCONFIGDIR=/home/sandbox/.config/matplotlib \
    LANG=C.UTF-8
ENV PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:$PATH
# Commands run as `bash -lc`, and a login shell's /etc/profile resets PATH: put the venv and the user's bins back.
USER root
RUN printf 'export VIRTUAL_ENV=/opt/py\nexport PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/opt/py/bin:$PATH\n' > /etc/profile.d/00-sandbox-path.sh

LABEL org.opencontainers.image.source="https://github.com/DE0CH/jarvis3" \
      org.opencontainers.image.description="Jarvis 3 chat sandbox"
# by name: E2B's template builder takes only a user name (node = uid/gid 1000)
USER node
WORKDIR /work
EXPOSE 7070
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["node", "/opt/agent/agent.mjs"]
