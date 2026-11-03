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
| `src/legacy/` | the switch window (WP-51; DESIGN 9.2-9.5, cutover 6.4 and 7.8): `window` (pigeon's view of the window and an upload's bytes), `tickets` (taken before every legacy write, closed only on an outcome), `fence` (the importer's fence, release and unfence, the 3 s lease, the fenced wait of native mutations), `mirror` (the running mirror's `run_state`), `paths` (old asset paths resolved through `uploads.legacy_path`, `import_uploads` and `ingest`'s synchronous ingest, a fresh upload copy per send), `metrics`; `a6_contraction.sql` (the A6 schema change, tested). Deleted in A6 except `paths`, which goes in A7 |

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
rewrites, `meta.replaces`, compaction, set-aside, away-and-back, the limbo debounce, overwrites, streamed reads,
a person's echo through every rewrite, the per-turn flush, appends' `offset_mismatch`, the lease against a newer
feed record, a 30,000-line transcript in bounded time); `tests/ccfs_model.rs` (random operation sequences with
person turns against a byte-vector model, 64 cases from a seed that changes every run and is printed,
`AVIARY_TEST_CCFS_SEED`/`AVIARY_TEST_CCFS_CASES` to repeat one: the file and the visible events plus limbo against the
classification of the model's lines after every step); `tests/ccfs_rows.rs` (the timeline of a main transcript after
random appends, truncates, rewrites of a line with its uuid, limbo take-backs and flushes, against a fresh projection
of only the transcript's surviving lines in file order, an oracle that shares none of the rewrite handling; and the
review's truncate, rewrite, truncate, original-bytes sequence); `tests/ccfs_claude_code.rs` (Claude Code 2.1.304's
transcripts from the FUSE gate, product text replaced by same-length placeholders, replayed in the shape of the
gate's recorded calls with every prompt a person turn's echo, with pinned event counts); `tests/ccfs_perf.rs` (ignored here; ci.yml's `magpie-perf` job runs it with a release build against
`.github/ci/postgres-sync.sh`'s primary and synchronous standby: p50 <= 15 ms for a 10 KiB append, p99 <= 150 ms
for a 1 MiB image line; an 87 MB transcript from a cold bucket in under 5 s). The scanner, extraction and classification have unit and property tests in the crate.

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
  `retryAfterMs` (DESIGN 12 D14, cutover 6.4); while it is fenced they wait for the release first (`legacy::fence::
  native`, at most what is left of the fence's 3 s lease) and then run natively.
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
  - A rewritten prompt line (`cc.user` with `meta.prompt` and `meta.replaces = R`, R a `cc.user` line with the same
    `uuid` that still counts) takes over R's `user` row the same way (same `row_id`, a new row seq only when the text,
    images or files changed), so no commit shows two prompt rows with one `messageId`.
  - A retract whose rebuild of a tool row gives the item the row already shows (a rewritten result line with the same
    content took the retracted one's place) only replaces the row's sources: no row seq, no new epoch. magpie 7.3
    rule 9 says a rebuilt row takes a new `rowSeq`; an unchanged one is no rebuild.
  - The block index of a new line is also past every row of the message's earlier lines. A line that counted while
    it waited in truncate limbo and was retracted at the flush leaves a gap in the indexes of the rows numbered
    meanwhile; counting alone would then give a later line an index a row already has. Indexes are never renumbered
    (a gap, not a duplicate or a reorder; `tests/ccfs_rows.rs` compares the order, not the numbers).
  - The derivation-rebuild job (below) must clear the conversation's `line_pos` and `retracted` (and its rows,
    `row_seqs` and `row_head`) before it projects the events again: `retracted` holds both the seqs retract events
    removed and the lines a rewrite took over, and the projection fills both as it goes. `line_pos` is written with
    `on conflict do nothing`, so a repeated projection of one event does not fail the transaction.
  - An event that changes nothing visible on its row (a repeated `task.status` progress report, an identical
    `files.sent` or result) takes no row seq and writes no version: it is only added to the row's sources (a
    rebuild reads it). magpie 7.3 says rules 5-7 update the row; an unchanged item is no update.
  - Rule 3 replaces the launch's result text with the notification's summary whenever the notification has one
    (today's folding, `replaceLaunchToolResultContent`, does the same for every launch); a launch whose output was
    extracted keeps `blobId` and `bytes` (the link to its full output), with `truncated` set. A notification's
    `killed`/`stopped` is `task.status: stopped`, row status `canceled`. `meta.taskNotification` gained an optional
    `taskId` (magpie-api).
  - A failed `turn.ended` without an error makes an `error` row `run.error`; failed tool rows carry the turn's error.
  - Spec gap (magpie 7.3 rule 8, "background launches stay running until their notification"): a notification
    may never come (a canceled turn, a kill after the 30 s force, a lost sandbox). magpie appends a `task.status`
    (event `updated`, status `stopped`) for every task of the conversation's `background` that it stops tracking
    without the supervisor's terminal report: at a turn's end, the tasks the supervisor's end report no longer holds
    (all of them for an end without one); when the supervisor's heartbeat goes idle; when hermit's feed says the
    sandbox of a held conversation is gone. Rule 7 then ends the launch's row (`canceled`); a later notification or
    report still updates it. The reverse race (the turn's end lands before the supervisor's `started` report, so
    rule 8 ends a row whose task then reports `running`): a `running` task report on a row that rule 8 canceled or
    failed (no result) sets it back to `running`, until one of the ends above.
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
    or a sandbox lost after the acknowledgement fail it. A replacement's deadline is 15 min from the replacement
    (`turns.deadline`, which the driver follows rather than counting from admission), but never later than
    admission + 45 min (`ADMIT_TOTAL_MAX`), so sandboxes that keep dying before the acknowledgement end the turn
    `sandbox.timeout`; the replaced sandbox's delivery of `start_turn` is cleared, so a cancel while the replacement
    starts ends the turn at once. A turn of a conversation in Recently deleted that never ran (waiting, or starting
    before the acknowledgement) fails `sandbox.failed`, as 8.3 step 2 maps a loss before ready; after the
    acknowledgement a loss is `sandbox.lost`.
  - A `start_turn` delivered by the command poll to a turn that records no sandbox (the window after a re-acquire,
    before the driver records the replacement) records the caller's sandbox, so a later `kill` of the turn reaches
    it.
  - hermit's `version` counts per sandbox, so only the feed (in `seq` order, or its recovery's re-read) records
    another sandbox over the one recorded; a driver's answer records versions of the recorded sandbox, or a sandbox
    when none is recorded. The lease route's own answer is recorded only when nothing was recorded since it asked
    (WP-12).
  - After 410 `cursor_expired` the feed's cursor moves to the head read before the re-reads only once every re-read
    was answered; otherwise the step fails and the next one gets 410 again and re-reads everything (each effect is
    idempotent).
  - The sweeper claims expired turns with `for update skip locked` by moving their deadline 15 s ahead
    (`SWEEP_CLAIM`) in that one statement, and handles a turn only while its deadline is still the claimed one; a
    claim held as a row lock for the whole pass would block the turn's writers, which take the conversation lock
    first. The deadlines that need no hermit call go first; the running checks ask hermit 8 at a time.
  - A `running` report may arrive while the turn is still `admitted`/`preparing` (the supervisor takes
    `start_turn` as soon as it boots, before hermit's ready reaches the driver): it is the turn's ready too. For the
    same reason an end report (`completed`, `failed`, `canceled`) is accepted from `admitted`, `preparing` and
    `starting` once `start_turn` was delivered, not only from `running`/`stopping` (8.1): a supervisor may fail a
    start before running it (supervisor 7.9's "Claude Code would not start"). Before the delivery it is 409
    `bad_transition`.
  - The heartbeat reconciles a `starting` turn whose `start_turn` was acknowledged too (8.5 names only running and
    stopping turns): `turnId == T` stands for the lost `running` report, `lastTurn.id == T` ends it, `idle` without
    it is `run.error`. The sweeper's +30 s for an acknowledged start (8.8) stops 5 min of magpie-available time
    after the acknowledgement: `sandbox.unreachable` and `kill {watchdog}`.
  - A cancel stops a turn whose `start_turn` a supervisor has received (`delivered_at` set) by `interrupt`, in any
    state; only an undelivered `start_turn` ends the turn `canceled` at once (8.6 names the states, not the
    delivery, which is what decides whether Claude Code may already run it). The cancel reads the delivery with the
    `start_turn` row locked, so a command poll delivering it at the same moment either comes first (the cancel
    interrupts) or finds it canceled.
  - The end of a turn cancels every unacknowledged command of the turn except a `kill` (8.3 says "undelivered"): a
    delivered but unacknowledged `start_turn` or `interrupt` of an ended turn must not reach the conversation's next
    supervisor, which polls from `after=0`. A `kill` is served only to the supervisor of its turn's sandbox; one
    whose turn ran in another sandbox is canceled at the poll (that process died with its sandbox).
  - The `starting` and `stopping` deadlines count, like the running one, from the later of their start and
    `availability.available_since`, so an outage of magpie never fails a turn whose supervisor's ack or report was
    only waiting for magpie.
  - An agent-started turn reported while another turn is active answers 409 `bad_transition` (with the active
    turn's state). The lines the supervisor writes under the refused turn's id are credited to the active turn
    (6.3: a `turnId` magpie does not know falls back to the active turn), and the refused turn's end report answers
    404; the supervisor treats both as settled and drops the refused turn's later reports (supervisor README).
  - A `running` report for a turn already `running` or `stopping` (the heartbeat reconciled it first, or a replay)
    still sets `provider_session_id` and `main_transcript` (promoting the earlier lines); an end report that carries
    them sets them too, so a reconciled turn never loses its Claude Code session for the next `resume`.
  - M11 queues one `answer_ask` per ask (a second answer is 409 `already_resolved`); the command's stored payload
    also keeps the person's whole answer, which `ask.resolved` carries; the supervisor receives the typed command.
  - `title` is queued only while no `title` command is pending.
  - The heartbeat's extra fields (`gen`, `supervisor`, `cc`) are logged; the admin reads that show them are WP-15's.
  - Old asset paths in `files[].path` (the switch window's legacy uploads) are resolved by `legacy::paths` before
    the admission (WP-51).
  - The end report's `transcript_pending` check reads the conversation's `cc.*` events and the last 2,000 lines of
    its main transcript (`cc_lines` stubs, which keep each line's `uuid`).
  - The sandbox routes check the lease as the cc routes do: the current `lease_id`, held by the calling sandbox.

### Claude Code state (WP-12)

- An append carries the expected offset as `append=1` on `cc/write` (a contract addition, magpie 16.2 names no
  field for it): after rule 0, an append whose offset is not the file's size answers 409 `offset_mismatch`, so a
  stale retry with other bytes under a new `opId` cannot write twice (6.10). A write without it is an overwrite
  inside the file (6.6 rule 4, as `pwrite`). The supervisor sends `append=0` today; setting it on its appends (and
  on the repair's suffix writes) is the supervisor's change.
- A truncate cannot grow a lines file (`offset_beyond_eof`, rule 1); a byte file grows and a write past its end
  zero-fills (6.8). The 256 MiB cap is a byte file's (magpie 17); a lines file is bounded by the 2 GiB quota, and a
  lines file over 256 MiB renamed to a byte name answers 413.
- Only the main transcript's lines go to limbo or are retracted: a file renamed away from the main path keeps its
  lines' events (`moved` limbo) and a later truncate of it retracts nothing. `moved` limbo is matched by sha256
  (the earliest entry with that sha), not in order first.
- Limbo holds every classified line, not only lines with events (migration `0009_cc_limbo.sql`: a nullable
  `event_seq`, the line's `turn_id`, the truncate's `op_turn`): a line written again takes its entry back (its
  event, its turn, its classification) and is never classified a second time, so the echo of a person's prompt
  stays an echo through a rewrite, a tombstone, a compaction (a rename-over matches the target's classified lines
  the same way) or a set-aside, and an old prompt is never taken as a pending turn's echo. Entries without an event
  are dropped at a flush.
- The end report's flush (`limbo::flush_scope(turn)`) flushes only the limbo that turn's truncates made (a truncate
  is credited to the active turn); the rest waits for its own turn's end or the sweeper's 10 s quiet rule.
- A line taken back from limbo keeps the turn it was first credited to (6.3), so a promotion credits it there.
- Every lines operation works in bounded batches (6.6): reads and seeks take 256 lines at a time through
  `cc_lines_off`; a truncate deletes its suffix and puts it in limbo in one statement; new lines are inserted 512 at
  a time, their limbo candidates fetched and the taken ones deleted in one statement each; an overwrite reads only
  the lines it touches; a promotion walks the file in keyset batches of 256 with one set-based update each; a
  rename-over is one set-based statement (the pool of the target's classified lines and limbo, ranked per sha256
  with `row_number`, joined to the source's unclassified lines), so its memory is Postgres's, not magpie's. Measured
  (`a_large_transcript_reads_and_rewrites_in_bounded_time`, debug build): 30,000 lines read in 0.7 s, `truncate(0)`
  in 0.3 s, the rewrite of the whole file in 1 MiB writes in under 3 s. The cost left is per event (an event and
  its row are made one at a time: about 8 ms each in a debug build, so a 1 MiB write of 2,000 new event lines takes
  seconds there).
- A read that answers in one batch compares the file's version after it and reads again (at most 3 times, then 503)
  when a write landed between its statements; a streamed read compares it at its end.
- The lease records hermit's answer only when the feed has recorded nothing since hermit was asked; a feed record
  made in between wins, and the caller gets the lease only if that record names it (6.9).
- A `type: "user"` line with `isCompactSummary` is a `cc.user` event that is not a prompt and never the echo (the
  table of 6.5 has no row for it; it is no message a person sent).
- A `custom-title`/`ai-title` record sets the provisional title cut to 200 characters; a title or model change
  emits `conversation_upserted`, a context-token change alone does not (the next turn change carries it). The
  model `<synthetic>` (Claude Code's own API error lines) is not recorded as the conversation's model.
- `meta.replaces` also names the event of an old line that this operation's rewrite replaces, or that waits in the
  file's limbo, and of the candidates (a retracted line, an old line of the rewritten suffix, a limbo line) it names
  the most recent: truncate, rewrite, truncate, rewrite again replaces the first rewrite, not the original.
- A line the new line rewrites this way is retracted at the end of the same operation (WP-13 review), not at the
  limbo flush: its `truncated` limbo entry is dropped and a later line with its bytes no longer takes its event back
  (it makes a new event that replaces the rewrite), and an old line of an overwritten suffix whose event a rewrite
  replaced makes a new event if its bytes come back in the same write. 6.7 has limbo take lines back by sha256 only
  so a byte-identical truncate-then-rewrite retracts nothing; a line with the same uuid and new content is gone from
  the transcript for good, and the projection has already handed its rows to the rewrite (magpie 7.3 rule 4), so a
  take-back would bring back an event that counts as gone with no rows. `tests/ccfs_rows.rs` checks the rows after
  random appends, truncates, rewrites and take-backs against a fresh projection of the transcript's lines.
- Migration `0004_ccfs.sql` adds the blob's preview and line count (an event for a stored line is made without
  its bytes), `cc_lines.classified` (a promotion classifies only lines never classified, so an echo or an
  internal line is never classified twice), `cc_retracted` (uuids of retracted lines, for `meta.replaces` by index)
  and two limbo indexes.
- `GET /v1/sandbox/cc/tree` and `/cc/file` need the sandbox identity, not the lease (they change nothing).
- A read's first batch of lines is made before the answer starts (a sha mismatch or a missing blob there answers
  500 / 503); a later failure, or the file changing while it streams, ends the body early. Every reconstructed
  line's sha256 is checked in a debug build and inside a transaction; one in 1,000 in a release build's reads.
  Blobs of a batch are read from staging in one query, the rest from the bucket 16 at a time
  (`blobs::fetch_stored`, `limits::BLOB_FETCH_CONCURRENCY`), and the next batch's blobs are fetched while the
  current one is sent (`limits::CC_READ_AHEAD`); no database connection is held while the bucket answers.
  `ccfs_perf.rs` measures 6.11's target in a release build: an 87.5 MB transcript (400 blobs, 52 MB, every one only
  in the bucket, read by a replica with an empty cache, the bucket behind a proxy that adds 30 ms to each request)
  streams in 1.28-1.56 s (the shared build runner, 2026-11-02; one GET at a time it would need at least 400 x 30 ms
  = 12 s, Claude's arithmetic).
- The lease asks hermit only when its feed's record does not name the caller as `starting` or `ready`; hermit's 404
  is "no sandbox" (`not_current_sandbox`), any other failure or 2 s without an answer 503.
- Recorded deviation: the 2.1.304 acceptance replays the real binary's transcripts from the FUSE gate through the
  routes, in the shape of the gate's recorded calls (the gate's `calls.log` records no truncate sizes or bytes, so
  an exact op-by-op replay is not possible); the real binary against a mock Messages API through a FUSE supervisor
  and magpie, with person turns, is carried to the e2e harness (WP-28) and blocks magpie 20's release gate, as does
  an end-to-end stale append with `append=1` (the supervisor sends `append=0` today, so `offset_mismatch` and the
  repair path are exercised only by magpie's own tests). The latency figures are measured at the client over loopback HTTP (they include the round
  trip), with a 2-node synchronously replicated Postgres in CI at `min_wal_size=1GB`, `max_wal_size=2GB` and its WAL
  warmed first (segments written once and recycled, as on a cluster that has run for a while). Those two settings
  are a required parameter of `magpie-pg` (handed to WP-32): creating a new 16 MiB segment file costs one write
  about 150-200 ms, and Postgres's defaults (80MB / 1GB) recycle too few segments for a burst of image lines.
  Measured on the shared build runner (2026-11-02): at the defaults on a fresh cluster, p50 7.9-9.7 ms for a 10 KiB
  append and p99 129-134 ms for a 1 MiB image line (inside the bound, with little margin); at 1GB / 2GB warmed,
  p50 9.4 ms and p99 47-53 ms; with 500 samples of the 1 MiB line (the p99 is the 5th slowest), p99 53.1 ms. The
  link to production is WP-32's: its acceptance includes a manifest check that `magpie-pg`'s
  `postgresql.parameters` carry both values.
- An echo of a person's prompt is remembered by its sha256 and its uuid for the conversation's life (`cc_echoes`,
  migration `0011_cc_echoes.sql`, and `cc_echo_uuids`, `0016_cc_echo_uuids.sql`; copied with a share's state, removed
  with the conversation): once a truncate's limbo is flushed (its turn ended, or 10 s of quiet) the tables are what
  keep a rewritten echo internal, so it neither takes a later person turn's echo nor becomes a `cc.user` prompt; the
  uuid covers the same message written again with other bytes (another `sessionId`, another field order). A line with the uuid of a retracted line is never taken as
  the running turn's echo (it is that line again, with `meta.replaces`).
- `set_main_transcript` checks the path is `projects/-workspace/<providerSessionId>.jsonl` (6.2 "checked to equal
  that form"; 400 `bad_request` otherwise, so a subagent's transcript can never become the main one). When the main
  path changes (a new provider session), the old file is treated as renamed away: its lines keep their events and
  enter `moved` limbo, so the same lines at the new path make no new events, and it is no longer protected.
- A rename applies the main-transcript rules around a kind change: a main transcript renamed to a byte-file name
  puts its lines in `moved` limbo before it is stored as pages, and a byte file renamed over the main transcript is
  stored as lines first (at its old path, unclassified) and then goes through the rename-over matching and the
  promotion, so the old transcript's dropped lines are retracted at once (6.6).
- Bucket down (6.13): rule 0 compares a line the range covers whole by its sha256 and length, so the sentry's resend
  after a checkpoint answers without the bucket; appends and truncates at a line start or inside a line without
  blobs need no bucket either. The uploader keeps the bytes it stores (blobs of at most 4 MiB) in its replica's
  recent-uploads cache (`limits::RECENT_UPLOADS_BYTES`, 32 MiB, LRU so it always admits, apart from the read cache so
  the blobs Claude Code wrote never push out the copies timeline reads keep there; the read cache is 224 MiB). So a
  write, truncate or overwrite that cuts the line usually also answers on that replica, and on any replica whose read
  cache holds the blob. This is best effort: the uploader is whichever replica claimed the blob, not necessarily
  the one serving the sandbox's later writes, a blob over 4 MiB is not kept, and the oldest of the last 32 MiB of
  uploads is dropped first (`blobs::tests::an_uploaded_blob_is_kept_with_the_read_cache_full`). A gap is left, which
  5.2's design implies (staged bytes leave Postgres once uploaded, so the bucket then holds the only copy): on a
  replica without the blob in either cache, a cut of a line whose blob has left staging, and a kind
  change on rename, reconstruct that line from the bucket and answer 503 while it is unreachable (the supervisor
  retries; `tests/blobs.rs` `a_resend_and_a_truncate_at_a_plain_line_need_no_bucket` pins every side).
- An upload's or a sent file's body (at most 50 MiB and 64 MiB) is read whole, hashed, then put in the bucket under
  its content key with one `PUT`, rather than hashed while it streams (the key is the hash, known only at the end).
  No object magpie writes is over 64 MiB, so magpie 5.2's multipart path has nothing to carry and is not built.
  Buffered bodies are admitted under a memory budget of their own (`limits::BODY_MEMORY_BYTES`, 256 MiB per replica,
  shared with the uploader's staged blobs and the cc store's fetched blobs): a body takes its `Content-Length` (else
  its route's limit) before a byte is read. The cc store's fetch of the blobs a transaction or a read needs
  (`blobs::fetch_stored`) takes the whole batch's size at once (twice a staged blob's), waiting at most 30 s, and the
  fetched buffers themselves (no copies) keep it until the transaction or the read is done.
  Blob reads sent to a client (M15, the files panel's downloads, the sandbox's blob reads) are sent at the client's
  pace, so they take a part of that budget of their own, `limits::READ_MEMORY_BYTES` (128 MiB), before the whole
  budget: reads never hold more than that, and slow or stalled downloads never keep uploads, sandbox `PUT`s or the cc
  store waiting. A read the read cache cannot answer takes the bytes it fetches (twice a staged blob's size while
  its row and its copy are both alive); a base64-json or json-string blob's decoded copy takes twice the stored size
  while it decodes and its own size afterwards. The bytes keep their share until the response has been sent or cut
  off; a read that waits more than 20 s for it answers 503 with a `retry-after`. Concurrent reads of one uncached blob
  make one fetch: the others wait for it and are answered from the cache. Bytes answered from the read cache take
  nothing (the cache counts them); not counted are a cached entry evicted while its response is in flight, a
  thumbnail or display copy (small WebP files), and a cc file read's assembled batch. Every blob response is sent in
  256 KiB copies by a task that cuts the response off when the client takes nothing for 60 s
  (`limits::SEND_IDLE`), so a client that vanished without closing gives the memory back within a minute instead of
  when TCP gives up. With the decode budget (384 MiB), the read cache (224 MiB) and the recent uploads (32 MiB) that
  keeps a replica inside magpie 18.2's 1 GiB (`tests/uploads_perf.rs` and `tests/bodies_perf.rs` measure it with the
  cache full). A decode whose estimate is over the whole decode budget answers 415, like one over 40 MP: in practice
  a 16-bit image above about 35 MP (an 8-bit one never is).
- A blob lock is given back when its holder is dropped (a request whose client went away mid-`PUT`), not only by an
  explicit release, and taking one runs in a task of its own, so cancellation never leaves a lock on the lock
  session. A lock session found ended (a failed statement, a lock found lost by the GC or by a writer's check) is
  replaced on the next lock, and its replacement connection is taken without blocking the session's other locks. A
  GC that loses its lock session mid-delete records the blob in `blob_reverify`, takes the blob's lock again and
  checks a stored row's object with a `HEAD`: a lost object clears `stored` (the next writer puts the bytes again)
  and logs `blob object lost`. The record is removed only once that check has run, so a check that could not run
  (the lock not had again within 5 min, the bucket or the database unreachable) is retried by every GC pass.
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
- A share freezes the rows whose source events are all within `upto_seq`, as magpie 12.1 says, and
  `nothing_to_share` uses the same rule for the assistant row (checked again under the conversation's lock, with the
  freeze point recomputed there). The accepted consequence: a finished turn's row that a later event changed (a
  background task's notification updating a tool row after its turn, a retract in the running turn) is not in the
  share, nor in a conversation continued from it. The frozen Claude Code copy cuts every lines file of the scope
  (the main transcript, subagent transcripts) at its first line of the running turn; byte files carry no turn and
  are copied whole.
- A share's blob references are a copy of the conversation's at the freeze (a superset of what the frozen lines,
  rows and events reference: it may keep a running turn's blobs alive for as long as the share lives).
- `share_rows` keeps every column of `rows` (migration `0006_shares.sql`), so a continued conversation's rows are
  rows the projection can go on updating; a continue also copies `retracted` and `cc_retracted` within `upto_seq`.
  A continued conversation's `cc_bytes` is the sum of its copied files' sizes.
- `ShareView.epoch` is `"1"`: a share's rows never change.
- The admin reads show the lease's grant time and the last heartbeat's `gen`, `supervisor` and `cc` from two
  columns migration `0007_admin.sql` adds (`lease_granted_at`, set by the lease grant; `last_heartbeat`, set by the
  heartbeat route); magpie 4 keeps only `heartbeat_at`. `GET /v1/admin/conversations` lists at most 500, newest
  activity first. Its titles come from content (a provisional title is the start of the first message), so it is
  audited like the per-conversation reads: an `admin_read` row for each listed conversation, the first per admin,
  conversation and UTC day. Those rows are written under a per-admin transaction lock rather than a unique index
  (migration `0012_admin_read_index.sql` builds a plain index concurrently), so no audit row is ever deleted and a
  replica on older code never meets a conflict during a rolling update. Migration `0010_shares_frozen.sql` was
  edited before any deployment (nothing of wave 1 is deployed) to drop its duplicate-deleting step and unique index.
- A person purge goes ahead only on hermit's 200 `allowed: false`; any other answer from hermit, a 404 included
  (hermit answers 200 for every well-formed address), is 503 and purges nothing.
- A person purge also deletes the leaver's unattached uploads, and answers 409 `conversation_migrating` (after
  purging the rest) while the importer holds one of their conversations; a repeat finishes it.

### The switch window (WP-51)

- Tickets, the fence and the legacy-path record are three pigeon routes in `magpie-api` (`legacy_ticket`,
  `legacy_ticket_done`, `upload_legacy_path`), with two more for pigeon's legacy upstream (WP-50): `legacy_window`
  (`GET /v1/legacy/window`: the gate `import_control.handover_allowed_at`, the rollback mark
  `import_control.rollback_at` of DESIGN 9.10 step 1, added by migration 0018, whether anything is legacy and how
  much is the person's) and `upload_raw` (`GET /v1/uploads/{uploadId}/raw`: the person's upload bytes, for the
  copy-at-send of DESIGN 9.5); the fence, release, unfence, running mirror and `import_uploads`
  writes are library functions the importer (`crates/magpie-import`, linked into `magpie import …`) calls on the
  same database, so they need no route.
- A ticket's outcome is one of `started`, `completed`, `failed` (a send), `applied` (anything else) or `refused`;
  `delivered` is recorded (`legacy_tickets.delivered_at`) and closes nothing (switch-final C5.1). A closed ticket
  notifies `magpie_legacy` (`<conversation>:0`) for the importer's wake on dirty.
- Design gap (recorded): nothing in the specs closes a ticket whose outcome never arrives. pigeon can die or roll
  out between the ticket and its report, or retry a ticket call that timed out after it committed; the open ticket
  then refuses every fence and release, `release --force` included (cutover 7.7, 7.9), for ever, against
  switch-final 8.1 ("after its last ticket was done or expired") and DESIGN 9.2 (fail-closed inputs only delay a
  release). The closest correct thing: (1) a send's ticket call carries the send's `clientMessageId` (only a send's
  may: 400 for any other kind, so a send's upload tickets, one per attached file, are each their own and the first
  file's `applied` never closes a ticket another copy still writes under) and is idempotent per
  `(conversation, clientMessageId)` while that ticket is open and younger than `magpie_api::legacy::TICKET_REUSE_MAX`
  (30 s; `legacy_tickets_open_send_cmid`), so a retry gets the first ticket with at least 30 s of its hold left; a
  repeat past that age supersedes the old ticket (`superseded_at`: it stays open, a write started under it may still
  be in flight) and gets a new one whose answer names it (`supersedes: {ticket, settleMs}`), so a retried send is
  never stuck behind a ticket pigeon may no longer write under; (2) every ticket answer carries `issuedAt` and
  `holdMs`, what is left of `TICKET_HOLD_MAX` (60 s) on the database clock, and pigeon starts no legacy write under a
  ticket later than `holdMs` after it sent the ticket call (its monotonic clock: no skew between pigeon and the
  database moves the bound) and aborts a ticketed legacy write that has no answer within `TICKET_WRITE_TIMEOUT`
  (20 s) (a contract for WP-50); (3) `legacy::tickets::expire(pool, id, issued_before)` closes the open tickets
  (superseded ones included) issued before a bound with outcome `expired` and wakes the importer. The importer
  (WP-52) calls `legacy::tickets::expire_settled(pool, id, read_started)`, with `read_started` from
  `legacy::tickets::now` (the database clock) taken right before an old-child read (running list and
  `pendingUserMessages`/`preparing`) that showed nothing for the conversation; it expires what was issued more than
  `TICKET_SETTLE_MAX` (90 s = the hold, the write timeout and 10 s for bytes sent just before an abort) before that:
  any write under those tickets had then either reached the old child, and would have shown in the read until it
  settled, or never will. The quiet period and two running-set reads still follow. `release <id> --force` (WP-29)
  calls `expire` with `tickets::now` after the operator's inspection, then fences and releases. A late outcome on an
  expired ticket keeps `expired`. Tested in `tests/legacy.rs`
  (`a_stranded_ticket_expires_and_a_retried_ticket_call_reuses_the_open_one`).
- The old stack does not de-duplicate a send by `clientMessageId`: CloudCLI's paseo hub (DE0CH/claudecodeui
  `server/modules/paseo/paseo-hub.ts` `send`, called from `paseo-connection.ts` `send_agent_message_request`)
  refuses a send only while a turn runs and otherwise delivers a second `chat.send`, a second turn. So a send whose
  repeat got a new ticket with `supersedes` is not simply sent again: pigeon waits `settleMs` from when it sent that
  ticket call, reads the old child (running list, `pendingUserMessages`, and the timeline for a user message with
  the send's `clientMessageId`, which the hub's rows carry as `clientMessageId` and id `paseo-user-<id>`), and sends under the new ticket only when the first send is absent; when it is
  there, it reports the new ticket `started` or `completed` from what the old child shows and answers the send as
  accepted (a carried note for WP-50). Until the importer's expiry exists (WP-52), a superseded ticket whose outcome
  never comes holds its conversation legacy; WP-52's hand-over pass calls `expire_settled` for every migrating
  conversation on every pass, which covers superseded tickets (their `done_at` is null).
- A fence is a lease: `conversations.legacy_fence_at` older than 3 s holds nothing, cannot be released, and is
  cleared by the next writer that finds it (a ticket call, a waiting mutation). The lease and the fence's held time
  (`magpie_fence_seconds`) are judged by `clock_timestamp()`, not the transaction's start, so the importer's work
  earlier in its release transaction counts; it calls `release_in` last, after the upload-ingestion pass, so writers
  queued in the fence wait on the row lock only for the release itself. `release_in` answers a `fence::Released`
  (`#[must_use]`) that the caller records on `magpie_fence_seconds` with `Released::record` after its commit, so a
  rolled-back release is never counted; every release runs in the import Deployment (`magpie import handover` and
  its one-shot commands), which registers `magpie::legacy::Metrics` on its own `:9090/metrics` registry (WP-29,
  WP-52), and that is where DESIGN 9.9's and cutover 10.3's A5 gate on the histogram reads it (magpie serve's
  `/metrics` registers the same family but never observes it). A draining replica, whose held polls answer
  at once, still waits out a fence's remaining lease (polling every 50 ms), so a fenced write becomes native at the
  release rather than 409 `route: "legacy"`. The release refuses a lapsed fence
  and an open ticket, and stores the old child's model and the mapped `name_source` (`coalesce`: `None` keeps the
  imported value).
- Old asset paths are held in canonical form, the file name inside the person's old asset folder
  (`magpie_api::legacy::legacy_asset_name`: the path ends in `.claude/cloudcli-assets/<name>`, and the folder above
  `.claude`, when there is one, must be the person's key), so `import_uploads` is keyed by `(person, legacy_path)`
  rather than by the absolute path (whose prefix is the old pods' home root).
- The synchronous ingest calls `POST http://magpie-import/v1/ingest-upload` (`{person, legacyPath}` → 200 ingested,
  404 no such file) with `MAGPIE_TOKEN_IMPORT` (required at start like every token: platform 3.7's pair table and
  the magpie Deployment carry it, and the import side accepts it as `MAGPIE_IMPORT_ACCEPT_MAGPIE`). A send's misses
  are ingested concurrently under one 5 s deadline for the whole send (magpie 8.2). An unreachable or late import
  answers the send 503 `unavailable` (retryable) rather than 400 `unknown_upload`, which is kept for a path that
  resolves nowhere. Every miss is ingested until the A6 release removes the ingest (DESIGN 9.5, magpie 8.2; no
  shortcut on `import_control.all_owned_at`, which a reverse hand-over would leave stale and which the A6
  contraction drops); nothing in the resolution reads a table the contraction drops, so the A6 release only removes
  the ingest call, after which a miss is `unknown_upload` (tested on the contracted schema). Before the import
  Deployment and its Service exist (A3; in A2 the importer runs as Jobs, including the A2 rehearsal R1) a miss is
  503: retryable, and pigeon's retry resolves once the periodic ingestion pass has recorded the file (recorded
  deviation from cutover's "synchronous ingest on a miss" before T0; no composer holds an untracked old path then,
  since the tee records every upload from T0).
- A native send (M7) to an unfenced legacy conversation skips the resolution, so its 409 `route: "legacy"` comes
  back at once with no ingest and no copies. The owner is read again on the attempt after a fence, so a send that
  found the conversation legacy, was fenced before its admission and then became native resolves its old paths on
  that attempt (an attempt that resolved keeps its copies for the next). The same old path twice in one send is
  one copy, so one attachment. An attempt that resolved and is then refused (`turn_running`, `conversation_deleted`,
  a lifted fence's 409, a 503 retried by pigeon) leaves its unattached copies, which retention collects after
  `UNATTACHED_UPLOAD` like any upload (they carry no `legacy_path`); making the copy inside the admission
  transaction would avoid them and is not done (the copies hold refs to blobs that already exist, no new bytes).
- Migrations `0013_window.sql` and `0010_shares_frozen.sql` were edited in place after they reached `main` (nothing
  was deployed); a database built from an earlier commit (for 0013, from 0822f0ad) fails aviary-pg's checksum
  check and must be recreated. Every migration is frozen from now on: further changes go in new migrations (0015
  added the superseded ticket). 0015 replaces 0013's reuse rule: 0013's header still describes a repeated ticket
  call returning the open ticket through `legacy_tickets_open_cmid`, which 0015 drops for
  `legacy_tickets_open_send_cmid` (sends only, not superseded), and 0015's own header names `TICKET_HOLD_MAX` as the
  reuse age, which is now `TICKET_REUSE_MAX` (30 s, enforced in `legacy::tickets::take`, not in the index). Neither
  header can change (checksums); this note and `legacy::tickets` are the current rule.
- The tee's record (`PUT …/legacy-path`) is an unattached copy of the upload carrying `legacy_path`, not the upload
  itself: pigeon may still attach the upload by its magpie id (DESIGN 9.5), and purging that conversation then
  deletes the upload row but not the record.
- Retention keeps unattached uploads that carry `legacy_path` or are in `import_uploads` until A7; every send
  attaches a fresh copy, which retention treats as any other upload. `uploads_legacy_path` is built concurrently by
  its own no-transaction migration (0014), since `uploads` is populated and written by every upload.
- A share revoke of a migrating conversation answers 409 `conversation_migrating` (cutover 6.4's table lists it;
  the revoke route checked nothing before).
