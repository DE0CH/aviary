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
  commands (`commands`: name, description, argumentHint, and `builtin: true` on Qwen Code's own), as Claude Code's
  does. aviary's supervisor fills the chat
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
- `packages/cli/src/nonInteractive/session.ts` and `control/` (`stop_task`),
  `packages/core/src/services/backgroundShellRegistry.ts`, `tools/shell.ts`: a stream-json host is told of
  background work as it is of Claude Code's. Upstream's headless session never listened to the background shell registry, so a shell started with
  `is_background` was invisible to the host: aviary took the conversation for idle, showed no task and released the
  sandbox under the shell (YYC AI, 2026-11-16), and nothing woke the model when the shell ended. Here:
  - a background shell is a `system` line `task_started` (`data`: `task_id`, `tool_use_id` of the
    `run_shell_command` call, `description`, `task_type: "local_bash"`), written at once, inside the turn that
    started it. Its end waits for the running turn, then: a `user` line with the model's `<task-notification>`, a
    `system` line `task_notification` (`data`: `task_id`, `tool_use_id`, `status`, `summary`, `output_file`), and a
    turn of the model's own on that notification.
  - a monitor's `task_started` is written at once as well (upstream held it back to the end of the turn) with
    `task_type: "monitor"`; a monitor that ends with no notification for the session (cancelled silently, or owned
    by a subagent) still gets its `task_notification`. A background agent's `task_started` names
    `task_type: "local_agent"`.
  - the control request `stop_task` (`task_id`) stops a background shell, a monitor or a background agent, as
    Claude Code's does. The answer is `{stopped: true}`, `{stopped: false}` for a task that had already ended, an
    error for an id no task has. The stopped task's `task_notification` (`cancelled`) follows; the model gets no
    turn for it.
  - `de0ch/background-tasks.e2e.mjs` drives the packed CLI through all of it against a stand-in model; the
    release build runs it.
- `packages/core/src/core/upstream-refusal.ts` (called from `llm-chat.ts`),
  `services/chatRecordingService.ts` (`supersedeRefusedParts`): **a request the provider refuses rolls the
  conversation back** (Deyao, 2026-11-16). The model keeps no state and every request is rebuilt from the
  conversation, so data the provider cannot take is refused again with every later request: in YYC AI the agent
  opened a half-written `.mp4` with `read_file`, DeepInfra answered "Could not open video stream", and every
  "continue" after that failed in 2 s. Here a refusal (400, 404, 413, 415 or 422, as the HTTP status or as an `error`
  frame inside an opened stream; 404 because OpenRouter answers "No endpoints found that support input video" with
  it; never a rate limit, a server error, a timeout or a sign-in error) is blamed on
  what was added since the last request the provider answered, in stages of one more request each:
  1. the videos, sounds, images and files of the newest entry (this step's tool results, or the person's message)
     are replaced by a line from the harness that names the provider's error;
  2. every tool result of that entry is replaced the same way (each call keeps its result);
  3. the newest earlier entry that carries media, at most five entries (this is what brings back a conversation
     that was already dead when this version started).
  The model then answers the replaced entry: it sees a tool result that failed and decides what to do. A request
  refused after every stage was not refused for its data: the conversation gets its data back and the error goes
  out as before. In the transcript the refused records stay as a branch no later record descends from; the
  records after them are written again with the notices, so `--resume` reads the replacements.
  Upstream's own answer to a 400 on a request with media is gone from `openaiContentGenerator/pipeline.ts`: it sent
  the request again with every modality off, for that one request, so the refused data stayed and every later
  request was refused and resent the same way; and an error inside an opened stream (how OpenRouter passes on its
  provider's refusal) never reached it. Its test `http400-media-wedge.test.ts` went with it.
  `de0ch/upstream-refusal.e2e.mjs` drives the packed CLI through a refused video, "continue" and a resume against
  a stand-in model; the release build runs it.
- `packages/cli/src/nonInteractive/io/StreamJsonOutputAdapter.ts`, `packages/core/src/services/chatRecordingService.ts`
  (`reserveAssistantRecordUuid`): **the stream names a reply as the transcript records it.** aviary shows a reply
  as it is written from the `stream_event` lines and then finds it in the transcript, matching the two by (message
  id, block index), as Claude Code's stream and transcript allow. Upstream names a reply's stream by an id of its
  own (that of the reply's first part: it cuts a reply into one message per block type) and numbers every block 0,
  while the transcript has one record per reply with its own uuid and its parts in order. Nothing matched: in YYC AI
  every answer stood twice in the chat for a moment (the streamed text and the finished message), the chat jumped
  down and back, and aviary never learned that a streamed block was finished. Here `message_start` carries the uuid
  the reply's record will have (reserved in the recording service, taken by the main agent's next assistant record,
  dropped when the reply is finalized or its attempt given up), and the main agent's blocks are numbered through the
  reply as the record's parts are. The `assistant` lines on stdout are as upstream's.
  `de0ch/stream-record-ids.e2e.mjs` compares the packed CLI's stream with the transcript it wrote; the release
  build runs it.
- `packages/cli/src/nonInteractive/control/controllers/systemController.ts` (`set_effort`),
  `packages/core/src/config/config.ts` (`setThinkingOff`), `core/openaiContentGenerator/pipeline.ts`: **the control
  request `set_effort` takes `none`, thinking off** (YYC AI, 2026-11-16). Upstream's `set_effort` takes only its
  tiers (low to max), and its own "thinking off" (`reasoning: false`) names no level in the request, in shapes
  (`enable_thinking`, `thinking.type`, `reasoning.enabled` on an openrouter.ai host) that aviary's gateway does not
  read: the gateway takes the level from `reasoning.effort` and gives a request that names none the model's
  default (`high` for YYC AI's), so its host had no way to switch thinking off. Here, after `set_effort` `none`
  every request carries `reasoning: {effort: "none"}` (which switches thinking off at OpenRouter) until
  `set_effort` names a tier again; the tier set before is kept underneath and is not sent.
  `de0ch/thinking-level.e2e.mjs` drives the packed CLI through a tier, `none`, a tier and `none` again against a
  stand-in model; the release build runs it.
- `de0ch/continue-last-turn.e2e.mjs` (no change to the code: upstream's control request `continue_last_turn`,
  `packages/cli/src/nonInteractive/session.ts`): **what aviary's supervisor relies on when an operator continues a
  conversation whose turn was cut off** (Deyao, 2026-11-17). The request has the agent go on with no message from
  anyone: tool results whose reply never came are sent again, the messages that failed after them are left out and
  do not come back with the next message, a second request is not accepted, and data the provider refuses on the
  way is replaced as above. The release build runs it, so a merge of upstream that changes any of this fails here
  and not in a conversation.
- `de0ch/` and `.github/workflows/de0ch-release.yml`: this file, the end-to-end checks and the release build.

Everything else aviary needs is configuration, not a change here: the system settings file
(`/etc/qwen-code/settings.json`: the models on the model route, `skills.directories` for the managed skills),
`--append-system-prompt` for the managed instructions, `--exclude-tools`, and the stream-json protocol on stdin and
stdout, which aviary's supervisor drives (`crates/supervisor/src/harness.rs`, `cc/launch.rs`, `cc/managed.rs`).

## Releases

A push to `de0ch` builds the CLI as upstream publishes it to npm (`npm run build`, `npm run bundle`,
`npm run prepare:package`, `npm pack` in `dist/`) and releases the tarball as `de0ch-<sha12>` with its sha256.
aviary's sandbox image installs that tarball, pinned by sha256, at `/opt/qwen-code`.
