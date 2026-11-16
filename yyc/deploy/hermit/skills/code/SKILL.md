---
name: code
description: Read the source code and the deployment of YYC's own systems (YYC AI itself, the news stream, the label store, yyc-mcp and the rest), read-only, with git. Use it when someone asks how YYC AI or another YYC system works, why it behaved a certain way, what it is set up to do, what version is running, or wants a bug or an improvement tracked down in the code.
---

# YYC's source code and deployment

Everything YYC runs is in four git repositories. You can clone and read all of them; none can be changed from here
(a push is refused).

| Repository | What it holds | Clone |
|---|---|---|
| `DE0CH/yyc-code` | YYC's deployment and its own systems: `yyc-ai/` (YYC AI's deploy tree `yyc-ai/deploy/`, runbook `yyc-ai/README.md`, design `yyc-ai/refactor/`), `news-stream/` (the news stream, yyc-mcp, the clusters and Cloudflare under `news-stream/deploy/`), `labels/`, `nametags/`, `feishu-index/`, `socks/`, `tools/`, `infra/`, `.github/workflows/` (what deploys each part) | `git clone --depth 1 http://code/DE0CH/yyc-code /tmp/code/yyc-code` |
| `DE0CH/aviary` | YYC AI's services, in Rust: `crates/pigeon` (gateway), `magpie` (conversations), `hermit` (sandboxes), `sandboxd`, `supervisor` (runs Claude Code in the sandbox), `herald` (Telegram channels); the sandbox image `images/sandbox/` | `git clone --depth 1 https://github.com/DE0CH/aviary /tmp/code/aviary` |
| `DE0CH/jarvis3` | `chat/src/egress` (the sandbox's proxy and the routes `http://tools/`, `http://labels/`, …) and `chat/src/tools` (the web, social and data tools) | `git clone --depth 1 https://github.com/DE0CH/jarvis3 /tmp/code/jarvis3` |
| `DE0CH/claudecodeui` | the web and iOS apps staff use: `paseo/` (on Android the site is installed from Chrome; there is no Android app) | `git clone --depth 1 https://github.com/DE0CH/claudecodeui /tmp/code/claudecodeui` |

- **Clone into `/tmp/code/`, never into /workspace:** a repository has thousands of files, more than /workspace
  holds. A clone is gone when the sandbox is released; clone again when you need it.
- `http://code/DE0CH/yyc-code` is the only address on `http://code/`. It needs no token or login, and `git pull` there
  gets the latest commit. Use `git` on it, not a browser or `curl`.
- For history (`git log`, `git blame`, an older version), leave out `--depth 1` or run `git fetch --unshallow`.

## What is running now

The `main` branch of `DE0CH/yyc-code` is what is deployed: a push there is applied by the workflows in
`.github/workflows/` (YYC AI: `yyc-ai.yml`). The versions of the other three repositories that YYC AI runs are the
image tags in `yyc-ai/deploy/kustomization.yaml`, each the first 12 characters of a commit:

```sh
grep -n 'newTag' /tmp/code/yyc-code/yyc-ai/deploy/kustomization.yaml
# pigeon, magpie, hermit, sandboxd and herald: DE0CH/aviary; pigeon-app: DE0CH/claudecodeui; jarvis3-chat: DE0CH/jarvis3
git -C /tmp/code/aviary fetch --depth 1 origin <full commit> && git -C /tmp/code/aviary checkout FETCH_HEAD
```

A 12-character tag is not enough for `git fetch`; get the full commit with a full clone (`git rev-parse <tag>`), or
read `main` when the exact version does not matter.

Your own instructions and skills are files there too: `yyc-ai/deploy/hermit/system-prompt.md`, `sandbox-rules.md`
and `skills/`. The models, the budget and what the sandbox may reach are in `yyc-ai/deploy/egress/egress.json`. The
list of who may use YYC AI is not in the repository.

## Answering from the code

- Find things with `grep -rn` and `git log -- <path>`; each folder's `README.md` says how that part works.
- Staff are not technical. Answer in everyday words what the system does and why; quote a file and line only when
  the user asks for it or is the admin.
- The repositories hold no passwords or keys: secrets are encrypted files (`sealed/`), unreadable here.
- A change to the code is made by the admin. When the user wants one, describe exactly what to change (file, line,
  the new text) so they can pass it on.
