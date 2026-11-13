# Harnesses

A conversation's turns run in a harness: the agent program in the sandbox. There are two, **Claude Code**
(`claude`) and **Qwen Code** (`qwen`, the fork DE0CH/qwen-code, whose upstream is merged by hand only). Both are
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

## In the sandbox

- **One image.** Claude Code at `/opt/claude-code`, Qwen Code at `/opt/qwen-code` (a release of the fork, pinned by
  sha256 in `images/sandbox/Dockerfile`), both root-owned.
- **One mount for both homes.** The supervisor's FUSE mount is `/home/sandbox/.harness`, with `claude/` and `qwen/`
  under it; `~/.claude` and `~/.qwen` are symlinks of the image. On the `magpie-sandbox` surface (protocol 2) a file
  is named with its home first (`claude/projects/…`, `qwen/projects/…`). magpie stores Claude Code's files without
  the prefix and Qwen Code's under `qwen/` (`ccfs/homes.rs`), so what was stored before the second harness is
  unchanged.
- **One driver.** Both speak stream-json on stdin and stdout, so `cc/run.rs` runs either; `supervisor::harness` and
  `cc/launch.rs` hold what differs: the binary and its arguments, the environment, the managed files
  (`/etc/claude-code`; `/etc/qwen-code/settings.json` with the model route and the managed skills' folder), the main
  transcript's path, the compaction command (`/compact`, `/compress`), the tool that asks the person a question
  (`AskUserQuestion`, `ask_user_question`; Qwen Code's answers are keyed by the question's index), and how the
  thinking level is set. A generation is reused only by a turn of the same harness and session; a turn in the other
  harness ends it and starts that harness's.
- **Model calls.** Claude Code calls `http://model/v1/messages`; Qwen Code calls `http://model/v1/chat/completions`
  (the gateway's `model` route must allow both paths) with the placeholder key `AVIARY_MODEL_KEY`.
- **Files to the person.** Claude Code has the `SendUserFile` tool and its hook. Qwen Code has no such tool: its
  shell runs `send-file [--caption TEXT] PATH…` (the supervisor binary under that name), which names no tool call;
  the driver puts the files on the open shell call that runs it, one send per call. Its instructions say so
  (`launch::QWEN_SEND_FILE`).

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

## Not there yet for Qwen Code

- Background tasks of Qwen Code's tools are not tracked (the Activity panel lists Claude Code's).
- A conversation is compacted for its media count only in Claude Code (`compactAtImages`); Qwen Code compacts
  itself at its token limit.
