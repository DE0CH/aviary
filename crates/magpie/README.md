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
| `src/db/` | the migrations (`0001_init.sql`: every table of magpie 4 and the importer's of cutover 7.3) and `Db` |
| `src/conversations/` | the Conversation object, the person row, the row lock; create, list, rename, delete, restore, purge |
| `src/events/`, `src/rows/` | `events::append` (seq allocation) and the projection seam `rows::project` |
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
- `fanout::notify(tx, channel, key, head)` and `Fanout::hold(channel, key, wait, draining, query, empty)`.
- `jobs::outbox::enqueue(tx, &Item)` queues a hermit call made by the job leader after the commit.

## Tests

`cargo test -p magpie` against the CI Postgres (`AVIARY_TEST_DATABASE_URL`): each test makes its own database and
runs one or more replicas in-process (`tests/common`). `tests/service.rs` checks every route's callers, readiness
(migrations and LISTEN gates; Postgres lost after start keeps `/readyz` 200 while requests answer 503), the
migrations under concurrent starts, the leader hand-over, the outbox and the ops reads; `tests/conversations.rs`
the conversation routes; `tests/changes.rs` the change feed and the fan-out across replicas.

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
- Until WP-14's admission, a create whose first message has text or attachments answers 501 `not_supported` and
  commits nothing; an empty first message creates the conversation without a turn.
