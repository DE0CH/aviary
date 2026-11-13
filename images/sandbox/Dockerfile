# ghcr.io/de0ch/hermit-sandbox:<sha12> (platform 5.2, hermit spec 17, supervisor spec 3): where Claude Code runs for
# a conversation, as uid 1000 in /workspace, under the supervisor. The keeper is PID 1 (no tini, platform R12): it
# starts `supervisor run`, which mounts ~/.claude (FUSE, backed by magpie), launches Claude Code by its absolute path
# /opt/claude-code/bin/claude and starts dockerd when the `docker` wrapper asks. The image holds no secrets; the
# sandbox's only credential is the proxy URL its runtime puts in HTTP_PROXY.
#
# Its context is images/sandbox/ plus dist/supervisor and dist/supervisor-keeper (the static musl binaries of the
# same commit); images/sandbox/build.sh assembles it and builds:
#   images/sandbox/build.sh <dir with the binaries> <tag> [<40 hex revision>]
# The slow layers (packages, Python, fonts, Docker, Claude Code) come first and are cached; the binaries are the last
# layer.
FROM node:22-trixie-slim@sha256:2117f713a417f3aff682ac8f5bc2ebbdf1beed51f36e67ab1e0fd3866b6d2d8f

# tools for news and PR work: documents, PDFs, spreadsheets, charts, CJK fonts for rendering Chinese text.
# PDFs: WeasyPrint (HTML/CSS -> PDF) is the one toolchain; it needs Pango + HarfBuzz's subsetter, and it embeds the
# Noto CJK fonts (OpenType/CFF, which reportlab can't use: with it Claude fell back to installing other fonts). PyMuPDF
# (Claude checks its PDFs with it) stays on 1.x: 2.0 pulls onnxruntime, networkx and pymupdf4llm (+100 MB
# compressed). reportlab is there because Claude probes for it first and then pip-installs it; it reads only
# TrueType, so it gets its own fonts (below).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates bash curl wget git jq unzip zip xz-utils file less procps sudo \
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

# Chinese for reportlab (A5.F1): it can't read the Noto CJK .ttc (CFF outlines: "postscript outlines are not
# supported"), so the image also has TrueType Noto Sans HK and Noto Serif HK, Regular and Bold (static instances of
# Google Fonts' variable files, pinned by commit and sha256; +34 MB). Claude takes its font path from fc-list, which
# lists the .ttc files, so sbx_reportlab_cjk (reportlab-cjk.py, loaded by a .pth at every Python start) has TTFont()
# use these for a Noto CJK file.
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

# Docker (A5.F2): dockerd starts on the first docker command (the wrapper /usr/local/bin/docker asks the supervisor,
# supervisor spec 4.6); with no supervisor socket the wrapper says Docker isn't available.
RUN install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo $VERSION_CODENAME) stable" > /etc/apt/sources.list.d/docker.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends docker-ce docker-ce-cli containerd.io docker-buildx-plugin \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man \
 && mkdir -p /var/lib/docker
COPY --chmod=755 docker-wrapper.sh /usr/local/bin/docker

# Claude Code at the version supervisor spec 3 pins, root-owned outside every directory uid 1000 can write (A11.F13):
# the supervisor runs /opt/claude-code/bin/claude by that absolute path (in 2.1.304 the package's native
# bin/claude.exe). /usr/local/bin/claude is a root-owned link to it for shells. The managed files in /etc/claude-code
# are the supervisor's (supervisor spec 6.4), written at its first start.
ARG CLAUDE_CODE_VERSION=2.1.304
RUN npm install --prefix /opt/claude-code -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
 && npm cache clean --force \
 && rm -rf /root/.npm \
 && chown -R root:root /opt/claude-code \
 && chmod -R go-w /opt/claude-code \
 && ln -s /opt/claude-code/bin/claude /usr/local/bin/claude \
 && mkdir -p /etc/claude-code \
 && /opt/claude-code/bin/claude --version | grep -qF "${CLAUDE_CODE_VERSION}"

# Qwen Code, the second harness (the supervisor runs /opt/qwen-code/bin/qwen by that absolute path): the DE0CH fork's
# release (github.com/DE0CH/qwen-code, de0ch/README.md), pinned by its sha256, root-owned like Claude Code. The
# managed files in /etc/qwen-code are the supervisor's, written at each Qwen Code start.
ARG QWEN_CODE_RELEASE=de0ch-11d16f9fd011
ARG QWEN_CODE_SHA256=4a060efefb5999d31e026c63034459c75a9486b00f6c61c31b6897c99c272a9b
RUN curl -fsSL -o /tmp/qwen-code.tgz "https://github.com/DE0CH/qwen-code/releases/download/${QWEN_CODE_RELEASE}/qwen-code-de0ch.tgz" \
 && printf '%s  %s\n' "${QWEN_CODE_SHA256}" /tmp/qwen-code.tgz | sha256sum -c - \
 && npm install --prefix /opt/qwen-code -g /tmp/qwen-code.tgz \
 && npm cache clean --force \
 && rm -rf /root/.npm /tmp/qwen-code.tgz \
 && chown -R root:root /opt/qwen-code \
 && chmod -R go-w /opt/qwen-code \
 && ln -s /opt/qwen-code/bin/qwen /usr/local/bin/qwen \
 && mkdir -p /etc/qwen-code \
 && /opt/qwen-code/bin/qwen --version

# Video editing, for a harness whose model takes video and sound (Qwen Code): ffmpeg; `clip` (a stretch of a video
# as a small picture file and a sound file the model opens itself), `speech-times` (where each stretch of speech
# starts and ends, measured from the sound) and `edit` (a JSON edit plan rendered by ffmpeg and HyperFrames:
# /opt/engine, its dependencies by its lock file, and the headless Chrome HyperFrames drives, pinned, with the
# libraries it needs; /opt/engine/GUIDE.md is how to work with the three, for a deployment's skill to point at); the library `edit assets` lists (/opt/assets: music and sound effects from Mixkit, animated
# emoji from Google's Noto set, fetched by a manifest that pins every file's sha256); and the fonts its looks name,
# with full Cantonese coverage where the look is for Chinese text (Google Fonts at the commit above).
ARG CHROME_HEADLESS_SHELL=152.0.7977.30
COPY video/engine /opt/engine
COPY --chmod=755 video/clip video/speech-times /usr/local/bin/
COPY video/assets-manifest.json video/fetch-assets.py video/fonts.txt /opt/image-setup/video/
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg aubio-tools fonts-noto-color-emoji \
      libnss3 libdbus-1-3 libatk1.0-0t64 libatk-bridge2.0-0t64 libgbm1 libasound2t64 libxrandr2 libxkbcommon0 \
      libxfixes3 libxcomposite1 libxdamage1 libcairo2 libcups2t64 \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man \
 && cd /opt/engine && npm ci --no-audit --no-fund \
 && npx --yes @puppeteer/browsers@2.10.10 install chrome-headless-shell@${CHROME_HEADLESS_SHELL} --path /opt/engine/chrome-install \
 && mkdir chrome && ln -s "$(find /opt/engine/chrome-install -type f -name chrome-headless-shell | head -1)" chrome/chrome-headless-shell \
 && test -x chrome/chrome-headless-shell \
 && npm cache clean --force && rm -rf /root/.npm \
 && printf '#!/bin/sh\nexec node /opt/engine/edit.mjs "$@"\n' > /usr/local/bin/edit && chmod 755 /usr/local/bin/edit \
 && python3 /opt/image-setup/video/fetch-assets.py /opt/image-setup/video/assets-manifest.json /opt/assets \
 && d=/usr/local/share/fonts/edit && mkdir -p $d && cd $d \
 && grep -v '^#' /opt/image-setup/video/fonts.txt | while read -r sum file; do \
      curl -fsSL -o "$(basename "$file")" "https://raw.githubusercontent.com/google/fonts/$GOOGLE_FONTS/ofl/$(printf '%s' "$file" | sed 's/\[/%5B/; s/\]/%5D/')" \
      && printf '%s  %s\n' "$sum" "$(basename "$file")" | sha256sum -c - || exit 1; \
    done \
 && test "$(ls | wc -l)" = "$(grep -vc '^#' /opt/image-setup/video/fonts.txt)" \
 && fc-cache -f && chown -R root:root /opt/engine /opt/assets && chmod -R go-w /opt/engine /opt/assets

# Installing system packages (A5.F4): `apt-get`/`apt` are wrappers that go through sudo for uid 1000 and fetch the
# package lists on the first install (the image ships without them). apt reaches the Debian mirrors through the
# sandbox's proxy: sudo keeps the proxy variables, and the supervisor writes /etc/apt/apt.conf.d/80sandbox-proxy at
# its first start (for root shells that reset the environment). No prompts (no one can answer them), no recommended
# packages (installs stay small: the root overlay is capped and travels with the sandbox).
COPY --chmod=755 apt-wrapper.sh /usr/local/bin/apt-get
COPY --chmod=644 90sandbox /etc/apt/apt.conf.d/90sandbox
COPY --chmod=440 sudoers /etc/sudoers.d/sandbox
RUN ln -s apt-get /usr/local/bin/apt && visudo -cq

# The image's environment defaults reach the agent only through files: the supervisor gives the processes it starts
# the image's PATH, LANG, LC_ALL and HOME and nothing else of it (supervisor spec 6.2), so npm's prefix (uid 1000's
# ~/.npmrc, below), pip's version check, Python's line-buffered output (PYTHONUNBUFFERED's job: a .pth in the venv
# makes stdout and stderr line-buffered when they are not a terminal) and a login shell's PATH are configured where
# each tool reads them whatever the environment (A5.F5).
COPY --chmod=644 pip.conf /etc/pip.conf
COPY --chmod=644 --chown=1000:1000 line-buffered.pth /opt/py/lib/python3.13/site-packages/sbx-line-buffered.pth
COPY --chmod=644 profile /etc/profile.d/00-sandbox-path.sh

# uid 1000 is the image's `node` user, with home /home/sandbox. /home/sandbox/.harness is an empty directory: the
# supervisor mounts the conversation's harness homes there (FUSE, supervisor spec 9; nothing is bind-mounted over
# it): `claude/` is Claude Code's ~/.claude and `qwen/` is Qwen Code's ~/.qwen, which are links into it.
# /workspace is the conversation's persistent folder (the runtime's mount; a plain folder elsewhere); /work is only
# the folder the runtime starts the first process in. /var/lib/supervisor (the supervisor's run state, generations,
# cache and journal; on the scratch disk with the rest of the overlay upper) is root's alone.
RUN mkdir -p /home/sandbox/.harness /work /workspace \
 && ln -s .harness/claude /home/sandbox/.claude && ln -s .harness/qwen /home/sandbox/.qwen \
 && chown 1000:1000 /home/sandbox /home/sandbox/.harness /work /workspace \
 && chown -h 1000:1000 /home/sandbox/.claude /home/sandbox/.qwen \
 && usermod -d /home/sandbox node \
 && install -d -m 0700 -o root -g root /var/lib/supervisor
COPY --chmod=644 --chown=1000:1000 npmrc /home/sandbox/.npmrc

# No HOME and no NPM_CONFIG_PREFIX here: a process's HOME is its user's unless its starter sets one (hermit 5.1 and the
# supervisor give /home/sandbox; Docker takes /etc/passwd's, /root for root), and npm's prefix follows that HOME
# (uid 1000's ~/.npmrc, above), so root's npm, with or without sudo, keeps node's /usr/local.
ENV PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1 \
    DEBIAN_FRONTEND=noninteractive \
    MPLCONFIGDIR=/home/sandbox/.config/matplotlib \
    LANG=C.UTF-8
ENV PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/home/sandbox/.cargo/bin:$PATH

# The image's own setup, readable inside the sandbox so Claude can see exactly what is installed and how (the
# instance prompt points at it, A5.F6): this Dockerfile and the files it installs.
COPY Dockerfile apt-wrapper.sh docker-wrapper.sh reportlab-cjk.py line-buffered.pth 90sandbox sudoers npmrc pip.conf profile /opt/image-setup/

# The supervisor and the keeper of this commit (static musl, root 0755), and `sandbox-secret`, which the supervisor
# binary answers by argv[0] (supervisor spec 3), as it does `send-file` (files to the person from a shell command:
# Qwen Code's way). Last, so a new commit rebuilds only this layer.
COPY --chmod=755 dist/supervisor dist/supervisor-keeper /usr/local/bin/
RUN ln -s supervisor /usr/local/bin/sandbox-secret && ln -s supervisor /usr/local/bin/send-file

ARG REVISION=dev
ARG VERSION=dev
ARG SPEAKS={}
ARG SUPERVISOR_SHA256=
LABEL org.opencontainers.image.source="https://github.com/DE0CH/aviary" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.description="hermit's sandbox: Claude Code or Qwen Code under the aviary supervisor" \
      dev.de0ch.aviary.speaks="${SPEAKS}" \
      dev.de0ch.aviary.supervisor="${SUPERVISOR_SHA256}"
USER root
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/supervisor-keeper"]
