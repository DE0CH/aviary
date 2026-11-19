---
name: code
description: Read the source code and the deployment of YYC's own systems (YYC AI itself, the news stream, the label store, yyc-mcp and the rest), read-only, with git. Use it when someone asks how YYC AI or another YYC system works, why it behaved a certain way, what it is set up to do, what version is running, or wants a bug or an improvement tracked down in the code.
---

# YYC's source code and deployment

Everything YYC runs is in two git repositories. You can clone and read both; neither can be changed from here
(a push is refused).

| Repository | What it holds | Clone |
|---|---|---|
| `DE0CH/aviary` | YYC AI and everything it runs on: `crates/` (the services, in Rust: `pigeon` the gateway, `magpie` the conversations, `hermit` the sandboxes, `sandboxd`, `supervisor` which runs Claude Code in the sandbox, `herald` the Telegram channels), `images/sandbox/` (the sandbox image), `app/` (the web and iOS apps staff use; on Android the site is installed from Chrome), `egress/` (the sandbox's proxy and the routes `http://tools/`, `http://labels/`, …, and the web, social and data tools), `harness/qwen-code/` (the second agent), `drive/` (YYC Drive), `yyc/` (YYC AI's deployment: `yyc/deploy/`, runbook `yyc/README.md`, design `yyc/refactor/`) | `git clone --depth 1 https://github.com/DE0CH/aviary /tmp/code/aviary` |
| `DE0CH/yyc-code` | YYC's other systems and its servers: `news-stream/` (the news stream, yyc-mcp, the clusters and Cloudflare under `news-stream/deploy/`), `labels/`, `nametags/`, `feishu-index/`, `socks/`, `tools/`, `infra/`, `.github/workflows/` (what deploys each part) | `git clone --depth 1 http://code/DE0CH/yyc-code /tmp/code/yyc-code` |

- **Clone into `/tmp/code/`, never into /workspace:** a repository has thousands of files, more than /workspace
  holds. A clone is gone when the sandbox is released; clone again when you need it.
- `http://code/DE0CH/yyc-code` is the only address on `http://code/`. It needs no token or login, and `git pull` there
  gets the latest commit. Use `git` on it, not a browser or `curl`.
- For history (`git log`, `git blame`, an older version), leave out `--depth 1` or run `git fetch --unshallow`.

## What is running now

The `main` branch of each repository is what is deployed: a push there is applied by the workflows in its
`.github/workflows/` (YYC AI: `aviary`'s `deploy.yml`, described in `docs/DEPLOY.md`). Each image's tag is the first 12
characters of the last commit that changed its part; `deploy/images.json` lists each part's folders, so the running
version of a part is:

```sh
git -C /tmp/code/aviary fetch --unshallow   # history is needed
git -C /tmp/code/aviary log -1 --format='%h %cs %s' origin/main -- app   # e.g. the app; use a part's folders
```

Your own instructions and skills are files there too: `yyc/deploy/hermit/system-prompt.md`, `sandbox-rules.md`
and `skills/`. The models, the budget and what the sandbox may reach are in `yyc/deploy/egress/egress.json`. The
list of who may use YYC AI is not in the repository.

## Answering from the code

- Find things with `grep -rn` and `git log -- <path>`; each folder's `README.md` says how that part works.
- Staff are not technical. Answer in everyday words what the system does and why; quote a file and line only when
  the user asks for it or is the admin.
- The repositories hold no passwords or keys: secrets are encrypted files (`sealed/`), unreadable here.
- A change to the code is made by the admin. When the user wants one, describe exactly what to change (file, line,
  the new text) so they can pass it on.
