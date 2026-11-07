# hermit (the YYC implementation of the hermit contract)

hermit spec sections 3-12, 19, 20, 23 and 24.1-24.2 (WP-18, the core) and 13 (WP-19, the fabric). The library
(`src/lib.rs`) is one replica; the binary (`src/main.rs`) runs it as `hermit serve` and checks catalogues as
`hermit catalogue-check <file>`, and runs the switch's one-shot commands (`legacy`, below).

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
| `fabric::gvisor` | 13: `Gvisor`, the fabric the binary runs (its outside world in `Deps`, so tests replace it) |
| `fabric::sandboxd` | 13.15: one mTLS `sandboxd_api::Client` per host, the create errors as the start reads them |
| `fabric::hosts`, `fabric::placement` | 10.2 `hosts`, `host_events`; 13.3 slots, units, placeability, `pick`, placement under lock 7300/5 |
| `fabric::scaler` | 13.4-13.9: the pass (leader of 7300/4), heartbeats, vanished, host loss, `JuiceFS` gating, launches under 7300/7, release of an empty host, moves off retiring hosts, packing, EC2 reconcile, the orphan sweep (12 step 8) |
| `fabric::packing` | 13.11: `plan_packing`, ported as is |
| `fabric::migrate` | 13.10: the resumable `migrate` task and its takeover |
| `fabric::network` | 13.8: gateway and mesh peers, the egress targets from the `EndpointSlices` watch |
| `fabric::ec2` | 13.5, 14.4: `RunInstances`/`TerminateInstances`/`DescribeInstances` (`aws-sdk-ec2`), `WireGuard` keys |
| `fabric::images` | 13.14: the warm-up of a host that just became active; the gateway pulls the configured image (it serves hosts without registry access only images it holds) |
| `server`, `app` | the `hermit_api::Server` implementation; start, loops, drain (20.2), kill |
| `legacy` | 15.4, 22.3: `jfs-owner`, `legacy-cleanup`, deleted after P6 (`import-jarvis` went in A6) |

## The switch's one-shot commands (`src/legacy.rs`; deleted after P6)

- `hermit import-jarvis` (the old stack's sandbox history and share workspaces, cutover 8.1) went in the A6
  contraction (cutover 10.5 step 7); its rows stay (`sandboxes.imported`, `sandbox_starts`, `host_events` and `hosts`
  by `source_id`), and so does what it recorded of the old shares in `legacy_shares`, which `legacy-cleanup` reads.
- **`hermit jfs-owner [--root /jfs]`** (Job `hermit-jfs-owner`, root, the volume mounted without squashing; A2
  before the first import, A6 after the old fabric's teardown; 15.4): `ws/` and everything under it to 1000:1000 with
  `lchown` (a symlink's target is never touched), staying on the volume, only entries not owned already, the top-level
  folders on 16 threads. Exit 1 when an entry could not be changed (the report names the first 20).
- **`hermit legacy-cleanup [--confirm] [--drop-share <id>]…`** (P6, 30 days after the switch; 22.3 step 6). Env
  `HERMIT_DATABASE_URL`, `HERMIT_TOKEN_MAGPIE`, the `JuiceFS` pair. `users/` and `sent/` hold the only other copy of
  what magpie imported, so they go only when magpie's `GET /v1/hermit/legacy-import` says its import is complete (the
  record a clean `magpie import verify --all` leaves once every conversation is owned and every source row imported,
  not stale since); otherwise the plan's `treesHeld` says why, the folder removals below still run, and the run exits
  1. First checks that every old share is carried over (from `legacy_shares`): revoked in the old UI, deleted on
  purpose through hermit (`legacy_share_deletes`), its clone `ws/<uuidv5>` there (not an empty folder hermit never
  cloned into while the share's folder has content), or cloned once with the clone gone since and magpie not referencing it (`sharesGone`). Blocking (exit 1,
  nothing removed): a share folder with no committed row (listed with its age), a live share never cloned, a clone
  that is gone while magpie references it, and a live share's clone with content that magpie does not name (its
  import may have missed it). `--drop-share <id>` lets a folder with no committed row go (a failed make of the old
  UI, which copies the folder before it commits the row) once it and its `ws/` are 24 h old; each is logged and
  listed in `dropped`. Then plans the removal of `users/` and `sent/`, of every empty `ws/<uuid>` folder and of every
  clone with content of a share revoked or deleted that magpie's `POST /v1/hermit/workspaces/exists` (batches of
  1 000; at least one call, so a magpie that does not answer stops the cleanup) does not name, no sandbox that is not
  gone uses, and that is older than 24 h (the folder and its `workspaces` row); other folders with content, other
  names (a clone's temporary tree) and everything magpie names are kept. Without `--confirm` it logs the plan,
  removes nothing and exits 2. With it: `juicefs rmr` of the two trees, then the folders batch by batch: magpie asked
  about the batch, the folders it does not name marked in `workspace_deletes` (starts and ensures refuse them from
  then on; a folder with a delete of hermit's own in flight is skipped), magpie asked again, then right before each
  goes its mark renewed (a mark lapses after 10 minutes; renewed every 200 s while a `juicefs rmr` runs) and the
  sandboxes and the folder's age checked again; then `rmdir` (a folder that got content since is kept) or, for an
  orphan clone, its `legacy_share_deletes` row and `juicefs rmr`; its quota, its `workspaces` row and its mark. Last,
  `workspaces` rows older than 24 h whose folder is gone (a removal cut short before its row went) lose their row and
  quota. Exit 1 when a removal failed or the trees were held.

## Tests

`cargo test -p hermit` needs `AVIARY_TEST_DATABASE_URL` (ci.yml's Postgres). `tests/common` is the harness: a fresh
database per test, a fake fabric whose sandboxes are `aviary-fakes` supervisors on local ports, in-memory workspaces,
and replicas started in-process with short timings (`Timings` is code, not configuration).

- `tests/conformance.rs`: `aviary-conformance hermit` (24.1) against a replica, every check passing, none skipped.
- `tests/legacy.rs`: the commands left after A6 on a scratch directory standing in for `/jfs`, with the old shares as
  `import-jarvis` recorded them: `jfs-owner` (as root: real ownership, a symlink's target untouched; as a user:
  nothing changed, EPERM counted), `legacy-cleanup` refused without `--confirm`, blocked while an old share is not
  carried over (no row, a lost clone magpie references, a live clone magpie does not name) and not by a share
  deleted or revoked or a lost clone magpie no longer references, `--drop-share` only for an old folder with no row,
  asking magpie even with nothing to ask about, keeping what magpie names, live sandboxes' and recent folders and
  folders with content, removing an orphan share clone, and every re-check between the plan and the removal (magpie
  naming a folder late, a sandbox started late, a delete of hermit's own in flight, a failed removal).
- `tests/legacy_juicefs.rs`: the same commands on a real `JuiceFS` volume (the `juicefs` 1.4.1 CLI, SQLite
  metadata, two FUSE mounts: one `--all-squash 1000:1000` as hermit's own, one plain as `hermit-jfs-owner`'s), with
  a committed share's clone as the import left it: `jfs-owner` `lchown`ing over FUSE (a symlink's target untouched),
  `legacy-cleanup` blocked by a share folder with no row and then removing `users/` and `sent/` with `juicefs rmr`
  and an old empty folder, keeping the clone magpie names. It runs only as root with `AVIARY_TEST_JUICEFS=1`
  (ci.yml's test job, under sudo); elsewhere it prints `skipped`.
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
- `tests/fabric.rs`: the fabric against fake sandboxd hosts (`sandboxd-api`'s router over in-memory hosts on local
  ports, which can go down and come back) and a fake EC2 (a launched instance becomes a fake host named and
  addressed by its user data): ported `vm.test.ts` (burst to AWS with the user data, active once mounted, the image
  warm-up, placement, the gateway's peers and egress targets, packing by live migration, not across gVisor releases,
  a send that would not converge, the emptied host terminated past its minimum lifetime; `maxHosts` and a full pool
  reported; capacity planning across two replicas and a burst; the demand window; the renamed gateway; host loss not
  judged while the gateway is away or settling; `JuiceFS` gating, retirement and the boot timeout), and what the
  port adds: host loss and vanished sandboxes (`alive:false`, not listed) `gone`/`sandbox_lost` within 10 s with the
  scaler running by itself, a migrating row never judged from its source, a CPU feature missing, the EC2 reconcile
  (orphan instances, a missing instance, a failed launch with its AWS code), orphan sandbox names, a migration
  resumed by another replica after the first one died mid-send, and a start and release through the whole core.
- Unit tests in `src/fabric/`: slots and sizes, `pick`, placeability (the fresh-host switch), addresses, packing
  (ported `packing.test.ts`), peers, `EndpointSlice` readiness, `WireGuard` keys (RFC 7748's vector), create errors.
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

- sandboxd's own side of hermit 13.2 beyond `pids` and the receive's state (`crates/sandboxd/README.md`): the
  fabric already reads `Listed.alive`, never sends `user` and asks `GET …/log`; today's sandboxd answers without
  `alive` (read as alive) and 404 for the log (no console tail is kept).
- Fabric acceptance on real hosts (24.3) is WP-40's; live migration's acceptance there waits for WP-57.
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
  `gateway_settled`) is the core's view of hermit 13; WP-19 grew it by `attach` (the fabric reads the database and
  the configuration through the replica's core), `tick` (the scaler loop, every 5 s, the pass on the leader of
  7300/4) and `migrate` (the `migrate` task), each with a default so the tests' fake fabric is unchanged.

### WP-20 (workspaces, ops page, admin routes)

- **The snapshot's extra top-level fields** `controls`, `catalogue`, `replicas` (each replica's workspace mount and
  last refused delete) and `fabricOps` (`lastPassAt`, `egressTargets`) carry what 18.6's verdict judges beyond 18.3's
  table; 18.3's fields are unchanged.
- **The fabric's part of the page** is `Fabric::ops()` (`FabricOps`: the scaler's `plan`, the leader's last pass,
  the egress target count, slots per host), a default method answering "nothing known"; the gVisor fabric (WP-19)
  answers it from the database (the plan as a launch would compute it, the leader's last pass and the gateway's
  egress targets from the gateway's row).
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
### The switch's commands (WP-30)

- **`legacy-cleanup` without `--confirm`** logs the full plan and exits 2 (a refusal), so a dry run can be read
  before the real one.
- **`legacy_shares` outlives the import** (migration 0005): `legacy-cleanup` needs it 30 days later, after the old
  databases are gone; it is dropped with the cleanup, with `legacy_share_deletes`. hermit's workspace delete writes a
  `legacy_share_deletes` row (migration 0007, by workspace id, for every workspace, since the id cannot tell a
  share's clone) until then, the only place the serving code touches a switch table: the cleanup cannot see a delete
  otherwise once its folder is gone.

### The fabric (WP-19)

- **User data names the hub peer `hub`**, as sandboxd's `host.json` parser does (`crates/sandboxd/src/config.rs`),
  not `hubPeer` as the example in 13.5; `egressPorts` is written as the spec has it (today's sandboxd ignores it).
- **Placement records `host` and `ip` on the row itself**, in the transaction that holds lock 7300/5, so the next
  placement on any replica counts the slot (13.3's `units` are rows); the start's own write of the same values
  follows. A row already placed is answered with its place (the host wait's poll may have placed it).
- **A migration's intent** (the `migrate` claim, `migrating_to`, `migration_started_at`, event `migration-start`) is
  written by the scaler in one transaction under the placement lock that also checks the target still has room,
  rather than by the task's step 1, so two passes cannot both count the same free slot. The task then begins at step
  2; a takeover (no memory of a fresh start) always asks the target first (13.10 "Takeover"). A takeover of an
  intent whose receive was never asked finds `none` and settles: the source lists the sandbox, so it is kept.
- **`maxDowntimeMs`** is sent only when the two hosts' kinds differ (0, no limit, otherwise): 13.10's cross-cloud
  rule; its step 4 lists 500 for every move.
- **A `host_lost` release pending on a row whose migration settled onto a live AWS host is dropped** when the
  migration ends (step 7), so a sandbox that moved off a dying source is not destroyed after it survived.
- **The orphan sweep** (janitor 12 step 8) runs in the scaler at the reconcile cadence (30 s), from the lists the
  pass's heartbeats just read.
- **Vanished** (13.4 step 3) judges only rows that were `ready` before the heartbeat was asked and have not moved
  since it was asked: a list read before a row became ready or moved cannot hold it.
- **The gateway's "answering since"** is kept on its row (`info.upSince`) by the leader and read by every other
  replica each tick, so `gateway_settled()` (the silence rule, the soft release's settle time) answers the same on
  every replica. AWS hosts are judged once the gateway has answered for `hostDeadSeconds` (13.4 step 4 says 30 s,
  YYC's `hostDeadSeconds`).
- **The fresh-host switch** (16) compares a host's activation time (`info.activeAt`, written when it turns active)
  with `controls.updated_at` while `fresh_host_until` is in the future.
- **A launch before the gateway's first heartbeat** (its `WireGuard` key unknown) answers `planned`: the start waits
  and the next pass launches (the waiting row counts in the plan).
- **`openFiles`** has no field in sandboxd's spec; it is not sent (sandboxd's own limit applies).
- **sandboxd connections across replicas** (docs/CONTRACTS.md's open question): kept at 8 per host per replica; the
  queueing beyond sandboxd's 16 workers costs latency during a burst only.
