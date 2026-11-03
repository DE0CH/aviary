# supervisor

The in-sandbox supervisor and its keeper (supervisor spec; platform R12, R32-R35). One supervisor per sandbox, one
sandbox per conversation. Built in three packages: WP-21 (this state of the crate: the process model, the lifecycle,
hermit's side), WP-22 (the `~/.claude` filesystem), WP-23 (the Claude Code driver, `src/cc/`).

## Binaries

| Binary | What |
|---|---|
| `supervisor-keeper` (`src/bin/keeper.rs`) | PID 1 (the image's entrypoint, no arguments, no environment read). Starts `supervisor run` with its own environment and restarts it (0.1, 0.2, 0.5, 1, 2, 5 s; reset after 60 s up); is the parent of everything the supervisor starts; owns their stdin pipes; keeps the `/dev/fuse` fd; reaps; remembers the last 64 exits; on SIGTERM waits 20 s for the supervisor, then SIGKILLs everything. One thread, `poll(2)`, every buffer allocated before the loop. |
| `supervisor` (`src/bin/supervisor.rs`) | `supervisor run` (the start sequence of 4.3), `supervisor hook send-user-file` (Claude Code's `SendUserFile` hook, 13), `supervisor --version`, and `sandbox-secret <KEY>` when called by that name (a symlink). |

## Modules

| Module | Spec |
|---|---|
| `limits` | 20: every constant, nothing configurable |
| `keeper::{proto, server, watch}` and `keeper::Client` | 4.1, 4.2: the protocol (SPAWN, STDIN/STDIN_MORE, CLOSE_STDIN, SIGNAL, PUT_FD, GET_FD, EXITS, STATUS, RELEASED), the keeper, the async exit stream |
| `state` | 4.4: `run.json`, atomic, at once on a state change, at most every 100 ms otherwise |
| `memguard` | 5.2, 5.3: the agent cgroup (v2 or v1) and the user-space guard |
| `docker` | 4.6: dockerd on demand |
| `boot` | DESIGN 12 D1, platform R32, 6.2: the sandbox token from `HTTP_PROXY`, the fixed-names client through that proxy, the child environment |
| `hermit::{Hermit, activity, control, exec}` | 15.1-15.4: the descriptor, activity reports, `:7070`, `/v1/exec` |
| `lifecycle` | 15.5: the state machine, admission, release, prepare, resumed, clock-jump detection; the `Agent` and `Filesystem` seams |
| `local` | 15.2: `agent.sock` secrets, `sandbox-secret` |
| `magpie` | 4.3 step 5: the lease at start; the cc operations' `opId`s (9.3: UUIDv5 of `bootId ‖ unique ‖ step` under `SUPERVISOR_OP_NS`) |
| `fuse::{proto, errno, scope}` | 2.5, 9.1, 9.3, 9.5 items 5-7: the 7.31 wire format, magpie's answers as errno, the scope table and path checks |
| `fuse::{nodes, cache, local}` | 9.7, 9.8, 9.1: node ids and `nodes.log`, the tree (in memory, loaded from magpie at every lease) and the content cache (`cache/<node>.<version>`, 256 MiB LRU, the main transcript pinned), local scope's passthrough and fd table |
| `fuse::remote` | 9.5, 19, 20: the lease, 16 calls at once, 30 s attempts, the retry rule (general outage vs one stuck op), INTERRUPT before any attempt |
| `fuse::{journal, dispatch}` | 9.3, 9.9, 9.10: the `MAP_SHARED` journal (requests read straight into a slot, the short-read check, the kept results of the resend rule), the workers, every opcode, the resend rule, the replay, the repair, the lease loop after `lease_lost` |
| `fuse` (`FuseFs`) | 4.3 step 6, 9.3: the `Filesystem` the binary mounts: a fresh `/dev/fuse` connection (`mount(2)`, the new `bootId` stored atomically, then `PUT_FD`) or the keeper's (`GET_FD`, the stored `bootId` or exit 75 `boot_id_unreadable`) |
| `cc::{launch, managed, stream, control, prompt, tasks, report, run}` | 6, 7, 8.2, 10, 11, 12.2: argv and environment, the managed files, the stdout tail and parser, control messages, the user message, the task tracker and hold, the ordered reports, the run state machine and the driver (`cc::run::Driver`, the `Agent`) |
| `uploads` | 12.1: blobs into `/workspace/uploads` as the agent |
| `sent_files` | 13: the hook process, `agent.sock`'s descriptor-passing acceptor, the snapshot upload |
| `model` | 14, 17.3: the models route and the title call |
| `start` | 4.3, 4.7: the start sequence, `/etc/hosts` and apt's proxy file, the re-attach check, SIGTERM |

## The seams for WP-22 and WP-23

`lifecycle::Agent` (the Claude Code driver) and `lifecycle::Filesystem` (`~/.claude`) are what the start sequence
and the lifecycle call: `Filesystem::mount` at step 6, `Agent::attach` at step 7 with an `AttachContext` (the prior
`run.json`, what `start::reattach` found of its generation, the lease, the descriptor, the persisted state, the keeper
client and exit stream, the boot environment, the agent cgroup), and the release, pause and resume hooks. A
`start_turn` goes through `Lifecycle::admit` before anything else. What the implementations must allow for:

- `attach` runs under the lifecycle's boot gate: never after a release has begun, and an attach in progress is
  dropped when one begins (the release waits for the gate at most until its interrupt time), so `end_turn` and
  `stop` must end whatever a dropped attach had started, a spawned generation included. `attach` returns once the
  generation is spawned or re-attached and its loops are started, without waiting for Claude Code's
  initialisation. Health turns `ready: true` only after `attach` returned (4.3 step 9).
- Wait for an exit with `Exits::wait(pid, since)`: `since` is `Client::spawn_tracked`'s (the keeper's exit counter
  at the fork) for a process spawned now, `Exits::origin()` for one found alive at start (a re-attached
  generation), so a reused pid never matches an older exit.
- `mount(restart)` is `true` exactly when the keeper holds the `fuse` fd; a mount future is dropped when a release
  begins meanwhile, and `settle`/`unmount` are then called on a filesystem that never mounted.
- `Reattach::Exited { exit: None }` is a generation whose exit code is unknown (`run.json` of another boot, or a
  `lost` record after the keeper's exits ring overflowed). `claude` children `run.json` does not name are killed
  before `attach`.
- `Agent::tasks()`/`asks()` (default empty) fill `GET /v1/state`'s `run.tasks[]`/`run.asks[]`: WP-23 implements them.
- The driver's `stop(deadline)` and `end_turn(deadline)` must end by their deadline: under SIGTERM the release has
  18 s in all (`lifecycle::ReleaseTimes`).
- The driver's view of the filesystem is `cc::Transcript` (`set_turn`, `set_main_transcript`, `wait_stored(uuids)`
  for 8.2, the heartbeat's `cc` facts and its `heartbeat_ok`, the lease after a re-take), implemented for
  `fuse::FuseFs` in `cc/mod.rs`. `AttachContext` carries the lifecycle (weak, for `Lifecycle::admit`) and the activity
  reports (generation exits).

What WP-23 wires to the FUSE layer (`fuse::Hooks`, `FuseFs::core()`): `lease_lost` (SIGKILL the generation, no more
reports for its turn; the binary already turns health `ready: false` and back on `lease_regained`), `write_stuck` (the
turn `failed` with `cc.write_stuck`, the generation ended), `forked` (stop the agent), `Core::set_turn` (every write's
`turnId`), `Core::set_main_transcript` (from the `running` report), and `Remote::record_ok` on each successful
heartbeat (the stuck rule's evidence that magpie answers other calls). The binary wires all of these to the driver
(`Driver::lease_lost`, `Driver::write_stuck`, `Driver::forked`).

## Tests

- Keeper protocol (`src/keeper/tests.rs`): spawn with an exact environment, stdin written whole and at most once per
  `msg_id` (the 16-entry ring), an incomplete message discarded (small and spooled), a message Claude Code has not
  read holding up nothing else, a child exiting mid-message, the fd store across clients, the exits ring, signals to
  children only, the supervisor's restart backoff, the SIGTERM stop, `boot_id` kept; a released exit 0 not restarted; a child stalled
  before exec (a FIFO stdout) holding up only its own SPAWN; a resend while the original is still being written
  (`ALREADY`); orphans counted, not in the ring; the exits gap reported `lost`, and not passed while the `STATUS`
  after it fails; a reused pid never matching an older exit; the keeper answering (but spawning nothing) during its
  stop grace; a SIGTERM during the restart backoff starting a supervisor at once and handing it the stop;
  `RELEASED` making exit 0 final.
- Lifecycle (`src/lifecycle.rs`): not ready until descriptor, lease, mount and workspace; unforced release and pause
  refused while busy or with an ask open (nothing changed); a forced release ends the turn first; a second release
  waits for the first; a `start_turn` racing a release is either refused or makes it busy; a blocked write refuses an
  unforced pause; clock jumps; ready only after `attach`; no `attach` once a release has begun and a hanging
  attach dropped by a SIGTERM release that still ends within 18 s; the release deadlines (a SIGTERM during a stuck
  turn finishes within 18 s, on paused time); no exit before the keeper took `RELEASED`; a pause during a release
  answered busy.
- Memory guard on a fake `/proc` (`src/memguard.rs`), `run.json` (`src/state.rs`), exec (`src/hermit/exec.rs`), dockerd
  with a stand-in daemon (`src/docker.rs`, as root) and a running one reused after a supervisor restart.
- Start sequence (`src/start/tests.rs`) through fake hermit and fake magpie behind `test-gateway`, the sandbox token in
  `HTTP_PROXY`: ready, reported, 401/503, not ready without the lease, release refused while busy, a forced release
  exiting 0, a pending release in the activity reply; a release while waiting for the lease or retrying the mount
  (exit 0, no attach); a state change during an in-flight report sent at once; a start after a release
  (the keeper says `released`: reports `released`, takes no lease); a SIGTERM release that ends within 18 s while
  hermit holds every activity report unanswered; restart detection without `run.json` (held `fuse` fd, a stray `claude`
  killed); the re-attach check against a real keeper.
- `tests/restart.rs`: real supervisor processes under a keeper (`tests/common`): the first start pre-spawns a
  stand-in Claude Code and records it in `run.json`; a supervisor killed with SIGKILL is restarted, takes the
  `fuse` fd back and re-attaches to the live generation; a release over `:7070` exits 0 and nothing is started or
  leased again; a runtime stop (SIGTERM to the keeper) is a clean forced release in well under the 20 s grace.
- `tests/soak.rs`: the same harness under `:7070`, exec and socket load for 20 s, with a supervisor restart while
  hermit's activity route and magpie's lease route answer 503: RSS under 64 MiB, and each supervisor process flat
  after its own warm-up (debug build: 23 MiB peak), every restart re-attached. `soak_long` (1 h, a restart with outages every 10 min, ignored) is for
  `nightly.yml`: `cargo test -p supervisor --test soak soak_long -- --ignored`.
- Test supervisors read a `/proc` of their own and run no memory guard (it starts only when the parent is PID 1):
  the guard must never kill a test host's processes. The guard's decisions are tested on a fake `/proc`; the guard
  running live under the keeper as PID 1 is left to the image smoke test (WP-24).
- `ci.yml` runs as a user, so the dockerd test there only sees the keeper refuse uid 0; a step after the suite runs
  it again under `sudo` (dockerd spawned as root, `daemon.json`, the `_ping` wait).

- FUSE (`src/fuse/tests.rs`, through the sentry stand-in `src/fuse/testing.rs`, a `/dev/fuse` in memory, against
  fake magpie): every opcode against both scopes (and refused paths, EXDEV across scopes, the root's union, INIT
  7.31); the journal replay at each fault point ("read returned", "magpie call sent", "magpie answered", "reply
  written") for a WRITE, MKDIR, CREATE `O_EXCL` and RENAME, each caller answered once with success, each op stored
  once, node ids kept; the resend rule with a WRITE, CREATE `O_EXCL`, MKDIR and RENAME in flight at a "checkpoint"
  (the stand-in forgets the request and resends it under a new `unique` with no INTERRUPT), both while the original
  is still in flight and after its reply was refused with ENOENT, each applied by one magpie operation; a general
  outage blocks without EIO; one stuck op answers EIO and `write_stuck`; the offset repair from the cache and a
  fork answering EIO; `lease_lost` and `not_current_sandbox` without a restart loop (unleased: magpie scope EIO);
  `bootId` per connection; an INTERRUPT before any attempt reached magpie answers EINTR. Unit tests: the wire
  layouts, the errno table, the scope table, node-log restarts, the tree and the cache (LRU, pin, reconcile), the
  journal (slots across mappings, the short-read check, kept results). As root (`ci.yml`'s sudo step): a real kernel
  FUSE mount with supervisor 2's options serving ordinary syscalls on both scopes (append, read, stat, rename, a
  `mkdir` lock, readdir, a refused path, `link(2)` EPERM, unlink).
- On the real runtime (`GATE.md` "WP-22", `fabric/fuse-gate/rig/`, outputs in `fabric/fuse-gate/results/wp22/`): the
  release builds of `supervisor` and `supervisor-keeper` (PID 1) in a runsc sandbox against `fuse-rig`'s world (fake
  hermit and fake magpie behind the reference gateway, in the sandbox). Gate items 1, 3, 4 and 5: the mount and uid
  1000 I/O on both scopes; 160 live migrations, each with a WRITE, CREATE `O_EXCL`, MKDIR or RENAME held in magpie
  across the checkpoint (80 between two hosts, 80 onto the same host; both layouts): the resend rule answered every
  restarted syscall with the original's result and magpie applied each operation once; the `fault-injection` build
  (which reads its point from `/run/fault-injection`, `fuse::FAULT_FILE`) SIGKILLed at each journal point of the four
  operations, journal on scratch and on tmpfs; locks. Items 2, 6, 9 and 10 need Claude Code in the sandbox (WP-23's
  driver, WP-24's image).

## Deviations from the spec text (WP-22)

1. **The resend rule's "any other request from that thread drops the entry"** counts only state-changing requests
   (SETATTR, MKDIR, UNLINK, RMDIR, RENAME, OPEN, WRITE, CREATE, SYMLINK): a restarted `mkdir`/`rename` walks its path
   again, and the LOOKUPs of that walk must not drop the result the resend needs. The rule applies to local scope's
   state-changing requests too (their passthrough is no more idempotent than magpie's).
2. **Kept results live in a 64-entry area after the 64 request slots** of `fuse-journal` (4 KiB each), not in the
   request's own slot: a kept result would otherwise occupy a reader slot until its resend arrives.
3. **A journal replay leaves namespace checks to magpie**: the tree loaded after a restart may already show the
   replayed request's own effect (a MKDIR magpie committed before the kill), so a replay never answers EEXIST/ENOENT
   from the tree; the derived `opId` returns magpie's stored result. Node ids are pruned against the tree only after
   the replay, so a replayed RENAME keeps its node's id.
4. **The repair's fresh `opId`s are not recorded in `run.json`** (9.5 item 6): a repair repeated after a crash re-sends
   bytes that are then already in magpie, which magpie answers with success and no change (magpie 6.6 rule 0).
5. **Repair candidates at a new lease** are cache files whose version is newer and size larger than the tree's (magpie
   lost acknowledged writes); a cache file older than the tree is dropped as 9.7 says. Without the version test a
   crash between magpie's 200 for a truncate and the cache update would re-append truncated bytes.
6. **INTERRUPT answers EINTR** while no attempt is in flight and every earlier attempt was refused by magpie itself
   (`503 unavailable`); after a connection error, a timeout or a gateway 502/504 the call may have been executed, so
   it runs to its end (the sentry discards a reply nobody waits for).
7. **A READ miss fetches the whole file, then answers** (9.7 also answers ranges already arrived while the rest
   streams); the file is streamed to the cache's disk, never held in memory. When the cache cannot hold it (disk
   full) the range is fetched from magpie for that READ.
8. **`close_connections` is a no-op**: the fixed-names client's pool is not the FUSE layer's to rebuild; a connection
   left half-open by a pause fails its next attempt, which the retry rule repeats with the same `opId`.
9. **Fault injection in tests** "kills" one server instance in-process (its workers stop at the point, without a
   reply or freeing the slot, and the next instance takes the connection, as from the keeper); the `fault-injection`
   cargo feature SIGKILLs the process instead (gate item 4 on the fork). "Magpie call sent" is the moment an attempt
   came back, before its answer is used; "magpie answered" is after the answer, before the tree and cache change.
10. **The bootId of a fresh connection is stored before `PUT_FD`** (9.3 says after the mount and before the first
    read): a crash between the two then leaves no keeper fd, never a keeper fd without its id.
11. **The migration acceptance ran against fake magpie** inside the sandbox (`fuse-rig`), on Hetzner hosts (not a
    fabric-AMI host), one way between the two hosts (the target's CPU features are a superset of the source's) plus
    80 restores onto the same host; the order "the original's reply refused with ENOENT before the resend arrives"
    never occurred in a real migration (the restarted request always arrived while the original was held), so that
    order is covered by the unit test only.

## Deviations from the spec text (WP-21)

1. **Stdin staging.** 4.2 has the keeper copy a message through one 64 KiB buffer per child as Claude Code reads it;
   but "an incomplete message is discarded, never half-written" needs the whole message before its first byte reaches
   the pipe. The keeper stages it: in one of 4 pooled 64 KiB buffers when it fits, in a spool file under
   `/var/lib/supervisor/keeper` (scratch) when larger, then copies it as the pipe accepts. Memory stays fixed.
2. **Keeper protocol details** the table leaves open: continuation packets are `STDIN_MORE`; `ALREADY` is answered
   after the message's bytes are read (and dropped); `SIGNAL` to a pid that is not the keeper's child (or its group) is
   refused `ESRCH`; the supervisor opens one connection per request, so a stdin message waiting for Claude Code never
   holds up a signal or an exit poll; `STATUS` lists the supervisor among the children.
3. **The activity report's shape** is `hermit-api`'s (`tasks`, flat `restarts` and `rssBytes`, `memory {limit, used}`),
   not 15.3's nested example: the contract crate is the text of record (platform R35).
4. **A third concurrent `/v1/exec`** is answered 429 `rate_limited` (15.4 says "at most 2 at once" without an answer).
5. **SIGTERM** is a forced release with reason `requested` and code `sandbox_lost` (4.7 names none).
6. **The 24 h soak with synthetic turns and 1 MiB lines** needs the driver (WP-23) and `nightly.yml` (WP-28); this
   package runs the 20 s soak (one supervisor restart, hermit and magpie outages) in `ci.yml` and provides
   `soak_long` (1 h, restarts and outages every 10 min). Open items: WP-23 adds turns and 1 MiB lines to the soak;
   WP-28 runs `soak_long` in `nightly.yml`; the keeper's 4 MiB RSS needs it as PID 1 with root (its fixed paths),
   so it is left to the image smoke test (WP-24).
7. **`RELEASED`.** 4.3 says exit 0 means a requested shutdown; the keeper (4.2) restarts the supervisor whenever it
   exits. Both hold with a keeper op the table does not have: when a release reaches `released` the supervisor
   sends `RELEASED` (retried every 100 ms; the process does not exit before the keeper took it), the keeper keeps it
   in memory, does not restart a supervisor that exits 0 after it, and says `released` in `STATUS`, so a supervisor
   that starts after it (a crash after the release) only reports `released` and exits 0. No file write is involved,
   and a checkpoint keeps the keeper's memory, so a restored released sandbox stays released.
8. **The exits ring holds tracked children only.** PID 1 reaps every reparented process; those are counted
   (`STATUS` gains `orphans`) and kept out of the 64-entry ring, so a burst of them cannot push out a Claude Code,
   exec or dockerd exit. If more than 64 tracked exits land between two polls, `keeper::watch::Exits` sees the gap,
   checks `STATUS` and reports every waited pid that is gone as a `lost` record (code unknown).
9. **The keeper during its stop grace** keeps answering on its socket (4.2 leaves it open; the forced release
   signals Claude Code and polls exits through it) and refuses `SPAWN` with `ESHUTDOWN`. `SPAWN` is answered from
   the poll loop when the child's error pipe closes, so a child stuck before exec (a stalled `/workspace` in its
   `chdir`) holds up only its own answer.
10. **SIGTERM's release deadline is 18 s**, not 30 s (`RELEASE_DEADLINE_MS`): 4.2 gives the supervisor 20 s before
    SIGKILL, and 15.5's 15 s interrupt plus the 5 s + 3 s stop does not fit. Every release keeps 1 s of its deadline
    for the final `released` report, stops and settles before that, and cuts the interrupt so the stop keeps its
    8 s: 15 s under hermit's 30 s deadline, 9 s under SIGTERM.
11. **The memory guard starts right after the descriptor** (4.3 lists it at step 9), so exec and dockerd before the
    mount are covered, and only when the supervisor's parent is PID 1 (the keeper, 4.1). Its `/proc` reads run on
    the blocking pool.
12. **A restart is detected from the keeper too** (4.3 reads only `run.json`): supervisor restarts, a held `fuse`
    fd or a live `claude` make a start a restart even when `run.json` is missing or unreadable; a `run.json` whose
    `boot_id` is not the keeper's is not re-attached. The first-start files (`/etc/hosts`, apt) are idempotent and
    are written unless a `run.json` of this boot exists.
13. **`GET /v1/state`'s `leaseEpoch`** is the filesystem's, else the lease the start sequence took.
14. **SIGTERM with no supervisor running** (the restart backoff, or a supervisor just forked): the keeper starts
    one at once, skipping the backoff, and hands it the SIGTERM; the supervisor is forked with SIGTERM and SIGINT
    blocked and unblocks them once its handlers are installed (`Start::spawn`), so a stop is always a forced
    release within the 20 s grace, never the default action. The stop ends when the supervisor that got the
    SIGTERM exits, or at the grace.
15. **SIGINT**, which the keeper forwards (4.2), is a forced release like SIGTERM (A0.F30, a graceful drain on
    SIGTERM or SIGINT); the keeper itself does not stop on it (4.2 stops only on SIGTERM), so the sandbox stays up,
    released, until the runtime stops it.
16. **`prepare pause` during a release** (15.5 has no row): `409 sandbox_busy` while the release runs (nothing is
    quiesced yet), `ready: true` once it has finished (everything is stopped and unmounted, a checkpoint is safe).
17. **`SPAWN` answers `OK pid since`**, the keeper's exit counter at the fork (4.2: `OK pid`), so a wait matches only
    exits after it (`Exits::wait(pid, since)`). `start::reattach` reads the newest exit of `run.json`'s generation
    pid, since `run.json` (4.4) records no counter.
18. **Every call to hermit is bounded by `HERMIT_TIMEOUT` (10 s)** (the client's default is 30 s), and each attempt
    of a release's final `released` report by what is left of the release's deadline: a hermit that holds the
    request never keeps the release past it.

## The Claude Code driver (WP-23)

`cc::run::Driver` keeps one run state behind a lock never held across an `await`: stdout lines (tailed from the
generation's file with inotify and a 250 ms poll), keeper exits, commands and a 1 s tick change it and return the
effects (stdin lines through the generation's ordered writer, reports, signals). Every state change is put into
`run.json` at once together with the stdout offset of the line that caused it (a line that changes nothing but the
offset is persisted within 100 ms); an agent turn's id and an ask's `askId` are derived from (bootId, generation,
stdout offset), so a line read again after a restart reports the same ids. `start_turn` installs the person's turn
(and `pending_inputs`) in the same change as the `written` record, before the message goes to the keeper, so
Claude Code's answer is never read without its turn; an interrupt arriving meanwhile is written right after the
message. Reports go through `cc::report::Reporter` in lanes: one per turn (its agent-turn open, states, the asks
opened during it and the acks of commands about it) and one for the rest, so an end report that waits for its
uuids (`Transcript::wait_stored`, 8.2) or for the filesystem to settle (a generation that exited) holds back only
its own turn. Task events go in 250 ms batches. Everything is retried at 0.5 s doubling to 10 s (or magpie's
`retryAfterMs`) on 5xx, connection errors, 429, `transcript_pending` and `conversation_migrating` until the lease is
lost; any other refusal is logged as `report_given_up` and counted.

The FUSE layer records the uuids of every `cc/write` magpie answered to a `.jsonl` file (`fuse::seen`: each
`"uuid":"…"`, a uuid split across two writes found through the previous write's tail); a wait that misses uuids
first scans the last 1 MiB of the main transcript as magpie holds it (writes answered before a restart). The wait
ends when every uuid is seen or the last one is (the transcript is appended in order), or with `Stored::Stuck` when
the stuck rule of 9.5 item 4 fires.

Tests (`src/cc/run/tests.rs`): a real keeper, fake hermit and magpie behind the reference gateway, the mock model,
and a scripted fake `claude` (the lib test binary again, `cc::run::tests::fake_claude`, behind a wrapper script whose
fd 3 is the generation's stdout): a turn and the generation reused across turns, failed results, interrupt, kill
(stop and watchdog), the 15 s interrupt grace, a crash (`run.exited` with the stderr tail) and the crash loop, agent
turns and a `start_turn` during one, the hold, supersede, `stop_tasks` and a task that ends and wakes the agent, an
ask relayed and answered (and `no_such_ask`), upload materialisation (replacing a model's file, a symlinked
`uploads` refused, a full workspace), the tags and image block of the prompt, titles through the model route, an
unknown command type, a restart replaying a `written` prompt exactly once (the keeper had it, or not), a restart
whose generation died, a magpie outage on the report route, the hook's descriptors over `agent.sock` (symlinks and
paths outside `/workspace` refused) and a forced release during a turn (`sandbox.lost`); an answer read before the
keeper's acknowledgement of the prompt reaches the driver (the person's turn, no agent turn); an interrupt and a kill
of a person's turn queued behind an agent turn; a blob outage during a start (retried), an interrupt and a forced
release while a turn is being prepared (nothing written or spawned); refused initialisations as a crash loop; a turn
whose end waits for its lines while another command is acked; ids derived from a stdout line. The reporter's retry
is checked over a 20 min outage on paused time (`src/cc/report.rs`); the uuid wait against fake magpie through the
sentry stand-in (`fuse::tests::the_end_report_waits_for_the_uuids_magpie_acknowledged`); FIFOs, directories and a
symlinked root refused by the hook (`sent_files::tests::only_regular_files_are_opened`).

### Deviations from the spec text (WP-23)

1. **Derived ids.** The stdin `msg_id` is UUIDv5(`SUPERVISOR_OP_NS`, `bootId` ‖ "stdin" ‖ `cmdSeq` as 8 big-endian
   bytes). An agent turn's id is a UUIDv7 whose time is the generation's start and whose other bits come from
   SHA-256(namespace ‖ bootId ‖ "agent-turn" ‖ generation ‖ stdout offset); an `askId` is a v4-shaped id from the same
   hash with "ask" (11.2 says only "uuidv7" and `<uuid>`; a random id would open the turn or ask a second time when a
   restarted supervisor re-reads the line, 4.5).
2. **Initialisation** waits only for the `initialize` control response (30 s). `system`/`init` is taken whenever it
   comes (in stream-json input mode it may follow the first user message); a different `session_id` there replaces
   the expected one for the next state report, as 7.2 says.
3. **Agent turns** open on an `assistant` message, a `stream_event` or a `user` message carrying a
   `<task-notification>`; a bare `user` line (a tool result, the interrupt marker written after a `result`) does not
   open one, so it can never leave a turn without a result.
4. **Commands after a restart** are polled from the lowest unacked `delivered` entry (minus 1), not from `cmd_after`
   alone: `cmd_after` advances past a received command, and magpie only re-sends commands above `after`.
5. **The crash loop** counts exits the supervisor did not cause and exits after a failed initialisation (a refused
   `initialize`, the 30 s timeout): any three in 60 s. 7.9 names only exits "without a completed initialisation";
   the limits table (20) says "3 exits in 60 s". Both are counted because a generation that initialises and then
   dies on resume is the same budget loop; DESIGN 12 does not settle it, so this is recorded for the operator.
6. **`ScheduleWakeup`'s fire time** is read from `delaySeconds`/`delay_seconds`/`seconds`/`delay`, `delayMs`/`delay_ms`
   or `at`/`fireAt`/`fire_at`/`time` (2.1.304's input shape is not in the inventory); an input with none of them
   gives the pseudo-task no `until`, so only the silence ceiling ends it.
7. **The hook opens paths component by component** with `O_NOFOLLOW` from an fd of `/workspace` instead of
   `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)`, which not every sandbox runtime implements; the guarantee is the same.
8. **A refused hook** is answered 502 `upstream_failed` with the reason as `message` (13 names no status).
9. **A turn ended by a generation exit** reports no `lastUuid` (the generation writes nothing more; naming a line it
   never wrote would make magpie answer `transcript_pending` for ever); its end report waits instead until the
   filesystem requests already in flight are answered (`Transcript::settle`).
10. **The extension table** of 13 moved into `magpie_api::mime` (pigeon uses it too).
11. **The `running` report** of 10.2 step 6 is queued in step 5, with the turn, before the message goes to the keeper
    (it must precede an end that Claude Code's fast answer can produce before the keeper's acknowledgement returns);
    a write that then fails reports the turn `failed` `run.exited`. A restart re-sends `running` (or the agent-turn
    open) for each turn it restores; magpie answers a replay 200 or 409 `bad_transition`.
12. **An interrupt of a person's turn queued behind an agent turn** interrupts Claude Code at once: the agent turn
    ends `canceled`, and the person's turn is interrupted again when it becomes the turn (its 15 s grace runs from
    the first interrupt). A kill of the queued turn kills the generation.
13. **The uuid wait** (8.2) treats the last uuid seen as all seen, and scans every `.jsonl` write and the
    main transcript's last 1 MiB for `"uuid":"…"` anywhere in a line (2.1.304 writes `uuid` after `message`, not
    at the line's start).
14. **Blob reads during a start** (uploads and the model copy of an image) are retried for 90 s, inside magpie's
    120 s `starting` watchdog; 19 asks a rollout to change nothing staff see, 12.1 names no retry.
15. **An agent turn magpie refuses** (409 `bad_transition`: a person's turn is active there, the race of 7.5) is
    settled, not retried or given up: magpie credits the lines written under its id to its active turn (magpie
    6.3), and that turn's own state reports answered 404 are settled too (11 says only that reports are never
    dropped). A 404 for any other turn is still a report given up.
16. **A turn whose end report is on its way** (waiting for its lines, 8.2, or for magpie) stays the heartbeat's
    `turnId` (state `running`, or `stopping` when interrupted) until magpie answers the end, and only then becomes
    `lastTurn`; run.json keeps it (`ending`) and a restarted supervisor sends the end again. Reporting it idle with
    no `turnId` would let magpie 8.5 rule 2 fail a completed turn; reporting `lastTurn` at once would let rule 1 end
    it before its last row.
17. **The uuid wait ends when its generation exits** (a kill, a release, a crash after the result): the writes in
    flight are answered, the uuids get one last 2 s look, and the end goes with `lastUuid` only if they are stored.
    A wait on a live generation is capped at 30 min (logged as an error) for a uuid stdout showed that Claude Code
    never writes to the main transcript.
18. **A `start_turn`'s message is kept** in the generation's directory (`stdin-<cmdSeq>.ndjson`) from its
    `written` record until it is delivered; a replay after a restart sends those bytes again and fetches no blob and
    renames nothing onto `/workspace/uploads` (the agent may have changed the files since). Without the file (it
    could not be written) the replay builds the message again.
19. **Preparation is cancellable** (7.8): an interrupt, kill, release or pause ends a start's blob retries, its
    wait for another spawn, the models route or the initialisation at once (reported `canceled` or `sandbox.lost`,
    acked `delivered: false`); a generation already started goes on initialising by itself.
20. **A generation found initialising after a restart** is killed and a new one started (the previous supervisor
    may have read its `initialize` answer); a spawn never starts while another generation lives.
21. **Upload names** are at most 255 UTF-8 bytes (`magpie_api::sandbox::WORKSPACE_NAME_MAX_BYTES`; magpie's
    `safe_name` cuts to 200 characters and 255 bytes, keeping the extension), and the part file is
    `.upload.part-<32 hex>` whatever the name.

### Open items (WP-23)

- Gate item 7 ran on 2026-11-02 with the real Claude Code 2.1.304 (`spikes/setting-sources/run.sh`: the argv of
  6.1 in a container, a model stand-in that answers the first turn with a `SendUserFile` tool_use). With the
  supervisor's managed settings (`allowManagedHooksOnly`, the managed PreToolUse `SendUserFile` hook) and
  `--setting-sources=`: the managed CLAUDE.md in both requests, `/workspace/CLAUDE.md` in none, the workspace
  SessionStart/UserPromptSubmit/PreToolUse hooks did not run, and the managed hook ran with `tool_name`,
  `tool_use_id`, `tool_input.files`/`caption` and `cwd` as `sent_files::hook` reads them. Positive control
  (`--setting-sources=project,user,local`, no `allowManagedHooksOnly`): `/workspace/CLAUDE.md` in both requests and
  the workspace hooks ran. Control b (`--setting-sources=`, no `allowManagedHooksOnly`): the workspace stays out, so
  `--setting-sources=` alone keeps it out. The request's `SendUserFile` schema has `files` (string array) and
  `caption`. Items 8 and 13 need the full image and the mock model (WP-24's smoke, WP-22's re-run).
  The spike runs the agent with `HOME=/home/node` (the container image's user) rather than the sandbox's
  `/home/sandbox`; `--setting-sources=` and the managed settings path do not depend on `HOME`. It is a recorded
  live check (the task record), not part of CI.
- The soak's synthetic turns with 1 MiB lines (WP-21 deviation 6) and `nightly.yml` (WP-28).
