# images/sandbox: `ghcr.io/de0ch/hermit-sandbox:<sha12>`

The sandbox image (hermit spec 17, supervisor spec 3, platform 5.2): where a conversation's harness (Claude Code or
Qwen Code, [docs/HARNESSES.md](../../docs/HARNESSES.md)) runs, as uid 1000 in `/workspace`, under the supervisor and
keeper of the same aviary commit. Ported from jarvis3's
`chat/sandbox/` with the in-sandbox Node agent replaced by the supervisor.

| File | Installed as | What |
|---|---|---|
| `Dockerfile` | `/opt/image-setup/` (with every file below) | the image; Claude can read how it was made |
| `apt-wrapper.sh` | `/usr/local/bin/apt-get`, `apt` | through sudo for uid 1000, package lists fetched on the first install, exit 100 with the reason when there is no root |
| `docker-wrapper.sh` | `/usr/local/bin/docker` | asks the supervisor to start dockerd (`GET /start` on `/run/supervisor/docker.sock`), adds `--network=host` |
| `reportlab-cjk.py` | `sbx_reportlab_cjk` in the venv, loaded by a `.pth` | reportlab's `TTFont()` given a Noto CJK `.ttc` uses the TrueType Noto Sans/Serif HK |
| `90sandbox` | `/etc/apt/apt.conf.d/90sandbox` | no prompts, no recommended packages |
| `sudoers` | `/etc/sudoers.d/sandbox` | passwordless sudo for `node` (uid 1000), proxy and mirror variables kept |
| `profile` | `/etc/profile.d/00-sandbox-path.sh` | a login shell's PATH and `VIRTUAL_ENV` |
| `npmrc`, `pip.conf` | `/home/sandbox/.npmrc` (uid 1000's), `/etc/pip.conf` | uid 1000's npm prefix and pip's settings, whatever the environment; root's npm keeps `/usr/local` |
| `line-buffered.pth` | `sbx-line-buffered.pth` in the venv | Python's stdout and stderr line-buffered when not a terminal (what `PYTHONUNBUFFERED` did for Claude Code's tools) |
| `video/engine/` | `/opt/engine`, `/usr/local/bin/edit` | the video edit engine: an edit plan (JSON, with HTML blocks) rendered by HyperFrames with a pinned headless Chrome; `GUIDE.md` is what the agent reads |
| `video/clip`, `video/speech-times` | `/usr/local/bin/` | a stretch of footage as a small picture file and a sound file the model can open; where speech starts and stops |
| `video/assets-manifest.json`, `video/fetch-assets.py`, `video/fonts.txt` | `/opt/assets`, `/usr/local/share/fonts/edit` | music, sound effects and animated emoji (Mixkit and Noto, fetched at build by URL and sha256: Mixkit's licence allows use, not redistribution) and Cantonese-complete fonts |
| `build.sh` | | the build, with platform 5.3's labels |
| `smoke.sh` | | the image smoke test (`ci.yml` job `sandbox-smoke`) |

What is in it: Debian trixie with Node 22; documents, PDF and spreadsheet tools, CJK fonts, a Python venv `/opt/py`
owned by uid 1000; Docker (dockerd started on demand); Claude Code 2.1.304 root-owned at `/opt/claude-code` (the
supervisor runs `/opt/claude-code/bin/claude` by that path; `/usr/local/bin/claude` links to it for shells);
Qwen Code (a release of DE0CH/qwen-code pinned by sha256: `QWEN_CODE_RELEASE`, `QWEN_CODE_SHA256`) root-owned at
`/opt/qwen-code` (`/opt/qwen-code/bin/qwen`, `/usr/local/bin/qwen`); the video edit engine, its tools and its asset
library; `/usr/local/bin/supervisor`, `supervisor-keeper`, and `sandbox-secret` and `send-file`, both `-> supervisor`.
`ENTRYPOINT ["/usr/local/bin/supervisor-keeper"]`, no CMD, no tini: the keeper is PID 1. `/home/sandbox/.harness` is
an empty directory (the supervisor's FUSE mount point); `/home/sandbox/.claude` and `/home/sandbox/.qwen` link to
`.harness/claude` and `.harness/qwen`; `/var/lib/supervisor` is root's, 0700.

A new Qwen Code: push to DE0CH/qwen-code's branch `de0ch` (its workflow `de0ch-release` tests, packs and releases
`qwen-code-de0ch.tgz` as `de0ch-<sha12>`), then put the tag and the tarball's sha256 in the Dockerfile.

The runtime starts it with the sandbox's proxy URL in `HTTP_PROXY` (the sandbox token as its password, DESIGN 12
D1) and root's capabilities inside the sandbox (`/dev/fuse`, mounts, dockerd). Without a proxy credential the keeper
exits 78 at once, logging `no sandbox proxy credential`.

## Building and testing

```
cargo build --release --locked --target x86_64-unknown-linux-musl -p supervisor -p mock-model -p fuse-rig --bins
images/sandbox/build.sh target/x86_64-unknown-linux-musl/release hermit-sandbox:dev
images/sandbox/smoke.sh hermit-sandbox:dev target/x86_64-unknown-linux-musl/release
```

`smoke.sh` checks the image as built (entrypoint, users and folders, Claude Code's path and owner, the tools, the
wrappers, sudo, apt as uid 1000 and without root, the keeper without a credential), then runs the image's own keeper
and supervisor (`--network host --privileged`) against `fuse-rig`'s world (fake hermit and fake magpie behind the
reference test gateway) and `mock-model`: a magpie `start_turn` runs Claude Code with the descriptor's managed skill
`echo`, and its Bash `curl`s the tools REST API (decision 15); its Messages API calls and that `curl` reach mock-model
and fake-tools only through the gateway's proxy as `http://model/…` and `http://tools/v1/tools/echo`; then
Docker on demand as uid 1000 and a runtime stop (exit 0). On a host whose Docker bridge has no route out (an
IPv6-only host), build with `SANDBOX_BUILD_FLAGS=--network=host` and smoke with `SMOKE_INTERNET_NETWORK=host`.
`build.sh` exports with `rewrite-timestamp=true` (platform 5.2: the layers' file times from the commit), which needs a
buildx builder with the `docker-container` driver (as `ci.yml`'s `setup-buildx-action` makes): Docker's own `docker`
driver on the containerd image store refuses it ("conflicts with unpack"). Elsewhere, create one and pass
`SANDBOX_BUILD_FLAGS=--builder=<name>`.

## Where this differs from the specs

- **linux/amd64 only**, while the service images are linux/amd64 and linux/arm64 (repository README). sandboxd's
  image service (`crates/sandboxd/src/images.rs`) takes the image's identity from `crane digest` and unpacks it with
  `crane export`, both without `--platform`; an index would need sandboxd to pick the host's platform first.
- **The image's environment defaults reach the agent through files.** Supervisor 6.2 gives every process it starts
  the image's `PATH`, `LANG`, `LC_ALL` and `HOME` and nothing else of the image's environment, so the old image's
  `NPM_CONFIG_PREFIX`, `PIP_DISABLE_PIP_VERSION_CHECK` and `DEBIAN_FRONTEND` (hermit 17's "env defaults", A5.F5)
  would be lost to Claude Code's tools. They are set where each tool reads them whatever the environment: uid
  1000's `~/.npmrc`, `/etc/pip.conf`, and the apt wrapper's own `DEBIAN_FRONTEND=noninteractive`. The image's `Env`
  has neither `NPM_CONFIG_PREFIX` nor `HOME`: hermit 5.1 and the supervisor set `HOME=/home/sandbox` themselves,
  Docker gives each user its `/etc/passwd` home, and npm's prefix follows `HOME`, so root's npm (`sudo npm install
  -g`, `docker exec` or `docker run` as root) reads `/root/.npmrc` and keeps node's `/usr/local`, as the old
  image's `sudo`, which dropped `NPM_CONFIG_PREFIX`, did. A root process started with `HOME=/home/sandbox` (hermit
  5.1's boot environment) reads uid 1000's dotfiles, `~/.npmrc` among them, as it was told to. `PYTHONUNBUFFERED`'s effect for output to a pipe comes from a `.pth` in the venv
  that makes `sys.stdout` and `sys.stderr` line-buffered when they are not a terminal (line, not byte, buffering:
  a `print` without a newline waits for one). `MPLCONFIGDIR` was matplotlib's default for `HOME=/home/sandbox`
  already; `VIRTUAL_ENV` is in the login profile (`/opt/py/bin` leads `PATH` either way).
- **No `fuse3`.** Hermit 17 lists it "for `fusermount3` if the supervisor needs it"; the supervisor mounts with
  `mount(2)` itself (supervisor 2), so the package (and its setuid helper) is left out.
- **What the gateway saw is checked by `User-Agent`.** The supervisor's own title and model calls take the same
  proxy, so the smoke reads the test gateway's record of every proxy request (fuse-rig `GET /gateway`) and requires
  absolute-form `POST http://model/v1/messages` from a client that is not `supervisor/<sha12>`, and `POST
  http://tools/v1/tools/echo` from `curl/…`. mock-model's script for the turn has Claude Code run, in Bash, the
  `curl` the `echo` skill shows; fake-tools' record (`GET /tools`) must have that call with the tools pair bearer,
  the sandbox owner as `Aviary-Sandbox-Person` and the conversation, and its answer must be the Bash result in the
  transcript. The managed files (`CLAUDE.md`, `managed-settings.json`, `.claude/skills/echo/SKILL.md`) are root's
  0644, there is no `managed-mcp.json`, Claude Code's init message lists no MCP server and no `mcp__*` tool, and its
  init message lists the skill `echo` among its `skills` (managed skills load under `--setting-sources=`).
- **Docker on demand under runc** needs `/var/lib/docker` on a volume (overlay snapshots cannot sit on the
  container's overlay root); in a sandbox it is on the scratch disk.
- **Built in the images job.** ci.yml's `images` job builds it as `hermit-sandbox:<sha12>` from `build`'s `dist`
  (layers cached in the GHA cache) and saves it as the artifact `image-sandbox`, which `sandbox-smoke` loads (runc),
  `gvisor` loads under runsc of the DE0CH/gvisor fork (smoke.sh part 2 with `SMOKE_RUNTIME=runsc`: the FUSE mount
  and a Claude Code turn in gVisor), and `e2e` runs as the harness's sandbox Pods; the `publish` job pushes it after
  every gate passed. Under runsc the keeper's VmRSS reads 9 MiB (gVisor's accounting; under 4 MiB under runc), so its
  4 MiB budget is checked under runc only.
