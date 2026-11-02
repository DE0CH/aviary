# hermit (the YYC implementation of the hermit contract)

hermit spec sections 3-12, 19, 20, 23 and 24.1-24.2 (WP-18, the core). The library (`src/lib.rs`) is one replica;
the binary (`src/main.rs`) runs it as `hermit serve` and checks catalogues as `hermit catalogue-check <file>`.

| Module | Spec |
|---|---|
| `config` | 9: `hermit.json`, strict (unknown keys refused), and the managed CLAUDE.md, title and rules texts |
| `catalogue`, `people` | 3.4: the staff catalogue, re-read every 30 s, the last good one kept |
| `deployment`, `warm` | 3.3, 3.5 |
| `tokens` | 6.1-6.2: sandbox and control tokens derived from `HERMIT_TOKEN_KEYS`, never stored |
| `db`, `migrations/` | 10: the schema, the row, `bump` (version, feed event under lock 7300/8, NOTIFY) |
| `live` | 3.9, 10.4: the in-memory map that answers liveness and the sandbox surface's identity without a connection |
| `tasks` | 10.3: row claims (15 s, renewed every 5 s) and the takeover sweep (every 2 s) |
| `sandboxes` | 3.6.2-3.6.4, 13.12: get-or-start, release (soft, forced, lost), destroy |
| `starts` | 11: the resumable start task |
| `feed` | 3.6.3, 3.6.5: the long-poll and the change feed |
| `sandbox_routes` | 3.7: the descriptor, secrets (YYC grants none), activity reports |
| `janitor` | 12, 10.6: the leader's pass and daily retention |
| `supervisor_client` | 3.10: health, release, the flush before a clone |
| `fabric` | 8.1: the `Fabric` and `Workspaces` traits (one provider each; tests inject fakes) |
| `server`, `app` | the `hermit_api::Server` implementation; start, loops, drain (20.2), kill |

## Tests

`cargo test -p hermit` needs `AVIARY_TEST_DATABASE_URL` (ci.yml's Postgres). `tests/common` is the harness: a fresh
database per test, a fake fabric whose sandboxes are `aviary-fakes` supervisors on local ports, in-memory workspaces,
and replicas started in-process with short timings (`Timings` is code, not configuration).

- `tests/conformance.rs`: `aviary-conformance hermit` (24.1) against a replica, every check passing, none skipped.

## Not here yet

- The gVisor fabric (hermit 13-14, WP-19) and JuiceFS workspaces (15, WP-20). Until they land, `hermit serve` runs
  with `fabric::Unbuilt`: every start fails `start_failed` (note `fabric_unavailable`), every workspace call is 503.
- The operator page, the snapshot, the health verdict, admin routes and console tails (3.11-3.12, 16, 18; WP-20):
  they answer 501 `not_supported` (the trait's default).
- The janitor's orphan sweep (12 step 8) and vanished/host-loss judgement (13.4, 13.7) are the fabric's scaler.
- Metrics beyond the platform's defaults (25).

## Deviations from the spec (recorded for the design's owners)

- **A `PUT` that finds a `releasing` sandbox for more than 5 s** answers 202 with the releasing sandbox instead of a
  new `starting` row "queued behind it" (3.6.2): the unique index `sandboxes_one_live` (10.2) allows one non-gone row
  per conversation, so no second row can exist. magpie long-polls it to `gone` and asks again (magpie 8.7).
- **A forced or lost release asked for a starting sandbox** ends the start at its next step (destroying what was
  made), rather than only once it reaches `ready` (11.2 step 6): a deleted conversation or a person who left has
  nothing to start for. A soft release (`requested`) still waits for `ready` and an idle point.
- **The workspace step's hand-over** to another replica on a broken local mount (11.2 step 2, 15.6) is not built:
  the core retries `ensure` 3 times 2 s apart, then fails `workspace_unavailable`. The hand-over needs the JuiceFS
  store's own "broken here" signal (WP-20), which `WorkspaceError::Unavailable` is the place for.
- **Migrations are recorded in `_migrations`** (aviary-pg's table, with each file's sha256) rather than
  `schema_migrations` (10.2).
- **`TokenLiveness.expiresAt`** is the row's hard expiry (the token's own expiry, 6.1); 3.9 does not say which.
- **`Timings::start_pause`** (zero in production) lets the conformance test hold a new row unchanged long enough to
  see a long-poll time out (24.1); every other timing is the spec's.
- **The `Fabric` trait** (`place`, `request_host`, `create`, `control_url`, `console_tail`, `destroy`,
  `gateway_settled`) is the core's view of hermit 13; WP-19 implements it and may grow it.
