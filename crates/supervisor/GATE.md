# FUSE gate (WP-07): `~/.claude` as an in-sandbox FUSE server

Gate before the supervisor is built beyond a spike (supervisor spec 2 and 22, items 1-6, 9, 10). Run on
2026-11-02 with the spike in `spikes/fuse-gate` (throwaway, its own Cargo workspace) and the host scripts in
`fabric/fuse-gate` (runtime install, bundle, migration, syscall inventory; they live outside this crate because
the source rules keep the runtime's name out of `crates/supervisor`). Raw outputs: `spikes/fuse-gate/results/`.

**Verdict: the gate passes; WP-22 may start, with three design corrections (section "Design consequences")
that change supervisor spec 9.3, 9.5, 9.10 and 16 before they are implemented.** No item was negative in the
sense of WP-07's stop rule (nothing the agent needs failed), but two measured facts contradict the spec text:
a namespace operation waiting on magpie blocks every path lookup on the whole mount, and a checkpoint resends
every request the server has read but not answered.

## Setup

| What | Value |
|---|---|
| Sandbox runtime | runsc from the fork release `precopy-20261021.2` (the tarball and SHA256SUMS check of jarvis3 `chat/sandboxd/host/setup.sh`, the pin of `GV=`), flags exactly sandboxd's `FLAGS` (`--platform=systrap --network=sandbox --overlay2=none --host-uds=none --host-fifo=none --net-raw --allow-packet-socket-write --allow-suid`) |
| Bundle | sandboxd's shape: first process root with the Docker capability set (incl. `CAP_SYS_ADMIN`), `HOME=/home/sandbox`, image rootfs read-only, `/srv` tmpfs (supervisor state, cache, journal, fake magpie store), `/mnt` a host-backed bind (scratch), the spike bound read-only at `/media`, a netns with only `lo` |
| Image | `ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f` (the production pin), digest `sha256:7604ae30…fdc67` |
| Claude Code | the image's `@anthropic-ai/claude-code@2.1.304`, the native `bin/claude.exe` (288,730,480 bytes, Bun-compiled), `-p --input-format stream-json --output-format stream-json --include-partial-messages`, `ANTHROPIC_BASE_URL` = the spike's mock Messages API |
| Hosts | two throwaway Hetzner Cloud cx33 runners (4 vCPU AMD EPYC-Rome, 8 GB; one in hel1, one in fsn1; identical `runsc cpu-features`, 73 features), deleted after the run. No production host was used. |
| Spike | `fuse-gate serve`: FUSE 7.31 server written against the raw protocol (no libfuse), `max_write` 1 MiB, `max_pages` 256, flags `BIG_WRITES|MAX_PAGES|ATOMIC_O_TRUNC`; every path served from the fake magpie (no local scope); write-through with a content cache; node ids persisted in `nodes.log`; the `/dev/fuse` fd held by `fuse-gate keeper` (SCM_RIGHTS); a 16-slot MAP_SHARED request journal read into directly; op ids `bootId-unique-step`; `--die-at POINT:OPCODE:NTH` fault injection. `fuse-gate magpie`: HTTP store with idempotent op ids, `latency_ms` and `hold` knobs. `fuse-gate model`: scripted streaming Messages API that logs each chunk's send time. |

## Results by gate item

### 1. Mount inside a running sandbox; uid 1000 I/O — pass

`/dev/fuse` is present (`crw-rw-rw- 10,229`). The supervisor (root in the sandbox, started by the first process
after the container is up) mounted `/home/sandbox/.claude` with `mount("supervisor", …, "fuse",
MS_NOSUID|MS_NODEV, "fd=3,rootmode=40755,user_id=0,group_id=0,default_permissions,allow_other")` in 51 µs; INIT
offered 7.31 with flags `0x400028` and accepted the reply. uid 1000 created directories and files, wrote, read
back, listed; `stat` shows `1000 1000`, mode as created (umask applied by the sentry), `st_ino` = node id.

### 2. A real Claude Code 2.1.304 session on the mount — pass, tombstone partly

Scripted session (`sandbox/mkscripts.py`, eight turns plus a resume): Bash, Write, Read, Edit, ToolSearch,
TodoWrite, Glob, Grep, a foreground Agent subagent with its own tool call, five failure-injected turns, seven
1 MiB Write tool calls, `/compact`, all with `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1`. Every turn `success`; the
resume turn `success` (`results/st/*.summary.json`).

- **Bytes in magpie equal the bytes Claude Code wrote**: every file read back through the mount equals the
  fake magpie's copy; both transcripts are valid JSONL ending in `\n` (`results/st/report.json`; main transcript
  374,698 bytes, subagent transcript 146,388 bytes).
- **Append**: per append Claude Code opens the file `O_WRONLY|O_APPEND` (no `O_CREAT`), one `write`, closes; the
  server sees WRITE at `offset == size` in order (127 WRITEs, 163 OPENs, 178 RELEASEs, 204 FLUSHes in the session).
- **Compaction rename**: the backstop compaction ran once the transcript reached 15,099,189 bytes:
  CREATE `<sid>.jsonl.compact.tmp.6af3e659` (`O_CREAT|O_TRUNC`), SETATTR size 0, one WRITE of 30,505 bytes,
  FSYNC, RENAME over `<sid>.jsonl` (`results/st/compaction-ops.txt`). The verification reads (3 × 4 KiB windows)
  were served from the cache.
- **Torn tail**: the last byte was cut off through the mount (SETATTR size); on `--resume` Claude Code read the
  tail and its first append started with `\n` (the byte at the torn offset is `b'\n'`, `results/st/byte-at-torn.txt`).
- **Tombstone rewrite**: Claude Code's own trigger was **not reproduced** with the mock. In 2.1.304 a stream that
  fails after a complete content block (overloaded error event, clean close without `message_stop`, TCP reset,
  malformed event, `stop_reason: refusal`) is kept and continued ("Your response above was cut off mid-stream…"),
  not tombstoned; `CLAUDE_STREAM_IDLE_TIMEOUT_MS=3000` with a 20 s stall did not trigger the non-streaming
  fallback either. Tombstones come from `abandonAttempt` on streaming fallback and refusal fallback with a
  fallback model (read from the binary). The removal itself (`performRemoveByUuid`: open `r+`, read the last
  64 KiB, `ftruncate` at the line start, `pwrite` the rest of the tail; else a whole-file rewrite under 50 MiB) was
  replayed byte for byte by `sandbox/tombstone.py` on the live transcript, both paths: whole-file (1,134 bytes
  removed) before the resume, paged (1,138 bytes) after it; Claude Code resumed the rewritten file and the
  result equals magpie's copy.
- **Subagent transcript**: 2.1.304 creates `subagents/agent-<id>.jsonl` directly (CREATE + appends) and its
  `.meta.json` by tmp + rename; it never called `link(2)` (the "link fallback" of spec 9.4 is not exercised by
  this version). It does `symlink("<mount>/…/subagents/agent-<id>.jsonl", "/tmp/claude-1000/…/tasks/<id>.output")`:
  the link lives in `/tmp`, its target on the mount.

### 3. Live migration with a write blocked in the server, Claude Code mid-turn — pass, two corrections

`fabric/fuse-gate/migrate.sh`: Claude Code streams an 8 s answer; the fake magpie holds every mutating call
(`hold`), so Claude Code's transcript append and a direct 1 MiB append are both blocked inside the supervisor;
`runsc checkpoint --migrate-to` (pre-copy, `--migrate-downtime 100ms`) from hel1 to `runsc restore
--migrate-listen` in fsn1; the hold is released on the target.

| Run | Stream bytes | Rounds | Final pause (`final_ms`) | Restore (`restore_ms`) | Both turns | Blocked append |
|---|---|---|---|---|---|---|
| empty cache, no Claude Code | 125,109,444 | 1 | 124.6 | 90.5 | — | completes on target |
| empty cache, Claude Code mid-turn (6 runs) | 125-197 MB | 1-3 | 62.4 / 123.7 / 128.2 / 142.1 / 144.2 / 145.7 | 85.7-99.1 | success in 5 of 6 (see below) | exact |
| full cache: 256 MiB of tool results + an 87 MiB transcript (343 MiB cache, 343 MiB store) | 864,878,652 | 1 | 148.4 | 96.3 | success | exact |

Full cache vs empty: final pause 148.4 ms vs 62.4-145.7 ms (median 135.2 ms: 1.10×; 2.4× the single fastest
run): **within 2× of the empty-cache runs' median**; restore 96.3 ms vs 85.7-99.1 ms. Here the cache and the fake store are tmpfs (sandbox memory, pre-copied as pages);
production keeps the cache on the scratch disk moved by rsync, which this gate did not exercise (deviation 3).

FUSE works on the target (new requests are served), the blocked append returns once the hold is released on
the target (caller waited 17.8-23.9 s, all bytes exact), Claude Code finishes the turn and the next one, the
transcript equals magpie's copy. Measured facts that the spec does not account for:

- **A checkpoint resends in-flight requests.** When a task waiting on a FUSE reply must stop for the checkpoint,
  the sentry queues `FUSE_INTERRUPT` for the request, forgets it, and restarts the syscall, which sends a **new
  request with a new `unique`** (`connection.go waitForResponse`/`abandonRequest`: "The restarted syscall should
  resend it"). Observed on every migration: one INTERRUPT per held request, then the same WRITE again
  (`results/mig-m1/calls-blocked.txt`: the 1 MiB append at offset 6 stored twice, op ids `…2b0` and `…2e4`).
  The fake magpie writes at an offset, so the duplicate is harmless here; magpie's append fast path (offset must
  equal size) would answer the second `offset_mismatch`. A resent CREATE with `O_EXCL`, MKDIR (a `mkdir` lock),
  RENAME or UNLINK would get EEXIST/ENOENT instead of its real result. This happens to any request in flight at
  the final stop, not only held ones. A write split into several requests is safe: a stop after the first
  request returns a short write.
- **A namespace operation waiting on magpie blocks the whole mount.** The fork's FUSE filesystem is kernfs, which
  takes one filesystem-wide lock for writing in MkdirAt, RmdirAt, UnlinkAt, RenameAt, LinkAt, SymlinkAt, MknodAt
  and OpenAt with `O_CREAT`, and holds it across the FUSE call; every path walk (stat, open, readdir of any path
  on the mount) takes it for reading. Measured on the target: while Claude Code's cleanup `rmdir` of
  `projects/-home-sandbox` (non-empty) was held, `stat` of another directory waited 4.35-4.47 s, exactly until the
  release (`results/mig-*/probe.json`); reads through an already open fd (0.003-0.008 s) and appends through open
  fds are not affected. Tasks waiting on that lock show as running in the sandbox's `ps` (95 % CPU) but cost no
  host CPU (sandbox 8.4 % of a CPU idle, 12 % with a held mkdir and a waiting stat).
- One run of six lost the mock model process on the target right after the hold was released (connection
  refused, no stderr); exits are logged since and the four later runs did not repeat it. Not explained; the mock
  is not part of production, but WP-22's soak should watch for an in-sandbox process exiting across a restore.

### 4. SIGKILL of the supervisor at each journal point — pass

`fabric/fuse-gate/killtest.sh`: the supervisor kills itself (`--die-at`) at "read returned", "magpie call sent",
"magpie answered", "reply written", during a 1 MiB append and during a READ (cache dropped, so the first READ
fetches the whole file from magpie). The restart loop starts a new supervisor, which gets the fd from the keeper,
replays the occupied slots in `unique` order and serves on. Run twice: journal on tmpfs and journal on the
host-backed scratch bind (16 slots, 16,908,288-byte sparse file).

- 16 of 16 kills: every caller's syscall returned once with the full count, every file is byte-exact (each 1 MiB
  chunk present once, in order); the slowest caller saw 71.9-123.8 ms (the 50 ms restart sleep of the test loop
  included). Kills after "sent"/"answered" were re-executed with the same op id and magpie answered from its
  stored result (`replayed=200`, no second apply); after "reply written" the replayed reply was refused with
  ENOENT (no waiter), as designed.
- **Keeper only**: with the supervisor killed and the keeper holding the only fd, the connection stays up;
  requests queue and are served by the next supervisor (`results/kill-k1/keeper.txt`).
- The MAP_SHARED journal keeps the request bytes after the process dies, on tmpfs and on the host-backed bind.
- Not covered: the 10.2 step 5 command-path points (the spike has no command loop; WP-23).

### 5. flock and fcntl locks between two processes — pass

`sandbox/locks.py`: a child holds `flock(LOCK_EX)` / `lockf(F_LOCK)` on a mount file; the parent's non-blocking
attempt fails EAGAIN (11), `F_GETLK` names the child's pid; after the child exits the parent gets the lock.
Identical on `/tmp`. No GETLK/SETLK/SETLKW request reached the server (the sentry keeps the locks).

### 6. Throughput — pass for the mount's own share; cluster path not measured

`sandbox/bench.py` (one `write` per line, `O_APPEND`, as Claude Code), fake magpie on loopback, magpie latency
injected per mutating call:

| Injected | 1,000 × 4 KiB p50 / p99 | 50 × 1 MiB p50 / p99 | 1 MiB throughput |
|---|---|---|---|
| 0 ms | 0.78 / 2.31 ms | 5.05 / 10.75 ms | 196.6 MB/s |
| 5 ms | 7.86 / 9.79 ms | 13.16 / 17.23 ms | 78.1 MB/s |
| 20 ms | 23.08 / 25.34 ms | 28.74 / 35.56 ms | 36.1 MB/s |

The sentry, FUSE and the supervisor add ≤ 2.3 ms p99 to a 4 KiB append and ≤ 10.8 ms p99 to a 1 MiB append on top of
magpie's own time, so the 200 ms p99 target for a 1 MiB image line leaves magpie's 150 ms plus about 40 ms for the
path. An 87 MiB file read through the mount: 0.543 s from a cold cache (one whole-file fetch from the fake
magpie) and 0.256 s warm. The sandbox → cluster round trip (WireGuard, gateway) was not measured (deviation 2).

### 9. Syscall inventory of the real binary — pass

`runsc --strace` over the whole scripted session (`fabric/fuse-gate/strace-inventory.py`,
`results/inventory-mount.json`; the same session with `HOME` on tmpfs as the reference,
`results/inventory-local.json`). Syscalls Claude Code issued on mount paths: `access chmod close
copy_file_range execve fchmod fsync ftruncate getdents64 inotify_add_watch ioctl mkdir newfstatat openat pread64
read readlink rename rmdir statx symlink unlink utimensat write`.

- No `mmap` of a mount file, no `link`, no `fallocate`, no xattr calls, no `sendfile`, no `renameat2` flags; no
  ENOSYS anywhere. FUSE opcodes received: INIT GETATTR LOOKUP MKDIR OPENDIR READDIR RELEASEDIR CREATE WRITE
  SETATTR FLUSH RELEASE RENAME OPEN READ LISTXATTR RMDIR FSYNC UNLINK.
- Answers other than success: ENOENT (lookups and opens of absent files), EEXIST (`mkdir -p`, and 4 `O_EXCL`
  opens of the main transcript), ENOTEMPTY (cleanup `rmdir`), and `ioctl(FICLONE)` EOPNOTSUPP on the
  `backups/.claude.json.backup.*` copy, after which `copy_file_range` succeeds (the sentry copies; the server
  sees WRITEs). The reference run on tmpfs gives the same set of (syscall, answer) pairs; the only pair seen only
  on the mount is `rmdir` ENOTEMPTY on the project directory (a cleanup the tmpfs run did not reach).
- `inotify_add_watch` on the mount root succeeds (events come from changes made through this sentry, which is
  every change).
- `utimensat` on the main transcript (SETATTR mtime/atime) after resume, `chmod` of `sessions/`, `ftruncate` of
  `sessions/*.tmp`, `.last-cleanup` and `backups/`.
- Paths 2.1.304 touches that spec 9.1's table does not list (all ENOENT-only probes unless noted): `.config.json`,
  `.credentials.json`, `AGENTS.md`, `access-audit`, `bridge-spawn`, `cache/`, `cowork_plugins/`,
  `cowork_settings.json`, `daemon/`, `daemon.json`, `daemon.log*`, `dev-mods/`, `dump-prompts`, `feedback/`,
  `feedback-bundles`, `file-transfers`, `hfi-auth.json`, `image-cache`, `jobs/` (unlink of `pins.json.tmp.*`),
  `keybindings.json`, `logs`, `mcp-needs-auth-cache.json`, `mcp-skill-archives`, `paste-cache`,
  `projects/<sid>/{mcp-tasks,proposals,remote-agents}/`, `remote-control/`, `scheduled_tasks.json`,
  `seed-admin`, `shares`, `startup-perf`, `state/` (unlink of `settings-review.json`), `teams`, `themes`,
  `traces`, `usage-data/`, `workflows`. With "anything else → local" they are harmless; `todos/` stayed empty
  (TodoWrite keeps its list in the transcript in 2.1.304).

### 10. Latency injection — pass at 50 ms

`sandbox/session-c` (five turns: Read, Glob, then 40 chunks 20 ms apart), token latency = arrival on Claude
Code's stdout minus the mock's send time, token gap = time between arrivals, tool round trip = the mock's
tool_use done → the next request (`sandbox/gaps.py`, `results/lat-l1/`):

| Injected per mutating call | Token latency p50 / p99 | Token gap p50 / p99 / max | Tool round trip p50 / p99 | Turn times (ms) |
|---|---|---|---|---|
| 0 ms | 1.4 / 16.5 ms | 21.1 / 35.5 / 45.4 ms | 82.4 / 121.6 ms | 1835, 1078, 1031, 1028, 1049 |
| 50 ms | 1.4 / 20.8 ms | 21.0 / 39.3 / 41.0 ms | 66.3 / 205.2 ms | 2169, 1071, 1066, 1034, 1034 |
| 500 ms | 1.4 / 22.1 ms | 21.1 / 39.5 / 42.6 ms | 70.6 / 635.1 ms | 6399, 1659, 1627, 2124, 1629 |
| every call held 30 s from 1 s in | 1.4 / 17.9 ms | 21.0 / 30.3 / 44.4 ms | 77.5 / 157.0 ms | 11949, 11068, 7954, 987, 1045 |

At 50 ms the p99 token gap is 1.11× the 0 ms run (gate: ≤ 2×): **pass; the 9.4 contingency (non-history paths to
local scope) is not needed for streaming**. Claude Code 2.1.304's transcript appends do not stall its event loop:
tokens kept flowing at the mock's pace during a 30 s hold; what waits is the end of the turn (the result is
emitted after the transcript write lands) and tool steps that touch the mount.

## Design consequences (input for WP-22; supervisor spec 9.3, 9.5, 9.10, 16)

1. **Deduplicate requests resent after a checkpoint.** After `FUSE_INTERRUPT` for a request the server has
   already acted on, the next request with the same opcode, node id and arguments (and, for WRITE, the same
   offset and bytes) is the restarted syscall: answer it with the original's result instead of a second magpie
   operation (keep the interrupted requests' results until the matching resend or the next generation). In
   addition magpie should treat a write whose range already holds identical bytes as success, so a resend that
   races the original cannot fail `offset_mismatch`. The `opId`-from-`unique` rule of 9.3 alone does not cover it.
2. **Spec 9.10 is wrong for namespace operations.** "Cached reads, stat and readdir keep working" holds only while
   no CREATE/MKDIR/RMDIR/UNLINK/RENAME on the mount waits on magpie. Answer every definitive case from the tree
   without a network call (ENOENT, EEXIST, ENOTEMPTY: the supervisor is the only writer), keep magpie-scope
   namespace operations to the ones that must be durable, and state the cost: during a magpie outage every path
   lookup on the mount waits behind the first pending namespace operation.
3. **`link(2)` is not used by 2.1.304**; the subagent fallback row of 9.4 can go, and `claude-code-gate` should
   watch for it instead. The 9.4 table is otherwise confirmed by the trace above (no mmap, FICLONE answered
   EOPNOTSUPP by the sentry, `copy_file_range` handled by the sentry).

## Deviations from the package text

1. Hosts: two throwaway Hetzner runners instead of "one AWS host of the current fabric" (the hosts that serve
   staff sandboxes were not to be touched); same runsc release, flags, bundle shape and image.
2. magpie is a fake inside the sandbox on loopback; the round trip to magpie in the cluster (item 6) is not
   measured. It is added by WP-22/WP-28 against a real magpie.
3. Supervisor state, content cache and journal were on tmpfs for the migrations (they move as memory pages);
   production puts the cache and journal on the scratch disk moved by rsync (sandboxd's migrate relay), which
   this gate did not run. The host-backed journal was tested for SIGKILL only.
4. The spike serves every path from magpie (no local-scope passthrough, no scope table), has no inotify tail
   (item 3's "the inotify tail continues" is not tested) and no command loop (item 4's 10.2 points).
5. The tombstone rewrite was replayed with Claude Code's algorithm on its live transcript, not triggered by
   Claude Code (item 2).
