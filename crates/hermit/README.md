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
- `tests/lifecycle.rs`: hermit 24.2's ported tests for the core: one sandbox per conversation, the `PUT`
  reservation, the janitor's idle and lifetime rules (busy never idled, 9 h forced), release refusal restarting the
  idle clock, forced delete, a requested release carried in the activity reply and done at the idle report, a `PUT`
  cancelling a pending soft release (one no task has sent, and one whose supervisor is being asked: refused -> 200
  the same sandbox, agreed -> 202 a new one), a `PUT` during a 10 s forced release (503s, then a new ready sandbox,
  never a `gone` the driver would fail the turn on), a forced release asked while a soft one is refused (ends the
  sandbox, reason `deleted`), a late forced release still asking the supervisor, the pool victim (least recently
  active, a refusing one skipped, `pool_full` otherwise), the silence rule (`ok:true, ready:false` alive; never
  while the gateway settles), a start killed in each phase in turn (`capacity`, `workspace`, `placing`, `host`,
  `image`, `creating`, `supervisor`) finished by another replica (one create, the same token, one start row,
  `start_ms` from `creating`), a release resumed by another replica after a kill, liveness, token keys, people and
  the descriptor answered with the database closed, the descriptor's ETag/304 identical across replicas, secrets
  `not_granted`, a removed person not live and released, a drain handing claims and leadership over, SIGTERM
  through the binary's drain hooks (`/readyz` 503 on the ops server, held long-polls answered, claims and
  leadership moved while a sandboxd call is out, drained after it returns), tasks completing under a 1 s
  `idle_in_transaction_session_timeout` while calls are held, start failures (`start_failed` with its class,
  sandboxd unreachable retried 3 tries then `start_failed`/`unreachable`, a failing placement ending
  `start_failed`/`placement` after `hostWaitSeconds`, `hosts_full`, `supervisor_timeout`), the operator's off
  switch, and the caller table.

## Not here yet

- The gVisor fabric (hermit 13-14, WP-19) and JuiceFS workspaces (15, WP-20). Until they land, `hermit serve` runs
  with `fabric::Unbuilt`: every workspace call is 503 `workspace_unavailable`, so every start ends at its
  `workspace` step, after 3 tries 2 s apart, as `gone` `workspace_unavailable` (`sandbox_failed`). (Past that step
  the `Unbuilt` fabric's placement errors would end it `start_failed`/`placement` after `hostWaitSeconds`.)
- The operator page, the snapshot, the health verdict, admin routes and console tails (3.11-3.12, 16, 18; WP-20):
  they answer 501 `not_supported` (the trait's default).
- The janitor's orphan sweep (12 step 8) and vanished/host-loss judgement (13.4, 13.7) are the fabric's scaler.
- Metrics (25) beyond the platform's defaults and `hermit_catalogue_refused_total` and `hermit_held_requests`
  (3.4.1, 23; registered in `main.rs`): the lookups, sandbox, host, start and request families are left to the
  package that builds the ops page and the health verdict (WP-20).

## Deviations from the spec (recorded for the design's owners)

- **A `PUT` that finds a `releasing` sandbox for more than 5 s** answers 503 `unavailable` with `retry-after: 1`
  instead of 202 with a new `starting` row "queued behind it" (3.6.2): the unique index `sandboxes_one_live` (10.2)
  allows one non-gone row per conversation, so no second row can exist, and handing magpie the releasing row would
  fail the turn (magpie 8.3 step 2 fails on `gone`, `sandbox_released` -> `sandbox.failed`). magpie 8.3 step 1
  retries "other 5xx" with backoff (0.5 s doubling to 10 s) until its deadline, so the `PUT` after the old sandbox
  is `gone` starts the new one: the sandbox is replaced silently, as 3.6.2 and 4.2 intend. The same answer covers a
  `ready` or `starting` sandbox with a forced release pending (it is about to go) and a soft release whose answer
  from the supervisor takes longer than the 5 s. (magpie's `PUT` timeout is 2.5 s, so in practice magpie's own
  timeout ends the wait and it retries; the result is the same.)
- **A `PUT` during a soft release a task is already asking the supervisor about** (13.12: "a `PUT` cancels it while
  not yet answered 200") clears `release_pending` on the row and waits for the supervisor's answer instead of
  answering 200 at once: refused, it answers 200 with the same sandbox; agreed, the sandbox goes and the `PUT`
  starts a new one (202). The supervisor's answer cannot be taken back, so a 200 at once could hand magpie a sandbox
  that is about to be destroyed (the A20 race). The release task re-reads `release_pending` before it sends and
  after a 409 or 200.
- **The long-poll holds a `releasing` sandbox** as it holds a `starting` one (3.6.3 names only `starting`): answered
  at once, magpie's loop, which waits for `ready` or `gone`, would spin until the destroy.
- **A forced release's `deadlineAt`** is written when the row enters `releasing` (13.12 forced step 1), not when the
  release was asked, so a forced release run late (its claim could not be taken at once) still asks the supervisor
  to end the turn; only a row already `releasing` when a takeover resumes it skips the call once its deadline passed.
- **Drain step 5** (20.2): the claims are handed back without stopping the tasks; the "in-flight requests and calls"
  hook waits up to 35 s for both the API server's requests and the tasks' supervisor and sandboxd calls already
  sent, at once rather than one after the other (the 90 s grace holds preStop 10 s + 10 + 35 + 5 s). A task whose
  call returns finds its claim gone at its next guarded write and stops; the new owner repeats the idempotent step.
  What is still running at 35 s is stopped.
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
  see a long-poll time out (24.1); every other timing is the spec's. Holding the fake fabric or the workspace store
  instead does not do it: the suite long-polls with the version its `PUT` answered, and the start's first step
  (`capacity`, a database step with no fabric call) bumps the row to `workspace` before any fake is reached, so the
  long-poll would answer at once with the new version.
- **sandboxd unreachable** (a transport error or a timeout, `CreateError::Unreachable`) is retried, 3 tries in all,
  before `start_failed` (note `unreachable`); a definitive sandboxd 4xx/5xx other than 503 `busy` and 409 ip in use
  fails at once (11.2 step 5). A placement that keeps failing (a fabric error, not "no slot") ends
  `start_failed` (note `placement`) after `hostWaitSeconds` instead of waiting out the 8 min start deadline.
- **The `Fabric` trait** (`place`, `request_host`, `create`, `control_url`, `console_tail`, `destroy`,
  `gateway_settled`) is the core's view of hermit 13; WP-19 implements it and may grow it.
