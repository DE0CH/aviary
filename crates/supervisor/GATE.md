# FUSE gate (WP-07): `~/.claude` as an in-sandbox FUSE server

Gate before the supervisor is built beyond a spike (supervisor spec 2 and 22, items 1-6, 9, 10). Run on
2026-11-02 with the spike in `spikes/fuse-gate` (throwaway, its own Cargo workspace) and the host scripts in
`fabric/fuse-gate` (runtime install, bundle, migration, syscall inventory; they live outside this crate because
the source rules keep the runtime's name out of `crates/supervisor`). Every run's raw outputs, failed runs
included: `fabric/fuse-gate/results/` (README there; `results/…` below means that directory, kept outside this
crate for the same rule). This is the gate's second run, with a third round on Hetzner after its review
(`results/hetzner3/`: the resend evidence, the matched control, the lock test, the tombstone triggers); the first run
(`results/run1-hetzner/`, Hetzner only) is superseded.

## Verdict

**Not a plain pass, and the package is not done:** three measured facts are negative against the spec text as it
stood, two faults after a restore are unexplained (handed to WP-57), and the tombstone rewrite was not reproduced
(a question for Deyao, DESIGN 14):

1. **Negative: a checkpoint resends every request in flight** (item 3). The spec said "the runtime's FUSE has no resend"
   (9.9) and "every write is stored once ... across a migration" (22.4). Measured: the sentry forgets the request at
   the stop and the restarted syscall sends it again after the restore under a new `unique` (so a new `opId`), from
   the same thread: in all 72 AWS migrations with requests held (both held requests, every time) and in all 13
   hold-on Hetzner runs of round 3, where the original's reply was refused with ENOENT every time. The sentry also
   queues `FUSE_INTERRUPT`, but the supervisor read both INTERRUPTs in only 70 of the 72 AWS runs (one in `mig-s06`,
   none in `mig-s09`: there, reads came back with no whole request in the journal slot, verdict 5). Under magpie's
   append rule the resend of an acknowledged append gets `offset_mismatch`, and a resent CREATE `O_EXCL`, MKDIR,
   RENAME or UNLINK would answer EEXIST/ENOENT instead of its real result. The corrected rule (supervisor 9.3) is keyed
   on the calling thread and the original's refused reply, not on INTERRUPT receipt.
2. **Negative: a namespace operation waiting on magpie blocks every path lookup on the mount** (item 3). The spec
   said "cached reads, stat and readdir keep working" while magpie is unreachable (9.10). Measured: `stat` of
   another directory waited 3.37-3.41 s, exactly until a held RMDIR was released.
3. **Negative: INIT "reply 7.23"** (2.5, 9.3) would not get 1 MiB writes: `FUSE_MAX_PAGES` needs minor ≥ 28. The fork
   offers 7.31 and the spike replies 7.31 (item 1).
4. **Unexplained failure: an in-sandbox process is killed by SIGSEGV after a restore** in 6 of 72 AWS migrations
   with requests held in the supervisor (the fake magpie 5 times, Claude Code once) and in 1 of 6 of run 1 (the mock
   model), and in 0 of 30 AWS control migrations without a hold. It is counted as a failure of item 3, not as a pass.
   The sentry's debug log puts the fault in a thread whose blocking `recvfrom` was restarted at the restore; it is not
   systrap's syscall patching (it recurred with patching off) and not stale pages written by the sentry (a copy-out
   check found none). **What causes it is not established:** 6 of 72 against 0 of 30 is not significant (p about 0.07),
   and those controls differed in more than the hold (no blocked-append process, no probe, a 2.3-3.5 MB pre-copy
   stream instead of about 128 MB, half the pause). Details and the matched control in item 3. Owned by WP-57.
5. **Unexplained failure: a journal read after a restore with no whole request in it** (item 3): in `mig-s06` and
   `mig-s09` (scratch layout: the journal is a `MAP_SHARED` file on the host-backed scratch dir), 3 reads of
   `/dev/fuse` returned while the slot's `len` field was still 0; the spike's worker thread panicked on the header and
   the runs were counted as passes, because nothing then read the supervisor's log. Re-checked with the rules of round
   3, `mig-s09` fails as well (7 of the 72 hold-on AWS migrations fail). Owned by WP-57; supervisor 9.9 records the
   reader's check.

**The corrections are handed back to the design of record** (text changed on 2026-11-02, marked "WP-07 gate"):
supervisor spec 2.5, 8.2, 9.1 (the gate's path findings are provisional until WP-22's re-run under 6.1-6.4), 9.3
(INIT 7.31; the resend rule keyed on the calling thread and the refused reply; `bootId` per connection, stored
atomically), 9.4, 9.9 (the short read), 9.10, 16.1, 22 (gate item 4 and "Open items"); magpie spec 6.6 (rule 0: a
write whose range already holds the written bytes is a success) and 6.10; DESIGN.md's FUSE row and section 14 (the
open items and the tombstone question); WORK-PACKAGES WP-05, WP-12, WP-19, WP-22, WP-40 and the new **WP-57** (the
fork: the SIGSEGV and the short read after a restore, with this reproduction), which WP-19's live migration, WP-22's
migration acceptance and WP-40 wait for; `magpie-api`'s `OffsetMismatch` doc (doc only, no type or fixture change).
**WP-22 starts only once these changes are agreed** (they are the inputs it builds against).

Items that pass as specified: 1, 4, 5, 6 (the mount's own share and the 87 MiB cold resume), 9, 10. Item 2 passes
except the tombstone rewrite, which Claude Code 2.1.304 was not made to do in seven triggers (not reproduced; see
item 2). 9 and the path table it feeds are provisional (deviation 4).

## Setup

| What | Value |
|---|---|
| Hosts, items 1, 3, 4 (and 2, 6 repeated) | two throwaway AWS `m7i.xlarge` (Intel Xeon Platinum 8488C, the fabric's instance type) in eu-central-1a (the fabric's region), Ubuntu 24.04 from Canonical's SSM parameter (the base `infra.sh ami` bakes from), kernel 7.0.0-1014-aws; identical `runsc cpu-features`. Not the fabric's AMI, and not in the fabric's account (deviation 1), deleted after the run |
| Hosts, items 2, 9, 10 | two throwaway Hetzner Cloud runners (cx33 in fsn1, cpx32 in nbg1), deleted after the run |
| Hosts, round 3 (items 2 tombstone, 3, 5) | two throwaway Hetzner Cloud cx33 in nbg1 (AMD EPYC-Rome, identical `runsc cpu-features`; IPv6 only), Ubuntu 24.04, deleted after the run |
| Sandbox runtime | runsc from the fork release `precopy-20261021.2` (the tarball and SHA256SUMS check of `fabric/host/setup.sh`, the pin of `GV=`), flags exactly sandboxd's (`--platform=systrap --network=sandbox --overlay2=none --host-uds=none --host-fifo=none --net-raw --allow-packet-socket-write --allow-suid`) |
| Bundle | sandboxd's shape: first process root with the Docker capability set (incl. `CAP_SYS_ADMIN`), `HOME=/home/sandbox`, image rootfs read-only, `/srv` tmpfs, `/mnt` a host-backed bind (scratch), the spike bound read-only at `/media`, a netns with only `lo` |
| Image | `ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f` (the production pin), digest `sha256:7604ae30…fdc67` |
| Claude Code | the image's `@anthropic-ai/claude-code@2.1.304`, the native `bin/claude.exe` (Bun 1.4.4), `-p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions --model claude-sonnet-4-5` (not supervisor 6.1's argv: deviation 4), `ANTHROPIC_BASE_URL` = the spike's mock Messages API |
| Spike | `fuse-gate serve`: FUSE server on the raw protocol (no libfuse), INIT reply 7.31, `max_write` 1 MiB, `max_pages` 256, flags `BIG_WRITES|MAX_PAGES|ATOMIC_O_TRUNC`; every path served from the fake magpie (no local scope); write-through content cache; node ids in `nodes.log`; the `/dev/fuse` fd held by `fuse-gate keeper` (SCM_RIGHTS); a 16-slot MAP_SHARED request journal read into directly; op ids `bootId-unique-step`, `bootId` drawn afresh on every new mount; `--die-at POINT:OPCODE:NTH`. `fuse-gate magpie`: HTTP store with idempotent op ids, `latency_ms` and `hold` knobs. `fuse-gate model`: scripted streaming Messages API that logs each chunk's send time |
| Lint | the spike is `cargo fmt --check` and `cargo clippy -- -D warnings` clean (run in its own directory; it is not an aviary workspace member) |

## Results by gate item

### 1. Mount inside a running sandbox; uid 1000 I/O — pass (AWS)

`results/aws/mount-m1/item1.txt`: `/dev/fuse` is `crw-rw-rw- 10, 229`. The supervisor (root in the sandbox, started
by the first process after the container is up) mounted `/home/sandbox/.claude` with `fd=3,rootmode=40755,
user_id=0,group_id=0,default_permissions,allow_other` (`/proc/mounts`: `fuse rw,nosuid,nodev,…`); INIT: the kernel
side offered **7.31** with flags `0x400028`, the reply 7.31 with the same flags was accepted. uid 1000 (`node`)
created nested directories, wrote, appended, read back and listed; `stat` shows `1000:1000`, the mode as created,
`st_ino` = node id; the fake magpie holds the same bytes.

### 2. A real Claude Code 2.1.304 session on the mount — pass except the tombstone (not reproduced)

Scripted session (`sandbox/mkscripts.py`, `fabric/fuse-gate/session.sh`), run on Hetzner (`results/hetzner/st2`,
quoted below) and on AWS (`results/aws/st-aws`, same results, byte counts within 100 bytes): Bash, Write, Read,
Edit, ToolSearch, TodoWrite, Glob, Grep, a foreground subagent with its own tool call, five failure-injected turns, a
model fallback (`--fallback-model`), a relocation onto an occupied destination (`set_cwd`), seven 1 MiB Write tool
calls, `/compact`, file history (`CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`), `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1`,
then two resumes after two kinds of torn tail. All 12 turns `success`.

- **Bytes in magpie equal the bytes Claude Code wrote**: every file read back through the mount equals the fake
  magpie's copy (`report.json`); main transcript 485,101 bytes, subagent transcript 146,388 bytes.
- **Append**: per append Claude Code opens the file `O_WRONLY|O_APPEND`, one `write`, closes; the server sees WRITE
  at `offset == size` in order.
- **Compaction rename**: once the transcript reached 15,114,844 bytes: CREATE `<sid>.jsonl.compact.tmp.c966ffe5`,
  SETATTR size 0, one WRITE of 30,654 bytes, FSYNC, RENAME over `<sid>.jsonl`.
- **Relocation** (`set_cwd` to `/home/sandbox/work`, destination `<sid>.jsonl` planted): RENAME of the occupant to
  `<sid>.jsonl.superseded-1793623733649` (then `utimensat` on it), RENAME of the transcript and of its `<sid>/`
  folder from `projects/-home-sandbox/` to `projects/-home-sandbox-work/`; Claude Code answered
  `transcript_relocated: true`.
- **File history**: with `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`, `file-history/<sid>/<hash>@v2` (seven of
  1,049,360 bytes, one of 15) written with `copy_file_range`; without it (supervisor 6.2 does not set it) `-p` mode
  writes no file history.
- **Torn tail, final byte** (`torn-b.json`, the last `\n` cut off): the resume's first WRITE starts at the cut with
  `\n` (`torn-b.byte-at-cut.txt`).
- **Torn tail, mid-line** (`torn-b2.json`: the file cut 100 bytes before the end of a 106,354-byte line, leaving a
  partial JSON object): on resume Claude Code read the last 64 KiB (`READ off=306057 size=65536`, i.e. size −
  65,536) and **appended one `\n` at the cut** (`WRITE off=371593 size=1`), then its own lines; it did not truncate.
  The partial line stays in the file as an unparseable 106,253-byte line (`torn-b2.after.json`), the resume turn
  `success`. magpie stores such a line as an unparsable internal line (magpie 6.3); spec 9.4 now says so.
- **Tombstone rewrite: not reproduced.** Claude Code's own trigger (`abandonAttempt`, which yields tombstones, read
  from the binary: on `streaming_fallback`, `chain_advance` (model fallback), `refusal_retry`, `refusal_decline`) was
  tried with the mock (`fabric/fuse-gate/tombstone-try.sh`, `results/hetzner/tomb-*`): (a) failures after a complete
  content block (overloaded error event, clean close without `message_stop`, TCP reset, malformed event, `stop_reason:
  refusal`): 2.1.304 keeps the partial message and continues ("Your response above was cut off…"); (b) with
  `--fallback-model`: a thinking-only response then an `api_error` mid-stream: Claude Code moved to the fallback model
  (`system/model_fallback`), but the abandoned thinking-only message had never been written, so nothing was removed
  (0 truncates, 0 writes below the end); (c) a refusal with `stop_details.category: cyber` and its continuation
  refused too, `--fallback-model` and `switchModelsOnFlag: true`: `system/model_refusal_no_fallback` (no fallback
  route without the server's routing config), the refused message stays in the file; (d) a 20 s stall with
  `CLAUDE_STREAM_IDLE_TIMEOUT_MS=3000` (run 1): no fallback. Round 3 (`results/hetzner3/tomb-t1`…`t5`): (e) `catchall`:
  a refusal without a category, refused again on the continuation, `--fallback-model` and
  `CLAUDE_CODE_REFUSAL_FALLBACK_CATCH_ALL=1`: `model_refusal_no_fallback` (2.1.304 arms the refusal fallback from its
  model catalog, `refusalFallbackModel`, not from `--fallback-model`, read from the binary); (f) `catalog`: the same
  with the catalog flag set the binary's way (`CLAUDE_CODE_MODEL_CAPABILITIES=claude-sonnet-4-5=refusal_fallback`, the
  default Opus as target): **`system/model_refusal_fallback` happened**, the retry went to `claude-opus-4-8` and the
  stream-json output retracted one message (`retracted_message_uuids`), but that message (the refused continuation)
  had never been written to the transcript: only appends, 0 truncates; (g) `overload`: a Bash tool call, then HTTP 529
  for every later request of the main model: after 3 attempts `system/model_fallback` to the fallback model, but
  `chain_advance` abandoned only the failed attempt, which had written nothing; the written tool_use and tool_result
  stayed. In all seven triggers 2.1.304 did not write a message it later abandoned. **So the tombstone's syscalls on
  the mount (`pwrite64` on a non-append fd, `ftruncate` of the main transcript by Claude Code) are not in the
  inventory (item 9), and this acceptance item is missing** (deviation 5). The removal
  algorithm (`performRemoveByUuid`, read from the binary) is replayed by `sandbox/tombstone.py` (a script, not Claude
  Code) on the live transcript, and the resume after it works: whole-file mode, 1,138 bytes removed in `st2`
  (`tombstone-replay.json`; 1,140 in `st-aws`). WP-22's re-run must trigger it for real (deviation 5).
- **Subagent transcript**: 2.1.304 creates `subagents/agent-<id>.jsonl` directly (CREATE + appends) and its
  `.meta.json` by tmp + rename; it never called `link(2)`. It does `symlink("<mount>/…/subagents/agent-<id>.jsonl",
  "/tmp/claude-1000/…/tasks/<id>.output")`: the link lives in `/tmp`, its target on the mount.

### 3. Live migration with a write blocked in the server, Claude Code mid-turn — failed: resend, mount-wide lock, SIGSEGV and short read after restore

`fabric/fuse-gate/migrate.sh` (one run) and `migrate-batch.sh` (N runs, every run's outputs kept): Claude Code
streams an 8 s answer; the fake magpie holds every mutating call (`hold`), so a direct 1 MiB append (3 × 1 MiB to an
existing file) and Claude Code's cleanup RMDIR are blocked inside the supervisor; `runsc checkpoint --migrate-to`
(pre-copy, `--migrate-downtime 100ms`) to `runsc restore --migrate-listen` on the other host; a probe on the target
times a `stat` and a cached read while the hold is still on; the hold is released on the target. A run fails when
the driver's summary does not appear within 120 s of the release, a turn is not `success`, the blocked append is not
exact, a transcript differs from magpie's copy, a `unique` repeats, or any in-sandbox process exits; since round 3
also on a keeper, supervisor or non-zero job exit (all in `exits.log`), a supervisor fault (a panicked thread, a short
read) and Claude Code's own non-zero exit (deviation 8)
(`failures.txt` per run; `results/aws/migrations.json` is `fabric/fuse-gate/migsummary.py`'s table of all runs).

Two layouts: **tmpfs** (supervisor state, cache, journal and the fake store on the sandbox's `/srv` tmpfs: they move
as memory pages) and **scratch**, the production path: state, cache and journal on the host-backed scratch dir,
moved the way sandboxd moves scratch (`gate.sh send-scratch`, a copy of sandboxd's `send_locked`: rsync rounds while
the sandbox runs, `checkpoint --migrate-to`, the final rsync delta while it is stopped, then `SBXDONE`; the target
restores behind this repo's `sandboxd migrate-relay`, which holds the stream's End until then). The pause the
sandbox sees is `final_ms` (the last memory round) + the stopped scratch delta + `restore_ms`.

| Configuration (AWS) | Runs | Failed | Stream bytes | `final_ms` | Stopped scratch delta | `restore_ms` | Pause |
|---|---|---|---|---|---|---|---|
| tmpfs, empty cache (batch `e`) | 20 | 2 (e02 magpie SIGSEGV, e14 Claude Code SIGSEGV) | 128.4-131.4 MB | 50.4-56.6 (median 53.3) | — | 71.0-78.8 | 122.6-131.1 (median 127.5) |
| tmpfs, full cache (343 MiB: 64 × 4 MiB tool results + an 87 MiB transcript-sized file) | 3 | 0 | 864.3-864.6 MB | 72.4-73.0 | — | 75.3-76.0 | 147.7-148.9 |
| scratch, empty cache | 11 | 2 (s06 magpie SIGSEGV and a short read; s09 two short reads, failed on re-check, deviation 8) | 126.7-127.4 MB | 182.4-188.7 (median 186.1) | 122-125 ms, 0 bytes | 71.1-77.8 | 378.5-387.7 (median 381.7) |
| scratch, full cache (same fill; cache, journal and the fake store on scratch) | 3 | 0 | 127.3-127.6 MB | 179.0-185.0 | 123-126 ms, 0 bytes | 73.6-75.7 | 376.4-386.8 |

Round 3 on Hetzner cx33 (scratch layout, empty cache, `results/hetzner3/`): 13 hold-on runs, 0 failed, stream
126.6-129.5 MB, `final_ms` 203.9-289.1 (median 219.8), pause 436.4-596.6 (median 457.5); 12 matched-control runs
(`HOLD=2`), 0 failed, stream 2.2-2.4 MB, `final_ms` 178.3-218.5, pause 387.1-451.6 (median 397.1). These hosts are
slower than the AWS ones; the 2× rule below uses the AWS runs.

**The 2× rule (22.3), stated per basis.** Production path (scratch): every full-cache run against every empty-cache
run (worst case: the slowest full run over the fastest empty run) is 1.01× for `final_ms` (185.0 / 182.4), 1.02× for
the pause (386.8 / 378.5) and 1.01× for the stream bytes (127.6 / 126.7 MB); the bytes moved while stopped are 0 in
both. The 343 MiB + the fake store (737 MB) went over rsync before the pause, while the sandbox ran (live rounds:
736.9 MB vs 17.4 MB empty). **Within 2× on every basis.** tmpfs layout (cache as memory, not production's):
`final_ms` 1.45× the fastest empty run (73.0 / 50.4), the pause 1.21× (148.9 / 122.6), within 2×; the stream bytes
6.7× (864.6 / 128.4 MB), **outside 2×**: a cache held in memory goes into the pre-copy stream (before the pause; the
final round's bytes are not reported by runsc), which is why the cache belongs on scratch (supervisor 9.7). Scratch
costs ~130 ms more `final_ms` than tmpfs in both empty and full runs plus the ~123 ms stopped delta; that is the
scratch path's own cost, independent of the cache.

On every run that did not lose a process: FUSE works on the target (new requests served), the blocked append
returns once the hold is released (caller waited 12.9-14.5 s for the first 1 MiB, all bytes exact), both of Claude
Code's turns end `success` on the target, the transcript equals magpie's copy, `unique` keeps increasing across the
migration (`uniques.json`: every request first read after the migration has a larger `unique` than every one before;
an INTERRUPT carries its target's `unique` + 1). The inotify tail was not tested (deviation 6).

Measured facts the spec did not account for:

- **A checkpoint resends in-flight requests** (verdict 1). When a task waiting on a FUSE reply must stop for the
  checkpoint, the sentry queues `FUSE_INTERRUPT` for the request, forgets it, and restarts the syscall after the
  restore, which sends a **new request with a new `unique`** (`fuse/connection.go waitForResponse`/
  `abandonRequest`: "The restarted syscall should resend it"). In every hold-on run (72 on AWS, 13 on Hetzner in
  round 3) both held requests were resent: the same WRITE at offset 6 with the same 1 MiB again (`mig-e01`: unique
  692 → 740, op ids `…2b4` and `…2e4`, both stored by the fake magpie, which writes at an offset), and the held RMDIR
  again (736 → 756); `uniques.py` now lists each resend (`resends` in `uniques.json`). **The INTERRUPT is not seen
  every time:** the supervisor read both INTERRUPTs right after the restore in 70 of the 72 AWS runs, one in
  `mig-s06` and none in `mig-s09`, which still had both resends (WRITE 692 → 740, RMDIR 736 → 756). In those two runs
  (scratch layout) the spike's worker threads panicked on reads that left no whole request in the journal slot (3
  such reads, `supervisor.log`: "slice index starts at 40 but ends at 0"), the missing INTERRUPTs' count; that the
  short reads were those INTERRUPTs is Claude's inference from the counts (the spike then logged neither the count
  read nor the bytes). Round 3 logs both (`SHORTREAD` in `ops.tsv`, with the journal file's own bytes at the slot),
  the caller's thread id on mutating ops (` pid=` in `ops.tsv`'s detail) and every refused reply write (`REPLYERR`):
  in all 13 hold-on runs (`results/hetzner3/mig-h*`, `mig-t09`, scratch layout) both INTERRUPTs were read, the
  original's reply was refused with ENOENT for both resends, the resend came from the same thread id as the original,
  and no short read occurred (so the short read was not reproduced in 13 runs; it is WP-57's).
  A signal does not do this: `sigresend.sh` (`results/*/sig-*`) interrupts a held 1 MiB append with SIGUSR1, with and
  without `SA_RESTART`: the sentry sends INTERRUPT and then waits for the original reply; one WRITE, no resend.
  A write split into several requests stops after a complete request and returns short, so only the request in
  flight is resent. Correction: supervisor 9.3's resend rule (keyed on the thread and the refused reply, which every
  run showed, not on the INTERRUPT) and magpie 6.6 rule 0.
- **A namespace operation waiting on magpie blocks the whole mount** (verdict 2). The fork's FUSE filesystem is
  kernfs, which takes one filesystem-wide lock for writing in MkdirAt, RmdirAt, UnlinkAt, RenameAt, LinkAt,
  SymlinkAt, MknodAt and OpenAt with `O_CREAT` and holds it across the FUSE call; every path walk takes it for reading.
  While Claude Code's cleanup RMDIR of `projects/-home-sandbox` (non-empty) was held, `stat` of another directory
  waited 3.37-3.41 s in every run where the RMDIR was held (`probe.json`; 0.42 s in e14, whose Claude Code had died),
  exactly until the release; reads through an already open fd took 0.001-0.013 s. Correction: supervisor 9.10.
- **SIGSEGV after the restore** (verdict 4). Hold-on migrations on AWS, batches `e`, `s`, `f`, `g`: 3 of 37 lost a process (e02 and s06: the fake
  magpie, `exits.log` "magpie exited status 139"; e14: Claude Code itself, Bun "panic(main thread): Segmentation
  fault at address 0x0", the turn never ended, `drive-stderr.log`); run 1 lost the mock model once in six. With
  `RESTORE_DEBUG=1` (the target restores with `--debug`; batch `d`, 15 runs) it recurred twice (d07, d11, the fake
  magpie): the sentry log (`debug-faults.txt`) shows the thread that died had its blocking `recvfrom` (syscall 45)
  restarted at the restore ("Restarting syscall 45: not interrupted by handled signal"), received SIGSEGV 4.0-4.7 s
  later, when data arrived for it (the release), and the thread group was killed. The programs differ (the
  spike's Rust/musl binary as magpie and as the mock model, Claude Code on Bun). What was ruled out, on the same
  hosts:

  | Batch | What differs | Runs | Lost a process |
  |---|---|---|---|
  | `e`, `s`, `f`, `g`, `d` | hold on (as above; `d` with the target's debug log) | 52 | 5 (e02, e14, s06, d07, d11) |
  | `p` | hold on, `--systrap-disable-syscall-patching` on both hosts | 20 | 1 (p20, the fake magpie) |
  | `n` | no hold: nothing blocked in the supervisor, no probe | 15 | 0 |
  | `l` | no hold, Claude Code's HOME on the sandbox's `/tmp` (no FUSE in its path) | 15 | 0 |
  | `co-c1`…`c6` | no FUSE traffic; `sandbox/copyout.py` reads a pipe into 8,192 pages with `readv` (only the sentry writes them) and checks every page against a note in its own memory every 0.25 s | 6 | 0; 0 stale pages before or after the restore (about 1 million reads per run) |

  So it is not systrap's syscall patching and not pages the sentry wrote between pre-copy rounds. **It is not
  attributed to the held requests.** It was seen only in hold-on runs (6 of 72 against 0 of 30), but that difference is
  not significant (0 of 30 would happen by chance with probability about 0.07 at the hold-on rate), and the AWS
  controls differed in more than the hold: `n` and `l` had no blocked-append process and no probe, their pre-copy
  stream was 2.3-3.5 MB instead of 126.7-131.4 MB, and their pause about half (`n`: `final_ms` 33.5-37.8, pause
  88.0-98.3 ms; `l`: 26.3-29.7 and 72.5-81.6 ms; hold-on tmpfs 122.6-131.1 ms). Round 3 added the matched control
  `migrate.sh HOLD=2` (the same append process and probe, the fake magpie not holding), alternating with hold-on runs
  on the same two Hetzner hosts (`results/hetzner3/batch-h.log`, `migrations.json`): 0 of 13 hold-on and 0 of 12
  matched-control runs lost a process (at 1 in 12, 0 of 13 happens with probability about 0.32: no information either
  way). It also showed that the stream size follows the hold itself, not the append process: hold-on 126.6-129.5 MB
  (`page_bytes` about 126 MB of non-zero pages), matched control 2.2-2.4 MB; pause 436-597 ms against 387-452 ms. So a
  control that differs only in the hold cannot separate "requests held across the checkpoint" from "a final round
  with about 125 MB more pages"; WP-57's acceptance adds a control that dirties that much memory without a hold.
  It concerns every live migration on the fork, so it belongs to the fork's owner (WP-57), with this reproduction:
  `migrate-batch.sh` with the hold, about one run in twelve on AWS.

### 4. SIGKILL of the supervisor at each journal point — pass (AWS)

`fabric/fuse-gate/killtest.sh` (`results/aws/kill-k1`: journal on tmpfs; `kill-k2`: journal on the host-backed
scratch bind, a 16-slot, 16,908,288-byte sparse file): the supervisor kills itself (`--die-at`) at "read returned",
"magpie call sent", "magpie answered", "reply written", during a 1 MiB append and during a READ (cache dropped, so the
first READ fetches the whole file from magpie). The restart loop starts a new supervisor, which gets the fd from the
keeper, replays the occupied slots in `unique` order and serves on.

- 16 of 16 kills: every caller's syscall returned once with the full count, every file is byte-exact (each 1 MiB
  chunk present once, in order); the slowest caller saw 65.7-85.0 ms (the 50 ms restart sleep of the test loop
  included). Kills after "sent"/"answered" were re-executed with the same op id and magpie answered from its stored
  result; after "reply written" the replayed reply was refused with ENOENT (no waiter), as designed.
- **`unique` never repeats across the 17 supervisor restarts** (`uniques.json`: every request read after a restart
  has a larger `unique` than every request before it; the only repeated `unique`s are the journal replays of the very
  same request). Across a migration: item 3.
- **Keeper only** (`keeper.txt`): the restart loop is held, the supervisor SIGKILLed; no supervisor process, the
  keeper holds the only `/dev/fuse` fd, one mount. A 19-byte append issued then (at …851.475705) had not returned
  2 s later. The restart loop was released at …853.512895; the new supervisor came up with `fresh_mount=false`
  and the stored `bootId`, read that request for the first time (`unique` 22, read at …853.539011) and answered it;
  the caller's write returned 1.97 s after it was issued; still one mount; the file holds `before|during-keeper-only|`.
  So requests queue in the sentry while only the keeper holds the fd and are served, with their original `unique`, by
  the next supervisor.
- The MAP_SHARED journal keeps the request bytes after the process dies, on tmpfs and on the host-backed bind.
- Not covered: the 10.2 step 5 command-path points (the spike has no command loop; WP-23).

### 5. flock and fcntl locks between two processes — pass (round 3)

`sandbox/locks.py`, run by `fabric/fuse-gate/mount-check.sh` (`results/hetzner3/mount-m1/locks.txt`; first seen in
run 1, whose output was not kept): a child holds `flock(LOCK_EX)` / `lockf(F_LOCK)` on a mount file; the parent's
non-blocking attempt fails EAGAIN (11), `F_GETLK` names the child's pid; after the child exits the parent gets the
lock. Identical on `/tmp`. No GETLK/SETLK/SETLKW request reached the server (the sentry keeps the locks; the file
lists every opcode the server received in that run).

### 6. Throughput — pass for the mount's own share and the cold resume; cluster path not measured

`sandbox/bench.py` (one `write` per line, `O_APPEND`, as Claude Code), fake magpie on loopback, magpie latency
injected per mutating call (Hetzner cx33, `results/hetzner/lat-l2`):

| Injected | 1,000 × 4 KiB p50 / p99 | 50 × 1 MiB p50 / p99 | 1 MiB throughput |
|---|---|---|---|
| 0 ms | 0.84 / 3.40 ms | 5.49 / 10.68 ms | 180.7 MB/s |
| 5 ms | 7.55 / 10.33 ms | 12.05 / 15.37 ms | 86.3 MB/s |
| 20 ms | 22.56 / 25.07 ms | 26.64 / 30.23 ms | 39.5 MB/s |

The sentry, FUSE and the supervisor add ≤ 3.4 ms p99 to a 4 KiB append and ≤ 10.7 ms p99 to a 1 MiB append on top of
magpie's own time, so the 200 ms p99 target for a 1 MiB image line leaves magpie's 150 ms plus about 40 ms for the
path. **87 MiB transcript resume from a cold cache** (`fabric/fuse-gate/resume87.sh`): a real session's transcript
grown to 91,723,353 bytes with valid chained lines (`sandbox/grow87.py`: 87 tool_use/tool_result pairs whose
`toolUseResult` carries 1 MiB each), the supervisor restarted with an empty cache, then `claude --resume` with one
turn: **2.47 s on AWS** (`results/aws/r87-aws`; warm cache 2.07 s), 4.15 s on Hetzner cx33 (warm 3.58 s); target
6 s. A plain read of an 87 MiB file through the mount: 0.457 s cold, 0.335 s warm. The sandbox → cluster round trip
(WireGuard, gateway) was not measured (deviation 2).

### 9. Syscall inventory of the real binary — pass, without the tombstone's own syscalls

`runsc --strace` over the whole scripted session of item 2 (`fabric/fuse-gate/strace-inventory.py`,
`results/hetzner/inv/inventory-mount.json`; the same session with `HOME` on tmpfs as the reference,
`results/hetzner/invl/inventory-local.json`). Syscalls Claude Code issued on mount paths: `access chmod close
copy_file_range execve fchmod fstat fsync ftruncate getdents64 inotify_add_watch ioctl mkdir newfstatat openat
pread64 read readlink rename rmdir statx symlink unlink utimensat write`.

- No `mmap` of a mount file, no `link`, no `fallocate`, no xattr calls, no `sendfile`, no `renameat2` flags; no
  ENOSYS anywhere. FUSE opcodes received: INIT GETATTR LOOKUP MKDIR OPENDIR READDIR RELEASEDIR CREATE WRITE SETATTR
  FLUSH RELEASE RENAME OPEN READ LISTXATTR RMDIR FSYNC UNLINK.
- Answers other than success: ENOENT (lookups and opens of absent files), EEXIST (`mkdir -p`), ENOTEMPTY (cleanup
  `rmdir`), and `ioctl(FICLONE)` EOPNOTSUPP on the `backups/.claude.json.backup.*` copy, after which
  `copy_file_range` succeeds (the sentry copies; the server sees WRITEs). The tmpfs reference run gives the same set
  of (syscall, answer) pairs except `rmdir` ENOTEMPTY on the project directory (a cleanup only the mount run reached).
- New in this run: relocation (`rename` of the transcript, its folder and the set-aside `superseded-*`, `statx` and
  `utimensat` on the set-aside), file history (`mkdir`, `openat`, `copy_file_range`, `fchmod`, `ftruncate`, `rmdir`
  ENOTEMPTY under `file-history/`), the model fallback and the mid-line torn tail (only `pread64`/`write`).
- **Missing: Claude Code's tombstone syscalls** (`pwrite64` on a non-append fd, `ftruncate` of the main transcript):
  item 2 did not get Claude Code to tombstone a written line. The `pwrite64`/`ftruncate` in the trace on the
  transcript are `tombstone.py`'s (program `python3`), not Claude Code's.
- Paths 2.1.304 touches that spec 9.1's table does not list (ENOENT-only probes unless noted): `.config.json`,
  `.credentials.json`, `AGENTS.md`, `access-audit`, `bridge-spawn`, `cache/`, `cowork_plugins/`,
  `cowork_settings.json`, `daemon/`, `daemon.json`, `daemon.log*`, `dev-mods/`, `dump-prompts`, `feedback/`,
  `feedback-bundles`, `file-transfers`, `hfi-auth.json`, `image-cache`, `jobs/`, `keybindings.json`, `logs`,
  `mcp-needs-auth-cache.json`, `mcp-skill-archives`, `paste-cache`, `projects/<sid>/{mcp-tasks,proposals,
  remote-agents}/`, `remote-control/`, `scheduled_tasks.json`, `seed-admin`, `shares`, `startup-perf`, `state/`,
  `teams`, `themes`, `traces`, `usage-data/`, `workflows`. With "anything else → local" they are harmless; `todos/`
  stayed empty (TodoWrite keeps its list in the transcript in 2.1.304).

### 10. Latency injection — pass at 50 ms

`sandbox/session-c` (five turns: Read, Glob, then 40 chunks 20 ms apart), token latency = arrival on Claude Code's
stdout minus the mock's send time, token gap = time between arrivals, tool round trip = the mock's tool_use done →
the next request (`sandbox/gaps.py`, `results/hetzner/lat-l2/`):

| Injected per mutating call | Token latency p50 / p99 | Token gap p50 / p99 / max | Tool round trip p50 / p99 | Turn times (ms) |
|---|---|---|---|---|
| 0 ms | 1.3 / 20.2 ms | 20.9 / 39.7 / 40.5 ms | 70.1 / 153.4 ms | 2031, 1118, 1013, 982, 1012 |
| 50 ms | 1.3 / 21.7 ms | 21.0 / 41.0 / 43.1 ms | 77.7 / 218.1 ms | 2118, 1087, 1052, 1098, 1057 |
| 500 ms | 1.2 / 17.2 ms | 21.0 / 37.1 / 38.9 ms | 78.0 / 614.7 ms | 6295, 1628, 1618, 2128, 1620 |
| every call held 30 s from 1 s in | 1.2 / 25.4 ms | 20.9 / 42.9 / 48.5 ms | 82.8 / 156.7 ms | 11881, 11015, 8069, 1066, 1052 |

At 50 ms the p99 token gap is 1.03× the 0 ms run (gate: ≤ 2×): **pass; the 9.4 contingency (non-history paths to
local scope) is not needed for streaming**. (`gaps-50.json`'s `missing: 39` are the previous run's last tokens,
which fall into the 2 s look-back window; they do not enter the figures.) Tokens kept flowing at the mock's pace
during the 30 s hold.

**The result can precede the transcript write.** Under the hold, turns 0 and 1 ended at 11.9 s and 22.9 s while a
transcript append of each was still held (`order-hold.json`: one append pending at each result, landed at 30.9 s,
the release). 2.1.304 waits at most 10 s for its transcript drain before it emits the result: `var FO=1e4` and
"transcript drain still running after ${r}ms; the result goes out without it" in the binary. With
`CLAUDE_CODE_EAGER_FLUSH=1` the same hold gives one 30.9 s turn whose result comes after every append landed
(`order-hold-eager.json`: 0 pending). Supervisor 8.2 already waits for magpie's acknowledgement of every streamed
uuid, not for Claude Code's result; it now says that the result is not that signal.

## Design consequences (folded into the specs on 2026-11-02; to be agreed before WP-22)

1. **INIT replies minor ≥ 28 (7.31).** `FUSE_MAX_PAGES`, and with it 1 MiB writes, needs 7.28; the fork offers 7.31
   (supervisor 2.5, 9.3; DESIGN.md's FUSE row).
2. **Resends after a checkpoint apply once.** Keyed on the calling thread (the header's `pid` is the thread id: Claude
   Code's writes come from several ids of one process) and the original's refused reply, never on INTERRUPT receipt:
   a request from a thread that still has one in flight with an equal key (opcode, node id, argument bytes; WRITE:
   offset and data) waits for it and takes its result; the result of a request whose reply write returned ENOENT
   answers the thread's next request if that one has an equal key; neither makes a second magpie operation; magpie answers a write whose range already holds exactly the written
   bytes with 200 and its current size and version, so a resend that races the original cannot fail
   `offset_mismatch` (supervisor 9.3, 9.9, 22.4; magpie 6.6 rule 0, 6.10; `magpie-api` `OffsetMismatch` doc;
   WP-12, WP-22 acceptance). The `opId`-from-`unique` rule alone does not cover it.
3. **Spec 9.10 is wrong for namespace operations.** Answer every definitive case from the tree without a network
   call (ENOENT, EEXIST, ENOTEMPTY: the supervisor is the only writer), and state the cost: during a magpie outage
   every path lookup on the mount waits behind the first namespace operation that needs magpie (supervisor 9.10).
4. **`bootId` is regenerated on every fresh mount** (a new `/dev/fuse` connection, whose `unique` counter starts
   again), never reused from disk; only a restart that took the fd from the keeper keeps it (supervisor 9.3). The
   spike does this now; its first version read the stored id even on a fresh mount. The spike writes the id with a
   plain `fs::write`; supervisor 9.3 requires an atomic write and a refusal to start when a keeper fd is held but the
   stored id is unreadable (a fresh id there would give the replays new `opId`s, executed a second time by magpie).
9. **A journal read can come back without a whole request after a restore** (verdict 5): the reader checks `len`
   against the count read and never panics on a header (supervisor 9.9); the cause is WP-57's.
5. **`link(2)` is not used by 2.1.304**; the subagent "link fallback" row of 9.4 stays only as a guard, and
   `claude-code-gate` watches for it. FICLONE is answered EOPNOTSUPP by the sentry, `copy_file_range` handled by the
   sentry; no mmap (9.4).
6. **A torn line stays as an unparseable line** (2.1.304 appends `\n` at a mid-line cut); magpie already stores such
   a line whole (magpie 6.3, 6.4); supervisor 9.4's torn-tail row says so.
7. **File history** is written only with `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`, which supervisor 6.2 does not
   set (9.1's row says so); TodoWrite writes nothing under `todos/`.
8. **Claude Code's result is not the transcript's acknowledgement** (supervisor 8.2): it can come up to 10 s before
   the write lands, and without any bound after 10 s.

## Deviations from the package text

1. **Hosts.** Items 1, 3 and 4 (and repeats of 2 and 6) ran on two throwaway `m7i.xlarge` in eu-central-1a, the
   fabric's instance type and region, from Canonical's Ubuntu 24.04 (the base of the fabric's AMI) with the same
   runsc release, flags, bundle shape and image, not on the fabric's AMI in the fabric's account (where and why: the
   task record). Round 3 ran on Hetzner. **What the fabric's AMI has that these hosts did not** (from
   `fabric/host/setup.sh` and `fabric/aws/infra.sh ami`): the kernel of Canonical's image on the AMI's bake date
   (these hosts: 7.0.0-1014-aws on 2026-11-02); sysctls `net.ipv4.ip_forward=1` and `rp_filter=0` (the gate sets only
   `vm.max_map_count=262144`, as the AMI does); systemd-networkd leaving foreign routes alone; sandboxd's unit and the
   `/sbx` cgroup hierarchy with its memory reservation and cap (the gate: `/gate/<name>`, a 4 GiB limit); the shared workspace filesystem on
   `/var/lib/sbx/jfs` as `/workspace` (absent here); the scratch dir under `/var/lib/sbx` on the 40 GB gp3 root (here
   `/var/lib/gate/scratch` on the instance's root disk, same filesystem type: Canonical's ext4); the WireGuard
   interface and the sandbox network sandboxd builds (here a netns with only `lo`). Items 3 and 4 are to be repeated
   (at least one batch) on a fabric-AMI host in the fabric's account by WP-57's acceptance runs and WP-40's fabric
   acceptance. Items 2, 9 and 10 ran on Hetzner cx33/cpx32 runners (the items do not depend on the host; 2 and 6 were
   repeated on AWS with the same results).
2. magpie is a fake inside the sandbox on loopback; the round trip to magpie in the cluster (item 6) is not measured.
   It is added by WP-22/WP-28 against a real magpie.
3. The full-cache migration's 343 MiB is a fill through the mount (`sandbox/fill.py`), not a real conversation's
   cache; in the scratch layout the fake magpie's store also sits on scratch (in production it is remote), which only
   adds to the rsync rounds before the pause.
4. **Argv.** The scripted sessions ran with `-p --dangerously-skip-permissions --model claude-sonnet-4-5` and default
   setting sources, not supervisor 6.1-6.4's argv, environment and managed files (`--permission-prompt-tool stdio`,
   `--setting-sources=`, managed settings). The path and syscall inventory may differ under them, so **the path table
   (item 9) and the 9.1/9.4 findings drawn from it are provisional** (supervisor 9.1 says so) until WP-22's re-run of
   items 2 and 9 with the spec 6 argv (recorded in its WORK-PACKAGES entry).
5. **Tombstone.** Claude Code's own tombstone rewrite was not reproduced (item 2, seven triggers); the replay by
   `tombstone.py` stands in for it and is labelled as such. This is a missing acceptance item, so the package is
   not done: either WP-22's re-run triggers it on the mount, or Deyao records the deferral as a decision (the question
   is in DESIGN 14).
6. The spike serves every path from magpie (no local-scope passthrough, no scope table), has no inotify tail
   (item 3's "the inotify tail continues" is not tested) and no command loop (item 4's 10.2 points).
7. Item 5 and the strace of run 1 were not repeated on AWS (5: locks are kept by the sentry and never reach the
   server, so the host does not matter); item 5 was re-run in round 3 by `mount-check.sh`, whose output is committed.
8. **Fail conditions of the AWS runs.** The AWS batches failed a run on the fakes' exits only; a supervisor, keeper or
   driver exit and a supervisor thread's panic were not checked, and Claude Code's exit only through the turn check.
   Re-checked afterwards against the committed outputs (`supervisor.log`, `drive.json`): no supervisor, keeper or job
   exit in any run; Claude Code exited non-zero only in `mig-e14` (-11, already failed); supervisor threads panicked
   in `mig-s06` (already failed) and `mig-s09` (counted as passed then; failed now). Since round 3 `init.sh` logs every
   exit to `exits.log` and `migrate.sh` fails a run on it, on a supervisor fault and on Claude Code's exit.
