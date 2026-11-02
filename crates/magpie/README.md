# magpie

The store of record for conversations (magpie spec). One binary: `magpie serve` (one replica; several run on one
Postgres) and `magpie --version`. Its REST contract is `crates/magpie-api`; this crate implements
`magpie_api::server::Server` and serves it behind the caller check.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | start (env, ops port 9090, migrations, LISTEN, jobs, API on 8080) and drain |
| `src/app.rs` | `Service` (start and the drain's last steps), `Magpie` (shared state), the `Server` impl, the router |
| `src/auth.rs` | callers from `MAGPIE_ACCEPT_*`, the route layer that checks them against `magpie_api::routes::ROUTES` |
| `src/limits.rs` | every limit and timeout (magpie 17) |
| `src/error.rs` | the one error type; Postgres unreachable answers 503 `unavailable` with `retry-after` |
| `src/db/` | the migrations (`0001_init.sql`: every table of magpie 4 and the importer's of cutover 7.3; `0005_timeline.sql`: the projection's lookups) and `Db` |
| `src/conversations/` | the Conversation object, the person row, the row lock; create, list, rename, delete, restore, purge |
| `src/events/` | `events::append` (seq allocation), which calls the projection |
| `src/rows/` | the timeline projection (magpie 7.3): `project` (rules 1-10, row seqs, the epoch), `derive` (what each event makes of a row, pure), `read` (M9 pages and the long-poll, M10, search) |
| `src/live.rs` | live frames (magpie 7.5): receiving, the `LiveState`, `GET …/live`, the cleanup run by every replica |
| `src/changes.rs` | the per-person change feed (M12): `emit`/`upserted` in a transaction, `read` with its long-poll |
| `src/fanout.rs` | the LISTEN hub (four channels) and the long-poll loop every held request uses |
| `src/jobs/` | the job leader, purge, outbox (hermit calls), retention |
| `src/stats.rs` | the ops reads (`/v1/ops/people`, `/v1/ops/view-as`, `/v1/ops/stats`) |
| `src/http/` | the handlers, one module per route group (no SQL) |
| `src/turns/` | turns (WP-14); today only the seam the create route calls |

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

## Tests

`cargo test -p magpie` against the CI Postgres (`AVIARY_TEST_DATABASE_URL`): each test makes its own database and
runs one or more replicas in-process (`tests/common`). `tests/service.rs` checks every route's callers, readiness
(migrations and LISTEN gates; Postgres lost after start keeps `/readyz` 200 while requests answer 503), the
migrations under concurrent starts, the leader hand-over, the outbox and the ops reads; `tests/conversations.rs`
the conversation routes; `tests/changes.rs` the change feed and the fan-out across replicas; `tests/timeline.rs`
every projection rule from events to rows, the epoch, the pages, the M9 long-poll (50 ms across replicas, no
connection held, drain), the E6-style cost test, prompts and search; `tests/projection.rs` the property test
(random event sequences against an in-memory reference projector, `row_seqs` density, `after` from every seq);
`tests/live.rs` live frames.

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
- Until WP-14's admission, a create whose first message has text or attachments answers 501 `not_supported` and
  commits nothing; an empty first message creates the conversation without a turn.
- Timeline rows (WP-13):
  - A row's block index counts the blocks of earlier lines of the same `message.id` that are not retracted; a line
    with `meta.replaces` counts from the replaced line's position, so a rewritten line keeps its `(messageId,
    blockIndex)` (magpie 7.3 rule 4 says only "counts earlier lines"). Retracted event seqs are kept in the
    `retracted` table (events stay immutable).
  - Rule 3 replaces the launch's result text with the notification's summary whenever the notification has one
    (today's folding, `replaceLaunchToolResultContent`, does the same for every launch); a notification's
    `killed`/`stopped` is `task.status: stopped`, row status `canceled`. `meta.taskNotification` gained an optional
    `taskId` (magpie-api).
  - A failed `turn.ended` without an error makes an `error` row `run.error`; failed tool rows carry the turn's error.
  - A rebuilt tool row (rule 9) is made from its surviving source events in the order they first landed on it, and
    keeps its `row_id` and block index.
  - No `derivationVersion` lazy rebuild (pigeon 14.1.18 item 3): magpie 7.3 changes the epoch only under rule 9.
  - M9: `minSeq` is 0 when the conversation has no rows; `after` entries carry `seqs` up to their own `seqEnd`;
    for an `after` page `hasOlder` means a row with `seqStart <= after` exists. `waitMs`/`liveAfter` without
    `after` answer 400. A held `after` poll also answers when the epoch changed (a retract that only deleted rows
    issues no row seq).
  - Live frames: a `liveAfter` greater than the turn's last frame is taken as another turn's (reset); a block whose
    first frames are missing (a truncated unlogged table) or a delta past the block's end resets, and the block's
    text starts at that delta's offset; a frame of a block whose durable row exists is ignored.
  - Search counts occurrences in the `user`/`assistant` rows an event made (events' `text` holds only those); its
    cursor is the event seq after which the next page starts.
