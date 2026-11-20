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
      ca-certificates bash curl wget git openssh-client jq unzip zip xz-utils file less procps sudo \
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

# gh (GitHub CLI): CI runs, PRs, releases and logs from a conversation; it reads GH_TOKEN/GITHUB_TOKEN when a user
# gives one. From GitHub's own apt repository (Debian's is far behind).
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends gh \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man \
 && gh --version

# The agent's general tools (Deyao, 2026-11-20: one image for every harness, Jarvis 4's included, and it may grow:
# disk is cheap). testing/sandbox-tools lists each with its check (ci.yml job `sandbox-tools`). Pinned where the
# upstream lets us, so the image changes only when this file does; Google Chrome is "stable" (Google serves no older
# build). The supervisor gives the agent only PATH, LANG, LC_ALL and HOME of the image's environment (A5.F5), so a
# tool that needs a variable (Rust's RUSTUP_HOME, Playwright's browsers) also gets it in /etc/profile.d (`profile`).
# System packages: shell, network and media tools, LibreOffice (Writer, Calc, Impress), Postgres client, QR codes,
# X11 input for a browser on Xvfb (xdotool, xclip), ImageMagick, WireGuard, Ruby (fastlane), the C toolchain.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      tmux openssl ripgrep nano vim build-essential gnupg rsync age htop sqlite3 bind9-dnsutils iputils-ping \
      librsvg2-bin psmisc lsof iproute2 sshpass pv zstd whois gawk bc xxd netcat-openbsd brotli icu-devtools \
      libheif-examples bsdextrautils postgresql-client zbar-tools libzbar0t64 wireguard-tools \
      libreoffice-writer libreoffice-calc libreoffice-impress \
      xdotool xclip imagemagick xdg-utils ruby-full \
      libgtk-3-0t64 libdbus-glib-1-2 libx11-xcb1 \
 && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man /usr/share/ri

# Single-binary tools, each at a pinned release: rclone, kubectl, kubeseal, cloudflared, versitygw, sops, litestream,
# crane, hcloud, mihomo, yt-dlp, typst, flyctl, uv.
RUN set -e; cd /tmp \
 && curl -fsSL -o rclone.zip https://downloads.rclone.org/v1.76.0/rclone-v1.76.0-linux-amd64.zip \
 && unzip -q rclone.zip && install -m 755 rclone-v1.76.0-linux-amd64/rclone /usr/local/bin/ && rm -rf rclone* \
 && curl -fsSL -o /usr/local/bin/kubectl https://dl.k8s.io/release/v1.33.4/bin/linux/amd64/kubectl \
 && curl -fsSL https://github.com/bitnami-labs/sealed-secrets/releases/download/v0.40.0/kubeseal-0.40.0-linux-amd64.tar.gz \
      | tar -xzO kubeseal > /usr/local/bin/kubeseal \
 && curl -fsSL -o /usr/local/bin/cloudflared https://github.com/cloudflare/cloudflared/releases/download/2026.10.0/cloudflared-linux-amd64 \
 && curl -fsSL https://github.com/versity/versitygw/releases/download/v1.8.0/versitygw_v1.8.0_Linux_x86_64.tar.gz \
      | tar -xz -C /usr/local/bin --strip-components=1 versitygw_v1.8.0_Linux_x86_64/versitygw \
 && curl -fsSL -o /usr/local/bin/sops https://github.com/getsops/sops/releases/download/v3.13.3/sops-v3.13.3.linux.amd64 \
 && curl -fsSL https://github.com/benbjohnson/litestream/releases/download/v0.5.17/litestream-0.5.17-linux-x86_64.tar.gz \
      | tar -xz -C /usr/local/bin litestream \
 && curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.22.1/go-containerregistry_Linux_x86_64.tar.gz \
      | tar -xz -C /usr/local/bin crane \
 && curl -fsSL https://github.com/hetznercloud/cli/releases/download/v1.70.1/hcloud-linux-amd64.tar.gz \
      | tar -xz -C /usr/local/bin hcloud \
 && curl -fsSL https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/mihomo-linux-amd64-v1.19.31.gz \
      | gunzip > /usr/local/bin/mihomo \
 && curl -fsSL -o /usr/local/bin/yt-dlp https://github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp \
 && curl -fsSL https://github.com/typst/typst/releases/download/v0.15.1/typst-x86_64-unknown-linux-musl.tar.xz \
      | tar -xJ -C /usr/local/bin --strip-components=1 typst-x86_64-unknown-linux-musl/typst \
 && curl -fsSL https://fly.io/install.sh | FLYCTL_INSTALL=/usr/local sh -s 0.5.14 \
 && curl -LsSf https://astral.sh/uv/0.13.9/install.sh | env UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh \
 && chmod 755 /usr/local/bin/kubectl /usr/local/bin/kubeseal /usr/local/bin/cloudflared /usr/local/bin/sops \
      /usr/local/bin/mihomo /usr/local/bin/yt-dlp \
 && kubectl version --client >/dev/null && flyctl version && uv --version && typst --version

# Tailscale (CLI and daemon; tailscaled runs with --tun=userspace-networking in a sandbox): its own apt repository.
RUN curl -fsSL https://tailscale.com/install.sh | sh \
 && rm -rf /var/lib/apt/lists/* && tailscale version

# Go at a pinned release, and two tools built with it: age-plugin-tag (sops to an age1tag1 recipient, a Secure
# Enclave key; age ships it as source only) and actionlint (checks a GitHub workflow before it is pushed).
RUN curl -fsSL https://go.dev/dl/go1.27.4.linux-amd64.tar.gz | tar -C /usr/local -xz \
 && ln -s /usr/local/go/bin/go /usr/local/bin/go && ln -s /usr/local/go/bin/gofmt /usr/local/bin/gofmt \
 && GOBIN=/usr/local/bin GOPATH=/tmp/gopath GOCACHE=/tmp/gocache go install filippo.io/age/extra/age-plugin-tag@v1.3.2 \
 && GOBIN=/usr/local/bin GOPATH=/tmp/gopath GOCACHE=/tmp/gocache go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 \
 && rm -rf /tmp/gopath /tmp/gocache && go version

# Rust (1.99.1, minimal profile + rustfmt + clippy): rustup in /opt, owned by uid 1000 so the agent adds targets
# (`rustup target add wasm32-unknown-unknown`) and `cargo install`s tools; RUSTUP_HOME and CARGO_HOME are in `profile`.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain 1.99.1 \
      --component rustfmt,clippy \
 && rm -rf /opt/rustup/downloads /opt/rustup/tmp /opt/cargo/registry \
 && chown -R 1000:1000 /opt/rustup /opt/cargo && /opt/cargo/bin/cargo --version

# fastlane (spaceship): App Store Connect and developer-portal steps the public API lacks.
RUN gem install fastlane -N && fastlane --version >/dev/null

# Node CLIs, root's npm (prefix /usr/local): wrangler (Cloudflare Workers), vercel, Tencent CloudBase (`tcb`),
# browse (the Browserbase CLI). Every npm RUN ends with the cache cleaned.
RUN npm install -g wrangler@4.165.0 vercel@65.10.0 @cloudbase/cli@3.9.4 browse@0.12.0 \
 && npm cache clean --force && rm -rf /root/.npm \
 && wrangler --version && vercel --version

# Browsers. Playwright for Node is global (`require('playwright')` through NODE_PATH, in `profile`) and Playwright for
# Python (in the venv) is the same version, so both share one set of browsers in /opt/pw-browsers: Chromium, its
# headless shell and WebKit (pages in Safari's engine without a Mac); `--with-deps` installs their system libraries.
# The Python package's own Node becomes a link to the image's. Stable links hide the revision-numbered directories,
# and the build fails if a browser isn't where they point. Then real Google Chrome stable (`google-chrome`): some
# anti-bot layers reject Chromium builds; only its English and Chinese UI locales are kept.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers NODE_PATH=/usr/local/lib/node_modules
RUN npm install -g playwright@1.62.0 ws@8.23.0 && npm cache clean --force && rm -rf /root/.npm \
 && playwright install --with-deps chromium chromium-headless-shell webkit \
 && /opt/py/bin/pip install --no-cache-dir playwright==1.62.0 \
 && ln -sf /usr/local/bin/node /opt/py/lib/python3.13/site-packages/playwright/driver/node \
 && ln -s "$(find /opt/pw-browsers -path '*/chromium-*' -type f -name chrome | head -1)" /opt/pw-browsers/chromium \
 && ln -s "$(find /opt/pw-browsers -path '*/chromium_headless_shell-*' -type f \( -name headless_shell -o -name chrome-headless-shell \) | head -1)" /opt/pw-browsers/headless_shell \
 && test -x /opt/pw-browsers/chromium && test -x /opt/pw-browsers/headless_shell \
 && test -n "$(find /opt/pw-browsers -maxdepth 1 -name 'webkit-*')" \
 && ln -s /opt/pw-browsers/chromium /usr/local/bin/chromium \
 && chmod -R a+rX /opt/pw-browsers \
 && curl -fsSL -o /tmp/chrome.deb https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb \
 && apt-get install -y --no-install-recommends /tmp/chrome.deb \
 && rm -rf /var/lib/apt/lists/* /tmp/chrome.deb /usr/share/doc /usr/share/man \
 && find /opt/google/chrome/locales -name '*.pak' ! -name 'en-US.pak' ! -name 'en-GB.pak' ! -name 'zh-CN.pak' ! -name 'zh-TW.pak' -delete \
 && google-chrome --version \
 && find /opt/py -name '__pycache__' -prune -exec rm -rf {} + && chown -R 1000:1000 /opt/py

# Python libraries in the venv: HTTP and HTML, TOTP, images and QR codes, browser-grade TLS (curl_cffi), SOCKS and a
# local proxy forwarder (pproxy), cloud SDKs (boto3, Aliyun core), Hugging Face, Telegram (telethon), Microsoft OAuth
# (msal), sealed boxes (pynacl), crypto wallets (eth-account, tronpy), voice-over (edge-tts), offline Chinese OCR
# (rapidocr on onnxruntime: rapidocr_onnxruntime has no Python 3.13 build), VNC screenshots (asyncvnc), Camoufox,
# Postgres (psycopg and its pool), Chinese words (jieba), HEIC photos, tokenizers, a KORAIL client. The venv's
# opencc stays opencc-python-reimplemented (above): the `opencc` package would replace the same module.
RUN /opt/py/bin/pip install --no-cache-dir \
      pyotp boto3 telethon PySocks python-socks pyzbar curl_cffi eth-account pproxy websocket-client zxing-cpp \
      'qrcode[pil]' numpy aliyun-python-sdk-core pyyaml huggingface_hub gradio_client msal korail-mobile-api \
      edge-tts tronpy rapidocr onnxruntime asyncvnc 'camoufox[geoip]' 'psycopg[binary]' psycopg-pool jieba \
      pillow-heif tokenizers pynacl \
 && find /opt/py -name '__pycache__' -prune -exec rm -rf {} + \
 && chown -R 1000:1000 /opt/py

# pymobiledevice3 (iOS devices over a network link; iOS 18.2+ tunnels need Python 3.13, the system Python here) in
# its own venv, so its pins don't move the agent's.
RUN python3 -m venv /opt/ios && /opt/ios/bin/pip install --no-cache-dir pymobiledevice3==11.19.4 \
 && ln -s /opt/ios/bin/pymobiledevice3 /usr/local/bin/pymobiledevice3 \
 && find /opt/ios -name '__pycache__' -prune -exec rm -rf {} + && pymobiledevice3 version

# Claude Code at the version supervisor spec 3 pins, root-owned outside every directory uid 1000 can write (A11.F13):
# the supervisor runs /opt/claude-code/bin/claude by that absolute path (in 2.1.325 the package's native
# bin/claude.exe). /usr/local/bin/claude is a root-owned link to it for shells. The managed files in /etc/claude-code
# are the supervisor's (supervisor spec 6.4), written at its first start.
ARG CLAUDE_CODE_VERSION=2.1.325
RUN npm install --prefix /opt/claude-code -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
 && npm cache clean --force \
 && rm -rf /root/.npm \
 && chown -R root:root /opt/claude-code \
 && chmod -R go-w /opt/claude-code \
 && ln -s /opt/claude-code/bin/claude /usr/local/bin/claude \
 && mkdir -p /etc/claude-code \
 && /opt/claude-code/bin/claude --version | grep -qF "${CLAUDE_CODE_VERSION}"

# Qwen Code, the second harness (the supervisor runs /opt/qwen-code/bin/qwen by that absolute path): the fork in
# harness/qwen-code, packed and released by ci.yml's `qwen` job as qwen-code-<sha12> (its last commit) and given here
# by tag and sha256 (build args; no default: a build without them fails here), root-owned like Claude Code. The
# managed files in /etc/qwen-code are the supervisor's, written at each Qwen Code start.
ARG QWEN_CODE_TAG
ARG QWEN_CODE_SHA256
RUN test -n "${QWEN_CODE_TAG}" -a -n "${QWEN_CODE_SHA256}" \
 && curl -fsSL -o /tmp/qwen-code.tgz "https://github.com/DE0CH/aviary/releases/download/${QWEN_CODE_TAG}/qwen-code.tgz" \
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
ENV PATH=/home/sandbox/.npm-global/bin:/home/sandbox/.local/bin:/home/sandbox/.cargo/bin:/opt/cargo/bin:$PATH

# The image's own setup, readable inside the sandbox so Claude can see exactly what is installed and how (the
# instance prompt points at it, A5.F6): this Dockerfile and the files it installs.
COPY Dockerfile apt-wrapper.sh docker-wrapper.sh reportlab-cjk.py line-buffered.pth 90sandbox sudoers npmrc pip.conf profile /opt/image-setup/

# The supervisor and the keeper of this commit (static musl, root 0755), and `sandbox-secret`, which the supervisor
# binary answers by argv[0] (supervisor spec 3), as it does `send-file` (files to the person from a shell command:
# Qwen Code's way), `ask` (a question for the person, blocking until the answer: every harness's way) and
# `video-check` (whether the generation's model watches video and hears sound, asked of the model route). Last, so a
# new commit rebuilds only this layer.
COPY --chmod=755 dist/supervisor dist/supervisor-keeper /usr/local/bin/
RUN ln -s supervisor /usr/local/bin/sandbox-secret && ln -s supervisor /usr/local/bin/send-file \
    && ln -s supervisor /usr/local/bin/ask && ln -s supervisor /usr/local/bin/video-check

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
