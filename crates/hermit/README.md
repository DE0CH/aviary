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
| `workspaces`, `migrations/0002` | 3.8, 15: `/workspace` on `JuiceFS` (hermit's mount `/jfs`, the `juicefs` CLI for quotas, `clone`, `rmr`; `openat2` beneath the workspace for listing and reading); each replica's own state in `replicas` (its mount, its last refused delete) for the ops verdict and the start hand-over |
| `console` | platform 15.4: the last 64 KiB of a sandbox's console in `sandbox_logs` at its destroy (a row saying so when nothing could be fetched) |
| `admin` | 16: the admin routes, each audited in `admin_events` (in the change's transaction where there is one) |
| `ops`, `ops_page.html` | 3.11, 18.1-18.2, 18.4-18.5: who may read `/ops/*`, the page (CSP, `no-referrer`), magpie's and egress's ops reads, the Kubernetes reader (`InCluster`, the Pod's ServiceAccount) |
| `snapshot`, `health_verdict` | 18.3, 18.6: the snapshot's sections (8 s each, `null` + `errors[name]` on failure, cached 4 s per zone) and the verdict |
| `scripts/hermit-admin.sh` | 16: the operator's and runbooks' client of the admin routes (token from `HERMIT_ADMIN_TOKEN`, passed to curl on stdin) |
| `server`, `app` | the `hermit_api::Server` implementation; start, loops, drain (20.2), kill |

## Tests

`cargo test -p hermit` needs `AVIARY_TEST_DATABASE_URL` (ci.yml's Postgres). `tests/common` is the harness: a fresh
database per test, a fake fabric whose sandboxes are `aviary-fakes` supervisors on local ports, in-memory workspaces,
and replicas started in-process with short timings (`Timings` is code, not configuration).

- `tests/conformance.rs`: `aviary-conformance hermit` (24.1) against a replica, every check passing, none skipped.
- `tests/workspaces.rs`: hermit 24.2 "Workspaces" on the real `JuiceFs` store over a plain directory (the CLI
  replaced by a copy/remove fake with an in-memory quota table): the quota on every ensure, path escapes refused
  (an absolute symlink, `..` through a symlink into a neighbour), symlinks inside followed, a FIFO never blocking a
  read, Range and 416, listing pages, clone with its own quota after the live sandbox's flush, delete after the
  sandbox is gone, a refused delete (503, recorded for the verdict), a broken mount (503 with `retry-after: 2`, the
  start handed to the replica whose mount works).
- `tests/ops.rs`: hermit 24.2 "Ops" (ported `ops.test.ts`) and the admin routes: pigeon's admin re-check, the
  monitor token's snapshot-only scope, the page's headers and halves, every snapshot section from live-shaped rows
  (hosts, sandboxes, events, starts and releases, an imported start keeping its `at`, hosts gone, the fabric's plan,
  a fake Kubernetes API, a fake magpie and an egress stub), a failed source as a `null` section, the verdict, the
  monitor's two contracts against `tests/fixtures/monitor-contract.json` (field paths and types, `what`, `owner`,
  every `at` in the one format), and every admin route (caps, retire, the off switch seen by `PUT …/sandbox`,
  fresh-host, the trace, exec through the supervisor, the operator's release and its console tail, recycle, purge
  through magpie) audited in `admin_events` with an exec's argv and exit code but never its input or output.
- `tests/ops.rs` also runs `scripts/hermit-admin.sh` against a replica (the off switch, the trace, a refused purge,
  the audit log).
- `tests/common_port.rs`: the hermit parts of jarvis3 `common.test.ts`: who is a user (`@domain` entries, no
  subdomains, a bare `@domain` or an empty address never), migrations applied twice change nothing, one janitor
  leader at a time with a takeover.
- `tests/lifecycle.rs`: hermit 24.2's ported tests for the core: one sandbox per conversation, the `PUT`
  reservation, the janitor's idle and lifetime rules (busy never idled, 9 h forced), release refusal restarting the
  idle clock, forced delete, a requested release carried in the activity reply and done at the idle report, a `PUT`
  cancelling a pending soft release (one no task has sent, and one whose supervisor is being asked: refused -> 200
  the same sandbox, agreed -> 202 a new one), a `PUT` during a 10 s forced release (503s, then a new ready sandbox,
  never a `gone` the driver would fail the turn on), a forced release asked while a soft one is refused (ends the
  sandbox, reason `deleted`), a forced release asked while a soft call fails (run at once, not left to the
  janitor), a cancelled soft release whose release replica died asked again before the sandbox is kept (agreed ->
  a new sandbox, refused -> the same one), a forced release ending a start that waits for a host at its next poll,
  a start whose create fails after the drain handed its claim back (the new owner's sandbox is not destroyed and no
  failed start is recorded), a late forced release still asking the supervisor, the pool victim (least recently
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

- The gVisor fabric (hermit 13-14, WP-19). Until it lands, `hermit serve` runs with `fabric::Unbuilt`, whose
  placement errors end every start `start_failed`/`placement` after `hostWaitSeconds`.
- The janitor's orphan sweep (12 step 8) and vanished/host-loss judgement (13.4, 13.7) are the fabric's scaler.
- Metrics (25) beyond the platform's defaults and `hermit_catalogue_refused_total` and `hermit_held_requests`
  (3.4.1, 23; registered in `main.rs`): the lookups, sandbox, host, start and request families of hermit 25 are not
  built (WP-20 built the page, the snapshot and the verdict, which read the tables directly).

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
  not yet answered 200") marks `release_pending` `cancelled` on the row (never clears it while a release task runs
  it or after it was `sent`) and waits for the supervisor's answer instead of answering 200 at once: refused, it
  answers 200 with the same sandbox and the release is cleared; agreed, the sandbox goes and the `PUT` starts a new
  one (202). The supervisor's answer cannot be taken back, so a 200 at once could hand magpie a sandbox that is
  about to be destroyed (the A20 race). The release task marks the release `sent` under its claim before the call
  and re-reads `release_pending` after a 409, a 200 or a failed call. A takeover of a cancelled release that was
  `sent` asks the supervisor again before it keeps the sandbox (the old replica may have died after a 200); one
  cancelled before it was sent is cleared. A soft release that a `PUT` cancelled is not offered in the activity
  reply. A pending soft release no task runs and none sent is still cleared outright.
- **Release precedence** (13.12): Lost > Forced > Soft. A pending release is never replaced by a weaker kind (a
  forced one never replaces a pending lost one, so a sandbox on a dead host is destroyed at once with
  `sandbox_lost`); the janitor's lifetime and not-allowed steps skip any row whose pending release is not soft.
- **The long-poll holds a `releasing` sandbox** as it holds a `starting` one (3.6.3 names only `starting`): answered
  at once, magpie's loop, which waits for `ready` or `gone`, would spin until the destroy.
- **A forced release's `deadlineAt`** is written when the row enters `releasing` (13.12 forced step 1), not when the
  release was asked, so a forced release run late (its claim could not be taken at once) still asks the supervisor
  to end the turn; only a row already `releasing` when a takeover resumes it skips the call once its deadline passed.
- **Drain step 5** (20.2): the claims are handed back without stopping the tasks; the "in-flight requests and calls"
  hook waits up to 35 s for both the API server's requests and the tasks' supervisor and sandboxd calls already
  sent, at once rather than one after the other (the 90 s grace holds preStop 10 s + 10 + 35 + 5 s). The hand-back
  marks every live claim of the replica lost in memory first, so each task stops at its next step, and a side
  effect no single guarded write covers (a sandboxd `DELETE`, a failed start's record) runs only under
  `Claim::guard`: a read hold on the replica's effects gate plus a guarded renewal of the claim. The hand-back takes
  that gate for writing, so it waits for an effect already under way and none starts after it; a failed start's
  `sandbox_starts` row is written in the same transaction as its guarded `gone`. A task whose call returns finds its
  claim gone and stops; the new owner repeats the idempotent step. What is still running at 35 s is stopped.
- **A forced or lost release asked for a starting sandbox** ends the start at its next step or poll (the host and
  supervisor waits re-read the row each poll), destroying what was made, rather than only once it reaches `ready`
  (11.2 step 6): a deleted conversation or a person who left has nothing to start for. A soft release (`requested`) still waits for `ready` and an idle point.
- **The workspace step's hand-over** (11.2 step 2, 15.6): after 3 tries 2 s apart that all met this replica's
  broken mount (`WorkspaceError::NotMounted`), the start hands its claim back when another replica reported a working
  mount in the last 30 s (`replicas`, written every 10 s); this replica's sweep skips workspace-step starts while its
  own mount is broken. Otherwise (a CLI or metadata failure, or no replica with a working mount) it fails
  `workspace_unavailable` as before.
- **Workspace usage** comes from `juicefs quota get`, whose table prints sizes in JuiceFS's human form (`1.5 MiB`),
  so `usage.bytes` is exact to the unit's first decimal (3.8 does not say how exact it must be). A workspace whose
  quota is missing reports `quota` and `usage` as zeros until its next ensure sets the quota.
- **uid 1000** (15.4) is the mount's (`all-squash=1000:1000`), not code: nothing in hermit chowns, and the tests,
  which run the store on a plain directory, do not check ownership; the fabric acceptance (24.3) does.
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

### WP-20 (workspaces, ops page, admin routes)

- **The snapshot's extra top-level fields** `controls`, `catalogue`, `replicas` (each replica's workspace mount and
  last refused delete) and `fabricOps` (`lastPassAt`, `egressTargets`) carry what 18.6's verdict judges beyond 18.3's
  table; 18.3's fields are unchanged.
- **The fabric's part of the page** is `Fabric::ops()` (`FabricOps`: the scaler's `plan`, the leader's last pass,
  the egress target count, slots per host), a default method answering "nothing known" until WP-19 implements it.
  Until then `scaler.plan` is `null` and `hosts.list[].slots` `null` (the monitor's `ai_slim` reads
  `scaler.plan.*`, so the YYC deployment needs WP-19's plan before the switch).
- **A workspace delete refused** (15.4) is a problem in the verdict while the refusal is younger than
  `recentMinutes` (15 min): magpie's outbox retries the delete, so a lasting cause keeps the finding; 18.6 does not
  say how long it shows.
- **"Not reporting"** (18.6 watch) is a ready sandbox whose last activity report is older than three report periods
  (`3 × activityEverySeconds`, 45 s).
- **`Aviary-Admin` missing** on an admin route answers 400 `bad_request` (24.2 says only that it is required); the
  person purge sends no `Aviary-Admin` to magpie (its caller is hermit) and records the operator in `admin_events`.
- **Admin reads are audited too** (`hosts.read`, `sandboxes.read`, `sandbox.log`, `events.read`): 16 says each call
  writes a row; a read has no change, so its row is written before it answers.
- **`Loaded::allowed`** matches a domain entry only for `local@domain` with a non-empty local part and one `@` (it
  matched a bare `@domain` before; every caller normalises the address first, so no request could use that).
- **The page's zone** is the browser's (`Intl`), falling back to `ops.timeZone`; times are shown in it (18.2: "polls
  `./snapshot?tz=<browser zone>`").
