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
stood, two faults after a restore were handed to WP-57 (verdicts 4 and 5: a fork bug was found and fixed whose link
to verdict 4 is probable, not proven; verdict 5's mechanism is reproduced, its end-to-end failure was not), and the
tombstone rewrite was not reproduced (a question for Deyao, DESIGN 14):

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
4. **Failure, cause found by WP-57: an in-sandbox process is killed by SIGSEGV after a restore** in 6 of 72 AWS
   migrations with requests held in the supervisor (the fake magpie 5 times, Claude Code once), in 1 of 6 of run 1
   (the mock model) and in 0 of 30 AWS control migrations without a hold. It is counted as a failure of item 3. The
   sentry's debug log puts the fault in a thread whose blocking `recvfrom` was restarted at the restore; it is not
   systrap's syscall patching (it recurred with patching off) and not stale pages written by the sentry (a copy-out
   check found none). Cause and fix: section "WP-57" (the final pre-copy round left freed or zeroed pages with their
   old bytes on the target; fixed in the fork release `precopy-20261102.1`, which on the same Hetzner hosts removed
   the losses that `precopy-20261021.2` showed). That this bug caused these particular SIGSEGVs is probable, not
   traced to a page (see there). Remaining: WP-40's must-pass batch on fabric-AMI hosts in the fabric's account.
5. **Failure, cause found by WP-57: a journal read after a restore with no whole request in it** (item 3): in
   `mig-s06` and `mig-s09` (scratch layout: the journal is a `MAP_SHARED` file on the host-backed scratch dir), 3
   reads of `/dev/fuse` returned while the slot's `len` field was still 0; the spike's worker thread panicked on the
   header and the runs were first counted as passes, because nothing then read the supervisor's log. Re-checked with
   the rules of round 3, `mig-s09` fails as well (7 of the 72 hold-on AWS migrations fail). Cause: the scratch move's
   final delta used rsync's quick check, which cannot see a write through a shared mapping (section "WP-57"; the
   mechanism is reproduced, the end-to-end short read was not: 0 of 24 hold-on runs with the quick check). Fixed in
   the gate's `send-scratch`, and in sandboxd's final delta by WP-19 (aviary `eedfbda`); supervisor 9.9 keeps the reader's check.

**WP-57 (2026-11-02): a fork bug found and fixed, probably verdict 4's cause; verdict 5's mechanism found, its
failure not reproduced** (section "WP-57" below). The fork's final, stopped round of a pre-copy migration did not
resend pages that had been sent earlier and were then freed or found zero, so the target kept their old bytes and
handed them out as fresh memory; fixed in the fork release `precopy-20261102.1`. That this bug caused verdict 4's
SIGSEGVs is Claude's inference, not traced to a page. For verdict 5, rsync's quick check in the scratch move (the
gate's copy of sandboxd's `send_locked`, and sandboxd itself until aviary `eedfbda`) cannot see a write made through a
shared mapping, and the INTERRUPT read during the stop is such a write; the gate's final delta now compares contents.
The end-to-end short read was not reproduced (0 of 24 runs with the quick check). On the same two Hetzner hosts, run
one after the other and not interleaved, `precopy-20261021.2` lost a process in 4 of 50 hold-on migrations (1
SIGSEGV, 3 SIGABRT) and `precopy-20261102.1` in 0 of 150, tmpfs layout only (section "Same hosts, both releases" for
what that comparison does and does not show). For the scratch layout there is no old-release baseline: the new
release had 0 lost processes and 0 short reads in 100 scratch hold-on runs on other hosts with the gate's diagnostic
comparison, and in 60 scratch runs with production's stopped-time path; so the fix's effect is shown for tmpfs only.
The fabric's AMI on AWS is untested and is WP-40's must-pass check, which is also the first scratch-layout evidence
for the fix.

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

## WP-57: the two faults after a restore (2026-11-02)

Hosts: two throwaway Hetzner Cloud cx33 in fsn1 (`runsc cpu-features` identical to round 3's), Ubuntu 24.04, kernel
6.8.0-138-generic, deleted after the run; outputs in `fabric/fuse-gate/results/wp57/` (its README lists every file).
Not run on the fabric's AMI in the fabric's account (no production AWS use in this run): that batch is left to the
operator phase (WP-40).

### The SIGSEGV (verdict 4): the final pre-copy round left freed pages with their old bytes

**Cause (from the fork's code, `pkg/sentry/pgalloc/save_restore.go` `SaveTo`, and measured below).** The final
round, while the sandbox is stopped, sent only the dirty pages that were still committed (non-zero). A page that was
sent with non-zero bytes in a live round and then freed (`munmap`, `MADV_DONTNEED`: the sentry decommits it, which
marks it dirty) or found all zero by the save's scan before the stop was dirty but not committed, so it was not sent
again. The target restores the main memory file from the received file as it is (`LoadOpts.ContentsPresent`: no
page is loaded, nothing is zeroed), so those offsets kept the old bytes while the target's allocator recorded them as
free, and free pages are handed out as zero memory without being zeroed. A process that later mapped fresh memory
(an allocator growing, `calloc` of a fresh mapping, a new buffer for a released request) got another process's or
its own old data where it relied on zeroes: garbage pointers, which on x86-64 are mostly non-canonical, so the fault
is a general protection fault that reports address 0 (the sentry's "fault addr: 0x0", Bun's "Segmentation fault at
address 0x0"; Claude's inference from the signal info, consistent with both logs). The fault comes when the
process next allocates, which in the gate is when the hold is released and the held requests are answered (the 4.0-4.7
s after the restore in `d07` and `d11`).

**Why the hold made it more likely.** The stream now reports what the source sent while stopped
(`final_page_bytes`, `final_zero_bytes` in `receive.json`); the zero ranges are the pages the old release did not
send. In the 200 tmpfs migrations below, the final round's zero ranges were 5.3-9.5 MB (median 7.2 MB) with the
hold, 0.7-3.5 MB (median 1.1 MB) in the matched control (`HOLD=2`) and 1.2-5.9 MB (median 2.5 MB) in the memory
control (`HOLD=3`, 125 MiB rewritten throughout, final round 4.6-89.5 MB of pages): the hold frees the most memory
around the stop, so more freed pages were left stale.

**Reproduction without FUSE** (`fabric/fuse-gate/stale-mig.sh`, `sandbox/stale.py`: a program that maps fresh memory
in mappings of 32 MiB, 2 MiB, 64 KiB and 4 KiB, checks each is all zero, fills and unmaps it, and checks a page it
overwrote with zeroes and a page after `MADV_DONTNEED`; a second process rewrites 128 MiB throughout; tmpfs layout):

| Release | Runs | Runs with memory read back wrong after the restore | What was wrong |
|---|---|---|---|
| `precopy-20261021.2` (the gate's) | 20 | 4 (so02, so06, so12, so17) | 8 fresh mappings (4 KiB, 64 KiB, 2 MiB) not zero, first bytes 15, 16, 72, 82, 133 (old fill bytes and text); one page overwritten with zeroes read back with the old fill byte |
| `precopy-20261102.1` | 40 | 0 | final round's zero ranges 48.1-121.3 MB |

0 of 40 at the old rate (4 of 20) happens by chance with probability 0.8^40 = 0.00013. A confound of these two
batches: during both (`so` and `sn`) two sandboxes left over from killed trial runs, each rewriting 300 MB in a loop,
were still running on the source host (removed before every later batch); they were the same for both releases, but
they loaded the host and so may have changed the race.

**How far the attribution goes.** What is shown: the bug exists in `precopy-20261021.2` (the code above; the
regression test fails on it, below), it hands stale bytes to a process as fresh memory after a restore
(`stale-mig.sh`), and on the same two hosts the gate's own hold-on migrations lose a process on
`precopy-20261021.2` and not on `precopy-20261102.1`, which differs from it by one commit: this fix, its test and
the final round's counts in the receive report (section
"Same hosts, both releases" below). What is not shown: no core, sentry log or fault address of a real failing run was
traced to a page in the ranges the old release left unsent (the old release does not report them, and a general
protection fault reports address 0, not the pointer it dereferenced). So the link from this bug to the gate's
SIGSEGVs and SIGABRTs (`d07`, `d11`, `e02`, `e14`, `s06`, `p20` and the `ob` runs below) is probable, not proven.

**Regression test against the old code.** `TestPrecopyContentsPresentAfterZeroing`, run with `make test
TARGETS=//pkg/sentry/pgalloc:pgalloc_test` on `1635a240b` with `pkg/sentry/pgalloc/save_restore.go` taken from
`b360d82cf` (the fix reverted, the test kept), fails: "zeroed page at 0x2000: destination holds first byte 3, want a
zero page", the same for the decommitted page and the eight freed pages, and "fresh allocation [0x8000, 0x18000) on
the destination: page at slice offset 0x0 is not zero (first byte 101)". With the fix the target passes
(`results/wp57/regress/`, on a throwaway Hetzner ccx33).

**Fix**: the runtime fork's commit `1635a240b` ("Pre-copy: the final save sends dirty pages that are no longer committed as
zeroes"), released as `precopy-20261102.1` by the fork's release workflow (tag `precopy-*` on `main`, the same path as
`precopy-20261021.2`): such pages are sent as zero ranges, which the target punches out of the received file. Its
test `TestPrecopyContentsPresentAfterZeroing` (pgalloc) sends a live round, then zeroes, decommits and frees pages,
saves, restores with `ContentsPresent` and checks those offsets and a fresh allocation are zero.

### The short read (verdict 5): the scratch move's quick check skipped the journal

**Cause.** When the checkpoint stops the sandbox, the sentry abandons the held requests and queues their
`FUSE_INTERRUPT`s (item 3). A worker thread blocked in `read(2)` of `/dev/fuse` can take one before it stops: the
sentry copies the 56-byte request into the slot through the journal's `MAP_SHARED` mapping, on the source, while the
sandbox is stopping, and the read's result (56) is saved with the thread. The scratch move then copies the scratch
dir with rsync's quick check (size and mtime). A write through a shared mapping to a page that is already dirty does
not change the file's mtime (the kernel updates it only when a clean page is first written), and the journal's pages
were dirty from the worker zeroing `len` before each read, so the final delta skipped the journal: `final_rsync` was
0 files and 0 bytes in `mig-s06` and `mig-s09` (as in every scratch run), and the target's journal kept `len` 0 where
the restored thread's read had returned 56. `fabric/fuse-gate/mmap-rsync.sh` shows the mechanism on a gate host
(`results/wp57/mmap-rsync.txt`): after a second write through the mapping the mtime is unchanged, the second rsync
transfers 0 files and the copy holds `len` 0 against the source's 56; with a writeback between the two writes the
mtime changes and the file is copied. Inside a sandbox (`fabric/fuse-gate/mapcount-mig.sh`, `sandbox/mapcount.py`: a
process writes a counter through a `MAP_SHARED` mapping of a scratch file and checks it every millisecond; scratch
layout, `gate.sh send-scratch`): with sandboxd's quick check (`FINAL_QUICK=1`) all 6 runs lost writes (each of the 16
pages read back an older counter on the target, e.g. 3920 where 4002 was written; `files_differing_after_final`:
`./mc/counter`); with the checksummed final delta 0 of 6. In the gate's own hold-on runs with the quick check
(`mig-q*`, `mig-r*`, 24 runs) the INTERRUPT race was not hit again: 0 short reads; `q04` stopped at the comparison
(below): the comparison found a differing file and a `set -e` bug in it ended `send-scratch` before `SBXDONE`, so the
target never restored (fixed; which file differed was not recorded). **So the end-to-end short read was not
reproduced in WP-57** (0 of 24 hold-on runs with the quick check, against 2 of 72 on AWS); what is reproduced is the
mechanism (`mmap-rsync.sh`, `mapcount-mig.sh`), and the 0 short reads of the `x` batch below come from the gate's
checksummed final delta plus its comparison, not from a production path.

This is not the fork's and not the INTERRUPT's alone: every scratch file written through a shared mapping after the
last live round is copied in an older state (a request read into a slot after it, an application's mapped file such
as SQLite's `-shm`), and sandboxd's `sync_scratch` (`crates/sandboxd/src/storage.rs`, `last = true`) has the same
quick check. **Fix in the gate:** `gate.sh send-scratch`'s final delta runs with `--checksum` and then compares every
scratch file on both hosts while both are stopped (`files_differing_after_final`, `compare_ms` in `send.json`;
`migrate.sh` fails a run with a difference). **Done in sandboxd (WP-19, aviary eedfbda):** the same `--checksum` on the final delta, tested through sandboxd's API
on two hosts (`crates/sandboxd/tests/real-host/`: with the quick check 3 of 3 migrations lost the counter's writes,
with `--checksum` 0 of 10; stopped time 15.5-15.8 s with 4800 MiB of scratch in use against 129-137 ms quick,
about 3.2 ms per MiB; until the owner of hermit 13 narrows the checksum, hermit moves no sandbox with more than
`migration.maxScratchMib` (512) in use). As written for sandboxd's owner before that: the same `--checksum` on the final delta
(it reads the whole scratch on both hosts while stopped: here 105-191 ms for 17 MB against 100-187 ms with the
quick check; it grows with the scratch's size, which this run did not measure beyond 17 MB), or the spec's fallback for the journal
(supervisor 9.9: read into private memory and `pwrite` the slot), which covers the journal but not applications'
mapped files.

### Acceptance batches on `precopy-20261102.1`

`migrate-batch.sh` with `HOLDS="1 2 1 3"` (hold-on, matched control, hold-on, memory control on the same two hosts,
alternating), every run's outputs kept (`results/wp57/mig-w*`, `mig-x*`, `batch-w.log`, `batch-x.log`,
`migrations.json`):

| Layout | Configuration | Runs | Failed | Lost a process | Short reads | Final round: pages / zero ranges | Pause (ms) |
|---|---|---|---|---|---|---|---|
| tmpfs | hold on | 100 | 0 | 0 | 0 | 0.1-0.3 MB / 5.3-9.5 MB | 164.0-278.7 (median 188.5) |
| tmpfs | matched control `HOLD=2` | 50 | 0 | 0 | 0 | 0-0.2 MB / 0.7-3.5 MB | 106.6-161.2 (median 131.4) |
| tmpfs | memory control `HOLD=3` | 50 | 0 | 0 | 0 | 4.6-89.5 MB / 1.2-5.9 MB | 128.7-259.4 (median 171.6) |
| scratch | hold on | 100 | 0 | 0 | 0 | 0.07-0.4 MB / 5.4-9.3 MB | 1024.9-2009.2 (median 1105.9) |
| scratch | matched control `HOLD=2` | 50 | 0 | 0 | 0 | 0-0.2 MB / 0-3.1 MB | 1010.2-1812.5 (median 1048.2) |
| scratch | memory control `HOLD=3` | 50 | 0 | 0 | 0 | 3.6-88.8 MB / 1.0-6.9 MB | 1038.8-1808.2 (median 1120.6) |

The scratch pauses include the diagnostic comparison of every scratch file on both hosts while stopped (`compare_ms`,
mean 331 ms) and the checksummed final delta (105-191 ms, mean 119 ms, for these 17 MB of scratch; the quick-check
final deltas of `mig-q*` and `mig-r*` took 100-187 ms); without the comparison they are about 330 ms shorter. Every
scratch run's comparison found no differing file. The checksummed final delta transferred a file in 8 of the 100
hold-on runs (`x019`, `x073`, `x117`, `x121`, `x127`, `x169`, `x171`, `x199`) and in none of the 100 controls.

**Scratch with production's stopped-time path** (batch `nc`, `results/wp57/samehost/`; the same-host pair below, on
`precopy-20261102.1`): `COMPARE=0 HOLD=1 migrate-batch.sh … nc 60 scratch`, the checksummed final delta and no
comparison, so the stopped time is the final delta alone, as in sandboxd. 60 hold-on runs, 0 failed, 0 lost
processes, 0 short reads (the supervisor's log checked in every run). Final delta 107-241 ms (median 112.5);
checkpoint end to `SBXDONE` 141-276 ms (median 153); pause 468.8-750.9 ms (median 515.9), against round 3's
436.4-596.6 ms with the quick check. The checksummed delta moved one file (16,908,288 bytes) in 5 of the 60 runs
(`nc02`, `nc24`, `nc33`, `nc44`, `nc57`). With no comparison these runs cannot tell whether a file still differed;
they check only that nothing failed.

For the 100-per-layout count, the scratch layout's 100 are the `x` batch, which ran with the gate's diagnostic
comparison; the production-path scratch runs (`nc`) are 60, not 100. These batches alone do not show that the fix cured anything: they ran on hosts where the old release was not
measured, and every arm passed, so the controls (`HOLD=2`, `HOLD=3`) separate nothing here either (a control only
says something against a hold-on arm that fails). The AWS rate (6 of 72) does not carry over to other hosts, so it
gives no probability for these 0-in-100s. The same-host batches below supply the old-release rate.

### Same hosts, both releases

Two further throwaway Hetzner cx33, in nbg1 (`runsc cpu-features` identical to the fsn1 hosts above and to round
3's), the same `migrate-batch.sh`, tmpfs layout, first on `precopy-20261021.2` and then, after `host-setup.sh` moved
both hosts to `precopy-20261102.1`, on the new release. **The batches ran one after another, not interleaved**: `ob`
(2026-11-02, first migration 20:34:23 UTC, last 21:13:24), then the release change, then `nb` (first migration
21:14:10), `nc` and `nd` (last migration 22:57:05). **`ob` was
started as a 200-run batch and stopped at 100; that stop was chosen during the batch, after its first 36 runs, when
3 of its losses (ob005, ob015, ob023) had already been seen** (ob051 came later). The reason was the plan, not the
data: a second host pair for the new release could not be created (Hetzner's shared-core limit for the project), so
the new release had to follow on the same pair. Still, a stop chosen with the failures in view is a data-dependent
stop and can bias the old release's rate upwards (`results/wp57/samehost/`: `mig-ob*`, `mig-nb*`, `mig-nd*`,
their `batch-*.log` and `run-*.txt`, `migrations.json`):

| Release | Batch | Configuration | Runs | Lost a process |
|---|---|---|---|---|
| `precopy-20261021.2` | `ob` (`HOLDS="1 2 1 3"`) | hold on | 50 | 4 (ob005 the fake magpie SIGSEGV, `exits.log` "status 139"; ob015, ob023, ob051 the fake magpie SIGABRT, "status 134", Rust's "fatal runtime error: current thread handle already set during thread spawn") |
| | | matched control `HOLD=2` | 25 | 0 |
| | | memory control `HOLD=3` | 25 | 0 |
| `precopy-20261102.1` | `nb` (`HOLDS="1 2 1 3"`) | hold on | 50 | 0 |
| | | matched control `HOLD=2` | 25 | 0 |
| | | memory control `HOLD=3` | 25 | 0 |
| `precopy-20261102.1` | `nd` (`HOLD=1`) | hold on | 100 | 0 |

The old release loses a process on these hosts in 4 of 50 hold-on runs and in none of the 50 controls; the new
release in 0 of 150 hold-on runs on the same hosts (0 of 100 more on the fsn1 hosts above). 4 of 50 against 0
of 150, if every run were equally likely to fail, would come out this lopsided by chance with probability 0.0036
(one-sided Fisher exact test: C(50,4)/C(200,4)). That premise does not hold for this design: the releases did not
alternate, so anything that changed over time on these hosts (a neighbour's load on the shared cx33 cores, a host's
state after 100 migrations) is mixed in with the release; the stop above was not fixed in advance; and the 150 pool
`nb` (`HOLDS="1 2 1 3"`, the same configuration as `ob`) with `nd` (`HOLD=1` only, hold-on runs back to back). Against
`nb` alone, the matching configuration, 4 of 50 against 0 of 50 gives C(50,4)/C(100,4) = 0.059. So the figure is
indicative, not a test result. The gate's fail rule counts the SIGABRTs and the SIGSEGV
alike as an in-sandbox process exit; that the SIGABRT is the same fault (the fake magpie's Rust runtime finding its
thread-spawn bookkeeping in memory that should have been zero) is Claude's inference from the message. The controls' 0 of 50 on the old release is consistent with
the AWS controls (0 of 30), but with only 4 failures among 50 hold-on runs it separates nothing: 4 of 50 against 0
of 25 comes out this way by chance with probability C(50,4)/C(75,4) = 0.19 for each control, so whether the held
requests themselves or the larger final round raise the rate is not measured.

**Still untested: the fabric's AMI on AWS**, where the gate measured the fault first, **and the fix's effect in the
scratch layout** (no old-release scratch batch ran on the same hosts as a new-release one). WP-40 runs it as a
must-pass check (WORK-PACKAGES WP-40), designed against the faults of the comparison above: two fabric-AMI hosts,
both layouts, the run count fixed before starting (100 hold-on runs per release per layout), the two releases
alternating run by run on the same host pair (`precopy-20261021.2`, `precopy-20261102.1`, `precopy-20261021.2`, ...;
both binaries installed, the runtime switched before each run) or run at once on two host pairs. It passes only if the
old release loses a process in at least one of its runs and the new release in none of its 100. If the old release
also shows 0, the fix is recorded as not shown on the fabric AMI and escalated to Deyao, not counted as a pass. The
short read is checked separately, because its fix is in the scratch move (the gate's `send-scratch`, sandboxd since
`eedfbda`), not in the fork, so the old-versus-new release comparison cannot tell it apart: on the new release, the
mapped-counter migration (`mapcount-mig.sh`, or `mig.sh` in `crates/sandboxd/tests/real-host/`) with the quick-check
final delta against the checksummed one (the quick check losing writes, the checksum none).

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
   against the count read and never panics on a header (supervisor 9.9). The cause (section "WP-57") is the scratch
   move's quick-check final delta; sandboxd's final delta compares contents once WP-19 (split c) changes it.
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

## WP-22: the re-run on the real build (2026-11-02)

The real `supervisor` and `supervisor-keeper` (musl release builds; the keeper is PID 1 of the sandbox and starts the
supervisor, which mounts `~/.claude` with `fuse::FuseFs`) against `fuse-rig`'s world (fake hermit and fake magpie
behind the reference gateway, inside the sandbox on loopback), with the gate's runtime flags and bundle shape
(`fabric/fuse-gate/gate.sh` `RIG=1`, scripts in `fabric/fuse-gate/rig/`). Two throwaway Hetzner ccx23 (AMD
EPYC-Milan), runsc `precopy-20261102.1` (WP-57's fix release). Raw outputs: `fabric/fuse-gate/results/wp22/`.

| Item | Result |
|---|---|
| 1 | pass (`rmount-m1/item1.txt`): PID 1 is `supervisor-keeper`; `/dev/fuse` `crw-rw-rw- 10, 229`; the mount `fuse rw,nosuid,nodev,fd=11,rootmode=40755,user_id=0,group_id=0,default_permissions,allow_other`; uid 1000 created `projects/d1/d2`, wrote and appended `f.txt` and listed it (`1000:1000`, modes as created); magpie holds `hello\nmore\n`; a local-scope file (`sessions/s.txt`) is on `/var/lib/supervisor/local` and not in magpie; `settings.json` is refused (EACCES) |
| 3 (the resend rule, the migration acceptance of WP-22) | pass, **160 of 160 migrations**: 80 from one host to the other (`rig-batch-X.log`) and 80 onto the same host (`rig-batch-L.log`), each with one WRITE (1 MiB, `O_APPEND`), CREATE `O_EXCL`, MKDIR or RENAME held in magpie across the checkpoint, both layouts. In every run the caller's syscall returned the original's success once (the write's full count), magpie saw one `opId` for the operation (every call of the route after the setup) and holds exactly the expected bytes or names, the supervisor never restarted (one `fuse connection`), nothing panicked, no process exited, and in the scratch layout no file differed after the final delta. In every run the restarted syscall's request arrived while the original was still held (`resend of a request still in flight: waiting for its result`, 160 lines); in one (`rig-X03-t-rename`) the original's reply was refused with ENOENT and kept before the waiting resend took the result, which then dropped the kept entry. Total migration 191-1,541 ms between hosts |
| 4 | pass (`kill-k1`: journal on the scratch bind; `kill-k2`: tmpfs): the fault-injection build SIGKILLed itself at read-returned, call-sent, call-answered and reply-written of a WRITE, MKDIR, CREATE and RENAME, 16 cases each; the keeper restarted it, the new supervisor took the fd back and replayed the journal; every syscall returned success once, magpie applied each operation under one `opId` and holds the expected state |
| 5 | pass (`rmount-m1/locks.txt`): flock and fcntl locks between two uid 1000 processes behave as on `/tmp` on a magpie-scope file and a local-scope file |
| 2, 6, 9, 10 | re-run with Claude Code launched by the real supervisor: "The Claude Code items" below |

Deviations: magpie is the fake in the sandbox (as in the gate); the hosts are Hetzner, not a fabric-AMI host; the
cross-host runs go one way (the target's CPU has `erms` and `fsrm` on top of the source's, so the reverse restore is
not expected to work); the 80 loopback runs restore on the same host from a second runsc root (`GATE_SIDE=b`), so
they move no host files except the scratch layout's rsync into another directory.

### The Claude Code items (2, 3 with Claude Code mid-turn, 6, 9, 10) on the real build

Claude Code 2.1.304 launched by the real supervisor: the turns arrive as magpie `start_turn` commands
(`rig/rigsession.py` through fuse-rig's admin port), the supervisor starts Claude Code through the keeper with the
argv, environment and managed files of supervisor 6.1-6.4, and reports the turn states back to fake magpie. The model
is the gate's scripted mock (`fuse-gate model`, now also answering `GET /v1/aviary/models`) behind the gateway's
`model` name, the MCP server is `fake-tools` behind `tools`; both reached by Claude Code through the proxy that the
descriptor's `env` names. Claude Code is the image's own binary at `/opt/claude-code/bin/claude` (`rig/host-rig.sh`
links it there), `/etc/claude-code` a tmpfs the supervisor writes. One throwaway AWS `m7i.xlarge` in eu-central-1
(Intel Xeon Platinum 8488C, Ubuntu 24.04, kernel 7.0.0-1014-aws), runsc `precopy-20261102.1`, image
`ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`, scratch layout (cache, journal and state on the host-backed scratch
dir), deleted after the run. Raw outputs: `fabric/fuse-gate/results/wp22/{rsess,rtomb,rlat,rmig}-*`.

**What Claude Code ran with** (`rsess-s1/claude-process.txt`, `managed.txt`): uid 1000, cwd `/workspace`, argv
exactly 6.1's (`--output-format stream-json --verbose --input-format stream-json --permission-prompt-tool stdio
--permission-mode bypassPermissions --setting-sources= --disallowedTools AskUserQuestion,EnterPlanMode,ExitPlanMode
--session-id <conversation id>`, then `--resume <id>` after a restart), the environment of 6.2 (the fixed table, then
the descriptor's `env` with the proxy URL and the session's own additions), `/etc/claude-code/{CLAUDE.md,
managed-settings.json, managed-mcp.json}` root-owned 0644 with 6.4's content; Claude Code reported the MCP server
`tools` as `connected` (`source: enterprise`) and called `mcp__tools__echo` through the gateway.

| Item | Result |
|---|---|
| 2 | **pass except the tombstone, as in the gate** (`rsess-s1`): 11 of 11 turns `completed` (session a: tools with Bash, Write, Read, Edit, ToolSearch, TodoWrite, Glob, Grep, a foreground subagent and an MCP call; five failure-injected turns; seven 1 MiB Writes; `/compact` with `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1` from the descriptor's env; then two resumes in new processes). Every magpie-scope file equals magpie's copy (main transcript 384,214 bytes, subagent transcript 156,367 bytes, its `.meta.json`); the other 10 files are in the local scope. **Compaction**: at 15,118,708 bytes a CREATE of `<id>.jsonl.compact.tmp.*`, a truncate to 0, the write, then a RENAME over the main transcript (`report.json` `truncates`, `renames`). **Subagent**: `subagents/agent-<id>.jsonl` created directly, its `.meta.json` by tmp + rename. **Torn tail, final byte** (Claude Code killed while idle, the last `\n` cut, the next `start_turn` resumes in a new process): the first write starts at the cut with `\n`. **Torn tail, mid-line** (a 111,565-byte line cut 100 bytes before its end): one `\n` written at the cut (a 1-byte cc write at offset 267,766), then the new lines; the partial line stays as the one unparseable line. **Tombstone: not reproduced** (`rtomb-t1`, `rtomb-t2`): with the refusal fallback armed through the descriptor's env (`CLAUDE_CODE_MODEL_CAPABILITIES`, the catch-all route) Claude Code fell back (`model_refusal_fallback` to `claude-opus-4-8`, one `retracted_message_uuids`), both when the refusals came first (t1) and after a written tool call (t2, new mock directive `truncafter`); the retracted message had never been written, the written `tool_use` and `tool_result` stayed: 0 writes below a transcript's end, 0 truncates. The open item stands (supervisor 22 "Open items" 3; DESIGN 14, Deyao's question 2) |
| 3, Claude Code mid-turn | **pass, 9 of 9 migrations** (`rig/rig-migcc.sh`, `rmig-e4`, `rmig-m1`…`m5` empty cache, `rmig-f1`…`f3` with the content cache filled through the mount first, 64 × 4 MiB files and an 87 MiB file: 267,646,009 bytes in the cache, the supervisor's 256 MiB cap; all onto the same host from a second runsc root, `LOOPBACK=1`, scratch layout moved sandboxd's way with the checksummed final delta behind `sandboxd migrate-relay`). Claude Code was streaming an 8 s answer (80 chunks 100 ms apart) while fake magpie held every cc write, so a direct 1 MiB `O_APPEND` and Claude Code's own transcript appends were blocked in the supervisor across the move; the hold was released on the target. In every run the turn ended `completed` on the target (8.6-17.9 s), Claude Code's stdout continued after the move, the blocked write returned its full count once and magpie holds it once, every magpie-scope file equals magpie's copy, one supervisor connection, no panic, no short read, no in-sandbox process exit, no scratch file differing after the final delta; each run logged one resend still in flight. Empty cache: stream 175.8-176.9 MB, the target's `final_ms` 530-835 ms, `restore_ms` 119.6-122.2 ms, the source stopped until `SBXDONE` 407-717 ms (two runs re-sent the 64 MiB journal in the final delta, which the checksum found changed). Full cache: the cache went over rsync while the sandbox ran (live round 335.6 MB, 0 bytes in the stopped delta), but the stream was 1,046.7-1,051.4 MB and `final_ms` 869-1,945 ms: **the 2× rule is not shown by this rig**, since fake magpie runs inside the sandbox and keeps every filled byte (and every call body) in its memory, which moves in the pre-copy stream; production's magpie is outside the sandbox. The gate's spike kept its fake store on scratch, and its full-cache runs are the evidence for the 2× rule (within 2× on every basis in the scratch layout, above) |
| 6 | pass for the mount's own share and the cold resume; the cluster path is not in this rig (no cluster in wave 1). `bench.py` through the real mount, fake magpie on loopback, `rlat-l4` and `rlat-l5` (two runs): 1,000 × 4 KiB p50 / p99 0.83-0.85 / 3.6-3.8 ms at 0 ms injected, 7.6 / 8.7-8.9 ms at 5 ms, 22.8 / 24.1-24.2 ms at 20 ms; 50 × 1 MiB 6.4-6.5 / 15.5-16.4 ms (153 MB/s) at 0 ms, 13.0 / 15.0-15.8 ms at 5 ms, 28.2-28.5 / 30.9 ms at 20 ms. So the supervisor, FUSE and the sentry add ≤ 3.8 ms p99 to a 4 KiB append and ≤ 16.4 ms p99 to a 1 MiB append on top of magpie's own time (the gate's spike: 3.4 and 10.7 ms). **87 MiB resume from a cold cache** through the real driver: the transcript grown to 91,993,640 bytes (`grow87.py`), Claude Code killed, the supervisor's content cache emptied and the supervisor killed (the keeper restarted it), then a `start_turn` with resume: the turn took 1,513-1,515 ms (warm cache 1,932-1,957 ms); the cold supervisor fetched the transcript with one whole-file `cc/file` (`rlat-l5/calls-cc_file.json`); target 6 s |
| 9 | **pass, without the tombstone's own syscalls** (`rsess-s2`, the whole item 2 session under `runsc --strace`, `inventory-mount.json`): Claude Code's syscalls on mount paths are the gate's 25 exactly (`access chmod close copy_file_range execve fchmod fstat fsync ftruncate getdents64 inotify_add_watch ioctl mkdir newfstatat openat pread64 read readlink rename rmdir statx symlink unlink utimensat write`); answers other than success: ENOENT, EEXIST, ENOTEMPTY and `ioctl(FICLONE)` EOPNOTSUPP on `backups/.claude.json.backup.*` (local scope; `copy_file_range` follows); no ENOSYS. The 9.1/9.4 findings drawn from the gate's inventory hold under 6.1-6.4 |
| 10 | **pass at 50 ms** (`rlat-l4`, `rlat-l5`; `rig/msgs.py`). Under 6.1's argv (no `--include-partial-messages`) Claude Code's stdout carries whole messages, so the measure is the message latency: the arrival on Claude Code's stdout (`cc-tap`, a stdout tap bound over `/opt/claude-code/bin`) of the assistant message minus the mock's send time of its last chunk. p99 3.2-3.8 ms at 0 ms injected per cc write, 3.2-5.2 ms at 50 ms (at most 1.63× the 0 ms run), 3.0-3.3 ms at 500 ms, 3.3-7.9 ms with every cc write held 30 s; tool round trip p99 66.6-68.9 / 40.9-59.8 / 39.8-50.7 ms. Turn times (five turns, 40 chunks 20 ms apart each): about 1.0 s at 0 and 50 ms, 1.42-1.66 s at 500 ms; under the 30 s hold 0.94-0.96, 11.0, 10.9, 8.2 s and 0.95-0.96 s: Claude Code's 10 s transcript-drain cap, as in the gate. The 9.4 contingency is not needed |

Deviations: magpie is the fake inside the sandbox (as in the gate), so magpie's `409 transcript_pending` and the
cluster round trip are not exercised; the host is AWS but not a fabric-AMI host, in Deyao's own account (as the
gate's AWS hosts); Claude Code is the image's 2.1.304 linked at `CLAUDE_BIN` and `/etc/claude-code` a tmpfs, standing
in for WP-24's image; in runs with `cc-tap` (all but the strace run) Claude Code's `argv[0]` is the real binary's path
because the tap execs it; the session's relocation (`set_cwd`) and `--fallback-model` of the gate are not in 6.1-7, so
they were not run. The first latency runs lost fuse-rig's world to the supervisor's memory guard (supervisor 5: it
counts every process in the sandbox against the descriptor's 1 GiB, and fake magpie keeps every byte written in
memory); the rig now sets the descriptor's `memoryBytes` (`RIG_SANDBOX_MEMORY`) for those runs.

## WP-22 review 2 (2026-11-03)

**Which build each piece of real-build evidence covers.** Items 1, 3 (the 160 resend migrations), 4 (`kill-k1`,
`kill-k2`) and 5 ran on `61023e3`; the Claude Code items (2, 3 mid-turn, 6, 9, 10) on `0a06093`. Neither includes
`797df2ce` and `31056c96` (`fuse::seen` on every `.jsonl` write; the end-of-turn uuid wait of 8.2, which item 10's
turn times under the 30 s hold depend on) nor this review's FUSE changes (`106166f3`). Item 4 was re-run on a build
of `106166f3` with `a36f76cf`'s `fuse request` log in the fault-injection build (`kill-k3`, below); items 2, 3, 6, 9 and 10 have not been re-run on current main.

**Item 4 on `106166f3`** (`results/wp22/kill-k3`, scratch layout; one throwaway Hetzner ccx23, runsc
`precopy-20261102.1`, image `ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`): **pass, 19 of 19 cases plus the plain kill.** The fault-injection build SIGKILLed itself at read-returned, call-sent, call-answered and reply-written of a WRITE (1 MiB, `O_APPEND`), MKDIR, CREATE `O_EXCL` and RENAME, and at read-returned and reply-written of a READ (a 1 MiB file read whole); the keeper restarted it, the next supervisor took the fd back and replayed the journal (19 `journal replay` lines). Every syscall returned success once (the READ: the file's 1,048,576 bytes and sha256), magpie applied each operation under one `opId` and holds the expected state. A plain SIGKILL (no fault point) while the 1 MiB write was held in magpie: one new supervisor, one replay, the write stored once (`kill-k3` case 19, and `kill-k4`, run again with the script's corrected release order: in `kill-k3` the rig waited for the mount before releasing the hold and the hold was released by hand, since a new supervisor replays the held write before it serves any new request, 9.9). `rig/rig-uniques.py` over `supervisor.log` (the test build logs every request it reads) and every route's calls: 38 supervisor instances on one connection read 933 requests, no `unique` twice, each instance only above every earlier one's, every replay of a `unique` an earlier instance read; 157 `opId`s, none naming two operations (`uniques.json`). The `unique`-across-a-migration half of 22 item 4 is not in this run: `rig-uniques.py` is ready for `rig-mig.sh`'s `supervisor.log` with `RIG_FI=1`, not yet run.

**Changed by the review** (supervisor 9.3-9.9; each with a unit test in `fuse::tests`):
- A start never runs on an unloaded tree: `Core::start` waits through magpie's unavailability for `cc/tree` and fails
  the start on a definitive answer (the mount is retried on the keeper's connection, `FuseFs::handed`), so the replay
  never answers ENOENT to an operation magpie applied and no node id is dropped against an empty tree.
- An `O_APPEND` handle's WRITE and the repair's suffix writes carry `append` (magpie 6.10): a size magpie does not
  have is `offset_mismatch` and the repair (or the fork path: EIO, the agent stopped), never an overwrite.
- `lease_lost` on a repair write runs the lease loop like a request's (`after_call`); the new lease is set before the
  tree reload so the reload's repair writes carry it, and requests answer EIO until the reload ends.
- Only the answers 9.5 items 5-7 name are definitive. **Recorded reading of 9.5**: a 500, another 5xx, an unknown code
  (409 `conversation_migrating`) and an undecodable body are in neither of 9.5's lists; they are retried under the
  general-unavailability and stuck-op rule (EIO and `cc.write_stuck` after 10 min while magpie answers others),
  because failing a transcript append on one transient answer is the log-and-drop outcome item 4 ends.
- Whole-file fetches (9.7) are single-flight per node and stream in the background; READs of ranges that arrived are
  answered from the staging file while the rest streams; the 30 s bound applies to the answer and to each next chunk,
  not to the whole body; a retry resumes by range while the file is still at the fetched version (after one of the
  supervisor's own writes it restarts the READ's path instead: the READ falls back to the range from magpie).
  Writes, truncates and renames magpie acknowledges while a fetch streams are applied to the fetched copy (at most
  64 MiB of them), so the main transcript's prefetch leaves it cached at the current version.
- Local-scope replays (9.9) whose effect the killed instance already made answer success (since review 3: only when
  the slot's done mark says so, below).
- A read that leaves no whole request in its slot answers EIO to the `unique` a copy of the header names (since
  review 3: only a `unique` newer than the slot's last, the header being zeroed before each read) and counts
  `fuse.short_read`; a read error other than the connection's end is logged and retried, not a worker's silent exit.
- The thread's next identical request (a second `mkdir` of a held lock) was taken for a resend of the one just
  answered when it arrived before that request left the in-flight list; the resend lock now covers the reply.
- RENAME2 follows the resend rule; an INTERRUPT between a backoff's interrupt check and its wait is not missed.

**Deviations recorded here** (from supervisor 9.5 item 6 and 9.7):
1. The repair's `opId`s are fresh v4 UUIDs not recorded in `run.json` first. A crash during a repair repeats it at the
   next tree load: magpie answers a repeated write whose range already holds those bytes with success (rule 0) and a
   write at its size as an append, so nothing is written twice.
2. `resync: true` on a lease is logged and needs no separate path: the reconcile that re-sends every suffix magpie
   lacks runs at every tree load, a superset of magpie 6.12's resync.
3. `tree.json` is not written: every start loads the tree from magpie, so nothing would read it. Its consequence is
   deviation 4 (review 3).

**Open, not done by this package** (supervisor 22 items 2, 3 and 6; WP-22's acceptance):
- (a) Item 2's tombstone rewrite is still not reproduced, and DESIGN 14 question 2 has no decision in DECISIONS.md:
  deviation 5 above stands, and WP-22 is not done until Deyao records the deferral or a run reproduces it.
- (b) Item 6's sandbox-to-magpie-in-cluster round trip (1 MiB write p99 <= 200 ms) is not measured: only loopback to
  the in-sandbox fake exists. It needs a magpie in the cluster and an AWS sandbox host (WP-40's fabric).
- (c) Item 3's full-cache rule (final pause <= 2x the empty cache's) is not shown on the real build: the rig's fake
  magpie lives in the sandbox and rides the pre-copy stream. It needs magpie outside the sandbox (as (b)).
- Items 2, 3, 6, 9 and 10 have not been re-run on current main (above).

## WP-22 review 3 (2026-11-03)

**Changed by the review** (each with a unit test in `fuse::tests`, `fuse::journal` or `fuse::remote`):
- **Short reads** (9.9). `Journal::free` zeroes the whole 40-byte header (`len` first) and the slot's done mark, after
  each reply and before each read, so a read that leaves the slot untouched can no longer show the header of the
  request that used the slot before. `lost_header` takes only a `unique` newer than the last one the slot held. When
  neither the mapping nor the file names one, nothing is answered: the request is logged (`fuse.request_lost`) and
  counted (`Core::lost_requests`), since a reply to an older `unique` is refused or reaches another request. Its caller
  waits for ever; that is the WP-57 case, whose fix is in the scratch move (9.9). Test
  `a_short_read_answers_only_the_request_it_names` (one slot; a read that copies nothing gets no reply and sends no
  EIO to the slot's previous `unique`; a header without its `len` gets EIO under its own `unique`). The test fails
  on the old `free`/`lost_header`.
- **Repair candidates** (9.5 item 6). A candidate's cache file is discarded only when magpie's bytes disagree
  (`Resent::Forked`), magpie has no such file, or magpie holds at least as many bytes. A failed read of magpie's
  window or the cached copy, or a repair write that ends in EDQUOT, stuck, `lease_lost` or a lease change, keeps
  the file for the next tree load. If some suffix writes were stored first, the file is renamed above magpie's new
  version, so the next reconcile still finds it newer. Test `a_repair_that_fails_without_a_mismatch_keeps_the_cached_copy`
  covers 507 `quota_exceeded`: the candidate is still on disk and the next tree load repairs from it. It also covers
  409 `lease_lost`: the lease loop's reload repairs from it.
- **Resend expiry** (9.3). A kept interrupted result expires `KEPT_RESULT_TTL` (30 s) after it was kept, restored from
  the journal, or the server `resumed` (the resend follows the restore, however long the pause). A much later
  identical request from a reused thread id is then executed, not answered with the old result. Test
  `a_kept_result_survives_a_path_walk_and_expires`: a GETATTR before the resend keeps the result. After the TTL, an
  identical `mkdir` of a lock another thread removed and took again gets EEXIST.
- **Streamed fetches** (9.5, 9.7). They take permits from their own pool (`MAGPIE_STREAMS` = 4 of the 16), so a
  slow-trickling fetch never holds a write's permit (test `streamed_fetches_never_hold_the_calls_permits`: 16 fetches
  held for ever, a write still runs). An attempt that advanced the fetch resets the stuck clock and the backoff, so a
  fetch that keeps advancing between dropped connections is not answered EIO 10 min after its first failure (test
  `a_streamed_fetch_that_keeps_advancing_is_not_stuck`).
- **Local replays** (9.9). A local MKDIR, CREATE, SYMLINK, UNLINK, RMDIR or RENAME writes its slot's done mark (the
  `unique`, in an 8-byte-per-slot area after the kept results) right after the change and before the reply. A
  replay answers success on EEXIST/ENOENT only when the mark names its `unique`; otherwise it answers the error.
  So an unlink of a missing file stays ENOENT, a rename whose `from` is missing stays ENOENT even when `to`
  existed, and a `mkdir` lock another process took meanwhile stays EEXIST (test
  `a_local_replay_that_was_never_applicable_keeps_its_error`). **The limit now**: a kill between the change and the
  mark (two adjacent instructions) answers the error for a change that was the request's own.
- **Prefetch** (9.7). `prefetch_main` runs after every successful start and does nothing while the main
  transcript's entry is current. So a first start retried on the keeper's connection, and a restart whose reconcile
  dropped the entry, prefetch it (test `the_main_transcript_is_prefetched_after_a_retried_start`).
- **opIds in the replay test.** `journal_replay_at_each_fault_point` asserts that each request names exactly one
  `opId` at magpie. After a kill past call-sent, the killed instance sent that `opId` and the replay sent no other.
  So a replay with a fresh `opId`, which magpie's rule 0 would hide for identical bytes, fails the test.

**Deviations recorded here** (for Deyao to accept):
4. **A supervisor restart during a magpie or gateway outage serves nothing until magpie answers** (9.10's
   "Supervisor crash" row assumes magpie answers). Requests queue in the sentry while magpie is unreachable: local
   scope, cached READs, LOOKUP and GETATTR, and the journal's replays. Once magpie answers, the replays complete and
   the new requests are served. Test `a_restart_during_an_outage_waits_for_magpie_then_completes`: a local MKDIR
   killed at read-returned and a new GETATTR both stay unanswered for 0.8 s while `cc/tree` answers 503; both
   succeed once it answers. This does not come from deviation 3. The start sequence of 4.3, which the spec applies
   to every start, takes the lease from magpie at step 5 (`magpie::take_lease`, retried until magpie grants it,
   `start.rs`) before the mount at step 6. So a restart during an outage would block before the mount even with a
   persisted tree. Keeping the mount up through such a restart would need a start that mounts before it has a lease,
   a change to 4.3's order (and magpie-scope replays still wait for the lease, since every state change carries
   `Aviary-Lease`). A supervisor that keeps running through an outage behaves as 9.10 says.
5. **A kept interrupted result is not dropped by the thread's non-mutating requests** (9.3 says "any other request
   from that thread drops the entry"). A restarted syscall can walk its path (LOOKUP, GETATTR) before the sentry
   resends it. Dropping the entry there would execute the resend a second time: EEXIST for a `mkdir` lock the
   original took, so the lock would be held with no owner. The entry is dropped by the thread's next mutating
   request, and expires after `KEPT_RESULT_TTL` (above).

**Item 4's rig and the stuck rule.** In `kill-k3` case 19 (`plain-19.json`), the plain SIGKILL during the held 1 MiB
write, the write took 1,210 s (31 `magpie attempt timed out`) and still succeeded. The 10-min stuck rule did not fire
because nothing called `Remote::record_ok` while the write was held: the rig's held replay ran with no successful
magpie call beside it. This is also the real supervisor's behaviour at a start, not only the rig's. The replay runs
inside the mount (4.3 step 6), and the heartbeat that records successes (`cc::heartbeat_ok`, the `heartbeat_loop`
in `cc/run.rs`) starts at step 7. So a replayed operation that magpie holds or refuses at a start, while magpie
answers everything else, blocks the mount with no `cc.write_stuck` after 10 min. Item 4 on the real build therefore
did not exercise the heartbeat and stuck-rule interplay, and that interplay at a start is **open**: it needs a call
that records magpie's health during the start (before step 7) or a start that serves before its replays end. In
`kill-k3` the hold was released by hand (the rig waited for the mount before releasing it); `kill-k4` re-ran only
that one case, with the corrected release order.

**Still open** (unchanged by review 3; WP-22 stays not done): (a)-(c) above; items 2, 3, 6, 9 and 10 have not been
re-run on current main; the `unique`-across-a-migration half of item 4 (`rig-uniques.py` on `rig-mig.sh` with
`RIG_FI=1`) is not run; the stuck rule during a start's replay (above); the migration acceptance waits for WP-57.
