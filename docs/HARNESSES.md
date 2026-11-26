# Harnesses

A conversation's turns run in a harness: the agent program in the sandbox. There are two, **Claude Code**
(`claude`) and **Qwen Code** (`qwen`, the fork in harness/qwen-code, whose upstream is merged by hand only). Both are
first-class: the supervisor drives each natively, and the store (magpie) turns each one's transcript into the same
events and rows. Nothing above magpie knows which harness a conversation is in, except that the app asks before a
conversation moves from one to the other. `aviary_core::harness::Harness` is the type; `claude` is the default
wherever a harness is not named.

## Who picks

The model. An entry of the gateway's model list (`gateway_api::models::Model`) has `harness` (absent: `claude`) and
may have `visibleTo` (the people who may see and pick it; absent: everyone). pigeon leaves a model out of a person's
list when they are not in `visibleTo`, and sends the model's harness to magpie with the pick
(`RenameRequest.harness`); a listed model of another harness carries `metadata.harness` in the provider snapshot.
magpie keeps `picked_harness` beside `picked_model`; a turn's `start_turn` names it (`harness`) and resumes that
harness's own session (`resume`). `conversations.harness` is the harness that ran last (the one
`provider_session_id` and `main_transcript` belong to); `harness_sessions` keeps the session of each harness that
ran before, for a pick back.
The supervisor resumes a session only when its main transcript is on the mount: a sandbox lost before the
harness's first transcript write leaves none, and the turn starts that session anew (`--session-id`) instead of a
resume the harness refuses ("No conversation found") at that turn and every later one.

## In the sandbox

- **One image.** Claude Code at `/opt/claude-code`, Qwen Code at `/opt/qwen-code` (a release of the fork, pinned by
  sha256 in `images/sandbox/Dockerfile`), both root-owned.
- **One mount for both homes.** The supervisor's FUSE mount is `/home/sandbox/.harness`, with `claude/` and `qwen/`
  under it; `~/.claude` and `~/.qwen` are symlinks of the image. On the `magpie-sandbox` surface (protocol 2) a file
  is named with its home first (`claude/projects/…`, `qwen/projects/…`). magpie stores Claude Code's files without
  the prefix and Qwen Code's under `qwen/` (`ccfs/homes.rs`), so what was stored before the second harness is
  unchanged.
- **Questions are a command, in every harness.** Neither harness's question tool is offered (`AskUserQuestion`,
  `ask_user_question`, plan mode's `ExitPlanMode`): the model runs `ask` (a link to the supervisor binary) with the
  questions as JSON, which opens the question with magpie through agent.sock (`POST /v1/questions`, the
  supervisor adding its lease) and blocks on a long poll until the person answers, then prints the answer. The
  question belongs to the command, not to the turn: it shows in `Conversation.asks` (tool `ask`) with or without a
  turn, nothing but the person's answer resolves it, and a cancelled or backgrounded command leaves it open
  (`ask wait ID`, `ask status ID`). A message the person sends meanwhile is delivered as during any long command.
  Every line of a printed answer ends with `(answer to <askId>)`: the chat shows the question as a card where it was
  asked until the first tool result carrying that marker, then on that call (magpie rule 11), so it shows where the
  AI actually read the answer, whether the command waited, went to the background or was picked up later.
- **One driver.** Both speak stream-json on stdin and stdout, so `cc/run.rs` runs either; `supervisor::harness` and
  `cc/launch.rs` hold what differs: the binary and its arguments, the environment, the managed files
  (`/etc/claude-code`; `/etc/qwen-code/settings.json` with the model route and the managed skills' folder), the main
  transcript's path, the compaction command (`/compact`, `/compress`), how the
  thinking level is set (Claude Code: thinking on or off, `set_max_thinking_tokens`, and its effort level; Qwen
  Code: its effort, `set_effort`, for every level, `none` (thinking off) included, which it sends with each model
  call as `reasoning.effort`; the gateway applies the level either way), and how a turn's end waits for its
  transcript (Claude Code's lines carry the uuids its
  stdout shows, so the end waits for those lines; Qwen Code's records have their own, so its end waits for the
  writes in flight). Qwen Code takes only a UUID of version 1 to 5 as a session id: a conversation whose id is
  another kind gets the version 5 UUID made from it (`launch::qwen_session_id`). A generation is reused only by a turn of the same harness and session; a turn in the other
  harness ends it and starts that harness's.
- **Model calls.** Claude Code calls `http://model/v1/messages`; Qwen Code calls `http://model/v1/chat/completions`
  (`gateway_api::models::CHAT_COMPLETIONS`: a gateway's `model` route allows both paths) with the placeholder key
  `AVIARY_MODEL_KEY`. The fork sends a plain-http request to the proxy itself; upstream Qwen Code tunnels every
  origin with `CONNECT`, which the sandbox proxy refuses for its named routes.
- **Attachments.** Every attachment is written to `/workspace/uploads/` and listed in the message's text. A file is
  listed in `<files_input>`, for the agent to open with its own tools. An image the model takes as it is
  (JPEG, PNG, GIF or WebP, long edge at most 2,000 px, at most 3,932,160 bytes, no EXIF rotation; magpie
  `images::model_takes`) is listed in `<image_uploads>` and sent with the message: Claude Code gets it as an image
  block, Qwen Code as an `@path` reference in the text (`prompt::at_path`, escaped as its reader expects), which its
  `@` reader turns into an image part of the same request. Its stream-json input takes no other block than `text`,
  `thinking`, `tool_use` and `tool_result`; the fork refuses any other with an error result instead of passing it on
  as its JSON text. Any other image (HEIC, TIFF, a large or rotated photo) is a file: magpie makes no copy of it
  for the model; the agent converts it in its own loop. The chat shows every image as a picture either way.
- **Files to the person.** Claude Code has the `SendUserFile` tool and its hook. Qwen Code has no such tool: its
  shell runs `send-file [--caption TEXT] PATH…` (the supervisor binary under that name), which names no tool call;
  the driver puts the files on the open shell call that runs it, one send per call. Its instructions say so
  (`launch::QWEN_SEND_FILE`). Either way the app gets the files and the send's caption on the call that sent them:
  magpie's rule 6 puts the snapshot and the caption on that call's row (`sentFiles`, `sentCaption`), pigeon on the
  call's metadata under the same names.
- **The reply as it is written.** Live frames are keyed by the message id and block index the transcript's rows
  will have (magpie 7.5). Claude Code's `stream_event` lines carry them. Qwen Code upstream names a reply's stream
  by an id of its own and numbers every block 0; the fork names it by the uuid of the record it is about to write
  and numbers the blocks as the record's parts, so the driver reads both harnesses' streams the same way.
- **Background tasks.** Both write `system` lines `task_started` and `task_notification` for a background shell, a
  monitor and a subagent, and take the `stop_task` control request; Claude Code writes a task's fields on the line,
  Qwen Code (the fork) under `data` (`cc/stream.rs` reads both). A Qwen Code monitor's event is a
  `task_notification` whose status is `running`: the task goes on and the event starts a turn. A task Qwen Code
  stopped says `cancelled`. Qwen Code's tools are `run_shell_command` (`is_background: true`), `monitor` and `agent`
  (`TaskType::of_tool`), so its tasks hold the run, show in the Activity panel and wake the agent as Claude Code's do.

## In the store

- **Transcripts.** `ccfs/qwen.rs` turns a Qwen Code transcript record (`message.parts` with `text`, `thought`,
  `functionCall`, `functionResponse`, `inlineData`) into Claude Code's line shape before it is classified, so both
  make the same `cc.user` / `cc.assistant` events and the same rows; sound and video in a record are kept as blobs
  as images are.
- **A change of harness carries the conversation over.** Each harness has its own session, so a turn in a harness
  that did not run the turns before it would start without them. Its `start_turn` then has `carryOver {from, name,
  after, before}`: the events that harness has not seen (`turns/carry.rs`; `conversations.admitted_harness` and
  `carried`). The supervisor reads their rows as text (`GET /v1/sandbox/earlier`), writes it to
  `/workspace/uploads/<name>` and appends `<earlier_conversation>` to the message: the harness reads the file and
  begins its reply with a summary of the conversation so far. A pick back hands the older harness only what
  happened since it last ran. The app asks before a pick that moves a conversation with messages (Continue /
  Cancel) and shows "Summarising the conversation so far…" beside the working indicator during that turn.
- **Shares** keep the harness, the other harnesses' sessions and `carried`; a conversation continued from a share
  starts where the shared one stood.

## Going on from a turn that was cut off (Deyao, 2026-11-17)

A model keeps no state: what a turn has done is the harness's transcript. A turn that failed after its tool results
were written (the model's provider refused the request that carried them, the sandbox was lost under a tool call)
can be taken up there, with no message from anyone.

- **Who asks:** an operator, `POST /v1/admin/conversations/{id}/continue` on magpie (caller admin, `Aviary-Admin`;
  `magpie::turns::continued`). There is no button for it in the app.
- **What magpie does:** the newest failed turn that wrote something is the one to go on from. The failed messages
  after it that got no reply (a person's "continue" into a conversation that failed every message) are rolled
  back: one `retract` event takes their rows out of the timeline, and the failure of the turn that goes on with
  them. Then it admits a turn of origin `agent` with no `user.sent`: `start_turn` with `continueLast`, in the
  harness and session the conversation last ran. The driver gets it a sandbox as for any turn.
- **What the supervisor does:** it hands the harness its own request to go on instead of a message. Qwen Code has
  one, `continue_last_turn` (tool results whose reply never came are sent again; tool calls nothing answered are
  closed; the messages that failed after them are left out, harness/qwen-code `de0ch/continue-last-turn.e2e.mjs`).
  Its answer says whether a turn was cut off: when none was, the turn fails with that. What the harness then
  writes is the turn, and its `result` ends it.
- **Claude Code has no such request:** magpie refuses (409 `nothing_to_continue`), and a supervisor asked anyway
  fails the turn before a line is written.
- **A sandbox image from before this** takes `continueLast` for a field it does not know and writes an empty
  message: the sandbox image goes out before the magpie that has the route.

## Not there yet for Qwen Code

- A conversation is compacted for its media count only in Claude Code (`compactAtImages`); Qwen Code compacts
  itself at its token limit.
