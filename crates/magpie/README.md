# magpie

The store of record for conversations (magpie spec). One binary: `magpie serve` (one replica; several run on one
Postgres) and `magpie --version`. Its REST contract is `crates/magpie-api`; this crate implements
`magpie_api::server::Server` and serves it behind the caller check.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | start (env, ops port 9090, migrations, LISTEN, jobs, API on 8080) and drain |
| `src/admin.rs` | hermit's routes (a leaver's purge, `workspaces/exists`) and the admin reads with their audit |
| `src/app.rs` | `Service` (start and the drain's last steps), `Magpie` (shared state), the `Server` impl, the router |
| `src/auth.rs` | callers from `MAGPIE_ACCEPT_*`, the route layer that checks them against `magpie_api::routes::ROUTES` |
| `src/limits.rs` | every limit and timeout (magpie 17) |
| `src/error.rs` | the one error type; Postgres unreachable answers 503 `unavailable` with `retry-after` |
| `src/db/` | the migrations (`0001_init.sql`: every table of magpie 4 and the importer's of cutover 7.3; `0005_timeline.sql`: the projection's lookups; `0008_timeline_derivation.sql`: `rows.derivation`, `line_pos`, the live delta index) and `Db` |
| `src/conversations/` | the Conversation object, the person row, the row lock; create, list, rename, delete, restore, purge |
| `src/events/` | `events::append` (seq allocation), which calls the projection |
| `src/rows/` | the timeline projection (magpie 7.3): `project` (rules 1-10, row seqs, the epoch), `derive` (what each event makes of a row, pure), `read` (M9 pages and the long-poll, M10, search) |
| `src/live.rs` | live frames (magpie 7.5): receiving, the `LiveState`, `GET …/live`, the cleanup run by every replica |
| `src/changes.rs` | the per-person change feed (M12): `emit`/`upserted` in a transaction, `read` with its long-poll |
| `src/fanout.rs` | the LISTEN hub (four channels) and the long-poll loop every held request uses |
| `src/jobs/` | the job leader, purge, outbox (hermit calls), retention |
| `src/files.rs` | blob reads (M15, the sandbox's), the sandbox's blob `PUT`, sent-file snapshots (magpie 11.2), the files panel and downloads (M14: a send's snapshot, or the live workspace file streamed from hermit) |
| `src/uploads.rs` | M13: uploads, image facts and the model copy |
| `src/shares.rs` | M16: freeze (rows, the cc scope cut before a running turn, blob refs, hermit's clone), list, revoke, view, downloads from the frozen copy, continue |
| `src/viewas.rs` | M17 and the view-as audit rows written with each change |
| `src/sandbox.rs` | the conversation behind a gateway call, and its lease |
| `src/stats.rs` | the ops reads (`/v1/ops/people`, `/v1/ops/view-as`, `/v1/ops/stats`) |
| `src/ccfs/` | Claude Code state behind the FUSE layer (magpie 6): `scan`/`extract` (a JSONL line into stub and blobs), `classify` (main-transcript lines into events), `lines` (the rewrite algorithm, main-path rules, promotion, rename-over), `limbo` (retraction, the quiet-file sweeper), `pages` (byte files), `reconstruct` (tree and streamed reads), `lease`, `ops` (the state-changing routes) |
| `src/blobs/` | the bucket (`store`), `staging` (blobs a cc write introduces, staged in its transaction), the read cache and every variant (`read`), images and their copies (`images`), the uploader of staged blobs (`uploader`), the GC and the bucket reconcile (`gc`) |
| `src/http/` | the handlers, one module per route group (no SQL) |
| `src/turns/` | turns (magpie 8): admission (M7, M3's first message), the driver (hermit's get-or-start and long-poll), the command queue and acks, the supervisor's reports and heartbeat reconciliation, cancel (M8) and ask answers (M11), titles, the sweeper (deadlines, watchdog, lapsed claims, availability tick), and `end::end`, the one way a turn ends |
| `src/hermit/` | the hermit sandbox-events feed follower (magpie 8.7, R8), run by the job leader |

## The internal seam

The packages after the core build on these, all inside one `aviary_pg::tx!` transaction:

- `conversations::lock(tx, person, id)` locks the conversation row first (one lock order: conversation, then
  person); `Locked::refuse_migrating()` answers 409 `conversation_migrating` with `route: "legacy"`.
- `events::append(tx, conversation, NewEvent) -> seq` allocates the next seq and calls `rows::project(tx, &stored)`.
- `changes::emit(tx, person, conversation, &ChangeBody)` / `changes::upserted(…)` append to the person's feed and
  notify `magpie_changes`.
- `fanout::notify(tx, channel, key, head)`, `Fanout::hold(channel, key, wait, draining, query, empty)` and
  `Fanout::hold_until(&[(channel, key)…], wait, draining, query)` (several watches; the last answer on timeout).
- `jobs::outbox::enqueue(tx, &Item)` queues a hermit call made by the job leader after the commit.
- `ccfs::lines::set_main_transcript(tx, conversation, path)`: the supervisor's `running` report (WP-14) sets the
  main path in its transaction; lines already written there are classified then (6.2).
- `ccfs::limbo::flush_scope(tx, conversation, turn)`: an accepted end report flushes the turn's limbo (6.7).
- `ccfs::set_resync(tx, conversation)`: a `cc.write_stuck` report; the next lease answers `resync: true`.
- Classification fills `tool_links` (`use_seq` for each `tool_use`, `result_seq` for each result, magpie 6.5), so the
  projection (WP-13) reads it rather than writing it.
- A cc transaction never calls the bucket: `cc_tx!`/`cc_op!` run it, and when it needs bytes of a blob that is only
  in the bucket (`Error::NeedBlobs`) they fetch those through `blobs::fetch_stored` (WP-15 fills it; until then it
  answers 503) and run it again.

## Tests

`cargo test -p magpie` against the CI Postgres (`AVIARY_TEST_DATABASE_URL`): each test makes its own database and
runs one or more replicas in-process (`tests/common`). `tests/service.rs` checks every route's callers, readiness
(migrations and LISTEN gates; Postgres lost after start keeps `/readyz` 200 while requests answer 503), the
migrations under concurrent starts, the leader hand-over, the outbox and the ops reads; `tests/conversations.rs`
the conversation routes; `tests/changes.rs` the change feed and the fan-out across replicas; `tests/timeline.rs`
every projection rule from events to rows, the epoch, the pages, the M9 long-poll (50 ms across replicas, no
connection held, drain), the E6-style cost test, prompts and search; `tests/projection.rs` the property test
(random event sequences against an in-memory reference projector, `row_seqs` density, `after` from every seq);
`tests/live.rs` live frames; `tests/turns.rs` the turn state machine with fake-hermit and a scripted
supervisor (the sweeper's passes are called directly, deadlines aged in SQL).

The ccfs suite (WP-12): `tests/ccfs.rs` (files, byte files, directories, renames, errno codes, `opId` replays,
rule 0, the lease with and without hermit, quota and staging limits, blobs, events, promotion, tombstones,
rewrites, `meta.replaces`, compaction, set-aside, away-and-back, the limbo debounce, overwrites, streamed reads);
`tests/ccfs_model.rs` (random operation sequences against a byte-vector model: the file after every step, and the
visible events against the classification of the model's lines); `tests/ccfs_claude_code.rs` (Claude Code
2.1.304's real transcripts from the FUSE gate, replayed in the shape of the gate's recorded calls, with pinned
event counts); `tests/ccfs_perf.rs` (ignored here; ci.yml's `magpie-perf` job runs it with a release build against
`.github/ci/postgres-sync.sh`'s primary and synchronous standby: p50 <= 15 ms for a 10 KiB append, p99 <= 150 ms
for a 1 MiB image line). The scanner, extraction and classification have unit and property tests in the crate.

Tests that store blobs also need the CI S3 server (`AVIARY_TEST_S3_*`; each test database gets its own bucket):
`tests/blobs.rs` (staged blobs served before the uploader stores them, a bucket outage, the GC's rules and the
reconcile, M15's variants and headers, the sandbox's blob routes, cc writes that add blobs while the bucket is unreachable) and `tests/uploads.rs` (M13's limits and types,
the model copy's bounds, EXIF rotation, HEIC through libheif, the decode cap, view-as recording of an upload and of a send, retention). `tests/files.rs`: the two-phase
sent-file snapshot, its validation and lease, the files panel, downloads of a send's snapshot with `Range`, live
workspace files through fake-hermit. `tests/shares.rs`: the freeze leaves a running turn out, the view equals the
timeline at the freeze, the frozen workspace is a clone, soft delete hides a link and restore brings it back,
continue, revoke, purge, view-as, a failed clone. `tests/admin.rs`: a leaver's purge, `workspaces/exists`, the admin
reads and their once-a-day audit. `tests/fixtures/` holds a small HEIC made with libheif's `heif-enc`.

## Deviations from the spec text

- `POST /v1/conversations/{id}/delete` answers the Conversation, as `magpie-api` (the contract) has it, not
  `{deletedAt}` (magpie 9.6).
- A migrating conversation's mutations answer 409 `conversation_migrating` with `route: "legacy"` and
  `retryAfterMs` (DESIGN 12 D14, cutover 6.4); the fence's wait is the window routes' (WP-51).
- The applied migrations are recorded in aviary-pg's `_migrations`, not `schema_migrations` (platform 4.3).
- `0001_init.sql` adds the index `events_asks` (the open ask of `Conversation.asks` is read from the ask events).
- The replica name comes from the downward API's `POD_NAME` (aviary-core's `log::init_in_pod` reads it), not a
  separate `MAGPIE_POD`.
- `/v1/ops/people`: `total` counts every conversation not purged (Recently deleted included); `today` and `last7d`
  count conversations active since the zone's midnight and in the last 7 days.
- `GET /v1/ops/view-as` pages by `before=<id>` (magpie 13), not `before=<RFC 3339>` (platform 15.3): a time
  truncated to the wire's milliseconds skips entries that share a time or differ by less than a millisecond. Each
  entry carries its `id`; entries are ordered newest first by `(at, id)` and a page holds those older than the
  `before` entry (migration `0003_view_as_order.sql` adds the index).
- `/v1/ops/stats` `events` and `rows` are Postgres's estimates (`pg_class.reltuples`), so the call does not grow
  with the two largest tables; the other figures are exact.
- Restore drops the delete's queued `release_sandbox {reason: deleted}`, and the outbox drops one whose
  conversation is no longer deleted before calling hermit (magpie 9.6 says only that the next turn gets a new
  sandbox; the release names only the conversation, so a late one would release the restored conversation's live
  sandbox). A release already in flight when the restore commits still lands; the window is one hermit call
  (10 s at most); a turn caught by it fails `sandbox.lost` (magpie 8.8) and the next turn gets a new sandbox.
- Timeline rows (WP-13):
  - A row's block index counts the blocks of the lines of the same `message.id` that still count and stand before
    it (magpie 7.3 rule 4 says only "counts earlier lines"). A line with `meta.replaces = R`, R a line of the same
    message, stands where R stood (`line_pos`), so a rewritten line keeps its `(messageId, blockIndex)`. WP-12
    writes the rewritten line before R's retract (at the end of an overwrite or rename-over; for a truncate when
    its limbo flushes, up to 10 s later): while R still counts, the new line takes over R's rows in its own
    transaction. R counts as gone from then on (it is recorded in `retracted`, beside the seqs retracts removed;
    events stay immutable); R's row at the same block index and of the same kind (the same call for a tool row) is
    updated in place at a new row seq and keeps its `row_id` (a tool row is rebuilt from the new line and R's later
    events, so it keeps its result, snapshot and task), or keeps its version when nothing visible changed; a row of
    R the new line has no block for, or of another kind, is deleted with a new epoch, as rule 9 would. R's retract
    then touches no row. So no commit shows two rows with one `(messageId, blockIndex)` or two tool rows with one
    `callId`. A `replaces` naming any other event is ignored.
  - An event that changes nothing visible on its row (a repeated `task.status` progress report, an identical
    `files.sent` or result) takes no row seq and writes no version: it is only added to the row's sources (a
    rebuild reads it). magpie 7.3 says rules 5-7 update the row; an unchanged item is no update.
  - Rule 3 replaces the launch's result text with the notification's summary whenever the notification has one
    (today's folding, `replaceLaunchToolResultContent`, does the same for every launch); a launch whose output was
    extracted keeps `blobId` and `bytes` (the link to its full output), with `truncated` set. A notification's
    `killed`/`stopped` is `task.status: stopped`, row status `canceled`. `meta.taskNotification` gained an optional
    `taskId` (magpie-api).
  - A failed `turn.ended` without an error makes an `error` row `run.error`; failed tool rows carry the turn's error.
  - A rebuilt tool row (rule 9) is made from its surviving source events in the order they first landed on it, and
    keeps its `row_id` and block index.
  - No `derivationVersion` lazy rebuild (pigeon 14.1.18 item 3): magpie 7.3 changes the epoch only under rule 9.
    Each row records the derivation version it was made or last updated by (`rows.derivation`,
    `rows::DERIVATION`, now 1): a release that changes the rules raises it and ships a job that rebuilds the rows
    below it from their events (the importer and shares project with the same code, so they carry the version too).
  - M9: `minSeq` is 0 when the conversation has no rows; `after` entries carry `seqs` up to their own `seqEnd`;
    for an `after` page `hasOlder` means a row with `seqStart <= after` exists. `waitMs`/`liveAfter` without
    `after` answer 400. A held `after` poll also answers when the epoch changed (a retract that only deleted rows
    issues no row seq).
  - Live frames: frames are numbered per turn, so M9's `liveAfter` goes with `liveTurn` (the `LiveState.turnId` it
    counts in; either alone answers 400; magpie-api `TimelineQuery.live_turn`). A `liveTurn` that is not the active
    turn, or a `liveAfter` greater than the turn's last frame, answers `reset: true`; a block whose
    first frames are missing (a truncated unlogged table) or a delta past the block's end resets, and the block's
    text starts at that delta's offset; a frame of a block whose durable row exists is ignored.
  - Migrations `0005_timeline.sql` and `0008_timeline_derivation.sql` create their indexes without `CONCURRENTLY`:
    they run on the empty database magpie migrates at start, before the importer fills it (cutover 7). An index
    added once the tables hold data goes in a migration of its own, `CONCURRENTLY`.
  - Search counts occurrences in the `user`/`assistant` rows an event made (events' `text` holds only those); its
    cursor is the event seq after which the next page starts.

- Turns (magpie 8), where the spec text leaves a case open:
  - A sandbox that goes away (released or lost) before the supervisor acknowledged `start_turn` sends the turn back
    to preparing and gets a new sandbox (hermit spec 28, magpie item 1), in the driver's long-poll and in the feed;
    only `sandbox_busy` / `sandbox_failed` (a start that failed), a release of a conversation in Recently deleted,
    or a sandbox lost after the acknowledgement fail it. A replacement's deadline is 15 min from the replacement.
  - A `running` report may arrive while the turn is still `admitted`/`preparing` (the supervisor takes
    `start_turn` as soon as it boots, before hermit's ready reaches the driver): it is the turn's ready too.
  - A cancel stops a turn whose `start_turn` a supervisor has received (`delivered_at` set) by `interrupt`, in any
    state; only an undelivered `start_turn` ends the turn `canceled` at once (8.6 names the states, not the
    delivery, which is what decides whether Claude Code may already run it).
  - The end of a turn cancels every unacknowledged command of the turn except a `kill` (8.3 says "undelivered"): a
    delivered but unacknowledged `start_turn` or `interrupt` of an ended turn must not reach the conversation's next
    supervisor, which polls from `after=0`.
  - The `starting` and `stopping` deadlines count, like the running one, from the later of their start and
    `availability.available_since`, so an outage of magpie never fails a turn whose supervisor's ack or report was
    only waiting for magpie.
  - An agent-started turn reported while another turn is active answers 409 `bad_transition` (with the active
    turn's state).
  - M11 queues one `answer_ask` per ask (a second answer is 409 `already_resolved`); the command's stored payload
    also keeps the person's whole answer, which `ask.resolved` carries; the supervisor receives the typed command.
  - `title` is queued only while no `title` command is pending.
  - The heartbeat's extra fields (`gen`, `supervisor`, `cc`) are logged; the admin reads that show them are WP-15's.
  - Old asset paths in `files[].path` (the switch window's legacy uploads) answer `unknown_upload` until WP-51's
    window routes resolve them.
  - The end report's `transcript_pending` check reads the conversation's `cc.*` events and the last 2,000 lines of
    its main transcript (`cc_lines` stubs, which keep each line's `uuid`).
  - The sandbox routes check the lease as the cc routes do: the current `lease_id`, held by the calling sandbox.

### Claude Code state (WP-12)

- No answer is `offset_mismatch`: a write's query has no append flag, and magpie 6.6 rule 4 makes a write inside
  the file an overwrite (as `pwrite`; the contract's fake does the same). A lost acknowledged append shows as the
  next append's offset past magpie's end, answered `offset_beyond_eof`, which the supervisor repairs the same way
  (supervisor 9.5 item 6).
- A truncate cannot grow a lines file (`offset_beyond_eof`, rule 1); a byte file grows and a write past its end
  zero-fills (6.8). The 256 MiB file cap applies to lines files too (the supervisor caches files of up to 256 MiB
  whole).
- Only the main transcript's lines go to limbo or are retracted: a file renamed away from the main path keeps its
  lines' events (`moved` limbo) and a later truncate of it retracts nothing. `moved` limbo is matched by sha256
  (the earliest event with that sha), not in order first.
- The end report's flush (`limbo::flush_scope`) flushes the conversation's whole `truncated` limbo: limbo rows carry
  no turn, and one turn runs at a time.
- A `type: "user"` line with `isCompactSummary` is a `cc.user` event that is not a prompt and never the echo (the
  table of 6.5 has no row for it; it is no message a person sent).
- A `custom-title`/`ai-title` record sets the provisional title cut to 200 characters; a title or model change
  emits `conversation_upserted`, a context-token change alone does not (the next turn change carries it). The
  model `<synthetic>` (Claude Code's own API error lines) is not recorded as the conversation's model.
- `meta.replaces` also names the event of an old line that this operation's rewrite replaces, or that waits in the
  file's limbo (a truncate-then-rewrite of a changed line retracts it only at the flush, after the new line).
- Migration `0004_ccfs.sql` adds the blob's preview and line count (an event for a stored line is made without
  its bytes), `cc_lines.classified` (a promotion classifies only lines never classified, so an echo or an
  internal line is never classified twice), `cc_retracted` (uuids of retracted lines, for `meta.replaces` by index)
  and two limbo indexes.
- `GET /v1/sandbox/cc/tree` and `/cc/file` need the sandbox identity, not the lease (they change nothing).
- A read's first batch of lines is made before the answer starts (a sha mismatch or a missing blob there answers
  500 / 503); a later failure, or the file changing while it streams, ends the body early. Every reconstructed
  line's sha256 is checked in a debug build and inside a transaction; one in 1,000 in a release build's reads.
  Blobs of a batch are read from staging in one query; the read cache, the 16-blob prefetch and bucket reads come
  with WP-15's store, and with them the 87 MB read target.
- The lease asks hermit only when its feed's record does not name the caller as `starting` or `ready`; hermit's 404
  is "no sandbox" (`not_current_sandbox`), any other failure or 2 s without an answer 503.
- The 2.1.304 acceptance replays the real binary's transcripts from the FUSE gate through the routes, in the shape
  of the gate's recorded calls; the real binary against a mock Messages API through a FUSE supervisor is the e2e
  harness's (WP-28). The latency figures are measured at the client over loopback HTTP (they include the round
  trip), with a 2-node synchronously replicated Postgres in CI whose WAL is warmed first (segments written once and
  recycled, as on a cluster that has run for a while; on a fresh cluster each new 16 MiB segment file costs one
  write about 150 ms on a CI runner's disk).
- An upload's or a sent file's body (at most 50 MiB and 64 MiB) is read whole, hashed, then put in the bucket under
  its content key with one `PUT`, rather than hashed while it streams (the key is the hash, known only at the end).
  No object magpie writes is over 64 MiB, so magpie 5.2's multipart path has nothing to carry and is not built.
- The bucket and the GC are kept consistent by an advisory lock per blob (the two-key space, classid `mblb`):
  every writer of a blob's object holds it shared from before its `PUT` until its row commits (uploads, sent files,
  the uploader); the GC holds it exclusively from before it deletes a row until it has deleted the objects, and the
  reconcile re-checks each orphan under it. Without it a GC deleting an old unreferenced blob could delete the
  object a concurrent writer had just put again (magpie 5.3 names only the row lock, which no bucket call can sit
  inside, platform 4.3).
- The GC also keeps a blob that a staging row, an upload row or a sent-file row names (their foreign keys), on top
  of magpie 5.3's references.
- The `display` and `thumb` variants have their own etags (`"<sha>-display"`, `"<sha>-thumb"`); `full` is
  `"<sha>"`. `display` of a media blob that is not an image (a PDF) answers 415 like `thumb`.
- A derived copy (thumbnail, display copy) is served without being kept while the bucket cannot be reached.
- An image upload's `width`/`height` are the upright dimensions (swapped for EXIF orientations 5-8).
- A share freezes the rows whose identifying event (`source_seqs[1]`) is within `upto_seq`, as they are at the
  freeze, rather than rows whose source events are all within it (magpie 12.1): a background task's notification
  updates a finished turn's tool row after the turn, and the stricter rule would drop that row from the share.
  `nothing_to_share` uses the same rule for the assistant row.
- A share's blob references are a copy of the conversation's at the freeze (a superset of what the frozen lines,
  rows and events reference: it may keep a running turn's blobs alive for as long as the share lives).
- `share_rows` keeps every column of `rows` (migration `0006_shares.sql`), so a continued conversation's rows are
  rows the projection can go on updating; a continue also copies `retracted` and `cc_retracted` within `upto_seq`.
  A continued conversation's `cc_bytes` is the sum of its copied files' sizes.
- `ShareView.epoch` is `"1"`: a share's rows never change.
- The admin reads show the lease's grant time and the last heartbeat's `gen`, `supervisor` and `cc` from two
  columns migration `0007_admin.sql` adds (`lease_granted_at`, set by the lease grant; `last_heartbeat`, set by the
  heartbeat route); magpie 4 keeps only `heartbeat_at`. `GET /v1/admin/conversations` lists at most 500, newest
  activity first, and is not audited (it shows no content); the per-conversation reads are.
- A person purge also deletes the leaver's unattached uploads, and answers 409 `conversation_migrating` (after
  purging the rest) while the importer holds one of their conversations; a repeat finishes it.
