# DE0CH/qwen-code

Qwen Code as Deyao's stacks run it: the second harness of aviary's sandboxes (YYC AI, Jarvis), beside Claude Code.
The default branch is `de0ch`, cut from upstream's release tag `v0.27.3`. **Nothing merges from upstream by
itself**: a new upstream release is merged by hand (`git fetch https://github.com/QwenLM/qwen-code
refs/tags/vX.Y.Z` and `git merge`, never a rebase), tested against aviary's supervisor, and only then released.
`main` is upstream's `main` as it was when the fork was made and is not used.

## What differs from upstream

- `packages/core/src/core/openaiContentGenerator/converter.ts`: an inline audio part is sent as the bare base64 the
  OpenAI chat completions API defines for `input_audio.data`. Upstream sends a `data:` URI, which DashScope takes
  and OpenRouter refuses with 400; OpenRouter is the only endpoint this fork is run against.
- `packages/core/src/utils/runtimeFetchOptions.ts`, `config/config.ts` (`PLAIN_HTTP_VIA_PROXY`): a plain-http request
  through an http proxy goes to the proxy itself (absolute-form), not through a `CONNECT` tunnel. aviary's sandbox
  proxy serves its named routes (`http://model/v1`) only that way and answers `CONNECT model:80` with 403; upstream
  tunnels every origin, so every model call failed ("Proxy response (403) !== 200 when HTTP Tunneling").
- `de0ch/` and `.github/workflows/de0ch-release.yml`: this file and the release build.

Everything else aviary needs is configuration, not a change here: the system settings file
(`/etc/qwen-code/settings.json`: the models on the model route, `skills.directories` for the managed skills),
`--append-system-prompt` for the managed instructions, `--exclude-tools`, and the stream-json protocol on stdin and
stdout, which aviary's supervisor drives (`crates/supervisor/src/harness.rs`, `cc/launch.rs`, `cc/managed.rs`).

## Releases

A push to `de0ch` builds the CLI as upstream publishes it to npm (`npm run build`, `npm run bundle`,
`npm run prepare:package`, `npm pack` in `dist/`) and releases the tarball as `de0ch-<sha12>` with its sha256.
aviary's sandbox image installs that tarball, pinned by sha256, at `/opt/qwen-code`.
