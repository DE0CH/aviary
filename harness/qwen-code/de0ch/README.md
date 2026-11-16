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
- `packages/core/src/core/openaiContentGenerator/media-budget.ts` (called from `converter.ts`): a request carries at
  most one video and two audio parts, the newest; an older one is replaced where it was by a line saying it can be
  opened again with `read_file`. DeepInfra's MiMo endpoint refuses more ("Too many videos in request: 2 > 1", "At most
  2 audio(s) may be provided in one prompt"), and upstream's answer to that 400 is to resend with every video, sound
  and image replaced by "[Unsupported … file]": from the second clip of a conversation on, the model was told it takes
  no media at all (YYC AI, 2026-11-16).
- `packages/cli/src/nonInteractive/control/controllers/permissionController.ts`: a permission request to the host
  (`can_use_tool`, which carries the agent's question to the person) waits as long as a timer can (24.8 days), not
  60 s. Upstream cancelled the tool call with "The host could not present the required approval" while the host
  still showed the question.
- `packages/cli/src/nonInteractive/control/controllers/systemController.ts`: the `initialize` answer names the slash
  commands (`commands`: name, description, argumentHint), as Claude Code's does. aviary's supervisor fills the chat
  page's "/" list from that answer; without it the list was empty for every Qwen Code conversation.
- `packages/core/src/telemetry/qwen-logger/qwen-logger.ts`: no usage statistics (Deyao, 2026-11-16). Upstream posts
  a record of every session, prompt, tool call and model request to Alibaba Cloud RUM
  (`gb4w8c3ygj-default-sea.rum.aliyuncs.com`) unless `privacy.usageStatisticsEnabled` is false. Here
  `QwenLogger.getInstance()` hands out no logger whatever that setting says, and the request, its endpoint and its
  proxy and retry handling are gone from `flushToRum`. The release build refuses a bundle that names the endpoint.
  `no-usage-statistics.test.ts` replaces upstream's `qwen-logger.test.ts`; upstream's tests that expect a
  `QwenLogger` to be called (`telemetry/loggers.test.ts` and others) do not hold in this fork. The setting itself
  still gates what else upstream hangs on it (the models.dev catalogue refresh). OpenTelemetry export
  (`telemetry.*` settings) is upstream's: off unless configured, and aviary's settings turn it off.
- `de0ch/` and `.github/workflows/de0ch-release.yml`: this file and the release build.

Everything else aviary needs is configuration, not a change here: the system settings file
(`/etc/qwen-code/settings.json`: the models on the model route, `skills.directories` for the managed skills),
`--append-system-prompt` for the managed instructions, `--exclude-tools`, and the stream-json protocol on stdin and
stdout, which aviary's supervisor drives (`crates/supervisor/src/harness.rs`, `cc/launch.rs`, `cc/managed.rs`).

## Releases

A push to `de0ch` builds the CLI as upstream publishes it to npm (`npm run build`, `npm run bundle`,
`npm run prepare:package`, `npm pack` in `dist/`) and releases the tarball as `de0ch-<sha12>` with its sha256.
aviary's sandbox image installs that tarball, pinned by sha256, at `/opt/qwen-code`.
