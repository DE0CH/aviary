# The sandbox image (docs/CLAUDE-CODE.md): where Claude Code runs for a conversation, as uid 1000 in /workspace, inside
# a gVisor sandbox whose only way out is the egress proxy. It holds no secrets. Build context: chat/sandbox/.
# On our own hosts (provider "vm") uid 1000 has passwordless sudo: root and its capabilities exist only inside gVisor
# (docs/SANDBOX-VM.md, "Installing packages"), so `apt-get install` works there.
FROM node:22-trixie-slim

# tools for news and PR work: documents, PDFs, spreadsheets, charts, CJK fonts for rendering Chinese text.
# PDFs: WeasyPrint (HTML/CSS -> PDF) is the one toolchain; it needs Pango + HarfBuzz's subsetter, and it embeds the
# Noto CJK fonts (OpenType/CFF, which reportlab can't use: with it Claude fell back to installing other fonts). Live
# PDF chats (2026-10-21/22) installed exactly these at run time on every new sandbox. PyMuPDF (Claude checks its
# PDFs with it) stays on 1.x: 2.0 pulls onnxruntime, networkx and pymupdf4llm (+100 MB compressed). reportlab is
# there because Claude probes for it first (4 of 4 PDF chats) and then pip-installs it (2 of them); it reads only
# TrueType, so it gets its own fonts (below).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates tini bash curl wget git jq unzip zip xz-utils file less procps sudo \
      python3 python3-venv python3-pip \
      poppler-utils pandoc fonts-noto-cjk fonts-noto-core fontconfig \
      libpango-1.0-0 libpangoft2-1.0-0 libharfbuzz-subset0 \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man

# A virtualenv owned by the sandbox user, first on PATH: `pip install x` works without root or
# --break-system-packages, and installs carry over while the sandbox lives.
ENV VIRTUAL_ENV=/opt/py PATH=/opt/py/bin:$PATH
RUN python3 -m venv /opt/py \
 && /opt/py/bin/pip install --no-cache-dir \
      pandas openpyxl xlsxwriter python-docx python-pptx matplotlib pypdf pdfplumber \
      beautifulsoup4 lxml requests httpx Pillow markdown tabulate python-dateutil chardet opencc-python-reimplemented \
      weasyprint 'pymupdf<2' reportlab \
 && /opt/py/bin/python -c "import weasyprint, pymupdf; d = pymupdf.open(stream=weasyprint.HTML(string='<html lang=zh-HK><p style=\"font-family:Noto Sans CJK HK\">繁體中文檔案測試</p>').write_pdf()); assert '繁體中文檔案測試' in d[0].get_text(), d[0].get_text(); assert any('CJK' in f[3] for f in d[0].get_fonts()), d[0].get_fonts()" \
 && find /opt/py -name '__pycache__' -prune -exec rm -rf {} + \
 && chown -R 1000:1000 /opt/py

# Chinese for reportlab: it can't read the Noto CJK .ttc (CFF outlines: "postscript outlines are not supported", live
# chat 2026-10-22), so the image also has TrueType Noto Sans HK and Noto Serif HK, Regular and Bold (static instances of
# Google Fonts' variable files, pinned by commit and sha256; +34 MB). Claude takes its font path from fc-list, which
# lists the .ttc files, so sbx_reportlab_cjk (reportlab-cjk.py) has TTFont() use these for a Noto CJK file.
ARG GOOGLE_FONTS=45e3d5269e4a17d823f6d84470bc084dc5ae46f6
COPY reportlab-cjk.py /opt/py/lib/python3.13/site-packages/sbx_reportlab_cjk.py
RUN set -e; d=/usr/share/fonts/truetype/noto-hk; mkdir -p $d; cd /tmp \
 && curl -fsSL -o sans.ttf "https://raw.githubusercontent.com/google/fonts/$GOOGLE_FONTS/ofl/notosanshk/NotoSansHK%5Bwght%5D.ttf" \
 && curl -fsSL -o serif.ttf "https://raw.githubusercontent.com/google/fonts/$GOOGLE_FONTS/ofl/notoserifhk/NotoSerifHK%5Bwght%5D.ttf" \
 && printf '%s  %s\n' 76098ee78ec234cd4f8c950742b3f766fea2f8b43d5180d901048f4fc86c6849 sans.ttf \
      66108860e321aa413e7cb346448b4a418b779d94ae392accb32d163c024ae661 serif.ttf | sha256sum -c - \
 && for f in Sans Serif; do lower=$(echo $f | tr A-Z a-z); \
      fonttools varLib.instancer -q --update-name-table -o $d/Noto${f}HK-Regular.ttf $lower.ttf wght=400; \
      fonttools varLib.instancer -q --update-name-table -o $d/Noto${f}HK-Bold.ttf $lower.ttf wght=700; done \
 && rm -f sans.ttf serif.ttf && fc-cache -f \
 && echo 'import sbx_reportlab_cjk' > /opt/py/lib/python3.13/site-packages/sbx-reportlab-cjk.pth \
 && /opt/py/bin/python -I -c "from reportlab.pdfbase import pdfmetrics; from reportlab.pdfbase.ttfonts import TTFont; from reportlab.pdfgen.canvas import Canvas; import io, pymupdf; pdfmetrics.registerFont(TTFont('cjk', '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc', subfontIndex=2)); pdfmetrics.registerFont(TTFont('cjkb', '/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc')); b = io.BytesIO(); c = Canvas(b); c.setFont('cjk', 14); c.drawString(72, 720, '繁體中文檔案測試'); c.setFont('cjkb', 14); c.drawString(72, 690, '香港特區政府'); c.save(); d = pymupdf.open(stream=b.getvalue()); t = d[0].get_text(); assert '繁體中文檔案測試' in t and '香港特區政府' in t, t; assert {f[3].split('+')[-1] for f in d[0].get_fonts()} >= {'NotoSansHK-Regular', 'NotoSerifHK-Bold'}, d[0].get_fonts()" \
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

# Claude Code (docs/CLAUDE-CODE.md): runs inside the sandbox, started by the CloudCLI UI through the agent's /proc
# endpoint with cwd /workspace. Pinned to npm's "stable" tag. The instance's CLAUDE.md and MCP servers are written at
# start by sbx-init.mjs into Claude Code's managed paths (/etc/claude-code), which uid 1000 can read but not change.
RUN npm install -g @anthropic-ai/claude-code@2.1.304 \
 && npm cache clean --force \
 && mkdir -p /etc/claude-code \
 && claude --version

# Installing system packages (docs/SANDBOX-VM.md, "Installing packages"): `apt-get`/`apt` are wrappers that go through
# sudo for uid 1000 and fetch the package lists on the first install (the image ships without them). apt reaches the
# Debian mirrors through the egress proxy: sudo keeps the proxy variables, and sbx-init.mjs writes them into
# /etc/apt/apt.conf.d/80sandbox-proxy at start (for root shells that reset the environment). No prompts (no one can
# answer them), no recommended packages (installs stay small: the root overlay is capped and travels with the sandbox).
COPY apt-wrapper.sh /usr/local/bin/apt-get
RUN ln -s apt-get /usr/local/bin/apt \
 && printf 'APT::Get::Assume-Yes "true";\nAPT::Install-Recommends "false";\nDpkg::Use-Pty "false";\n' > /etc/apt/apt.conf.d/90sandbox \
 && printf '%s\n' \
      'Defaults env_keep += "HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy"' \
      'Defaults env_keep += "PIP_INDEX_URL PIP_TRUSTED_HOST npm_config_registry DEBIAN_FRONTEND"' \
      'Defaults !lecture, !syslog' \
      'node ALL=(ALL:ALL) NOPASSWD: ALL' > /etc/sudoers.d/sandbox \
 && chmod 0440 /etc/sudoers.d/sandbox \
 && visudo -cq

# uid 1000 is the image's `node` user; the sandbox's home is /home/sandbox (not /home/user: E2B's template builder
# makes that for its own user, mode 700). /workspace and ~/.claude are the conversation's persistent folders, mounted
# from the host (JuiceFS) on our own hosts; elsewhere they are plain folders. /work is only the folder sandboxd starts
# the sandbox's first process in.
RUN mkdir -p /home/sandbox/.claude /work /workspace /opt/agent \
 && chown 1000:1000 /home/sandbox /home/sandbox/.claude /work /workspace \
 && usermod -d /home/sandbox node
COPY agent.mjs sbx-init.mjs send-file-hook.mjs /opt/agent/
# The image's own setup, readable inside the sandbox so Claude can see exactly what is installed and how (an instance's
# system prompt points at it): this Dockerfile and the files it installs.
COPY Dockerfile apt-wrapper.sh docker-wrapper.sh reportlab-cjk.py /opt/image-setup/

ENV HOME=/home/sandbox \
    PORT=7070 \
    NPM_CONFIG_PREFIX=/home/sandbox/.npm-global \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1 \
    DEBIAN_FRONTEND=noninteractive \
    MPLCONFIGDIR=/home/sandbox/.config/matplotlib \
    LANG=C.UTF-8
ENV PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/home/sandbox/.cargo/bin:$PATH
# A login shell's /etc/profile resets PATH: put the venv and the user's bins back
# (~/.local/bin and ~/.cargo/bin: where `curl … | sh` installers such as uv's and rustup's put their tools).
USER root
RUN printf 'export VIRTUAL_ENV=/opt/py\nexport PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/home/sandbox/.cargo/bin:/opt/py/bin:$PATH\n' > /etc/profile.d/00-sandbox-path.sh

LABEL org.opencontainers.image.source="https://github.com/DE0CH/jarvis3" \
      org.opencontainers.image.description="Jarvis 3 chat sandbox"
# by name: E2B's template builder takes only a user name (node = uid/gid 1000)
USER node
WORKDIR /workspace
EXPOSE 7070
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["node", "/opt/agent/agent.mjs"]
