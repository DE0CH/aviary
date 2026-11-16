# supervisor: implementation spec

Status: design for implementers, 2026-11-02, revised after review round 1 (section 26 says where each point landed,
section 27 which were not taken and why). Authoritative inputs: `magpie-target-design.md` (Deyao, 2026-11-02) and
the refactor brief's decisions. Sources of today's behaviour: `inventory.json` (the 147 features whose target names
the supervisor; coverage table in section 25), gap reports 0-3, and the other specs (`spec-magpie.md`,
`spec-pigeon.md`, `spec-hermit.md`, `spec-cutover.md`, `spec-platform.md`). Platform's conventions apply throughout:
camelCase JSON bodies, lower_snake_case HTTP error codes, `Aviary-*` headers, every route under `/v1/` (platform 3.4-3.6,
3.10). Feature references are `A<area>.F<feature>` (inventory indices, the same notation the other specs use).

The supervisor is one long-running process per sandbox that owns the sandbox from the inside. It runs Claude Code
detached, presents `~/.claude` as plain files through FUSE backed by magpie (a write returns once magpie stored it),
turns Claude Code's stream-json output into turn reports for magpie, executes magpie's commands (start a turn,
interrupt, stop tasks, kill, answer an ask, make a title), hands out per-key secrets by asking hermit, and answers
hermit's lifecycle calls (health, prepare for pause/snapshot/migrate, resumed, release). Token-level live frames are
specified (8.3) but ship in a later supervisor release, once pigeon renders them. It is written in Rust, lives in the
public repo DE0CH/aviary, talks only REST, and reaches everything at the fixed endpoint names `magpie`, `hermit`,
`model` and `tools` through the sandbox gateway. The same build runs in every deployment; nothing in it knows about
YYC, JuiceFS, AWS, OpenRouter, Fly or gVisor beyond the runtime facts it can observe in its own sandbox (section 5.4).

Contents
1. Boundaries
2. Feasibility: FUSE under gVisor (checked against the source)
3. Crate layout, binaries and what changes in the sandbox image
4. Process model: keeper, supervisor, Claude Code, dockerd
5. Memory and OOM protection
6. Launching Claude Code: argv, environment, managed files, tool and settings policy
7. The stream-json driver: messages, control requests, the run state machine
8. How turn events reach magpie: transcript writes for history, stream-json for turn state (live frames deferred)
9. The `~/.claude` filesystem (FUSE backed by magpie)
10. magpie's commands
11. Reports to magpie
12. Uploads: materialising files and building the prompt
13. The SendUserFile hook (sent-file snapshots)
14. Titles
15. hermit: what the supervisor consumes and the lifecycle API it serves
16. Live migration, pause and snapshot (gVisor checkpoint/restore)
17. Network: egress use and the fixed endpoint names
18. What replaces today's in-sandbox agent on :7070
19. Failure modes
20. Limits and constants
21. Logging, metrics, versioning
22. Tests and acceptance
23. What the other specs must provide
24. Decisions taken in this spec
25. Coverage table
26. Review round 1: where each point landed
27. Rejected review points

---

## 1. Boundaries

| The supervisor does | Who else is involved |
|---|---|
| Is the sandbox's first process (via its keeper), mounts `~/.claude`, writes the managed Claude Code files, starts dockerd on demand | hermit builds the sandbox (image, bundle, `/workspace`, network) and starts the keeper |
| Runs Claude Code as uid 1000, detached from every network connection, survives its own restarts | — |
| Executes magpie's command queue (long-poll) and reports turn state, tasks, asks, heartbeats (live frames in a later release, 8.3) | magpie owns turns, admission, events, seq |
| Stores every byte Claude Code writes under its conversation's part of `~/.claude` in magpie before the write returns | magpie (section 6 of its spec) |
| Materialises uploads into `/workspace/uploads`, builds the user message | magpie stores uploads and makes model copies |
| Takes sent-file snapshots for the SendUserFile hook | magpie stores them |
| Makes the title model call | magpie decides when; hermit owns the title instructions; `model` serves the call |
| Reports activity (busy/held/idle) to hermit every 15 s, answers hermit's lifecycle calls, refuses a pause or an unforced release while busy (`409 sandbox_busy`) | hermit owns lifecycle policy (idle release, lifetime, packing, migration) and honours the refusal (15.5) |
| Hands out secrets per key by asking hermit at the moment a process asks | hermit owns grants; YYC grants none |
| Protects itself and the sandbox against memory exhaustion | hermit sets the sandbox's memory |

What the supervisor never does: talk to pigeon, read or write Postgres or the bucket, hold model or tool
credentials, decide who may use the deployment, read `/workspace` on behalf of the files panel (hermit serves live
workspace reads to magpie, magpie spec 11.3), run a shell for the apps (Paseo terminals stay unsupported, pigeon spec
7.5.10), or hold any deployment-specific setting. Every limit and timeout is a constant in `limits.rs`.

One sandbox serves exactly one conversation (hermit gives one sandbox per conversation), so one supervisor, one lease
and one Claude Code process belong to one conversation.

---

## 2. Feasibility: FUSE under gVisor (checked against the source)

Checked on 2026-11-02 against google/gvisor master and the exact fork tag YYC runs, DE0CH/gvisor
`precopy-20261021.2` (`setup.sh` installs it; `runsc.rs` FLAGS):

1. **FUSE is always on in runsc; no flag is needed.** `runsc/boot/vfs.go` registers the `fuse` filesystem type with
   `AllowUserMount: true` and calls `fuse.Register(vfsObj)` unconditionally (fork tag lines 129 and 170). There is no
   `--fuse` flag any more in `runsc/config/flags.go` (master and the tag). Today's runsc flags (`--platform=systrap
   --network=sandbox --overlay2=none --host-uds=none --host-fifo=none --net-raw --allow-packet-socket-write
   --allow-suid`) need no change.
2. **`/dev/fuse` exists in every container.** `fuse.Register` registers the misc device with `Pathname: "fuse"`,
   mode 0666, and runsc's `dev` filesystem creates every registered device at mount. The bundle's `/dev` tmpfs entry is
   rewritten by runsc to its own `dev` filesystem (`vfs.go`: `case "/dev": m.Type = dev.Name`), so `/dev/fuse` is
   present without a bundle change.
3. **Mounting needs CAP_SYS_ADMIN inside the sandbox**, which the sandbox's root has (bundle capabilities include
   `CAP_SYS_ADMIN`, `runsc.rs` DOCKER_CAPS). The supervisor runs as that root. Accepted mount options are exactly `fd`,
   `user_id`, `group_id`, `rootmode` (all mandatory), `max_read`, `default_permissions`, `allow_other`; anything else
   is EINVAL (`fusefs.go parseOptions`). The supervisor mounts with `mount("supervisor", "/home/sandbox/.claude",
   "fuse", MS_NOSUID|MS_NODEV, "fd=<n>,rootmode=40755,user_id=0,group_id=0,default_permissions,allow_other")`.
4. **Checkpoint/restore keeps an in-sandbox FUSE server working; a host-side server would not.**
   `fuse/save_restore.go`: a connection whose server is outside the sandbox (`hostTransport`) is aborted at save
   ("can't be checkpointed"; requests then fail ENOTCONN); a connection on an in-sandbox `/dev/fuse` DeviceFD is saved
   with its queues and restored (`afterLoad` recreates the channels as a `deviceConn`). The supervisor is a process
   inside the sandbox, so the server, its memory, its pending requests and the kernel-side connection are all in the
   same checkpoint. This is why the FUSE server must be the in-sandbox supervisor and never a host daemon.
5. **Protocol level and opcodes.** The fork's FUSE client offers protocol **7.31** in INIT (measured by the WP-07
   gate, `crates/supervisor/GATE.md` item 1: INIT 7.31, flags `0x400028`); the supervisor's reply must name minor
   **≥ 28**, because `FUSE_MAX_PAGES` (and with it 1 MiB writes, item 6) exists only from 7.28; it replies 7.31. It sends LOOKUP, GETATTR, SETATTR, OPEN, CREATE, READ,
   WRITE, FLUSH, FSYNC, RELEASE, OPENDIR, READDIR, RELEASEDIR, MKDIR, RMDIR, UNLINK, RENAME (no RENAME2, so no
   NOREPLACE/EXCHANGE flags), LINK, SYMLINK, READLINK, MKNOD, STATFS, ACCESS, xattr ops, FALLOCATE, INTERRUPT. It does
   not support POSIX/flock locks through FUSE, READDIRPLUS, writeback-cache invalidation, or mmap of FUSE files
   (`ConfigureMMap` returns ENOSYS). It keeps no page cache for FUSE files: every `read()` reaches the server.
6. **Write sizes.** `max_write` is taken from the INIT reply; `max_pages` is capped at 256 (`fuseMaxMaxPages`), so the
   effective maximum per WRITE request is 1 MiB. Larger `write()` calls are split by the sentry into sequential
   requests, each waiting for its reply.
7. **O_APPEND is computed by the sentry** from its cached inode size (`regular_file.go`: `if O_APPEND { offset =
   inode.size }`) and sent as an ordinary WRITE at that offset; writes to one inode are serialised by the inode's
   `attrMu`. The supervisor therefore sees appends as writes at `offset == size`, in order, never concurrently for one
   file.
8. **The restore's open-file size/mtime check** (SANDBOX-VM.md "Live migration", A8.F30) applies to gofer (host)
   files. FUSE files are not host files; their attributes come from the restored supervisor's memory and are stable by
   construction.

Consequences built into this design: the supervisor (not a host daemon) serves FUSE; it never needs locks forwarded
(Claude Code's locks are `mkdir`-as-lock directories plus in-sentry advisory locks, section 9.6); it answers mmap-free
access only; it negotiates 1 MiB writes; and it must survive its own restart without losing requests the sentry has
already handed it (section 9.9).

What remains to be proven by test on the fork (section 22, gate before implementation goes beyond a spike): mounting
from inside a running sandbox; an open, written FUSE file across `runsc checkpoint --migrate-to` / `restore` with a
request blocked in the server; the connection staying up while only the keeper holds the `/dev/fuse` fd; a request
read into the memory-mapped journal surviving a SIGKILL of the supervisor right after `read(2)` returns (9.9); flock
and POSIX locks on FUSE files working locally between two processes; the syscalls the real Claude Code binary issues
against the mount (it is a Bun-compiled native executable, not Node, 9.4); and the measured write and read throughput.

---

## 3. Crate layout, binaries and what changes in the sandbox image

```
aviary/
  crates/
    supervisor-api/           # serde types of the :7070 API (surface `supervisor-control`) and the local socket API
                              # (hermit's sandbox routes are typed in `hermit-api`, magpie's in `magpie-api`)
    supervisor/
      src/bin/supervisor.rs   # the supervisor (tokio, 2 worker threads) and `supervisor hook …` / `sandbox-secret`
      src/bin/keeper.rs       # supervisor-keeper: PID 1, no async runtime, fixed buffers, no heap after start
      src/limits.rs           # every constant in section 20, nothing else configurable
      src/keeper/             # keeper protocol client (spawn, fd store, stdin relay, exits)
      src/fuse/               # /dev/fuse protocol (no libfuse): init, dispatch, request journal, node table
        proto.rs  dispatch.rs  journal.rs  nodes.rs  scope.rs  local.rs  remote.rs  cache.rs  errno.rs
      src/cc/                 # Claude Code: argv/env, managed files, stream-json reader/writer, run state machine,
        launch.rs  managed.rs  stream.rs  control.rs  run.rs  tasks.rs  prompt.rs   (live.rs: the later release, 8.3)
      src/magpie.rs           # client of magpie-api (/v1/sandbox/**), retries, lease
      src/hermit.rs           # client of hermit-api's sandbox surface (SandboxSelf, ActivityReport); :7070 server
      src/model.rs            # title call
      src/sent_files.rs       # hook socket handler (fd passing), snapshot upload
      src/uploads.rs          # blobs into /workspace/uploads
      src/memguard.rs         # user-space OOM guard
      src/docker.rs           # dockerd on demand
      src/state.rs            # persisted run state (/var/lib/supervisor/state)
      tests/                  # integration tests with a fake magpie, a fake hermit, a fake `claude`
```

Nothing of the supervisor is deployed on its own: its binaries go into the sandbox image (`images/sandbox`). CI is
aviary's one `ci.yml` plus `nightly.yml` (soak), and the binaries ship in the one `aviary-<sha12>` release (platform
R16, R17).

Libraries: tokio, axum 0.8 (the :7070 server and the local socket), hyper/reqwest with rustls (outbound through the
HTTP proxy), serde/serde_json, sha2, uuid, time, nix (mount, setpriv, signals, SCM_RIGHTS), tracing (JSON).
The FUSE layer speaks the kernel protocol directly over `/dev/fuse` (about 1,500 lines; the `fuse-backend-rs` or
`fuser` crates assume libfuse-style mounting and multi-threaded readers that do not fit the journal of 9.9). Both
binaries are static (`x86_64-unknown-linux-musl`), stripped, and released with SHA256SUMS.

Sandbox image (built by hermit's YYC implementation, today DE0CH/jarvis3 `chat/sandbox/Dockerfile`; the image stays
hermit's, A5.F0):
- Add `/usr/local/bin/supervisor`, `/usr/local/bin/supervisor-keeper`, and the symlink
  `/usr/local/bin/sandbox-secret -> supervisor` (the binary dispatches on argv[0]), root 0755.
- Install Claude Code root-owned outside every user-writable PATH entry: `npm install --prefix /opt/claude-code -g
  @anthropic-ai/claude-code@2.1.304`, executed as `/opt/claude-code/bin/claude` by absolute path (fixes A11.F13: the
  bare `claude` was resolved on a PATH whose first entries uid 1000 can write). In 2.1.304 that path is the package's
  `bin/claude.exe`, a Bun-compiled native executable (checked with `npm pack`), not Node's `cli.js`; section 9.4 and the
  gate (22) treat it as such.
- `ENTRYPOINT ["/usr/local/bin/supervisor-keeper"]`, no CMD, no tini: the keeper is PID 1 and reaps by design
  (platform R12). hermit's bundle args become `["/usr/local/bin/supervisor-keeper"]` (today `[tini, --, node,
  /opt/agent/sbx-init.mjs]`, A5.F32).
- Remove `/opt/agent` (agent.mjs, sbx-init.mjs, send-file-hook.mjs, e2b-template.mjs), tini, `PORT=7070` and the
  `/run/sbx-docker` socket path; the docker wrapper calls `/run/supervisor/docker.sock` (section 4.6).
- `/home/sandbox/.claude` stays an empty directory in the image (the mount point). hermit's bundle no longer
  bind-mounts anything there (today `users/<key>/claude` from JuiceFS, A8.F1).
- `/var/lib/supervisor` is created root 0700 (it lives on the scratch disk with the rest of the overlay upper).
- The image smoke test (A20.F12) gains the supervisor checks of section 22.

---

## 4. Process model: keeper, supervisor, Claude Code, dockerd

### 4.1 The process tree

```
PID 1   supervisor-keeper            root, oom_score_adj -1000, mlockall, fixed memory (<= 4 MiB RSS)
 ├─ supervisor                       root, oom_score_adj -1000, RSS <= 64 MiB (soak-tested)
 ├─ claude  (generation g)           uid/gid 1000, own session and process group, oom_score_adj 100,
 │   └─ tool processes …             children of Claude Code (Bash, python, MCP stdio servers if any)
 ├─ supervisor hook send-user-file   short-lived, uid 1000, a child of Claude Code (shown here for clarity)
 └─ dockerd (on demand)              root, oom_score_adj 0, started through the keeper
```

The keeper is the parent of everything the supervisor starts, so a supervisor restart never orphans or kills Claude
Code. The keeper is deliberately tiny: no async runtime, no allocation after start, one thread, `poll(2)` over a fixed
set of fds. It cannot be restarted (it is PID 1; if it dies the sandbox ends, which is the runtime's normal "sandbox
died" path).

### 4.2 The keeper

Duties, in order at start:
1. `mlockall(MCL_CURRENT|MCL_FUTURE)`, write `-1000` to `/proc/self/oom_score_adj`, mount `/run/supervisor` (tmpfs,
   mode 0755, size 1 MiB) if not mounted, create `boot_id` (16 random hex) in `/run/supervisor/boot_id` unless present
   (a restored sandbox keeps it: tmpfs is in the checkpoint).
2. Open the control socket `/run/supervisor/keeper.sock` (SOCK_SEQPACKET, root 0600).
3. Start the supervisor (`/usr/local/bin/supervisor run`) with the keeper's own environment (hermit's sandbox env).
4. Loop: reap every child with `waitpid(-1, WNOHANG)`; record exits; restart the supervisor when it exits, after
   0.1, 0.2, 0.5, 1, 2, 5, 5, … seconds (reset to 0.1 s after 60 s of uptime); forward SIGTERM/SIGINT to the supervisor;
   on SIGTERM to PID 1 (the runtime is stopping the sandbox) wait up to 20 s for the supervisor's graceful exit, then
   SIGKILL every process and exit 0.

Keeper protocol (one SEQPACKET message per request, little-endian length-prefixed fields, max 64 KiB per packet;
the supervisor is the only client):

| Request | Fields | Answer |
|---|---|---|
| `SPAWN` | `name` (`claude`, `dockerd`, `exec`), argv, env, cwd, uid, gid, `setsid`, `oom_score_adj`, `cgroup` path, stdout path, stderr path, `stdin: pipe\|null` | `pid` (and the stdin pipe stays with the keeper) |
| `STDIN` | `pid`, `msg_id` (16 bytes), total length, then continuation packets with the bytes | `OK msg_id` after the last byte is written to the pipe; `ALREADY msg_id` (nothing written) when that `msg_id` is among the last 16 the keeper fully wrote to this child; an incomplete message (the supervisor died mid-send) is discarded, never half-written |
| `CLOSE_STDIN` | `pid` | `OK` |
| `SIGNAL` | `pid` or `-pgid`, signal | `OK` |
| `PUT_FD` | `name` (`fuse`), one fd as SCM_RIGHTS | `OK` (the keeper keeps a dup) |
| `GET_FD` | `name` | the fd as SCM_RIGHTS, or `NONE` |
| `EXITS` | `after` (exit counter) | `[{n, pid, name, code\|null, signal\|null, at}]`, the last 64 exits |
| `STATUS` | | `{boot_id, supervisor_restarts, started_at, children:[{pid,name}]}` |

The stdin relay copies through one fixed 64 KiB buffer per child, writing to a non-blocking pipe as Claude Code
reads; a message is acknowledged only when fully written, so a supervisor crash can never leave a partial NDJSON line
in Claude Code's stdin (the reason the keeper, not the supervisor, owns the pipe). The keeper also remembers the
`msg_id`s of the last 16 messages it fully wrote per child (a fixed ring, 256 bytes), so a message re-sent by a
restarted supervisor is written at most once (10.2: a crash between the write and the record of it can never make
Claude Code answer the same prompt twice).

### 4.3 The supervisor's start (every start, first or after a restart)

1. `oom_score_adj -1000`; read `boot_id`; read `/var/lib/supervisor/state/run.json` if present (a restart).
2. Read the sandbox token from `HERMIT_SANDBOX_TOKEN` (hermit 5.1; the only identity the supervisor is given) and
   remove it from the environment every child gets. Read the sandbox descriptor `GET http://hermit/v1/sandbox/self`
   (`SandboxSelf`, section 15.1), retried with backoff (0.25 s doubling to 2 s) until it answers. It gives the
   conversation id, limits, the managed CLAUDE.md, the title instructions, the environment for the agent's processes
   and the control-token hashes for :7070.
3. Start the :7070 server (health answers `ok: true, ready: false` until step 9) and the local socket
   `/run/supervisor/agent.sock` (section 13, 15.4).
4. First start only (no `run.json`): add `127.0.0.1 localhost`, `::1 localhost` and `127.0.1.1 sandbox` to
   `/etc/hosts` where missing (keeping every line hermit's implementation wrote, such as the four fixed names, hermit
   5.1 item 3), write `/etc/apt/apt.conf.d/80sandbox-proxy` from `SandboxSelf.env`'s `http_proxy`/`https_proxy` when
   present (quotes and newlines stripped, mode 0644), and the managed Claude Code files (section 6.4). These were
   sbx-init's (A5.F7, A5.F8).
5. Take the lease: `POST http://magpie/v1/sandbox/lease` → `{leaseId, epoch, conversationId, providerSessionId,
   mainTranscript, resync}` (magpie 6.9); check `conversationId` equals the descriptor's. `409 not_current_sandbox`
   (hermit names another sandbox as the conversation's live one) or `409 conversation_migrating`: stay unready, keep
   :7070 and activity reports running, and try again every 30 s, without exiting (so two live sandboxes for one
   conversation never steal the lease from each other in a restart loop). `503`: retry with backoff.
6. Load the tree `GET /v1/sandbox/cc/tree`; reconcile the local cache (9.7); load or create the node table (9.8). If
   the lease said `resync: true` (magpie 6.12), run the resync of 9.5 item 6 before step 8.
7. FUSE: on the first start open `/dev/fuse`, mount (section 2 item 3), `PUT_FD fuse` to the keeper. On a restart
   `GET_FD fuse` from the keeper, then replay the request journal (9.9) before reading new requests. (FUSE requests
   that arrive while the supervisor waits for a lease in step 5 queue in the sentry; no agent runs then.)
8. Claude Code: on a restart, re-attach to the live generation from `run.json` (its pid, stdout offset, run state,
   pending inputs, tasks), checking with `EXITS` whether it died meanwhile. On a first start, if the lease carries a
   `providerSessionId`, prefetch the main transcript into the cache, then pre-spawn Claude Code (section 7.2) so the
   first turn does not pay for start-up and resume.
9. Start the command poll from `run.json.cmd_after` (0 on a first start), the heartbeat loop, the activity loop and
   the memory guard; health answers `ready: true`.

Supervisor exit codes: 0 on a requested shutdown (release), 70 on an internal error (the keeper restarts it). A lost
lease is not an exit: 9.5 item 7.

### 4.4 Persisted run state

`/var/lib/supervisor/state/run.json` (root 0600, atomic replace, written at most every 100 ms and at once on state
changes):

```json
{"version": 1, "boot_id": "…", "lease_epoch": 7, "cmd_after": 45, "msg_seq": 12,
 "gen": {"n": 3, "pid": 812, "started_at": "…", "session_id": "279aabf0-…", "stdout": "/var/lib/supervisor/run/3/stdout.ndjson",
         "stdout_offset": 18234411, "initialized": true},
 "run": {"state": "running", "turn_id": "0190f6c2-…", "origin": "person", "pending_inputs": 1,
         "interrupt_sent_at": null, "pending_uuids": ["…"], "last_uuid": "…"},
 "last_turn": {"id": "0190f6b9-…", "outcome": "completed", "error": null},
 "tasks": [{"task_id": "…", "tool_use_id": "…", "type": "local_bash", "description": "…", "started_at": "…",
            "source": "event"|"static", "until": "…"|null}],
 "asks": [{"ask_id": "…", "cli_request_id": "…", "tool_name": "AskUserQuestion", "since": "…"}],
 "delivered": {"41": {"state": "acked"},
               "42": {"state": "written", "msg_id": "8f0c…", "gen": 3, "pending_inputs_after": 1}}}
```

`run.json` is the supervisor's private file (snake_case like every on-disk format; the wire is camelCase).
`delivered[cmdSeq].state` is `received` (in `run.json` before any effect), `written` (recorded, together with the
`msg_id` and the `pending_inputs` it implies, before the user message is handed to the keeper, 10.2), `running`
(reported to magpie) or `acked`.

Everything the supervisor must not forget across its own restart is here; everything else is rebuilt from magpie
(lease, tree, commands) or from Claude Code's stdout file.

### 4.5 Claude Code generations

A generation is one Claude Code process. It starts when needed (pre-spawn, a turn with no live process, a restart
after an exit or a configuration change) and lives across turns: turn N+1 is another user message on the same stdin,
not a new process with `--resume`. This removes the per-turn resume of an up-to-87 MB transcript (A8.F10), the held
stdin trick (A9.F0, A11.F5) and the 30-minute hold timer that existed only to keep a per-turn process alive (A9.F1).

A generation ends when: Claude Code exits by itself (crash, fatal error); the supervisor kills it (a `kill` command,
an interrupt that did not end the turn in 15 s, the memory guard, a configuration change while idle, stdout file over
1 GiB while idle); or the sandbox is released. The next start uses `--resume <provider_session_id>`.

Generation files: `/var/lib/supervisor/run/<n>/{stdout.ndjson, stderr.log}` on the scratch disk, owned by root, opened
by the keeper at spawn (Claude Code writes them directly; it never sees the path). Generations older than the current
one are deleted at the next idle point. A generation's stdout is a regular file, not a pipe, so a supervisor restart
loses nothing: the new supervisor continues reading at the saved offset (re-reading at most 100 ms of lines; every
report is idempotent, section 11).

### 4.6 dockerd on demand

`/run/supervisor/docker.sock` (mode 0666, HTTP): `GET /start` → `200 ok\n` | `500 <error>\n`. The supervisor spawns
dockerd through the keeper as root with `/etc/docker/daemon.json` `{"iptables": false, "ip6tables": false,
"bridge": "none", "group": "node", "log-level": "warn"}`, stdout/stderr to `/var/log/dockerd.log`, environment = the
sandbox environment minus anything section 6.3 strips, then waits up to 60 s for `/var/run/docker.sock` `_ping`. The
image's `docker` wrapper (A5.F2) keeps its behaviour and only changes the socket path. dockerd is never started unless
asked. Whether Docker works at all is the runtime's business (YYC: gVisor `--net-raw`, root caps); the supervisor only
starts it.

### 4.7 Shutdown

On SIGTERM (keeper forwarding a runtime stop: always treated as a forced release, since the runtime is stopping the
sandbox whatever the supervisor answers) or an accepted `POST /v1/lifecycle/release` the supervisor runs the release
sequence of 15.5, then exits 0. It never exits while a FUSE request it has read is unanswered, except by crash
(covered by the journal).

---

## 5. Memory and OOM protection

### 5.1 The problem in each runtime

- **gVisor (YYC):** the sandbox's memory cgroup is on the host and contains the sentry, the gofer and every
  application page. At the limit the host memcg OOM killer kills the sentry and the whole sandbox dies; gVisor has no
  OOM killer of its own, and its in-sandbox cgroupfs `memory.limit_in_bytes` is a stub that stores but does not
  enforce a value (`cgroupfs/memory.go`, `newStubControllerFile`). `oom_score_adj` exists but no in-sandbox killer reads
  it. Usable memory is about the limit minus 50-70 MiB (A5.F44, gap-0 1). A user-space guard alone samples too
  coarsely for a fast allocator (a tool can go from 80 % to the limit inside one 100 ms sample), so hermit's YYC
  implementation adds a host-side brake (section 23, hermit item 6): sandboxd sets cgroup v2 `memory.high = L - 256
  MiB` on the sandbox's host cgroup, below `memory.max`, so the host kernel throttles and reclaims the sentry instead
  of killing it. Throttled allocation slows the tool down to a rate the guard's 100 ms sampling catches. The brake is
  the runtime's; the supervisor needs nothing from it and reads nothing about it.
- **A Linux-kernel runtime (a VM such as a Fly Machine):** the guest kernel's OOM killer exists and honours
  `oom_score_adj`; cgroup v2 limits are enforced.

The supervisor does the same three things in every runtime; their effect differs only because the runtime differs:

### 5.2 Scores and cgroups

- keeper and supervisor: `oom_score_adj = -1000`; Claude Code: `100` (inherited by its tools); dockerd: `0`.
- At start, if `/sys/fs/cgroup` is a writable cgroup2 mount, the supervisor creates `/sys/fs/cgroup/agent` with
  `memory.max = L - 128 MiB` and `memory.oom.group = 0` and asks the keeper to spawn Claude Code into it (the keeper
  writes the child's pid into `cgroup.procs` before exec). If it is cgroup v1 with a `memory` hierarchy, the same in
  `/sys/fs/cgroup/memory/agent` with `memory.limit_in_bytes`. In gVisor these writes succeed and are not enforced; in a
  VM they make the kernel kill inside the agent's cgroup before it touches the supervisor. Failures to create the
  cgroup are logged once and not fatal.

### 5.3 The user-space guard (the protection that works under gVisor)

`memguard.rs`, a task in the supervisor:
- `L` = `resources.memory_bytes` from hermit's descriptor; if `/proc/meminfo` `MemTotal` is smaller, `L = MemTotal`.
- Usage `U` = `MemTotal - MemAvailable` from `/proc/meminfo` (gVisor computes these from its own accounting, so this
  is application memory; the sentry's own 50-70 MiB is covered by the reserve).
- Sampled every 500 ms; every 100 ms while `U > 0.8 L`.
- High watermark `H = L - 128 MiB` (`MEM_RESERVE`). When `U >= H`: choose a victim and SIGKILL it, then wait 2 s
  before the next kill decision (the freed memory needs to show).
- Victim choice: among all processes except PID 1, the supervisor, and Claude Code's main process, the one with the
  largest RSS (`/proc/<pid>/statm`), if it is at least 32 MiB. Otherwise Claude Code's main process (the generation
  ends; section 7.9). Never the keeper or the supervisor.
- Each kill is logged (`memory guard killed`, pid, comm, rss, U, L) and reported to hermit as an activity event
  (15.3) and, when it is Claude Code, ends the running turn with `run.oom` (section 11.6). A killed tool shows to the
  model as an ordinary killed command (exit 137) in its tool result; the turn goes on.

This replaces today's outcome (sandbox dies, the turn spins until TCP gives up, gap-0 1, A16.F6) with a contained kill
in the common case (a tool process blowing up) and a clean failed turn in the rare case (Claude Code itself too big).
The claim is tested, not assumed: gate item 11 (section 22) runs a tool that allocates 2 GiB/s and passes only with
`memory guard killed` and a live sandbox. Inside the sandbox the guard is not a security boundary (6.3: uid 1000 has
sudo and could change scores or stop the supervisor); it protects against accidents, which is what it is for.

### 5.4 The supervisor's own memory

Bounded by construction and checked by a soak test (the same principle as sandboxd's budget, A5.F43, A20.F13):
- FUSE: at most 64 requests read and unanswered at once (`FUSE_SLOTS`), each with a buffer of at most 1 MiB + 4 KiB,
  pooled; the reader blocks when all slots are busy (the sentry queues up to 10,000 requests itself).
- magpie calls: at most 16 concurrent; bodies are streamed from slot buffers or cache files, never accumulated.
- Blob downloads and sent-file uploads stream through 256 KiB buffers.
- stream-json lines are parsed one at a time from a 64 KiB read buffer; a line longer than 64 MiB (Claude Code echoes
  tool results with images in `--verbose` output) is skipped with a log line after extracting only the fields the
  driver needs (type, subtype, uuid, message id) by a streaming scan.
- Target RSS 40 MiB, hard check 64 MiB in the soak test (RSS flat over 24 h of synthetic turns with 1 MiB lines,
  supervisor restarts and magpie outages).

---

## 6. Launching Claude Code: argv, environment, managed files, tool and settings policy

### 6.1 argv

Executed by the keeper with an absolute path, cwd `/workspace`, uid/gid 1000, `setsid`, supplementary groups cleared,
inheritable and ambient capability sets cleared, bounding set kept and `no_new_privs` not set (so `sudo` works as
today, A5.F9):

```
/opt/claude-code/bin/claude
  --output-format stream-json
  --verbose
  --input-format stream-json
  --permission-prompt-tool stdio
  --permission-mode bypassPermissions
  --setting-sources=
  --disallowedTools EnterPlanMode,ExitPlanMode
  ( --resume <provider_session_id> | --session-id <conversation_id> )
```

- The first five flags are today's SDK argv (A11.F0). `--include-partial-messages` is not passed in this release:
  live frames ship later (8.3), and staff see whole blocks as today. No `--model` (the environment names it), no
  `--mcp-config` (the managed MCP file decides), no
  `--effort`/thinking flags (the `model` endpoint applies reasoning policy, A11.F15), no `--resume-session-at` (edit
  and rewind are dropped, magpie 21.1 decision 9, A1.F42).
- `--setting-sources=` (empty): only managed settings and the managed CLAUDE.md load. This closes A11.F12: today
  `project,user,local` let a model-written `/workspace/.claude/settings.json` hook run and `/workspace/CLAUDE.md` load
  as instructions on the next process. The empty list is the Agent SDK's own default ("isolation mode"); section 22
  repeats the inventory's experiment to prove neither file loads.
- `--disallowedTools EnterPlanMode,ExitPlanMode`: plan mode is not offered (Deyao, 2026-11-05). AskUserQuestion is
  (Deyao, 2026-11-05, as on the old stack): its question reaches the person through the ask path (7.6), and a question
  nobody answers parks the turn and frees the sandbox (DECISIONS 17), so it no longer hangs a turn (A11.F11).
- `--session-id <conversation_id>` for a conversation with no `provider_session_id`: Claude Code's session id equals
  the conversation id, so `transcript_path` is known before the first write (`projects/-workspace/<id>.jsonl`) and
  magpie never has to promote early lines (magpie 6.2). Imported conversations keep their old ids through `--resume`.

### 6.2 Environment (constant in every deployment)

The keeper's environment is what hermit's implementation started the sandbox with (the image's `Env`, `HOME`, and
`HERMIT_SANDBOX_TOKEN`, hermit 5.1). No child inherits it as such. Every process the supervisor starts (Claude Code,
dockerd, `/v1/exec`) gets exactly: the image's `PATH`, `LANG`, `LC_ALL` and `HOME`, then every entry of
`SandboxSelf.env` (15.1) applied unchanged and never interpreted, then the fixed table below (which wins on a clash, and
a clash is logged once). `HERMIT_SANDBOX_TOKEN` and `SUPERVISOR_*` are never passed on. This is the one place where the
agent's internet access comes from: in YYC `env` carries `HTTP(S)_PROXY`/`http(s)_proxy` with the sandbox's proxy
credential and the package mirrors (hermit 3.7.1); in a deployment with open egress it may be empty. The supervisor
itself never uses that proxy (17.1). Claude Code gets:

| Variable | Value | Why |
|---|---|---|
| `HOME` | `/home/sandbox` | as today |
| `PATH` | the image PATH | tools find user installs; `claude` itself is absolute |
| `http_proxy`, `https_proxy`, `HTTP_PROXY`, `HTTPS_PROXY`, mirrors | from `SandboxSelf.env` | the gateway (17) |
| `NO_PROXY`, `no_proxy` | `localhost,127.0.0.1,::1` (overrides `env`'s) | the four fixed names go through the proxy when there is one (17.2) |
| `ANTHROPIC_BASE_URL` | `http://model` | the fixed endpoint; the gateway serves it |
| `ANTHROPIC_AUTH_TOKEN` | `sandbox` | a placeholder: credentials live behind `model`, never in the sandbox |
| `ANTHROPIC_MODEL` | `default` from `GET http://model/v1/aviary/models` (17.3) | |
| `ANTHROPIC_SMALL_FAST_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL` | `small` | |
| `CLAUDE_CODE_MAX_CONTEXT_TOKENS` | `contextTokens` when given | auto-compact at the real window |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_AUTOUPDATER`, `DISABLE_TELEMETRY`, `DISABLE_ERROR_REPORTING` | `1` | as today |
| `CLAUDE_CODE_ATTACHMENTS_BY_PATH` | `1` | enables SendUserFile without a claude.ai login, as today |
| `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` | `5000` | only matters when stdin closes at release; the process no longer waits 30 min |
| `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS` | `120000` / `600000` | today's defaults made explicit (gap-0 13) |
| `BASH_MAX_OUTPUT_LENGTH` | `30000` | same |
| `MAX_MCP_OUTPUT_TOKENS` | `25000` | same |
| `API_TIMEOUT_MS` | `600000` | same |
| `CLAUDE_CODE_ENTRYPOINT` | not set (Claude Code reports `sdk-cli`) | the SDK set `sdk-ts`; section 22 checks the system prompt snapshot is unchanged |

Today's `JARVIS_*` variables (`JARVIS_EXEC_TOKEN`, `JARVIS_CLAUDE_MD/MCP/SETTINGS`, `JARVIS_SENT_FILES_URL`)
disappear (A5.F18, A11.F14). The earlier `env_private` list is gone: nothing is stripped from `env` because nothing in
it is private from the agent.

If `model/v1/aviary/models` cannot be read at spawn time, the spawn waits and retries (it is part of starting a
turn; magpie's 120 s `starting` watchdog bounds it).

### 6.3 What processes in the sandbox can see

uid 1000 has passwordless sudo (`node ALL=(ALL:ALL) NOPASSWD: ALL`, A5.F4), so it can read any process's
environment (the keeper's holds the sandbox token, and in YYC `env` carries the same credential inside `http_proxy`,
A5 surprises). That is today's accepted model: **the sandbox is one trust domain**, and everything inside it, the
agent and any prompt injection in content it fetched included, can do what root can. This bounds three claims of this
spec, and each says so where it is made:
- "OOM-protected" (5): protection against accidents (a tool that eats memory), not against the agent; root in the
  sandbox can reset `oom_score_adj`, stop the guard or kill the supervisor (the keeper then restarts it).
- "single writer" and "the main transcript is protected" (9.3, 9.5): the agent can unmount `~/.claude` or call
  magpie's cc API with the sandbox's own credential, for example to truncate the main transcript to 0. What protects
  history is on magpie's side and holds anyway: events are immutable, a retraction keeps their rows, unlink of the
  main transcript is refused, and an operator can rebuild a damaged cc file from its events (section 23, magpie
  item 9). The damage is limited to the agent's own conversation, which it could already write anything into.
- "the agent cannot change its configuration" (6.4): only against accidental or model-written files in
  `/workspace` and `~/.claude`, not against root rewriting `/etc/claude-code`.
The boundaries that do hold are outside the sandbox: the gateway's source-address binding of the sandbox token
(hermit 6.1), magpie's lease and per-conversation scope, egress's allowlist and budget. The supervisor adds no secret
to any environment: per-key secrets are fetched on demand (15.2), and the control token for :7070 is never in the
sandbox (only its hashes are). Narrowing sudo is not done (section 27, review point 28).

### 6.4 Managed files

Written by the supervisor as root, mode 0644, into `/etc/claude-code/` (Claude Code's managed paths on Linux), at
first start and again whenever the descriptor's `etag` changes (checked at every `start_turn`, 7.4):

`/etc/claude-code/CLAUDE.md` = `claude.managedClaudeMd` from `SandboxSelf`, byte for byte. hermit owns all of
it, including the sandbox facts that are deployment-specific (today's `SANDBOX_RULES` with the 1 GiB / 1,000-file
quota and the stale "check big writes" rule, gap-2 2.4; hermit generates the quota text from what it provisions).

`/etc/claude-code/managed-settings.json` (constant):

```json
{
  "allowManagedHooksOnly": true,
  "cleanupPeriodDays": 36500,
  "hooks": {
    "PreToolUse": [
      {"matcher": "SendUserFile",
       "hooks": [{"type": "command", "command": "/usr/local/bin/supervisor hook send-user-file", "timeout": 120}]}
    ]
  }
}
```

- `allowManagedHooksOnly`: no hook from any other source can run, even if a future flag re-enabled a setting source.
- `cleanupPeriodDays: 36500`: Claude Code's daily sweep never deletes a transcript (A8.F17); magpie also refuses the
  main transcript's unlink (EPERM), and retention is magpie's.

No `managed-mcp.json` (decision 15): Claude Code has no MCP servers. The supervisor writes the descriptor's
`managedSkills` ({"<skill>/<file>": text}) to `/etc/claude-code/.claude/skills/<skill>/<file>` (root, 0644, replaced
atomically like the other managed files, stale skill folders removed); the skills tell Claude Code to call
`http://tools/v1/tools/<name>` and the deployment's other fixed names with `curl`. The rest of this section (the
identity headers, tools' use of them) still holds for those calls.


No `Authorization` header: the gateway authenticates the sandbox and forwards the verified identity as
`Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation` and `Aviary-Sandbox-Person` (the sandbox owner's lowercased email,
platform 3.5). What tools does with it is tools' business (platform 17.2: it maps `Aviary-Sandbox-Person` to the
`X-YYC-User` header yyc-mcp needs). The server name changes from `yyc` to `tools`, so tool names become
`mcp__tools__<name>` (clean cut). Old transcripts keep `mcp__yyc__*` as data: pigeon's mapper ignores the server part
of an MCP name (pigeon 7.8), and the supervisor has no allow or deny list keyed by a tool name except the three
built-in tools of 6.1, so a resumed imported conversation that calls a tool again works (test in 22, item 13).

A configuration change while Claude Code runs is applied at the next idle point: the supervisor rewrites the files
and, if a generation exists and no turn or task runs, ends it gracefully (close stdin, 5 s, SIGTERM, 3 s, SIGKILL) so
the next turn starts with `--resume` and the new instructions (fixes A5 surprise "a config change reaches a running
sandbox only after a release").

---

## 7. The stream-json driver: messages, control requests, the run state machine

### 7.1 Framing

stdin: NDJSON, one message per line, written through the keeper's `STDIN` relay (4.2). stdout: NDJSON in the
generation's stdout file, read by tailing (inotify on the file, plus a 250 ms poll as a backstop). stderr: the
generation's `stderr.log`; the last 4 KiB is attached to `run.exited` errors; lines are copied into the supervisor's
log prefixed `claude[g]:`, at most 100 lines/s.

### 7.2 Initialisation

Immediately after spawn the supervisor writes:

```json
{"type":"control_request","request_id":"init-<gen>","request":{"subtype":"initialize"}}
```

(no hooks, no SDK MCP servers, no system prompt fields: identical in effect to today's SDK initialize except the
Notification hook, which is dropped as pigeon drops notifications, A11.F9) and waits up to 30 s for the matching
`control_response` and the `system`/`init` message, which gives `session_id`, `model`, `tools`, `permissionMode`.
`session_id` must equal the expected provider session id; if Claude Code reports another (a future resume
behaviour), the supervisor reports the new `providerSessionId` and `transcriptPath` with the next state report.

### 7.3 Messages the supervisor writes

| Message | When |
|---|---|
| `{"type":"user","message":{"role":"user","content":<string or blocks>},"parent_tool_use_id":null,"timestamp":"<sent_at>"}` | a `start_turn` command (section 12 builds the content) |
| `control_request {subtype:"interrupt"}` | an `interrupt` command |
| `control_request {subtype:"stop_task", task_id}` | `stop_tasks` command, or a new turn superseding held tasks |
| `control_response {subtype:"success", request_id, response:{behavior:"allow", updatedInput, toolUseID}}` | a `can_use_tool` that needs no person, or an answered ask |
| `control_response {subtype:"success", request_id, response:{behavior:"deny", message, toolUseID}}` | a denied/unanswered ask |
| `control_response {subtype:"success", request_id, response:{action:"decline"}}` | `elicitation` (as the SDK) |
| `control_response {subtype:"success", request_id, response:{behavior:"cancelled"}}` | `request_user_dialog` (as the SDK) |
| `control_response {subtype:"error", request_id, error}` | `hook_callback` (none registered), `mcp_message` ("no SDK MCP servers"), any unknown subtype ("unsupported") |

Never sent: `set_permission_mode`, `set_model`, `set_max_thinking_tokens`, `apply_flag_settings`, `rewind_files`,
`mcp_*` (A11.F10).

### 7.4 Messages the supervisor reads

| stdout message | Used for |
|---|---|
| `system` `init` | session id, model (logged), readiness |
| `stream_event` (`message_start`, `content_block_start/delta/stop`, `message_delta`, `message_stop`) | not produced in this release (no `--include-partial-messages`); in the later release, live frames (8.3); always marks activity |
| `assistant`, `user` (complete messages, with `uuid`) | activity; `uuid` goes into the turn's `pending_uuids` so the end is reported only after the transcript holds them (8.2); static background rule (7.7) reads `tool_use` blocks |
| `system` `task_started` / `task_updated` / `task_notification` / `task_progress` | the task tracker (7.7) and `task.status` reports |
| `system` `api_retry`, `status`, `compact_boundary` | logs (a `status` live frame in the later release) |
| `system` `hook_started` / `hook_response` | logs |
| `result` (`success`, `error_during_execution`, `error_max_turns`, …, with `usage`, `total_cost_usd`, `duration_ms`) | the end of one input's processing (7.5) |
| `control_request` `can_use_tool`, `hook_callback`, `elicitation`, `request_user_dialog`, `mcp_message`, other | 7.3 and 7.6 |
| `control_response` | resolves `initialize`, `interrupt`, `stop_task` |
| `control_cancel_request {request_id}` | cancels a pending ask (7.6) |
| `keep_alive` | ignored |
| anything else | logged once per type and ignored (forward-compatible) |

### 7.5 The run state machine

The supervisor keeps one run state for the conversation (persisted in `run.json`):

```
            start_turn delivered (user message written)
  idle ─────────────────────────────────────────────────────> running(person)
   ^  ^                                                          │  │
   │  │ last result, no tasks                                    │  │ interrupt cmd
   │  └──────────────────────────────────────────────────────────┘  v
   │                 last result, tasks outstanding           stopping ──(result, or 15 s → kill gen)──> idle/held
   │                         │
   │                         v
   │  tasks all ended      held ───(start_turn: stop_task each, then message)──> running(person)
   └────────────────────── held ───(Claude Code output without a pending input)──> running(agent)
   idle ───(Claude Code output without a pending input, e.g. a wake-up)──> running(agent)
   running(agent) ──(result)──> idle | held
   any ──(generation exits)──> idle (turn failed run.exited / run.oom)
```

- `pending_inputs` counts user messages written and not yet answered by a `result`. Each `result` decrements it.
  A `running` turn ends at the `result` that brings it to 0.
- **CLI-started turns** (A9.F5): when the run is `idle` or `held`, `pending_inputs == 0`, and stdout produces an
  `assistant`, `user` or `stream_event` message (not a `system` task message, not `keep_alive`), the supervisor opens a
  turn with a new UUIDv7, `POST /v1/sandbox/turns {turnId, origin: "agent", at}` (origin enum `person | agent |
  import`, platform R30), and that turn ends at the next `result` with `pending_inputs == 0`. This gives every follow-up turn explicit start and end events (today they have
  none, A9 surprises; pigeon 7.6.2).
- **A start_turn while an agent turn runs** (a race magpie's admission cannot fully exclude): the supervisor writes
  the user message at once (Claude Code queues it internally and answers it after its current internal turn) and
  reports the agent turn ended at the result that follows; the person's turn is reported `running` when its message is
  written and ends at the result that brings `pending_inputs` to 0.
- A turn ends with outcome `completed` for `result.subtype == "success"` and `is_error == false`; `failed` with
  `run.error` and `result.subtype`/the first 1,000 characters of `result.result` for other results not caused by an
  interrupt; `canceled` when the supervisor sent an interrupt for this turn.

### 7.6 Asks (`can_use_tool`)

Under `bypassPermissions`, Claude Code 2.1.304 sends `can_use_tool` only for tools that require user interaction
(A11.F8, A11.F11):
- `requires_user_interaction` absent or false: answer `allow` with the input unchanged at once (bypass semantics, as
  the SDK's canUseTool).
- `requires_user_interaction: true`: `POST /v1/sandbox/asks {askId: <uuid>, toolName, toolUseId, input}`; keep the
  CLI's `request_id`; wait for an `answer_ask` command. `allow` → `allow` with `updatedInput` (AskUserQuestion's
  answers travel in `updatedInput.answers`); `deny` → `deny` with the given message. No answer for 7 days
  (`ASK_TIMEOUT`, a safety limit only) → `deny` "User has not answered in 7 days, the session has timed out, ask again
  if needed." (the limit's real duration) and `POST /v1/sandbox/asks/{askId}/resolved {outcome: "canceled"}`. An
  `interrupt` or a `control_cancel_request` → `resolved {outcome: "canceled"}`.
- With the three interaction tools disallowed (6.1) this path is not expected to run in YYC; it exists so a turn can
  never hang silently again.

**A question nobody answers never holds a sandbox** (DECISIONS 17). The run is `asking` while it waits only on the
person: questions are open, every main-thread tool call still without a result is one of them (the supervisor tracks
`tool_use` ids from assistant lines and their `tool_result`s, kept in `run.json`), no background task or pseudo-task is
outstanding, nothing is queued and no interrupt was sent. `asking` is not busy (15.3): hermit's idle clock runs from
the question (`idleSince` = when it opened), and its idle release, a pool victim's release, the soft lifetime or any
forced release finds the supervisor willing (15.5). Such a release **parks** the turn instead of ending it:
1. Under the run lock (an answer that arrived first wins: the release is refused as busy, or if forced goes on as a
   forced release of a running turn), take the turn and its asks out of the run and report
   `turns/{turnId}/state {"state": "parked", "durationMs": <working time so far>}` (11.1); the asks are not resolved:
   they stay open in magpie, with their `toolUseId`, and on the person's screen. Until magpie answers the report the
   heartbeat still names the turn as `running`, so magpie's reconciliation never takes it for one the sandbox forgot.
2. End the generation with SIGTERM to its group (3 s, SIGKILL) and **stdin left open**: closing it while a
   `can_use_tool` is pending makes Claude Code 2.1.304 answer the question itself ("Tool permission request failed:
   AbortError: Tool permission stream closed before response received") and call the model again during the release.
   SIGTERM leaves the transcript ending at the question's `tool_use`.
3. The rest of the release as 15.5. No checkpoint, freeze or pause of the sandbox is ever used for a question (wrong
   layer, DECISIONS 17).

When the person answers, magpie gets a new sandbox and sends `start_turn` for the **same turn** with `answers[]`
(10.2). A resumed Claude Code does not take an answer to a question its predecessor asked, measured with 2.1.304
(aviary `crates/supervisor/spikes/ask-resume`, 2026-11-05): a late `control_response` for the old `request_id` is
ignored (no output, no model call); on the next user message it first closes the open `tool_use` itself with a
synthetic `tool_result` ("[Tool call interrupted: the session ended before this call's result was recorded, so its
outcome is unknown. …]", `is_error`) and a synthetic assistant line "No response requested." (model `<synthetic>`, no
model call); a user message holding a `tool_result` block for the question's id reaches the model as "(no content)".
So the answer is delivered as the turn's **next user message**, worded as the tool's own result would read
(AskUserQuestion: `Your questions have been answered: "Q"="A". You can now continue with these answers in mind.`, or
`The user answered: … Read the answers carefully …` when an answer is not one of the options; ExitPlanMode: `User has
approved your plan. …`; a deny: its message). The person sees the same either way: the question's row gets their
answer as its result (magpie 6.5 hides the two synthetic lines and reads the message as that result), then the reply,
a little later because a sandbox starts.

**No turn timer counts the wait.** The turn's reported `durationMs` leaves out the time its questions were open; the
hold's silence ceiling (7.7) stands still while a question is open and goes on after the answer; magpie's and
pigeon's working time do the same (magpie 9.1 `askWaitMs`, pigeon 7.7). The wait counts only as idle time for hermit.

### 7.7 Background tasks and the hold

The tracker (A9.F3) keeps outstanding tasks from two sources:
- **Events:** `system` `task_started` with a `tool_use_id` adds `{task_id, tool_use_id, type, description,
  workflow_name?, started_at}`; `task_notification` (any status) or `task_updated` with status `completed`, `failed`,
  `killed` or `stopped` removes it. Each is reported as a `task.status` event (11.4). Tasks started inside a subagent
  are kept with `nested: true`.
- **Static rule** (A9.F2), only for tools that emit no task events: a main-thread `tool_use` of `Monitor`,
  `ScheduleWakeup`, `CronCreate` or `TaskCreate` adds a pseudo-task `{task_id: "static:<tool_use_id>", source:
  "static"}` with `until`: for `ScheduleWakeup`, its fire time plus 2 min; for the others, none. A pseudo-task ends when
  its `until` passes, when an agent turn starts that names its tool_use id in a `<task-notification>`, or by the silence
  ceiling. `Bash` with `run_in_background`, `Agent` and `Workflow` emit task events and need no static rule.

Unlike today, the static rule and the tracker are combined (fixes the A9 surprise where any task event in a turn made
the tracker alone decide and released a Monitor). At each turn end the run is `held` when any task is outstanding.

The hold has no stdin to keep open any more; it is a reported state. Its only timer is the **silence ceiling**
`HOLD_SILENCE = 30 min` with no stdout output and no `until` in the future (time a question is open does not count,
7.6): then the supervisor sends `stop_task` for
every event task, drops pseudo-tasks, logs `hold ended by silence ceiling` with the task list, and reports `idle`
(today's 30-minute bound, now logged, A9.F20). A `ScheduleWakeup` within hermit's lifetime is therefore honoured at
any delay instead of dying at 30 min (A9 surprise).

**Supersede** (A9.F7, pigeon 7.5.4): a `start_turn` while `held` sends `stop_task` for every event task, waits up to
5 s for their `task_notification`s, clears pseudo-tasks, then writes the message. This keeps the agreed "a send ends
the background work, no confirmation" semantics; with a long-lived process the stop is a choice, not a necessity (24).

**Cancel during a hold** (A9.F8, pigeon 7.5.6): the `stop_tasks` command (10.6) stops them; the run goes `idle` when
the notifications arrive (or after 10 s regardless).

### 7.8 Interrupt, stop and kill

- `interrupt` (a person's Stop while a turn runs, A11.F6, A1.F43): write `control_request interrupt`; mark the turn
  `stopping`; cancel any pending ask. Expected: `control_response`, then `result` (`error_during_execution`), the
  transcript gains `[Request interrupted by user]`. Report `canceled` at that result. If no result comes within 15 s
  (`INTERRUPT_GRACE`, below magpie's 30 s), end the generation (SIGTERM to its process group, 3 s, SIGKILL) and report
  `canceled`. Interrupt never waits on a network link: it is local (fixes A16.F14).
- `kill` (magpie's forced stop after 30 s, or its watchdog): SIGKILL the generation's process group at once; report
  the turn `canceled` (or `failed` if the payload says `reason: "watchdog"`; magpie has usually ended the turn already
  and answers the report 200 as a replay or 409 `bad_transition`, both fine).
- `stop_tasks`: as 7.7.

### 7.9 Generation exits

On an exit reported by the keeper (`EXITS`):
- During a turn: report `failed` with `run.exited` (`message`: `exit <code>` or `signal <name>`, plus the stderr tail),
  or `run.oom` if the memory guard killed it, or `canceled` if it followed an interrupt/kill. The report still waits
  for the generation's last transcript writes (8.2); a killed process writes nothing more, so the wait ends at once.
- Tasks are cleared (their processes are Claude Code's children and die with the process group).
- Restart policy: the next `start_turn` spawns a new generation with `--resume`. Three exits within 60 s without a
  completed initialisation mark the conversation's agent as failing: further `start_turn`s are answered by an
  immediate `failed` `run.error` "Claude Code would not start" until 5 min pass (prevents a crash loop burning the
  model budget on resume).

---

## 8. How turn events reach magpie: transcript writes for history, stream-json for turn state (live frames deferred)

### 8.1 Decision

- **Durable events come from FUSE writes of the transcript.** Every byte Claude Code appends to
  `projects/-workspace/<sid>.jsonl` is stored by magpie before the write returns; magpie classifies main-transcript
  lines into `cc.user`/`cc.assistant` events with the next seq (magpie 6.5, 7). The supervisor never creates
  conversation content events itself. Each write carries `turnId` = the turn the supervisor was running when it read
  the FUSE request (or none when idle), so a line that lands late is credited to its own turn (magpie 6.3).
- **Turn boundaries, tasks, asks and errors come from stream-json** (7.5-7.9) as reports.
- **Token-level live text is deferred to a later supervisor release** (8.3). This release does not pass
  `--include-partial-messages` and sends no live frames; staff see each content block when it is complete, which is
  what they see today (no partial messages are requested today either, A11 surprise).

Why the transcript, not stream-json, is the record:
1. It is exactly what Claude Code itself reads on `--resume`; history the staff see and the agent's memory cannot
   diverge.
2. It carries what stream-json does not (attachments, `api-request*`, `cost-state`, compaction boundaries, the
   torn-tail repairs, tombstone rewrites and compaction renames, A8.F6, A8.F9, A11.F19); magpie must store it byte-exact
   anyway for resume, so deriving events from the same bytes needs one pipeline, not two that must agree.
3. Durability is simple and strict: a write returns only once stored, and a supervisor restart loses nothing (the
   write blocks in the sentry, the journal replays it, 9.9). Events derived from stdout would need their own
   exactly-once machinery.
4. Imported conversations are transcripts; one classifier serves both.

### 8.2 Ordering at a turn's end

A turn's `completed`/`failed`/`canceled` report is sent only after every `uuid` of the `assistant`/`user` messages
stdout showed during the turn has been acknowledged by magpie in a main-transcript write (the FUSE layer records the
uuids it sees in main-transcript writes with a 1 MiB-bounded scan for `"uuid":"…"` at line starts). There is no time
cap: writes are never dropped (9.5), so the wait ends when magpie has them, however long magpie is slow. The report
carries `lastUuid` (the last of those uuids), and magpie answers `409 transcript_pending` until that line is stored
(magpie 8.5), so `turn.ended` can never precede the reply's last row (pigeon 8.4 relies on it) even if the two checks
disagreed. A `transcript_pending` answer is retried like a 503. Claude Code's own `result` is not that signal: 2.1.304 waits
at most 10 s for its transcript drain and then emits the result without it ("transcript drain still running after
10000ms; the result goes out without it", measured by the WP-07 gate item 10 under a 30 s hold;
`CLAUDE_CODE_EAGER_FLUSH=1` removes the cap). Two exceptions end the wait at once: the generation
was killed (it writes nothing more), and the one-stuck-operation rule of 9.5 item 4 fired (the turn is then reported
`failed` `cc.write_stuck`).

A new `start_turn` is not admitted by magpie while the previous turn is still `running` (magpie 8.2), so the next
turn's lines can never begin with the previous answer's tail.

### 8.3 Live frames (specified now, shipped in a later supervisor release)

magpie already provides the receiving side (magpie 7.5: `POST /v1/sandbox/turns/{turnId}/live`, unlogged storage
idempotent by `(turnId, n)`, `LiveState` on M9 with `liveAfter`, `GET /v1/conversations/{id}/live` for a
mid-message join, supersede by `(messageId, blockIndex)` in the durable row's transaction). The supervisor's sender
ships in the first supervisor release after pigeon maps `LiveState` to an in-progress row that the durable row
replaces (never a second `assistant_message`), and pigeon's contract test that runs the app's real timeline reducer
over a live turn passes (pigeon 8.7, magpie 21.2). Turning it on is a code change in a new aviary release (argv plus
the sender), never a deployment setting; sandboxes on the older image keep sending nothing. Until then platform's E2
must not assert live frames (section 23, platform item 4).

The sender, as it will ship: from each `stream_event` the supervisor keeps, per open content block `(messageId,
index)`: the block type (`text`, `thinking`, `tool_use` with its `name`), the accumulated text length, and unsent text.
Every 50 ms (`LIVE_BATCH`), or when unsent text reaches 4 KiB, it sends one batch:

`POST http://magpie/v1/sandbox/turns/{turnId}/live` (`Aviary-Lease`)
```json
{"frames": [
  {"n": 118, "messageId": "msg_01H…", "index": 0, "op": "start", "block": "text",
   "parentToolUseId": null, "at": "2026-11-02T03:46:12.345Z"},
  {"n": 119, "messageId": "msg_01H…", "index": 0, "op": "delta", "offset": 0, "text": "今日港珠澳大橋…"},
  {"n": 120, "messageId": "msg_01H…", "index": 1, "op": "start", "block": "tool_use", "name": "mcp__tools__search"},
  {"n": 121, "messageId": "msg_01H…", "index": 0, "op": "stop"},
  {"n": 122, "op": "status", "status": "api_retry", "attempt": 2, "retryInMs": 4000}
]}
```
→ 204. `n` is per turn, contiguous from 1; `offset` is the byte offset of `text` within the block, so a duplicate
batch (a retry, or a supervisor restart re-reading stdout) is idempotent. `tool_use` input deltas are not sent (only
the start with the name); `thinking` deltas are sent and pigeon decides whether to show them.

At most one live request is in flight per turn; while it waits, the next batch accumulates (natural backpressure).
- `409 turn_not_active`, or a `404` (a magpie without the route): stop sending frames for this turn; never retried.
- 5xx or unreachable: frames older than 5 s are dropped (the durable events will come; live frames are a
  convenience, not a record) and a `gap` frame `{"op": "gap"}` is sent first when sending resumes (magpie answers the
  next poll with `reset: true`).
- Live frames are never retried past 5 s and never block anything else; they are the only supervisor report that
  may be dropped (11).

## 9. The `~/.claude` filesystem (FUSE backed by magpie)

### 9.1 Scopes: what goes to magpie and what stays in the sandbox

Every path under the mount (relative to `~/.claude`) belongs to one scope, decided by the first path component(s),
from a constant table (`scope.rs`). The table is tied to Claude Code internals that change between minor versions
(2.1.316 adds `.ccr-tip.json`, `.precompact.json`, `storageV5` paths and the `<sid>.jsonl.superseded-<ms>` set-aside),
so it is not "reviewed": a CI job checks it. `claude-code-gate` (nightly and on every change of the pinned version)
runs the FUSE plus fake-magpie harness against the newest Claude Code release with a scripted session (resume,
compaction, tombstone, relocation with an occupied destination, subagents, TodoWrite, file history) and fails on any
`unclassified path` log line, any syscall the mount answers ENOSYS/EOPNOTSUPP that the pinned version did not issue,
or any drift in the events magpie derives. The sandbox image's Claude Code pin moves only on a green gate.

| Path | Scope | Why (inventory) |
|---|---|---|
| `projects/**` (transcripts `<sid>.jsonl`, `<sid>/tool-results/*`, `<sid>/subagents/**` incl. workflows, `memory/`, `*.jsonl.compact.tmp.*`, `*.jsonl.superseded-*`) | **magpie** | conversation content (A8.F5, F11, F12, F18); magpie decides kind by name and treats only the main path as the main transcript (magpie 6.2) |
| `todos/**`, `plans/**`, `tasks/**`, `file-history/**` | **magpie** | per-session state a resume may need (absent today). 2.1.304 in `-p` mode writes `file-history/<sid>/<hash>@v<n>` (with `copy_file_range`) only when `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` is set, which 6.2 does not set, and keeps TodoWrite's list in the transcript (`todos/` stays empty; WP-07 items 2 and 9) |
| `sessions/**` (`<pid>.json`, `<pid>.<sha>.key`) | local | live-process registry; the supervisor knows liveness itself (A8.F13) |
| `shell-snapshots/**` | local | per process (A8.F14) |
| `session-env/**` | local | (A8.F15) |
| `backups/**` | local | `.claude.json` backups; `~/.claude.json` itself is on scratch in HOME (A8.F16) |
| `.last-cleanup` | local | (A8.F17) |
| `statsig/`, `telemetry/`, `debug/`, `ide/`, `cache/`, `mcp-discovery-cache/`, `uploads/`, `plugins/` | local | |
| `settings.json`, `settings.local.json`, `CLAUDE.md`, `CLAUDE.local.md`, `commands/`, `agents/`, `skills/`, `output-styles/`, `hooks/`, `rules/` | **refused** | always absent; create → EACCES (A8.F28, A11.F12) |
| anything else | local | logged once as `unclassified path` so a Claude Code upgrade that adds state is noticed |

**Provisional (WP-07 gate, deviation 4).** What this table and 9.4 take from the WP-07 gate (the paths 2.1.304
touches, file history only with `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`, `todos/` unused, no `link(2)`, the
syscall inventory) was measured with `-p --dangerously-skip-permissions` and default setting sources, not 6.1-6.4's
argv, environment and managed files. It stays provisional until WP-22's re-run of gate items 2 and 9 under the 6.1-6.4
launch confirms or corrects it; until then neither is a reason to move a path between scopes.

magpie scope = the conversation's files in magpie (magpie spec 6). Local scope = a backing directory
`/var/lib/supervisor/local/` on the scratch disk, served by passthrough; it lives and dies with the sandbox, which is
what these files are (today they were shared across a person's sandboxes and collided, A8.F31). Because one sandbox
holds one conversation, the per-person `~/.claude` ends (A8.F0) and auto-memory becomes per conversation (magpie 6.1).

The root directory lists the union: the magpie-scope top directories present in the tree plus the local backing
directory's entries.

### 9.2 Attributes

- Every file and directory: owner 1000:1000; mode as stored (Claude Code creates transcripts 0600 and directories
  0700); `st_nlink` 1 for files, 2 for directories; `st_blksize` 4096; `st_blocks = ceil(size/512)`.
- `st_ino` = the node id (9.8), stable for the sandbox's life and across supervisor restarts and migrations.
- `st_size` and `st_mtime` (nanoseconds) are the supervisor's: on every write/truncate it sets `mtime_ns = now`
  and sends it with the operation (magpie stores and returns it in the tree), so a new supervisor, a migration target
  and magpie agree to the nanosecond (A8.F30). `st_atime = st_mtime`; `st_ctime` = last metadata change.
- FUSE attribute and entry validity: 1 s. All changes pass through this mount (the supervisor is the only writer,
  enforced by magpie's lease), and gVisor updates its own cached size on its own writes, so the short validity only
  bounds staleness if something unexpected changes magpie's copy.

### 9.3 Operations

| FUSE request | magpie scope | local scope |
|---|---|---|
| INIT | reply 7.31 (minor ≥ 28 is required for `FUSE_MAX_PAGES`, 2.5), `max_write = 1 MiB`, `max_pages = 256`, flags `FUSE_BIG_WRITES|FUSE_MAX_PAGES|FUSE_ATOMIC_O_TRUNC`; never `FUSE_WRITEBACK_CACHE` (writes must be write-through) | same mount |
| LOOKUP, GETATTR, READDIR, OPENDIR, RELEASEDIR, STATFS, ACCESS | answered from the tree cache, no network | passthrough |
| OPEN | assign `fh` (stateless, 9.8); `O_TRUNC` → `cc/truncate size 0` before replying | `open(2)` on the backing file, fd kept in a table keyed by `fh` |
| CREATE | `cc/write?create=1&offset=0` with an empty body and the requested mode | passthrough |
| READ | from the local cache (9.7); a miss fetches `cc/file?path&offset&length` (whole file for files ≤ 256 MiB, then served locally) | passthrough |
| WRITE | `POST cc/write?path&offset&mtimeNs&opId&create=0&mode&turnId`, body = the request's bytes (≤ 1 MiB); reply after magpie's 200 | passthrough |
| SETATTR size | `cc/truncate {path, size, mtimeNs, opId}` | `ftruncate` |
| SETATTR mtime/atime | `cc/setattr {path, mtimeNs, opId}` | `utimensat` |
| SETATTR mode | `cc/setattr {path, mode, opId}` | `chmod` |
| SETATTR uid/gid | accepted if equal to 1000, else EPERM | same |
| RENAME | both in magpie scope: `cc/rename {from, to, opId}` (replaces `to`, magpie 6.6); across scopes: EXDEV | `rename(2)` |
| UNLINK | `cc/unlink {path, opId}` (main transcript: magpie answers 403 `protected` → EPERM) | passthrough |
| MKDIR / RMDIR | `cc/mkdir` / `cc/rmdir` (EEXIST / ENOTEMPTY are how `mkdir`-locks work, 9.6) | passthrough |
| LINK | EPERM, the same in magpie's spec (platform R29; Claude Code's create-only subagent write falls back on EPERM/ENOSYS/EOPNOTSUPP, A8.F12) | EPERM |
| SYMLINK, MKNOD | EPERM | passthrough for SYMLINK, EPERM for MKNOD |
| READLINK | EINVAL | passthrough |
| FLUSH, FSYNC, FSYNCDIR | 0 at once: every acknowledged operation is already durable in magpie (magpie 6.8) | `fsync(2)` |
| RELEASE | forget `fh` | close the backing fd |
| GETXATTR/LISTXATTR | ENODATA / empty | ENODATA / empty |
| SETXATTR/REMOVEXATTR | ENOTSUP | ENOTSUP |
| FALLOCATE | EOPNOTSUPP | EOPNOTSUPP |
| INTERRUPT | If the target is waiting in the retry loop (9.5) and its magpie call has not been sent, reply EINTR to it; otherwise let it complete (its reply is discarded by the sentry if the caller is gone). A signal to the caller sends INTERRUPT and then waits for the original reply; a checkpoint forgets the request and **resends** it after the restore, and its INTERRUPT may not be seen (below: the resend rule does not depend on it) | ignored |
| FORGET / BATCH_FORGET | no-op (nodes are kept, 9.8) | no-op |

STATFS (magpie scope): `f_bsize 4096`, `f_blocks = quotaBytes/4096`, `f_bfree = f_bavail = (quotaBytes -
ccBytes)/4096`, `f_files 1,000,000`, `f_namelen 255`, from the tree response (magpie 6.11).

**Requests resent after a checkpoint (WP-07 gate, item 3).** When a task waiting on a FUSE reply must stop for a
checkpoint (`runsc checkpoint`, a live migration's final stop), the sentry forgets the request (the fork's
`fuse/connection.go` `waitForResponse`/`abandonRequest`), queues `FUSE_INTERRUPT` for it, and restarts the syscall
after the restore, which sends **the same operation again under a new `unique`** from the same thread. Measured in
all 72 AWS migrations with requests held (and the Hetzner re-runs): both held requests (a WRITE at the same offset
with the same bytes, an RMDIR) were resent, and the original's reply write was refused with ENOENT (the sentry no
longer waits for that `unique`). The INTERRUPT is **not** a reliable sign: the supervisor read both INTERRUPTs in 70
of the 72 AWS runs, one in `mig-s06` and none in `mig-s09`, where a resend came anyway (in those two runs a read of
the request journal slot had no whole request in it, GATE.md item 3). It concerns every request in flight at the final
stop, held or not. A write split into several requests stops after a complete request and returns short, so only one
request per caller thread is resent.

The rule (keyed on what every run showed: the calling thread and the original's refused reply, never on INTERRUPT
receipt):
- **Key**: opcode, node id, the header's `pid` (the calling thread: the sentry puts the thread id there; Claude
  Code's writes arrive from several ids of one process, WP-07 item 3) and the argument bytes (for WRITE: offset and
  data).
- **A request from a thread that already has a request in flight** is proof that the sentry forgot the earlier one (a
  thread blocks in its syscall; a signal makes the sentry send INTERRUPT and wait for the original reply, measured).
  If the keys are equal, the new request waits for the earlier one and gets its result, without a second magpie
  operation, unless the earlier one ended without a magpie operation (answered EINTR from the INTERRUPT row above):
  then the new one is served on its own. With different keys both are served on their own.
- **A request whose reply write returns ENOENT** keeps its result when a magpie operation produced it (the
  "interrupted results": in memory and in its journal slot until matched, at most 64). The next request from the same thread with an equal key is answered with
  it, without a second magpie operation; any other request from that thread drops the entry. Results of journal
  replays after a supervisor restart are not kept: there an ENOENT means the reply was already delivered before the
  kill (WP-07 item 4), and no resend follows.
- An entry is also dropped when its connection generation ends. A thread id reused by a new thread inside the
  window of one entry is the residual case (a caller killed with its request in flight, a new thread with the same id
  issuing the identical request first); it is not handled.
- magpie's side covers a resend that races the original anyway: a cc write whose range already holds exactly the
  written bytes is a success (magpie 6.6 rule 0).

Without this, the resend of an acknowledged append would get `offset_mismatch`, and a resent CREATE with `O_EXCL`,
MKDIR (a `mkdir` lock), RENAME or UNLINK would answer EEXIST/ENOENT instead of the real result. An INTERRUPT that is
read is handled as in the table above; nothing depends on it.

**`bootId` is one per `/dev/fuse` connection.** The kernel side's `unique` counter starts again on a new
connection, so the supervisor draws a fresh `bootId` whenever it mounts (a new connection), and reuses the stored one
only when it took the existing connection's fd from the keeper (9.9). A `bootId` read back from disk after a fresh
mount would repeat `opId`s for different requests, and magpie would answer a new write with an old stored result. The stored id is written atomically (a temporary file, `fsync`, `rename`, `fsync` of the
directory) right after a fresh mount and before the first request is read. A supervisor that took a keeper fd but
cannot read a valid stored id refuses to start (exit with a logged `boot_id_unreadable`; the keeper keeps the fd and
the restart loop backs off) rather than draw a new id on the old connection, whose replays would then get new `opId`s
and be executed a second time by magpie.

Every magpie call is made with `opId` = UUIDv5(`SUPERVISOR_OP_NS`, `bootId ‖ unique ‖ step`), where `unique` is the
FUSE request's own id and `step` numbers the calls one request makes (OPEN with `O_TRUNC` makes one, CREATE one).
The connection and its `unique` counter live as long as the sandbox (the keeper holds the fd, 9.9; migration and
restore keep it; measured: `unique` keeps increasing across supervisor restarts and a migration, an INTERRUPT carries
its target's `unique` + 1), so the id is unique for the sandbox's life and a replayed request recomputes the same ids
without having stored them. A resend after a checkpoint is a new request with a new `unique`; the interrupted-results
rule above, not the `opId`, makes it apply once.

Paths are validated before any call: at most 4,096 bytes, components at most 255 bytes, no NUL, no `.`/`..`
components (the sentry resolves those itself).

### 9.4 The semantics Claude Code 2.1.304 needs, and how each is met

The table below was derived from reading Claude Code's JavaScript (A8.F8, F9, F12-F17). The shipped 2.1.304 is a
Bun-compiled native binary (3), so the JS API names are a guide, not the contract: what the binary actually does is
the syscalls it issues, and gate item 9 (section 22) records them with `runsc --strace` on the real binary against the
mount for a scripted session (resume, compaction, tombstone, relocation, subagents, TodoWrite, file history). This
section is rewritten from that trace before implementation goes beyond the spike; a syscall the mount cannot serve
(for example `mmap` of a mount file, which gVisor's FUSE answers ENOSYS, or `copy_file_range`/`sendfile` for a
`copyFile`) moves that path to local scope or gets an explicit answer in 9.3.

| Claude Code behaviour (as read from the JS; to be confirmed by the trace) | Met by |
|---|---|
| Batched `appendFile(path, chunk, {mode: 0o600})`, `mkdir -p 0700` and retry on ENOENT; also synchronous `appendFileSync` (16 call sites in the binary, among them an `appendFileSync(path, s, {mode: 384})` / `mkdirSync` retry path and `appendEntryToCurrentTranscriptNow`) | O_APPEND offset from the sentry (2.7) → WRITE at `size`; magpie's fast path (magpie 6.6 step 3); ENOENT from magpie's 404 for a missing parent |
| Torn tail: `pread(size-1)` then a `\n`-prefixed append. Measured (WP-07 gate item 2): with only the final `\n` missing the next append starts with `\n`; with a line cut mid-way (a partial JSON object) 2.1.304 reads the last 64 KiB and appends one `\n` at the cut, so the partial line stays in the file as an unparseable line and the resume works | READ of the last byte from the local cache (no network); magpie stores an unparseable line as such (it is a line, never an event) |
| Tombstone removal: open `r+`, read the last 64 KiB, `ftruncate` at a line start, `pwrite` the tail back; or a whole-file rewrite for files ≤ 50 MiB | local-cache READs; SETATTR size → `cc/truncate`; WRITE at the offset → `cc/write`; magpie's limbo keeps unchanged lines' events (magpie 6.7) |
| Compaction: write `<file>.compact.tmp.<hex>`, verify 3 × 4 KiB windows, rename over the original | the tmp file is a lines file in magpie scope; RENAME → `cc/rename`, matched line by line (magpie 6.6) |
| Relocation: rename a transcript and its session folder between project dirs, comparing `st_dev`/`st_ino`; 2.1.316 sets an occupied destination aside as `<sid>.jsonl.superseded-<ms>` and writes a fresh file. Measured on 2.1.304 (WP-07 item 2, `set_cwd` onto an occupied destination): RENAME of the occupant to `.superseded-<ms>` (then `utimensat` on it), RENAME of the transcript and of its `<sid>/` folder into the new project dir | RENAME of a file and of a directory in magpie scope; `st_ino` stable (9.8), `st_dev` the mount's; magpie keys the main transcript by path, so the fresh file at the main path is the main transcript (magpie 6.2) |
| Atomic writes elsewhere: `<file>.tmp.<pid>.<hex>` then rename; the writer refuses symlinks and hard-linked targets | RENAME; `st_nlink` always 1; no symlinks in magpie scope |
| `mkdir`-as-lock (`<file>.lock`), lock refresh by `utimes`, stale lock by mtime | MKDIR is atomic (EEXIST), SETATTR mtime, stable mtimes (9.6) |
| `copyFile` of `~/.claude.json` into `backups/`, keep 5 | local scope |
| Subagent transcript create-only via tmp + `link(2)` (2.1.304 does not call `link(2)`: it creates `subagents/agent-<id>.jsonl` directly and writes `.meta.json` by tmp + rename, WP-07 item 9) | LINK → EPERM → fallback; `claude-code-gate` watches for `link` |
| `readdir`, `stat`/`statx`/`lstat` everywhere; the daily sweep by mtime | tree cache; sweep cannot delete history (`cleanupPeriodDays` 36500 and magpie's EPERM) |
| `open(O_TRUNC)` | OPEN → `cc/truncate size 0` before the reply (`FUSE_ATOMIC_O_TRUNC`) |
| `mmap` of a file under `~/.claude` (not expected; the trace decides) | ENOSYS from the sentry before the supervisor sees anything; such a path goes to local scope |
| No `fsync` on transcripts; synchronous writes on exit | every write is durable when it returns; on exit the final flush blocks until stored |
| `sessions/<pid>.json` read-modify-write, sweep by `pidDomain` | local scope: no collisions across sandboxes (A8.F31, A8.F13) |

**What a blocked write costs the agent.** A synchronous `appendFileSync` on a FUSE file blocks the thread that calls
it, which in Bun is the event loop: while magpie is slow or down, model streaming, stream-json output and tool
dispatch stall with it, not just persistence. Asynchronous writes occupy Bun's small worker pool, which also serves
Read/Glob file work and DNS. The design accepts this for the main transcript (it is the record, and Deyao's rule is
that a write returns once magpie stored it), and measures it: gate item 10 injects 50 ms and 500 ms of magpie latency
and a 30 s outage and records stream-json token gaps and tool latency. If the event loop stalls measurably at 50 ms,
the paths that are not history (`todos/`, `file-history/`, `plans/`, `tasks/`) move to local scope (they are per-session
working state a resume does not need; file history serves rewind, which is dropped), and only `projects/**` stays
write-through. Nothing in magpie scope is ever acknowledged before magpie stored it (no write-behind).

### 9.5 The write path and what "a write returns once magpie stored it" means

1. The request is in the journal from the moment it is read (9.9). The FUSE handler builds the magpie call with the
   derived `opId` (9.3), the expected `offset`, `mtimeNs` and `turnId` (8.1).
2. The call goes directly to the fixed name `magpie` (17.1) with `Aviary-Lease`. magpie answers after one Postgres
   transaction committed on a synchronous standby too; new blob bytes are staged in Postgres in that transaction and
   uploaded to the bucket afterwards, so the write never waits for the object store (magpie 5.2, 6.10).
3. On 200: update the tree cache (size, version, mtime) and the local cache file (write-through), free the journal
   slot, reply to the sentry. The `write()` in Claude Code returns now.
4. On 503, a connection error, a timeout (30 s per attempt), or a gateway 502/504: retry with the same `opId` after 0.2 s doubling to 10 s. One rule, the same in magpie 6.12
   (platform R26):
   - **General unavailability** (magpie, its database or the gateway is down for everyone: the heartbeat fails too):
     retried for as long as it takes while the lease is valid. Claude Code's write blocks (9.4 says what that costs);
     nothing is dropped, nothing returns EIO (today's log-and-drop and JuiceFS ENOTCONN outcomes end, A16.F12).
     magpie's watchdog measures heartbeat silence only over time magpie could accept heartbeats (magpie 8.8), so a
     healthy turn that was only waiting is not failed when magpie returns.
   - **One stuck operation** (the same op keeps failing for `STUCK_OP` = 10 min while heartbeats to magpie succeed: a
     per-conversation bug, an oversized rewrite that always times out): answer that request EIO, report the turn
     `failed` with `cc.write_stuck` (message: path, offset, last error), and end the generation (SIGTERM, 3 s,
     SIGKILL). magpie sets `cc_resync` (magpie 6.12); staff see an error row and can send again. Because writes to one
     inode are serialised by the sentry, every later write to that file was queued behind the stuck one; they die with
     the generation, so the sandbox recovers instead of hanging for good. The stuck op's own bytes are not kept: Claude
     Code was told the write failed.
5. Definitive answers map to errno once, without retry: 404 `not_found` → ENOENT, 403 `protected` → EPERM, 409
   `exists` → EEXIST, 409 `not_empty` → ENOTEMPTY, 413 `too_large` → EFBIG, 507 `quota_exceeded` → EDQUOT, 400
   `bad_request` → EINVAL.
6. 409 `offset_mismatch` / `offset_beyond_eof`, and `resync: true` on a lease: the repair of magpie 6.12. Refetch the
   tree. If magpie's file is shorter than the local cache copy and magpie's bytes are a prefix of it (checked by
   reading magpie's last 64 KiB and comparing with the cache), re-send `[magpie.size, local.size)` with fresh `opId`s
   (UUIDv4, recorded in `run.json` before sending), then retry the operation. This covers a restore or failover that
   lost an acknowledged write: the write-through cache holds the bytes magpie lost. Only when the cache disagrees with
   magpie's bytes (or holds nothing for the file) does the supervisor log an error with path, offsets and versions,
   answer EIO and stop the agent (a forked history must not continue). With magpie's synchronous replication this path
   is not expected; it exists so it repairs itself if it happens.
7. 409 `lease_lost`: another writer exists. Answer EIO to this and every request in flight, SIGKILL the generation,
   send no more reports for its turn, and go back to the lease loop of 4.3 step 5 (no exit, no restart). magpie grants
   a lease only to the sandbox hermit names as the conversation's live one (magpie 6.9), so the sandbox that is not
   current stays unready with `not_current_sandbox` and the two never take the lease from each other in turns. While
   unleased, magpie-scope requests answer EIO at once (no agent runs).

Writes to different files may be in flight concurrently (up to the slot limit); writes to one file arrive serialised
by the sentry (2.7), so line order in the transcript is the order of syscalls.

Latency budget: each WRITE costs one round trip sandbox → gateway → magpie → Postgres commit on a synchronous standby.
magpie's acceptance numbers are p50 ≤ 15 ms for a 10 KiB append and p99 ≤ 150 ms for a 1 MiB image line at its
handler (magpie 6.10); the gateway may add at most 5 ms p99 (section 23, gateway item 2); the AWS sandbox → cluster
WireGuard path adds its own round trip, measured in gate item 6. A transcript append is typically 1-50 KiB; the
largest single lines are about 1.1 MB (base64 images, A8.F7), i.e. two WRITEs. An 87 MB conversation's lifetime of
writes is about 3,000 round trips spread over its turns.


### 9.6 Locks

gVisor does not forward `flock`/`fcntl` locks to FUSE servers; it keeps advisory locks in the sentry for the files it
serves. Within one sandbox that is exactly the semantics needed: every process that can contend for a lock is in this
sandbox. Claude Code's cross-process locks are `mkdir` lock directories (atomic MKDIR, EEXIST) refreshed by `utimes`,
which go through the operations above. The supervisor itself never takes a lock on the mount and never touches the
mount from its own process (that would deadlock its own FUSE loop); its caches live elsewhere.

### 9.7 What is cached locally

- **Tree cache** (memory, persisted to `/var/lib/supervisor/state/tree.json` every 5 s): every magpie-scope path
  with kind, size, `mtimeNs`, mode and `version`. Loaded from `GET /v1/sandbox/cc/tree` at each lease; then
  maintained by the supervisor's own acknowledged operations. Serves LOOKUP/GETATTR/READDIR/STATFS with no network.
- **Content cache** (`/var/lib/supervisor/cache/<node id>.<version>`, root 0600, on the scratch disk): a file's bytes
  as last read or written. Budget 256 MiB (`CACHE_BUDGET`) for every file except the main transcript, LRU by last
  read; the main transcript is held outside the budget and never evicted (it is what `--resume` reads; today's largest
  is 87 MB, and magpie caps a file at 256 MiB). The cache is on the scratch disk that live migration copies (16.1), so
  it is kept small rather than moved to a path migration skips: gVisor's restore checks every open host file against
  the checkpoint (A8.F30), so a skipped file the supervisor has open would fail the restore. Pre-copy migration copies
  the cache before the pause; only cache files written since (in practice the main transcript's appended tail) add to
  the final-pause delta. Gate item 3 measures the pause with a full cache. A READ is served from
  it when the cached version equals the tree's version. Writes update it in place after magpie's 200, so Claude Code's
  `--resume` read, `pread(size-1)` and the 64 KiB tombstone window are local disk reads. A file not in the cache is
  fetched once (the whole file up to 256 MiB, streamed into the cache file; READs for already-arrived ranges are
  answered while the rest streams). magpie's target is an 87 MB transcript in under 5 s (magpie 6.11).
- On a new lease (a supervisor restart, after a pause, after `resumed`): compare each cache entry's version and size
  with the tree; keep the equal ones, drop the others. The cache is also what 9.5 item 6 repairs magpie from.
- Prefetch: at first start, the main transcript (so a pre-spawned resume is local); at each tree load, nothing else.

### 9.8 Node ids and file handles

- `node id` (u64, FUSE root = 1): assigned per path on first LOOKUP/CREATE/MKDIR, moved with RENAME, kept after
  UNLINK until the next start (open files keep working). Persisted in `/var/lib/supervisor/state/nodes.log`
  (append-only: `assign <id> <path>`, `rename <id> <path>`, `drop <id>`; compacted at start). A restarted supervisor
  must reproduce the exact mapping because the sentry still holds the old ids. Node ids are never reused.
- `fh` (u64): `node id << 8 | flags` (read, write, append). Stateless for magpie scope, so a restarted supervisor
  accepts every handle the sentry holds. Local-scope handles index a table of backing fds that a restart rebuilds by
  reopening the node's path with the recorded flags (a local file unlinked while open is lost across a supervisor
  restart: acceptable for these files).

### 9.9 Surviving a supervisor restart: the request journal

The sentry delivers each FUSE request once while the sandbox runs: a supervisor restart causes no resend (a
checkpoint does, and 9.3's interrupted-results rule handles that). If the supervisor dies after reading a
request and before replying, the calling thread in Claude Code would wait for ever. So the design leaves no instant at
which a read request exists only in the supervisor's private memory:
- The `/dev/fuse` fd is held by the keeper (`PUT_FD`), so the connection stays up while the supervisor is down;
  requests queue in the sentry (up to 10,000) and their callers wait.
- The journal `/var/lib/supervisor/fuse-journal` is a 64-slot file (one slot per `FUSE_SLOTS`, each 1 MiB + 8 KiB;
  sparse, about 65 MiB of address space on the scratch disk) mapped `MAP_SHARED`. **The FUSE reader reads each request
  directly into a free slot's mapped memory**: `read(fuse_fd, slot_ptr, slot_len)`. The kernel's request header
  (`fuse_in_header`: `len`, `opcode`, `unique`, `nodeid`, …) lands at the slot's start, so a slot is occupied exactly
  when its `len` field is non-zero; the reader zeroes `len` before each read. A SIGKILL can only take effect at a
  syscall boundary, so once `read(2)` has returned the request is in the shared mapping, which is page cache of the
  journal file and outlives the process: there is no window between "read" and "journaled". No fsync: the journal only
  has to survive a process crash (a sandbox crash ends every caller anyway).
- `opId`s are derived from `bootId` and `unique` (9.3), so nothing else needs recording before the first magpie call.
- A slot is freed (`len` zeroed) after the reply is written to `/dev/fuse`. A crash between the reply and the zeroing
  replays a request that was already answered: the re-executed magpie call returns the stored result for its `opId`,
  and the reply's write to `/dev/fuse` fails with ENOENT (no waiter), which is ignored.
- A new supervisor replays every occupied slot in `unique` order before reading new requests: it re-executes the
  request with the derived `opId` (magpie returns the stored result for a committed `opId`, so a write is never
  applied twice) and writes the reply with the original `unique`. A reply for a `unique` the sentry no longer waits for
  (the caller was interrupted) is refused with ENOENT and ignored.
- Proven by gate item 4 with fault injection: a test build of the supervisor (cargo feature `fault-injection`, never
  in a release binary) SIGKILLs itself at each of the points "read returned", "magpie call sent", "magpie answered",
  "reply written"; every case must complete the caller's syscall exactly once with the bytes stored exactly once.
  If that test cannot be made to pass on the gVisor fork (for example if a shared mapping of a scratch-disk file
  loses dirty pages when the mapping process dies), the fallback is section 27, review point 21's simplification, taken then
  and recorded in this spec.
- **Explained (WP-57, 2026-11-02; was open from WP-07 gate item 3): the read after a restore that left no whole
  request in the slot.** The cause is the scratch move, not the runtime. When the checkpoint stops the sandbox, the
  sentry queues the held requests' `FUSE_INTERRUPT`s and a reader thread can take one before it stops: the request is
  written into the slot through the shared mapping on the source, during the stop. A write through a shared mapping to
  a page that is already dirty does not change the file's mtime, and the scratch move's final rsync delta compared
  size and mtime only, so it skipped the journal (0 files in `mig-s06` and `mig-s09`); the target's slot kept `len` 0
  while the restored read returned 56 (GATE.md "WP-57", `fabric/fuse-gate/mmap-rsync.sh`). Any request read into a
  slot after the last live rsync round is exposed the same way, and so is every application file on scratch written
  through a shared mapping. The fix is in the scratch move: its final delta, while the sandbox is stopped, compares
  contents (`rsync --checksum`), as the gate's `send-scratch` does now; sandboxd's `sync_scratch(last = true)` needs
  the same (16.1). With that, the journal keeps reading into the mapping. The reader's check stays: it checks that
  `len` equals the count `read(2)` returned, logs `fuse.short_read` with both and the journal file's own bytes at that
  slot, serves the file's copy if that one is whole, and never panics on a header.

### 9.10 Failure behaviour, summarised

| Situation | What Claude Code sees |
|---|---|
| magpie or the gateway unreachable, Postgres down | writes and uncached reads block until it is back (the agent's event loop may stall with them, 9.4). Cached reads through open fds keep working; stat, open and readdir of any path on the mount keep working only while no namespace operation waits on magpie: gVisor's FUSE filesystem (kernfs) holds one mount-wide lock for writing across MKDIR, RMDIR, UNLINK, RENAME, LINK, SYMLINK, MKNOD and OPEN with `O_CREAT`, and every path walk takes it for reading (WP-07 gate item 3: a `stat` waited 3.4-4.5 s behind a held RMDIR). So the supervisor answers every definitive case of a namespace operation from the tree without a network call (ENOENT, EEXIST, ENOTEMPTY: it is the only writer), and during a magpie outage every path lookup on the mount waits behind the first namespace operation that needs magpie |
| Bucket down | nothing: writes stage their blobs in Postgres (magpie 5.2); uncached reads of an unstaged blob block |
| One write keeps failing for 10 min while magpie answers other calls | EIO on that write; the generation is ended; the turn fails `cc.write_stuck` (9.5 item 4) |
| magpie quota (2 GiB per conversation) reached | EDQUOT on the write (Claude Code logs; the turn goes on); the supervisor logs and reports `quotaExceeded` in its heartbeat |
| A line over 64 MiB or a file over 256 MiB | EFBIG |
| Lease lost (a second sandbox for the conversation) | EIO; the generation is killed; the supervisor stays unready |
| Supervisor crash | operations pause for the restart (about 0.1-1 s), then complete exactly once (9.9) |
| Sandbox migration | operations pause for the checkpoint pause (84-580 ms measured, A5.F49) |
| Disk full on scratch (cache) | the cache stops growing (evicts to make room, else serves reads by streaming from magpie); never fails a write |

---

## 10. magpie's commands

### 10.1 The loop

`GET http://magpie/v1/sandbox/commands?after=<cmdAfter>&waitMs=25000` with `Aviary-Lease`, request timeout 40 s,
immediately repeated (a cut poll, for example by a gateway or magpie replica draining, is simply repeated; nothing
is lost because commands stay until acked, magpie 8.4). For each command in `cmdSeq` order: if `run.json.delivered`
already holds it as `acked`, ack again and skip; if it holds it as `written`, resume `start_turn` at step 5 (10.2);
else record it as `received` and execute it (several may run concurrently: a `title` beside a turn), and when its
outcome is known `POST /v1/sandbox/commands/{cmdSeq}/ack {"result": {…}}`. `cmdAfter` advances past a command once it
is recorded as received in `run.json` (so a restart re-delivers unacked commands but never re-executes an acked
one). An unknown `type` is acked `{"result": {"error": "unsupported"}}` (version skew, 21.3).

A received `start_turn` makes the sandbox busy at once (the activity report goes out immediately with `busy: true`,
15.3, and an unforced release answers `409 sandbox_busy`, 15.5), so hermit cannot release it between the command's
arrival and the turn's start.

### 10.2 `start_turn`

Payload (magpie 8.4): `turnId`, `text`, `files[]` (`uploadId`, `name`, `mimeType`, `size`, `workspacePath`, `blob`),
`images[]` (`uploadId`, `mimeType`, `workspacePath`, `blob`, `modelBlob`, `modelMime`), `resume.providerSessionId`,
`sentAt`, and for a parked turn's resumption `answers[]` (`askId`, `toolName`, `toolUseId`, `input`, `allow`,
`updatedInput`, `message`; DECISIONS 17): the user message is then the answers as 7.6 words them, not `text`.
1. Re-read hermit's descriptor (`If-None-Match`; a 304 is the normal case); if it changed and the run is `idle` with
   no tasks, rewrite the managed files and end the generation (6.4).
2. If the run is `running` (an agent turn) see 7.5; if `held`, supersede (7.7).
3. Materialise uploads (12.1). A failure (blob unreadable, workspace write refused) fails the turn: report `failed`
   `run.error` with the reason, ack `{"result": {"error": …}}`.
4. Ensure a generation (spawn with `--resume <providerSessionId>` when magpie names one, else `--session-id
   <conversationId>`; wait for initialisation).
5. Pick `msgId` = UUIDv5(`SUPERVISOR_OP_NS`, `bootId ‖ "stdin" ‖ cmdSeq`) and record `delivered[cmdSeq] = {state:
   "written", msg_id, gen, pending_inputs_after: pending_inputs + 1}` in `run.json` (atomic replace, at once) **before**
   handing the user message (12.2) to the keeper's `STDIN`. Then set `pending_inputs` from the record. On a replay
   that finds `written`, send the same message with the same `msgId` again: the keeper answers `ALREADY` if it had
   fully written it (4.2), in which case nothing is written twice, or writes it now if the crash came before the keeper
   had it all. Either way `pending_inputs` is the recorded value, never incremented again. So a supervisor crash at any
   point of this step makes Claude Code see the prompt exactly once. If the recorded `gen` is no longer alive at
   replay, the message is not re-sent to a new generation (the dead one may have written it to the transcript): the
   turn is reported `failed` `run.exited` as in 7.9.
6. Report `POST /v1/sandbox/turns/{turnId}/state {"state": "running", "providerSessionId", "transcriptPath":
   "projects/-workspace/<sid>.jsonl", "at"}` and ack `{"result": {"delivered": true}}`.

Steps 1-5 normally take well under a second with a live generation, about 2-3 s with a spawn (Claude Code start plus
a local-cache resume); magpie's 120 s `starting` watchdog bounds the worst case.

### 10.3 `interrupt`, `kill`

As 7.8. Ack when the outcome is reported.

### 10.4 `answer_ask`

Payload `{askId, allow, updatedInput?, message?}`. Unknown or already resolved `askId`: ack `{"result": {"error":
"no_such_ask"}}`. Otherwise answer the CLI (7.6), report `asks/{askId}/resolved {outcome: "answered"}`, ack.

### 10.5 `title`

Payload `{userPrompt, maxTokens}`. As section 14. Ack `{"result": {"response": <Messages API response JSON>}}` or
`{"result": {"error": "<reason>"}}` (magpie 8.5, 9.4).

### 10.6 `stop_tasks`

Payload `{}`. Sent by magpie when a cancel arrives while the conversation is `held` (magpie 8.6, pigeon 7.5.6). Stops
every task (7.7) and acks `{"result": {"stopped": n}}`.

---

## 11. Reports to magpie

All go directly to the fixed name `magpie` with `Aviary-Lease`, and are retried on 5xx, connection errors and
`409 transcript_pending` with backoff (0.5 s doubling to 10 s) **for as long as the lease is valid; no report is ever
dropped** (magpie 8.5). Reports of one turn are sent in order (a state report is not sent before the earlier ones of
the same turn were answered). If the lease is lost, the reports of the lease's turns stop (magpie ends those turns
through hermit's feed or its watchdog). As a backstop for a report lost against all odds (a crash between the event and
its record), every heartbeat carries the run state and `lastTurn`, and magpie reconciles its active turn against them
(11.3). The only exception to never-drop is live frames (8.3).

### 11.1 Turn state

`POST /v1/sandbox/turns/{turnId}/state` (magpie 8.5):

```json
{"state": "running"|"completed"|"failed"|"canceled"|"parked", "at": "…",
 "providerSessionId": "279aabf0-…", "transcriptPath": "projects/-workspace/279aabf0-….jsonl",
 "exitCode": 0, "error": {"code": "run.exited"|"run.error"|"run.oom"|"cc.write_stuck"|"sandbox.lost", "message": "…"},
 "held": false, "tasks": [{"taskId": "…", "toolUseId": "…", "type": "local_bash", "description": "…", "startedAt": "…"}],
 "lastUuid": "<uuid of the turn's last transcript message>"|null,
 "usage": {"inputTokens": 0, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0},
 "durationMs": 41230}
```

`parked` (7.6) is no end: the turn waits only on its open questions and its sandbox goes; magpie keeps it and its asks
and resumes it in a new sandbox when the person answers. `usage`/`durationMs` are informational (cost and budget stay
egress's); `durationMs` is working time, the time questions were open left out. A 409 `bad_transition` whose current state
equals the reported one is treated as success (idempotent replay after a restart); one whose current state is an end
state (magpie ended the turn first, through a `kill`, its watchdog or hermit's feed) is accepted as final and logged.
`409 transcript_pending` is retried (8.2). Once an end report is answered, `run.json.last_turn` records `{id, outcome,
error}` for the heartbeat.

### 11.2 Agent-started turns

`POST /v1/sandbox/turns {"turnId": "<uuidv7>", "origin": "agent", "at"}` (7.5), then state reports as above.

### 11.3 Heartbeat

`POST /v1/sandbox/heartbeat` every 15 s:
```json
{"turnId": "…"|null, "state": "running"|"stopping"|"held"|"idle", "tasks": 2,
 "lastTurn": {"id": "…", "outcome": "completed"|"failed"|"canceled", "error": {"code", "message"}|null} | null,
 "gen": 3, "supervisor": {"version": "supervisor-1a2b3c4d5e6f", "bootId": "…", "restarts": 0},
 "cc": {"quotaExceeded": false, "blockedWrites": 0, "oldestBlockedMs": 0}}
```
`lastTurn` is the last turn whose end report magpie answered (from `run.json`, so it survives a supervisor restart).
magpie uses `heartbeat_at` for its watchdog and reconciles its active turn against `turnId`, `state` and `lastTurn`
(magpie 8.5 rules 1-4): an end it never received is applied from `lastTurn`, a turn the supervisor no longer knows is
failed, an agent turn whose start was lost is opened. A parked turn whose `parked` report magpie has not answered is
still named, `running` (7.6). The other fields are for ops.

### 11.4 Tasks

`POST /v1/sandbox/tasks {"events": [{"id": "<gen>:<stdout offset>", "taskId", "toolUseId", "event":
"started"|"progress"|"updated"|"notification", "status", "description", "summary", "outputFile", "at"}]}` batched
every 250 ms. `id` makes a re-sent event idempotent (magpie `report_ids`).

### 11.5 Asks

`POST /v1/sandbox/asks {askId, toolName, toolUseId, input}` and `POST /v1/sandbox/asks/{askId}/resolved {outcome}`
(7.6).

### 11.6 Error codes the supervisor produces

| Code | When |
|---|---|
| `run.exited` | Claude Code exited during a turn without being asked (`message`: exit code/signal + stderr tail) |
| `run.oom` | the memory guard killed Claude Code (pigeon localises it) |
| `run.error` | a result that is an error not caused by a stop; an upload that could not be materialised; Claude Code would not start |
| `cc.write_stuck` | one transcript write kept failing for 10 min while magpie answered other calls (9.5 item 4) |
| `sandbox.lost` | a forced release (15.5) ended the turn; `message` = hermit's release reason (the same code magpie sets when hermit's feed reports the sandbox gone, magpie 8.7, so whichever commits first gives the same row) |

Model errors stay tags on the `cc.assistant` line Claude Code writes (`API Error: …`), classified by magpie (magpie
8.9).

---

## 12. Uploads: materialising files and building the prompt

### 12.1 Into `/workspace/uploads`

The supervisor is the only writer of uploads into the workspace: magpie keeps the bytes as blobs and never writes
`/workspace` (magpie 8.4, 10.3), and hermit's `PUT /v1/workspaces/{wid}/files` is not used for uploads (section 23,
hermit item 4). For each `files[]` entry and each `images[]` entry (the original) of a `start_turn`:
1. Create `/workspace/uploads` if missing (uid 1000, 0755); refuse (fail the turn) if `/workspace/uploads` or
   `/workspace` is a symlink.
2. `GET http://magpie/v1/sandbox/blobs/{sha}?as=raw`, streamed into `/workspace/uploads/.<name>.part-<rand>` opened
   `O_CREAT|O_EXCL|O_NOFOLLOW` as uid 1000 (the supervisor uses `setfsuid(1000)` for these writes, so ownership and
   the workspace quota apply as if the agent wrote it), verifying the sha256 while streaming.
3. `rename` onto `/workspace/<workspacePath>` (magpie assigned a unique `uploads/<name>`, magpie 8.2 step 4; an
   existing file of that name, e.g. written by the model, is replaced).
4. EDQUOT/ENOSPC fail the turn with `run.error` "the attachment does not fit in the workspace".

Model copies of images (`modelBlob`, prepared by magpie at upload, magpie 10.2) are fetched into memory only to be
base64-encoded into the message (12.2); they are not written to the workspace.

### 12.2 The user message

`content` is a string when there are no images, else an array: one `text` block, then one `image` block per image
(`{"type":"image","source":{"type":"base64","media_type": <modelMime>, "data": <base64>}}`; the Messages API's own
field names). The text is the
person's `text` followed by today's tags, byte-compatible with the importer's display fixes (magpie 19.3) and
CloudCLI's `appendFilesInputTag`/`appendImageUploadsTag` (`claudecodeui/server/shared/image-attachments.ts:234-345`):

```
<text>

<files_input>
The user attached N file(s) to this message. Read each file listed below with your file reading tools and use its contents to answer the prompt above. Do not mention this block or the file paths unless the user asks about them.
1. /workspace/uploads/Q3 report.pdf (original name: Q3 report.pdf)
</files_input>

<image_uploads>
The N image(s) attached to this message, which you can see, are also saved as files (the originals, full size):
1. /workspace/uploads/IMG_0042.jpg (original name: IMG_0042.jpg)
Use these paths when you need the image files themselves (to crop, convert, measure, read metadata or put them in a document). Do not mention this block unless the user asks about it.
</image_uploads>
```

Names have `(`, `)`, CR and LF removed in the `original name` suffix, as today. The message is one NDJSON line
(images inline: at most 10 × 3.9 MB model copies ≈ 52 MB base64, magpie 17); the keeper relay has no frame limit, so
today's 16 MiB `/proc` frame kill (A11 surprise) cannot happen.

---

## 13. The SendUserFile hook (sent-file snapshots)

Managed hook (6.4): `/usr/local/bin/supervisor hook send-user-file`, run by Claude Code as uid 1000 with the hook
JSON on stdin (`{tool_name, tool_input: {files: [...], caption?}, tool_use_id, cwd, session_id, transcript_path}`).

The hook process:
1. Exit 0 without output if `tool_name != "SendUserFile"`, no files, or no `tool_use_id` (as today, A5.F17).
2. Resolve each path against `cwd` (default `/workspace`), strip `file://`, normalise; refuse any path not under
   `/workspace` and any path with a symlink component (opens each component with `O_NOFOLLOW` via `openat2` with
   `RESOLVE_NO_SYMLINKS|RESOLVE_BENEATH` from an fd of `/workspace`). It opens each file read-only as itself (uid
   1000), so permissions are the agent's.
3. Sends `{toolUseId, caption, files: [{path, size}]}` plus the open fds (SCM_RIGHTS, at most 100) to
   `/run/supervisor/agent.sock` `POST /v1/hooks/send-user-file` (HTTP over the unix socket, the fds on the first
   message), waits up to 110 s.
4. Exit 0 on 200; otherwise print to stderr `The files were not sent: they could not be saved for the user (<reason>).
   Try sending them again.` and exit 2, which blocks the tool so the model knows (today's text and behaviour).

The supervisor, for each fd: computes size and sha256 by reading it (256 KiB buffer), guesses `mime` from the
extension table shared with magpie, then runs magpie 11.2: `POST /v1/sandbox/sent-files` → `missing`; for each missing
sha, `PUT /v1/sandbox/blobs/{sha}` streaming the fd again from offset 0 (if magpie answers `sha_mismatch` the file
changed between passes: one full retry, then fail with "the file changed while it was being sent"); `POST
/v1/sandbox/sent-files/{tool_use_id}/commit`. Retries 5xx with backoff within the 110 s. Limits are magpie's (100
files, 64 MiB each, 256 MiB total; refused before uploading).

This replaces the egress `sent-files` route, `JARVIS_SENT_FILES_URL`, the egress token as hook credential and the
router's JuiceFS clone (A3.F17, A5.F17, gap-2 2.17).

---

## 14. Titles

On a `title` command `{userPrompt, maxTokens}`:
`POST http://model/v1/messages` with `{"model": <title from /v1/aviary/models>, "max_tokens": <maxTokens>, "system":
<SandboxSelf.title.system>, "messages": [{"role": "user", "content": <userPrompt>}]}`, header
`anthropic-version: 2023-06-01`, timeout 60 s, response capped at 1 MiB. The ack carries the response JSON as is
(magpie extracts the title, magpie 9.4); errors ack `{"result": {"error": "<status> <first 500 bytes>"}}`. The supervisor holds no
title logic; this replaces today's `node -e` script over `/proc` (A11.F18, A1.F16, A5.F15). The title instructions
are hermit's instance content (magpie 21.1 decision 7; YYC: a file in `YYC/yyc-ai/deploy/hermit/`, delivered in
`SandboxSelf.title.system`, 15.1); the title model id comes from the `model` endpoint (17.3).

---

## 15. hermit: what the supervisor consumes and the lifecycle API it serves

The shapes in this section are the one contract between the two: they are the types of `hermit-api` (surface
`hermit-sandbox`: `SandboxSelf`, `ActivityReport`, `ActivityReply`) and `supervisor-api` (surface `supervisor-control`:
`Health`, `ReleaseRequest`, `ReleaseAnswer`, `PrepareRequest`), camelCase, and both specs quote them unchanged
(section 23, hermit item 1 lists what hermit 3.7, 3.10, 5, 11.2, 12 and 13.12 must change to match).

### 15.1 `SandboxSelf` (consumed): `GET http://hermit/v1/sandbox/self`

Sent directly to the fixed name with the sandbox token (17.1); the gateway identifies the sandbox and forwards with
`Aviary-Sandbox-*` (platform 3.5). `If-None-Match: <etag>` → 304 when unchanged. 200, at most 64 KiB:

```json
{"sandbox": {"id": "…", "conversationId": "0190f6c2-…", "person": "staff@yyc.hk",
             "createdAt": "…", "expiresAt": "…", "image": "sha256:…"},
 "control": {"port": 7070, "tokenSha256": ["9f…", "c1…"]},
 "claude": {"managedClaudeMd": "…the deployment's managed CLAUDE.md text…"},
 "title": {"system": "…the deployment's title instructions…"},
 "env": {"HTTP_PROXY": "http://sbx:hst_…@10.202.1.1:3128", "HTTPS_PROXY": "…", "http_proxy": "…", "https_proxy": "…",
         "PIP_INDEX_URL": "…", "PIP_TRUSTED_HOST": "…", "npm_config_registry": "…"},
 "workspace": {"path": "/workspace", "quotaBytes": 1073741824, "quotaFiles": 1000},
 "limits": {"memoryBytes": 1073741824, "cpus": 1, "pids": 4096, "tmpBytes": 5368709120, "openFiles": 65536},
 "lifecycle": {"activityEverySeconds": 15, "idleMinutes": 30, "maxLifetimeHours": 8, "lifetimeGraceMinutes": 60},
 "secrets": {"keys": []},
 "etag": "\"b41c…\""}
```

- `control.tokenSha256`: the SHA-256 (hex) of every control token hermit may present on :7070, a list so a key
  rotation can run with two (hermit 6.2). The supervisor compares `sha256(presented bearer)` with each, in constant
  time; the plaintext control token never enters the sandbox.
- `claude.managedClaudeMd`: written to `/etc/claude-code/CLAUDE.md` (6.4). YYC: the instance prompt plus the sandbox
  rules hermit renders from what it provisions (hermit 3.7.1).
- `title.system`: the title instructions for the `title` command (14). YYC: hermit renders it from its deployment
  folder (`YYC/yyc-ai/deploy/hermit/title-system.md`, the text of today's `config.json` title prompt). hermit 3.7.1
  must add this field (23).
- `env`: the environment every process the agent starts must have, applied by the supervisor unchanged (6.2). The
  supervisor's own calls never use it (17.1). At most 128 entries and 16 KiB.
- `limits.memoryBytes` is `L` for the memory guard (5.3); `workspace.quotaBytes` only feeds the managed text, which
  hermit renders; `lifecycle.activityEverySeconds` is the activity cadence (15.3; the supervisor uses `min(value,
  15)`); the other lifecycle values are informational (logged; hermit applies its own policy).
- `secrets.keys`: the keys this sandbox may ask for (YYC: empty, a valid answer).
- `etag`: hermit's hash of the rendered body (also the `ETag` header). It changes when hermit renders something
  different (a prompt edit after a hermit roll, a rotated control token). The supervisor re-reads at start, at every
  `start_turn` and every 10 min; a changed `managedClaudeMd` is applied at the next idle point (6.4), a changed
  `tokenSha256` at once, a changed `env` at the next generation start.

### 15.2 Secrets (consumed, handed out per key)

Processes in the sandbox ask the supervisor, never hermit directly:
- `sandbox-secret <KEY>` (the supervisor binary under that name) prints the value to stdout, or exits 3 with
  `not granted` on stderr.
- Over `/run/supervisor/agent.sock` (mode 0666): `GET /v1/secrets/{key}` → `200 {"key","value"}` | `404
  {"code":"not_granted"}`.
The supervisor answers each ask with `GET http://hermit/v1/sandbox/secrets/{key}` at that moment (no cache, so a
revocation takes effect at once), logs the key (never the value), and never writes a value to disk or environment.
YYC grants nothing, so every ask is `not_granted`.

### 15.3 `ActivityReport` (consumed by hermit; replaces `/internal/cc/touch`): `POST http://hermit/v1/sandbox/activity`

Every 15 s and at once on every change of `busy` or `state`:
```json
{"busy": true, "state": "running"|"stopping"|"held"|"starting"|"asking"|"idle"|"released",
 "turnId": "…"|null, "backgroundTasks": 2, "idleSince": "…"|null, "lastOutputAt": "…",
 "bootId": "…", "protocol": 1, "version": "supervisor-1a2b3c4d5e6f",
 "supervisor": {"restarts": 0, "rssBytes": 31457280, "ready": true},
 "memory": {"limitBytes": 1073741824, "usedBytes": 612368384},
 "events": [{"kind": "memory_kill", "pid": 912, "comm": "python3", "rssBytes": 402653184, "at": "…"}]}
```
- `busy` is derived from `state`: true for `starting` (a `start_turn` received and not yet running, 10.1), `running`,
  `stopping` and `held`; false for `asking` (the run waits only on a person's answer, 7.6), `idle` and `released`.
  hermit's idle clock counts from the last busy report (hermit 3.7.3); `idleSince` is the supervisor's own view, for
  ops (in `asking`: when the question opened).
- `events` carries what happened since the last accepted report (memory-guard kills, generation exits); a report that
  fails is retried with the events kept, at most 32 events.
- `protocol` is the `hermit-sandbox` number the supervisor speaks (platform 3.4).

`ActivityReply`, 200:
```json
{"state": "ready"|"releasing",
 "release": null | {"reason": "max-lifetime", "code": "sandbox_released", "force": false, "deadlineAt": "…"}}
```
A non-null `release` repeats a pending orderly release in case hermit's direct call (15.4) was lost. The supervisor
acts on it exactly as on `POST /v1/lifecycle/release` with the same fields (15.5): unforced and busy → it keeps going
and keeps reporting `busy: true` (hermit then decides, 15.5); otherwise it runs the release sequence and reports `state:
"released"` when done, which tells hermit it may destroy the sandbox.

This replaces the 60 s `/internal/cc/touch` from the gateway pod (A3.F1, A11.F16, A9.F16) with the owner's own
knowledge: a hung or held sandbox is visible as such, and an idle one is not kept alive by an open socket.

### 15.4 The supervisor's API on :7070 (served to hermit)

Plain HTTP on `[::]:7070` inside the sandbox (hermit's network decides who can reach it; YYC's per-sandbox nft lock
already admits only the control CIDR to :7070, A5.F34). **Every route** needs `Authorization: Bearer <control token>`
with `sha256(token)` in `control.tokenSha256`; otherwise 401 `unauthenticated`. Until `SandboxSelf` has been read
(4.3 step 2) the supervisor knows no hash and answers every call 503 `unavailable` with `retry-after: 1`. Bodies are
camelCase; errors use platform's envelope `{"code", "message", …}`. Requests carry `Aviary-Protocol:
supervisor-control=<n>`.

| Route | Request | Response |
|---|---|---|
| `GET /v1/health` | | `200 {"ok": true, "ready": bool, "protocol": 1, "version": "supervisor-<sha12>", "bootId": "…", "busy": bool, "state": "<as 15.3>"}` |
| `GET /v1/state` | | `{"run": {state, turnId, origin, tasks[], asks[]}, "gen": {n, pid, startedAt} \| null, "leaseEpoch", "fuse": {"inflight", "blocked", "oldestBlockedMs"}, "memory": {limitBytes, usedBytes}, "supervisor": {version, bootId, restarts, rssBytes}, "magpieReachable": bool}` |
| `POST /v1/lifecycle/prepare` | `{"kind": "pause"\|"snapshot"\|"migrate", "force": false, "deadlineMs": 10000}` | `200 {"ready": true}` or `409 {"code": "sandbox_busy", "message", "run": {state, turnId, tasks}}` |
| `POST /v1/lifecycle/resumed` | `{"kind": "pause"\|"migrate", "at": "…"}` | `204` |
| `POST /v1/lifecycle/release` | `{"reason": "<hermit's reason, free text ≤ 200>", "code": "sandbox_released"\|"sandbox_lost", "force": bool, "deadlineMs": 30000}` | `200 {"released": true}` (after the sequence of 15.5 has finished) or `409 {"code": "sandbox_busy", "message", "run": {state, turnId, tasks}}` (nothing done) |
| `POST /v1/exec` | `{"argv": [...], "cwd": "/workspace", "stdin": "<base64>", "timeoutMs": 60000, "env": {}}` | `{"exitCode", "signal", "stdout": "<base64>", "stderr": "<base64>", "truncated": bool}` |

`/v1/health` semantics, which hermit must read this way (23):
- `ok: true` = the process answers; `ready` = `SandboxSelf` read, lease held, FUSE mounted, `/workspace` present.
- `ok: true, ready: false` = starting (or waiting for a lease, 4.3 step 5): hermit's starter keeps polling and never
  treats it as unhealthy; it marks the sandbox `ready` only on `ready: true`.
- No answer within 2 s is not proof of death: a keeper restart of the supervisor takes up to 5 s (4.2). hermit
  decides "unhealthy" only from two failed checks 5 s apart, as its janitor does.
- `busy` is the same value the activity reports carry.

`/v1/exec` exists for hermit's operator tooling and acceptance checks (isolation probes, A5.F15 and gap-2 2.13): it
runs as uid 1000 (never root), argv non-empty without NUL, cwd `/workspace` or below, stdin ≤ 1 MiB, timeout ≤ 60 s,
stdout/stderr ≤ 1 MiB each, at most 2 at once. It is not used by any product path.

### 15.5 Lifecycle state machine

```
 booting ──(ready)──> serving ──(prepare pause|snapshot, idle or force)──> quiesced ──(resumed | clock jump)──> serving
                          │                                                     │
                          │ (prepare migrate) ──> serving (no change)            └─(release)─┐
                          └──────────(release: not busy, or force)─────────────────────────────> released ──> exit 0
                          (release, busy, not forced) ──> 409 sandbox_busy, stays serving
```

- **prepare `migrate`** (live migration, 16.1): answers `ready` at once; nothing to do. Optional for hermit.
- **prepare `snapshot`** (a hermit implementation about to copy `/workspace` of a live sandbox): `sync(2)` so every
  `/workspace` write the sandbox made has reached its filesystem; answer `ready`. Never refuses. YYC does not call it
  (its snapshot is 501, and share freezes clone the workspace, 16.3).
- **prepare `pause`** (a checkpoint that may be restored much later, possibly elsewhere): refuses `sandbox_busy` while
  busy (15.3) or an ask is pending (`asking` included: a question is never waited out in a paused sandbox, DECISIONS
  17), unless `force`. Otherwise: stop the command long-poll, wait up to `deadlineMs` for
  in-flight FUSE operations and reports to finish (a FUSE write still blocked on magpie at the deadline answers
  `sandbox_busy` too; with `force` it is left blocked and simply resumes after restore), close every pooled HTTP
  connection so none is half-open after a long pause, then answer `ready`.
- **resumed**: re-open connections, re-read the descriptor, re-take the lease if magpie answers `lease_lost` (a pause
  without release keeps the lease; magpie clears it only on hermit's `gone`), re-load the tree, revalidate the content
  cache (9.7), restart loops. If hermit restores without calling it, the supervisor detects a wall-clock jump
  (CLOCK_REALTIME advanced more than 30 s beyond CLOCK_MONOTONIC across one loop tick) and does the same by itself.
- **release**:
  - **Unforced** (hermit's idle release, pool victim, soft lifetime at `expiresAt`, a lost-call repeat): if busy
    (`starting`, `running`, `stopping`, `held`, or an ask open beside other work), answer `409 sandbox_busy` and
    change nothing; the turn goes on. A run that is `asking` (7.6) is released: its turn is parked, not ended. hermit treats 409 as "not released": the row goes back to `ready` and it tries again later
    (section 23, hermit item 2). This is what makes "the lifetime and idle policies never kill a running turn"
    (gap-0 14.2, A16.F5) true rather than advisory.
  - **Forced** (hermit's hard lifetime after `lifetimeGraceMinutes`, conversation deleted, person offboarded or no
    longer allowed, operator release or recycle; also SIGTERM from the runtime, 4.7): an `asking` run's turn is
    parked (7.6) as for an unforced release; otherwise, if a turn runs, interrupt it
    (7.8, at most 15 s, within `deadlineMs`) so the transcript ends cleanly, and report it `failed` with
    `{"code": "sandbox.lost", "message": "released: <reason>"}` (11.6); stop tasks.
  - Then, in both cases: close Claude Code's stdin (not for a parked turn's generation, 7.6), wait 5 s, SIGTERM its
    group, 3 s, SIGKILL; wait for every
    in-flight FUSE operation and report to be acknowledged (until `deadlineMs`); final heartbeat `idle` and activity
    `released`; unmount; answer 200 `{"released": true}`; exit 0. hermit destroys the sandbox only after this answer
    or after `deadlineMs` + 5 s (section 23, hermit item 2). Everything that matters is already in magpie; local scope
    and caches die with the sandbox by design.
- A second release call while one runs waits for the same outcome (idempotent).

## 16. Live migration, pause and snapshot (gVisor checkpoint/restore)

### 16.1 Live migration (hermit's YYC implementation: pre-copy `runsc checkpoint --migrate-to` / `restore`)

What the supervisor relies on, all of which today's migration already preserves (A5.F49): PIDs and memory (the
keeper, the supervisor, Claude Code and its tools continue mid-instruction), TCP connections in gVisor's netstack (the
supervisor's connections to the gateway, Claude Code's model stream; the sandbox IP and gateway conntrack follow),
the scratch disk (overlay upper, `/var/lib/supervisor` with its journal, cache (kept small for this reason, 9.7), state
and local scope; rsynced with mtimes to the nanosecond, the final delta while stopped comparing contents
(`--checksum`), because writes through a shared mapping, the journal's included, do not change a file's mtime: WP-57), `/run/supervisor` (tmpfs, in the checkpoint), and the in-sandbox FUSE connection (section
2 item 4). A FUSE request blocked in the supervisor during the stop completes after the restore, but its caller does
not get that reply: the sentry forgot the request at the stop and resends it under a new `unique` (9.3, resends;
measured by the WP-07 gate on every migration with a request in flight). Nothing in
`~/.claude` is a host file any more, so the "runsc delete closes files so JuiceFS commits them" step only concerns
`/workspace` (A8.F30, A18.F22).

The supervisor needs no notice; hermit may send `prepare migrate` and `resumed migrate` for logging symmetry. The one
case to handle: a migration paused long enough for magpie or the gateway to drop a connection (a cross-cloud move
aborted late): requests on that connection fail and are retried (9.5, 11), so nothing is lost.

### 16.2 Pause (a stored checkpoint, restored later)

`prepare pause` → hermit `runsc checkpoint` to an image → … → `runsc restore` → `resumed pause`. Only idle sandboxes
are paused unless hermit forces it. After a long pause every TCP connection is dead (the peers timed out); the
supervisor closed its own before the checkpoint and reconnects after. Claude Code's model connection is idle when
idle; had a forced pause cut one, Claude Code's API retry handles it.

### 16.3 Snapshot

In YYC hermit's sandbox snapshot stays 501 (`capabilities.snapshot` false, hermit 3.6.4), and share freeze and
"continue from a share" use only `POST /v1/workspaces/{wid}/clone` (platform R28, magpie 21.1 decision 1); neither
involves the supervisor. `prepare snapshot` exists in the contract for a hermit implementation that snapshots live
sandboxes; its only effect is the `sync(2)` of 15.5.

### 16.4 What the fork must keep doing (test gate, 22)

Restoring a sandbox whose `/dev/fuse` connection has requests queued and one request being served; the keeper's
SEQPACKET socket and the inotify watch on the stdout file across restore; `CLOCK_MONOTONIC` continuity (gVisor
restores it; the clock-jump detection uses REALTIME minus MONOTONIC); after a pre-copy migration, memory a process
freed or zeroed before the stop reads as zero (fixed in `precopy-20261102.1`; `fabric/fuse-gate/stale-mig.sh` checks
it, WP-57).

---

## 17. Network: egress use and the fixed endpoint names

### 17.1 Endpoints

| Name | Used by | For |
|---|---|---|
| `magpie` | supervisor | `/v1/sandbox/**` (lease, cc files, commands, reports, blobs, sent files; live frames later) |
| `hermit` | supervisor | `/v1/sandbox/self`, `/v1/sandbox/secrets/{key}`, `/v1/sandbox/activity` |
| `model` | Claude Code (Messages API), supervisor (titles, `/v1/aviary/models`) | the model |
| `tools` | Claude Code (MCP over HTTP at `/mcp`) | read-only tools |
| internet | Claude Code's tools (pip, npm, apt, curl…) | whatever the gateway lets through |

**The supervisor's own calls** go to `http://<name>/…` directly: the names resolve inside the sandbox to the sandbox
gateway on port 80 (hermit 5.1 item 3; YYC: sandboxd's `/etc/hosts` maps them to the egress VIP, which forwards to
egress's fixed-name listener 8081). Each call carries the sandbox token as `Proxy-Authorization: Basic
base64("sbx:" + token)`, the form the gateway contract accepts on its fixed-name listener and on its proxy port alike
(platform 8.2), plus `Aviary-Protocol` and `User-Agent: supervisor/<sha12>`. HTTP/1.1 keep-alive, at most 8 pooled
connections per name, connect timeout 10 s; a response with `Connection: close` (a draining gateway replica) just
retires that connection. The supervisor never uses the proxy in `SandboxSelf.env` and never reaches the internet; it
hard-codes no address (platform R13: it knows only the four names, and its token from `HERMIT_SANDBOX_TOKEN`).

**Claude Code's calls** to `model` and `tools` travel through the proxy that `SandboxSelf.env` names, as absolute-form
`http://model/…` and `http://tools/mcp` requests authenticated by the proxy credential in that URL (17.2). In a
deployment whose `env` has no proxy they go directly to the names, exactly like the supervisor's (the gateway then
authenticates them by source address alone where it can, which is that implementation's business; YYC always has the
proxy).

The gateway verifies the sandbox, routes the name, strips client `Aviary-*`/`Authorization`, adds the verified
`Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation` and `Aviary-Sandbox-Person` (the owner's lowercased email) and its
own pair bearer for the callee, and holds the model credential (platform 3.5, 17.2). What the supervisor needs from it
beyond routing (hot-path latency, no per-request log rows for `/v1/sandbox/**`, drain rules) is section 23, gateway.

### 17.2 Claude Code and the proxy

Claude Code honours `HTTP(S)_PROXY` for its API and MCP HTTP traffic; `ANTHROPIC_BASE_URL=http://model` and the MCP
URL `http://tools/mcp` therefore travel to the gateway as proxied absolute-form requests and never need DNS (today the
sandbox has no DNS at all: its only allowed destination is the proxy port, A5.F34). The image smoke test proves both
paths through a test gateway (22); if a future Claude Code bypassed the proxy for either, the test fails before
release. A model stream cut by a gateway replica's drain is a connection error to Claude Code, which retries the
request on a new connection (its own API retry, shown as `api_retry`); the gateway's drain rules (23) make that rare.

### 17.3 Model facts

`GET http://model/v1/aviary/models` → `{"default": "<id>", "small": "<id>", "title": "<id>", "contextTokens":
200000, "models": [{"id", "label", "reasoning"}]}` (platform R18; the supervisor reads `default`, `small`, `title`,
`contextTokens`). Read at each spawn and each `title`. This keeps model choice out of the supervisor and out of
hermit's descriptor: the model list, defaults and reasoning policy are the gateway's (gap-2 5.4; A1.F12, A7.F6).

---

## 18. What replaces today's in-sandbox agent on :7070

| Today (agent.mjs, sbx-init.mjs, send-file-hook.mjs) | Target |
|---|---|
| `GET /livez`, `GET /healthz {ok, boot_id}` (A5.F10) | `GET /v1/health {ok, ready, protocol, version, bootId, busy, state}` (bearer) |
| Exec token from `JARVIS_EXEC_TOKEN` in the environment (A5.F11) | control-token hashes from hermit's descriptor; no token in the sandbox |
| `GET /proc` upgrade, framed stdin/stdout/stderr/exit/signal, process dies with the socket (A5.F12, A5.F13, A11.F1, A11.F2) | no process transport at all: Claude Code is the keeper's child, driven by the supervisor; output reaches magpie as transcript writes and reports |
| `runInSandbox` short commands for titles (A5.F15) | the title call goes straight to `model` (14) |
| (debugging by `/proc` from a ui pod, gap-2 2.13) | `POST /v1/exec` for hermit's tooling |
| `POST /init` and the wire proxy (e2b, A5.F16) | dropped (YYC runs one provider; another runtime is hermit's implementation) |
| sbx-init: managed files from env, `/etc/hosts`, apt proxy, setpriv, docker start socket (A5.F7-F9, A5.F2) | the supervisor's start (4.3) and 4.6 |
| send-file-hook via egress `/x/sent-files` (A5.F17) | `supervisor hook send-user-file` → local socket → magpie (13) |
| touch every 60 s from the ui pod (A3.F1) | activity reports to hermit (15.3) |
| files panel reading the transcript / JuiceFS in the ui pod | magpie's own rows; live workspace reads via hermit (magpie 11) |
| shell | none (no client; Paseo terminals unsupported) |

---

## 19. Failure modes

| Failure | Supervisor behaviour | What staff see |
|---|---|---|
| pigeon or magpie replica rollout / crash | nothing changes in the sandbox; a cut command long-poll is repeated, requests in flight are retried on another replica with the same `opId` | nothing (fixes A16.F0, A16.F1, A16.F2) |
| gateway (egress) replica rollout | the replica stops taking new connections and answers `Connection: close` on kept-alive ones, then lets in-flight requests and model streams finish up to its drain bound (23, gateway item 3); the supervisor's FUSE writes, reports and long-polls that are cut anyway are retried on a new connection with the same `opId` (the poll is simply repeated); a model stream cut at the bound is retried by Claude Code's own API retry | nothing in the normal case; a reply longer than the drain bound restarts its model request (an `api_retry`), never an `API Error: Connection error.` row unless Claude Code's retries run out (A16.F8, egress's to fix, 23) |
| magpie down (Postgres, all replicas) | transcript writes block (the agent's event loop may stall with them, 9.4); reports and heartbeats retry without dropping | reconnecting; turns are not failed for silence magpie caused (magpie 8.8 measures silence from `availableSince`); when magpie returns the writes land, the heartbeat reconciles, and the turn finishes |
| gateway down (every replica) | as magpie down for the supervisor; Claude Code's model calls fail and it retries (`api_retry`) | the gateway is a single point of failure of its implementation (egress runs 2 replicas, `maxUnavailable: 0`) |
| One magpie op fails for 10 min while magpie answers everything else | EIO on that op, generation ended, turn `failed` `cc.write_stuck`, magpie marks the conversation for resync (9.5 item 4) | an error row; the next send works and repairs history (magpie 6.12) |
| Supervisor crash | keeper restarts it in ≤ 5 s; Claude Code keeps running; FUSE requests wait in the sentry and the journal replays the ones already read (9.9); a half-delivered prompt is never sent twice (10.2); stdout tail resumes | a short pause |
| Claude Code crash mid-turn | turn `failed` `run.exited` with exit code and stderr tail; next turn resumes | an error row instead of today's silent cut-off |
| Tool process eats memory | memory guard kills it; the model sees a killed command | nothing, or the model explains |
| Claude Code itself too big | guard kills it; `run.oom` | an error row; the sandbox survives |
| Sandbox OOM anyway (guard too slow even with hermit's host-side `memory.high` brake, 5.1) | — | hermit's host heartbeat reports it gone within 10 s (hermit 4.2) → magpie ends the turn `sandbox.lost` (A16.F6) |
| Host loss | — | same: hermit `gone` event → magpie ends the turn (no 15-min TCP wait) |
| Live migration | invisible (0.1-0.6 s pause) | nothing (A16.F7) |
| Network blip sandbox ↔ gateway | retries; nothing is tied to a connection | nothing (fixes A16.F13) |
| Stop while the link is dead | interrupt is local; if magpie cannot deliver the command, magpie ends the turn itself after 30 s and the supervisor applies the queued `interrupt`/`kill` when the link returns | Stop takes effect within 30 s at most (fixes A16.F14) |
| hermit's idle, pool or soft-lifetime release while busy | `release` answers `409 sandbox_busy`, nothing changes; hermit puts the row back to `ready` | nothing (A16.F5) |
| hermit's forced release during a turn (hard lifetime, deletion, offboarding, operator) | interrupt, report `failed` `sandbox.lost` with the reason, orderly stop, then 200 | an error row, no endless spinner |
| A release decided just as a turn is admitted | the received `start_turn` already makes the sandbox busy (10.1), so an unforced release gets 409; the gap before the command arrives is closed by hermit's reservation at `PUT` (23, hermit item 2) | nothing |
| Two sandboxes for one conversation | only hermit's current sandbox gets the lease; the other gets `lease_lost`, kills its agent, and waits with `not_current_sandbox` without restarting | one consistent history |
| Lost lease while magpie still runs the turn | magpie's watchdog or hermit's feed ends it | an error row |
| Claude Code hangs (no output, no exit) during a turn | none by the supervisor (a long tool call is legitimate; Bash has its own 10 min cap); Stop works (interrupt, then kill after 15 s) | the person can always stop |
| Supervisor bug: unknown stdout message shape | logged once, ignored | nothing |
| Descriptor unavailable at start | not ready; hermit's start watchdog decides | the preparing chip, then hermit's failure code |
| `/workspace` missing at start | not ready, log `workspace missing` | same |
| A tools call longer than the gateway's route timeout | the MCP call fails as a tool error; the model may retry | a tool error (A16.F9: the gateway's tools route timeout must be at least tools' own 150 s drain, 23) |

---

## 20. Limits and constants (`limits.rs`)

| Constant | Value |
|---|---|
| `MEM_RESERVE` (high watermark below the limit) | 128 MiB |
| Memory sampling | 500 ms, 100 ms above 80 % |
| Minimum victim RSS | 32 MiB |
| Kill cooldown | 2 s |
| Supervisor RSS budget (soak gate) | 64 MiB (target 40) |
| Keeper RSS | ≤ 4 MiB |
| FUSE `max_write` / `max_pages` | 1 MiB / 256 |
| FUSE slots (requests in flight in the supervisor) | 64 |
| FUSE attr/entry validity | 1 s |
| Journal | 64 slots × (1 MiB + 8 KiB), `MAP_SHARED`, requests read straight into it |
| magpie call concurrency | 16 |
| magpie per-attempt timeout (cc ops) | 30 s |
| cc write retry | 0.2 s doubling to 10 s, unbounded under general unavailability |
| `STUCK_OP` (one op failing while heartbeats succeed) | 10 min → EIO, `cc.write_stuck` |
| Report retry | 0.5 s doubling to 10 s, never dropped while the lease is valid |
| Lease retry on `not_current_sandbox` / `conversation_migrating` | every 30 s, no exit |
| Command poll | `waitMs=25000`, request timeout 40 s |
| Heartbeat to magpie | 15 s |
| Activity to hermit | on change and every 15 s |
| Keeper stdin dedupe ring | last 16 `msg_id`s per child |
| Descriptor refresh | at start, each `start_turn`, every 10 min |
| Live frame batch (later release) | 50 ms or 4 KiB; drop frames older than 5 s on outage |
| Turn-end flush wait | none: until every uuid of the turn is stored (8.2) |
| Initialisation wait | 30 s |
| `INTERRUPT_GRACE` | 15 s, then SIGTERM, 3 s, SIGKILL |
| Graceful generation end | close stdin, 5 s, SIGTERM, 3 s, SIGKILL |
| `HOLD_SILENCE` | 30 min |
| ScheduleWakeup pseudo-task slack | 2 min |
| Supersede wait for stopped tasks | 5 s; cancel-during-hold wait 10 s |
| `ASK_TIMEOUT` | 7 days (a safety limit: a waiting question holds no sandbox, 7.6) |
| Crash loop | 3 exits in 60 s → refuse for 5 min |
| Content cache (`CACHE_BUDGET`) | 256 MiB on scratch, LRU, plus the main transcript outside the budget, never evicted |
| stdout file rotation | at an idle point once over 1 GiB (graceful generation end) |
| stdout line handling | 64 KiB read buffer; lines over 64 MiB scanned, not parsed |
| stderr copy to log | 100 lines/s; 4 KiB tail in errors |
| Hook wait / snapshot upload budget | 110 s (hook timeout 120 s) |
| Title call | 60 s, 1 MiB |
| `/v1/exec` | uid 1000, 60 s, stdin/stdout/stderr 1 MiB each, 2 concurrent |
| Keeper restart backoff | 0.1, 0.2, 0.5, 1, 2, 5 s (reset after 60 s up) |
| Keeper stop grace | 20 s |
| Clock-jump detection | REALTIME − MONOTONIC change > 30 s |

---

## 21. Logging, metrics, versioning

### 21.1 Logs

JSON lines on stderr (the keeper's stdout, i.e. the sandbox's console log; YYC: `/var/log/sbx/<name>.log` on the
host, A5.F50): `{"t", "level", "msg", "conversation", "sandbox", "turn"?, "gen"?, …}`. Stable messages: `supervisor
started` (version, boot_id, restart), `lease taken` (epoch), `fuse mounted`, `claude started` (gen, pid, resume),
`claude exited` (gen, code, signal), `turn started`/`turn ended` (turn, origin, outcome, ms), `hold started`/`hold
ended` (tasks, reason: tasks done | supersede | stopped | silence ceiling) (A9.F20), `interrupt sent`/`interrupt
forced kill`, `memory guard killed`, `magpie unavailable` (first failure, then every 60 s with the number of blocked
operations), `lease lost`, `unclassified path`, `lifecycle` (kind, answer). Never logged: file contents, prompts,
secret values, tokens.

### 21.2 Metrics

No scrape endpoint inside the sandbox; the numbers ops need travel in the heartbeat (11.3), the activity report
(15.3) and `GET /v1/state` (15.4).

### 21.3 Versioning and skew

- The supervisor's version is `supervisor-<aviary sha12>`, reported in heartbeats, activity, `/v1/health` and the
  `User-Agent` of every call (`supervisor/<sha12>`). Every call carries `Aviary-Protocol` with the surface it speaks
  (`magpie-sandbox=<n>` to magpie, `hermit-sandbox=<n>` to hermit); the :7070 API is surface `supervisor-control`
  (platform 3.4).
- A running sandbox keeps the supervisor it booted with until released (no in-place upgrade; a new image reaches new
  sandboxes). magpie and hermit must therefore accept the previous supervisor release's calls for as long as sandboxes
  live (hermit's max lifetime, today 8 h); new fields are additive, unknown command types are acked `unsupported`,
  unknown response fields are ignored (A15.F17).
- Claude Code's version is pinned in the image; the scope table (9.1) and the stream-json handling are reviewed and
  the acceptance suite rerun at every bump.

---

## 22. Tests and acceptance

Gate before building beyond a spike (on the DE0CH/gvisor `precopy-20261021.2` runsc, the YYC sandbox image, one AWS
host):
1. `/dev/fuse` present; the supervisor mounts `~/.claude` as root inside the sandbox; uid 1000 reads and writes.
2. Claude Code 2.1.304 runs a real turn with its transcript on the mount: append, torn-tail repair, a tombstone
   rewrite and a compaction rename (forced with `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1` on a > 5 MiB transcript), a
   subagent transcript (link fallback); bytes in magpie equal the bytes Claude Code wrote.
3. Live migration with a write blocked in the supervisor (fake magpie holding the response) and Claude Code mid-turn:
   the turn completes on the target; FUSE still works; the inotify tail continues. Repeated with a full content cache
   (256 MiB plus an 87 MB main transcript): the final-pause time and bytes are recorded and must stay within 2× of the
   empty-cache run.
4. Kill the supervisor (SIGKILL) during a 1 MiB write and during a READ, and with the `fault-injection` build at each
   point of 9.9 ("read returned", "magpie call sent", "magpie answered", "reply written") and of 10.2 step 5 ("written
   recorded", "keeper acknowledged"): every caller's syscall completes once, every write is stored once, Claude Code
   sees each prompt once. `unique` never repeats on the connection across the restarts and a migration. Every write
   is stored once also across a migration with requests in flight, where the sentry resends each of them under a new
   `unique` (9.3, interrupted results); the check counts the resends and the magpie operations they caused.
5. flock and fcntl locks between two processes on a mount file behave as on a local file.
6. Throughput: 1,000 × 4 KiB appends and 50 × 1 MiB appends from the sandbox to magpie in YYC's cluster (the round
   trip number feeds 9.5; target p99 of a 1 MiB image-line write seen by Claude Code ≤ 200 ms: magpie's 150 ms plus
   the WireGuard path and the gateway's 5 ms); an 87 MB transcript resume from a cold cache within 6 s.
7. `--setting-sources=` with a `/workspace/.claude/settings.json` SessionStart hook and a `/workspace/CLAUDE.md`
   present: neither loads (repeats the inventory's A11.F12 experiment the other way round); the managed CLAUDE.md does.
8. The system prompt snapshot (`prompt_snapshot` attachment) under the supervisor's argv/env equals today's except the
   documented differences (MCP server name, no Notification hook).
9. Syscall inventory: `runsc --strace` (or a seccomp trace) of the real 2.1.304 binary through a scripted session
   (resume, compaction, tombstone, relocation, subagents, TodoWrite, file history) against the mount; every syscall on
   a mount path is listed with its answer, nothing gets ENOSYS/EOPNOTSUPP that the session needed, and 9.4 is rewritten
   from the list.
10. Latency injection: fake magpie adds 50 ms, then 500 ms, to every cc write, then holds every call for 30 s; record
    the stream-json token gaps (from the mock model's chunk times to stdout) and Read/Glob tool latency. Pass at 50 ms:
    p99 token gap within 2× of the 0 ms run. Fail → the contingency of 9.4 (non-history paths to local scope).
11. Memory: a Bash tool that allocates 2 GiB/s until killed, in a 1 GiB sandbox with hermit's `memory.high` brake:
    log `memory guard killed` for the tool, the sandbox and the turn survive. Repeated without the brake to record the
    margin.
12. Gateway drain (with the platform conformance check, 23): a 60 s model SSE stream, a 25 s command long-poll and a
    stream of FUSE writes survive a SIGTERM of the gateway replica that carries them.
13. A resumed imported conversation whose history calls `mcp__yyc__search` calls the tool again as
    `mcp__tools__search`: the call works and both render (pigeon's mapper).

**Open items from the WP-07 gate (2026-11-02; WP-57 owns them; every package that relies on live migration waits for
it: WP-19's live migration (split c), WP-22's migration acceptance, WP-40's fabric acceptance on real hosts):**
1. **A process killed by SIGSEGV after a restore: explained and fixed (WP-57, 2026-11-02).** The fork's final,
   stopped pre-copy round did not resend pages that had been sent earlier and were then freed or found zero; the
   target, which uses the received memory file as it is, kept their old bytes and handed them out as fresh zero
   memory, so a process's next allocation could hold garbage pointers. The hold freed the most memory around the stop
   (the final round's zero ranges: median 7.2 MB with the hold, 1.1 MB in the matched control). Fixed in fork release
   `precopy-20261102.1` (such pages are sent as zero ranges): 0 lost processes in 100 hold-on migrations per layout,
   0 in the matched and the memory controls; a FUSE-free reproduction (`stale-mig.sh`) fails 4 of 20 runs on the old
   release and 0 of 40 on the new one (GATE.md "WP-57"). Left for the operator phase (WP-40): one batch on a
   fabric-AMI host in the fabric's account.
2. **A journal read with no whole request after a restore: explained (WP-57)**: the scratch move's quick-check final
   delta (9.9, last item; 16.1); the gate's move now checksums its final delta, sandboxd's needs the same.
3. **The tombstone rewrite on the mount was not reproduced** (gate item 2): in every trigger reachable offline
   (model fallback after a thinking-only message, after a written tool call, refusal fallbacks routed by the catalog
   and the catch-all) 2.1.304 had not written the abandoned messages, so nothing was removed from the file. WP-22's
   re-run under 6.1-6.4 must trigger it on the mount, or the gate's deferral is recorded as a decision (DESIGN 14).

Unit and integration (CI on every push, fake magpie/hermit/model and a scripted fake `claude` speaking stream-json):
the run state machine (every transition of 7.5 including agent turns, the start-turn-during-agent-turn race, supersede,
cancel during hold, interrupt timeout, crash loop); command idempotency across a supervisor restart (including a crash
between the `written` record and the keeper's acknowledgement); report idempotency and never-drop across a 20 min
fake-magpie outage; the heartbeat's `lastTurn`; the scope table; every FUSE opcode against both scopes; the errno map;
the retry rule (general outage vs one stuck op → `cc.write_stuck`); the offset and resync repair from the cache; the
journal replay; node-id persistence; the tree/cache reconciliation; `lease_lost` and `not_current_sandbox` (no restart
loop); the memory guard's victim choice (fake `/proc`); upload materialisation (symlink refusal, quota); the hook with
fd passing; lifecycle (pause and unforced release answer `sandbox_busy` while busy, a received `start_turn` counts as
busy, force, the activity reply's `release`, resumed, clock jump, release sequence); `/v1/health` needs the bearer and
reports `ready: false` while unleased. A contract test runs both against `hermit-api`'s and `magpie-api`'s fixtures.

Soak (CI nightly, like sandboxd's, A20.F13): 24 h of synthetic turns with 1 MiB lines, random supervisor kills,
magpie outages of 1 s to 30 min, a full scratch disk; gates: RSS flat under 64 MiB, no lost or duplicated transcript
bytes, no duplicated prompt, no dropped report, no hung FUSE request.

Claude Code release gate (`claude-code-gate`, nightly and before the pin moves): 9.1's harness run against the newest
Claude Code release; fails on an `unclassified path`, a new unanswered syscall, or event-count drift.

Image smoke (A20.F12, hermit's image CI): supervisor and keeper present and executable, `claude` resolves to
`/opt/claude-code/bin/claude` and is root-owned, a test gateway sees Claude Code's model call and MCP call as proxied
`http://model/…` and `http://tools/mcp` requests, `sudo`/apt wrappers as today, Docker on demand through the new socket.

The end-to-end harness (A20.F10): the fake web becomes a fake hermit, `/proc` framing becomes the supervisor's command
queue against a test magpie; the mock Anthropic API is reused as is. Ported acceptance checks: stop while preparing
(A20.F1, magpie), image attachments (A20.F5), titles (A20.F3), the runtime hold and stop-task tests (A20.F9). Measured
baselines to record (A20.F22): supervisor memory, first-block latency for a warm and a cold sandbox (model block end
to pigeon socket), resume time.

---

## 23. What the other specs must provide

magpie (spec-magpie.md already provides items 1-7 after its review round; 8-10 are new):
1. Live frames' receiving side (magpie 7.5); the supervisor sends from a later release (8.3).
2. `stop_tasks` and `kill {reason}` commands; `run.oom` and `cc.write_stuck` codes; task event `id`s; the heartbeat's
   `lastTurn` and its reconciliation; `lastUuid` with `409 transcript_pending`; `turnId` on cc writes (magpie 8.4,
   8.5, 8.9, 16.2).
3. Reports are never dropped by the supervisor; magpie's watchdog measures silence from `availableSince` (8.8).
4. The cc write retry rule (R26), the offset and resync repair, `resync` on the lease (magpie 6.12).
5. The lease only for hermit's current sandbox, `409 not_current_sandbox` (6.9).
6. New blob bytes staged in Postgres on the write path; synchronous replication (5.2, 6.10, 18.2).
7. The tree's `ccBytes` and `quotaBytes` (6.11).
8. **Accept `sandbox.lost` as a turn error code in a state report** (16.2's list), sent when a forced release ends a
   turn (15.5); it is the code magpie's own feed path sets, so whichever commits first gives the same row.
9. **An operator route to rebuild a cc file from its events** (`POST /v1/admin/conversations/{id}/cc/restore {path}`,
   caller `admin`): rewrites the main transcript's lines from the stored events and their stubs after the agent (root
   in its own sandbox, 6.3) truncated or rewrote it. Events stay immutable; the route only rebuilds `cc_lines`.
10. **A `sandbox_released` `gone` event for a turn whose `start_turn` was not yet acked** (the turn is `admitted`,
    `preparing` or `starting`): cancel its command and run the driver again (a new `PUT`) instead of failing the turn
    `sandbox.lost`; nothing was wrong with the turn, the sandbox was released before it reached it (review point 9).

hermit (spec-hermit.md must change these to match section 15; both specs then quote the `hermit-api` and
`supervisor-api` types):
1. **Shapes.** `GET /v1/sandbox/self` returns `SandboxSelf` exactly as 15.1: `control.tokenSha256` becomes a list;
   add `title.system` (YYC: rendered from `YYC/yyc-ai/deploy/hermit/title-system.md`) and `etag` with 304 on
   `If-None-Match`; drop "the answer is the same on every call" (it is the same until hermit renders something
   different). `POST /v1/sandbox/activity` takes `ActivityReport` (15.3), every 15 s, with `busy` derived from `state`
   (a received `start_turn` counts as busy); `ActivityReply.release` carries `{reason, code, force, deadlineAt}`, and an
   activity `state: "released"` tells hermit the supervisor has finished its release. hermit 3.7, 3.10 and 5.2 use the
   `/v1/sandbox/*` paths (platform R4) and the supervisor's `/v1/health` (bearer) →
   `{ok, ready, protocol, version, bootId, busy, state}`, marking a sandbox `ready` only on `ready: true` and treating
   `ok: true, ready: false` as starting.
2. **Release.** `POST /v1/lifecycle/release {reason, code, force, deadlineMs}` → `200 {released: true}` | `409
   sandbox_busy` (15.4). `force: true` only for hard lifetime (`hard_expires_at`), `deleted`, `offboarded`, `not
   allowed`, `operator`/`recycle`; idle, pool victim and soft lifetime (`expires_at`) are unforced, and a 409 means
   "not released": the row goes back to `ready` (hermit 12 steps 1-2, 11.2 step 1, 13.12 steps 2-3). hermit destroys
   only after the 200 or after `deadlineMs` + 5 s. `PUT /v1/conversations/{cid}/sandbox` (and every get-or-start for a
   turn) sets `last_active_at = now()` and `reserved_until = now() + 2 min`, which idle and pool-victim selection
   respect, so a sandbox handed to a turn cannot be released before its `start_turn` arrives (10.1 covers it from
   then on). `PUT`'s health rule: an activity report younger than 20 s (the cadence is 15 s) is enough; otherwise
   check health twice 5 s apart before declaring it unhealthy, and never release as unhealthy a sandbox that reported
   busy in the last 90 s.
3. **Bootstrap.** The entrypoint is the keeper as PID 1 with no tini (hermit 5.1 item 1, 17; platform R12). The
   identity stays `HERMIT_SANDBOX_TOKEN` (hermit 5.1 item 2). The supervisor presents it as `Proxy-Authorization: Basic
   sbx:<token>` on its fixed-name calls (17.1, platform 8.2), so hermit 6.3 step 1 lists that form for fixed names.
4. **Uploads.** Drop 3.8's sentence that magpie stages uploads with `PUT /v1/workspaces/{wid}/files`; the supervisor
   writes them (12.1) and no other caller needs the route, so remove the route.
5. **Image** (hermit 17): the docker wrapper uses `/run/supervisor/docker.sock` (`GET /start`, 4.6), not
   `docker-start.sock`; `ENTRYPOINT ["/usr/local/bin/supervisor-keeper"]` without tini; Claude Code installed under
   `/opt/claude-code` (3); `/var/lib/supervisor` root 0700; sudoers unchanged (6.3, section 27, review point 28); the smoke test
   no longer expects tini.
6. **YYC host-side memory brake** (5.1): sandboxd sets cgroup v2 `memory.high = memoryBytes - 256 MiB` on the
   sandbox's host cgroup below `memory.max`; hermit 24.3's fabric acceptance runs gate item 11.
7. YYC as before: no bind mount at `/home/sandbox/.claude`; `/workspace` stays the JuiceFS bind with its quota; the
   managed CLAUDE.md text and the title instructions as `SandboxSelf` content; `users/<key>/claude` on JuiceFS is no
   longer mounted anywhere after the import (A7.F32, A8.F2, A19.F17: the host-to-Postgres path for JuiceFS metadata
   remains for `/workspace` only). Live migration copies `/var/lib/supervisor` with the rest of the scratch disk (no
   exclusion: 9.7).

The sandbox gateway (YYC egress, specified in platform 17.2; jarvis3 has no spec of its own):
1. Route the fixed names `magpie` (`/v1/sandbox/**` only), `hermit` (`/v1/sandbox/**` only), `model` (Messages API
   plus `GET /v1/aviary/models`) and `tools` (`/mcp`), on the proxy port and the fixed-name listener, authenticated by
   the sandbox token in `Proxy-Authorization`, adding `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation` and
   `Aviary-Sandbox-Person` (the owner's lowercased email) and the callee's pair bearer; long-lived responses (25 s
   polls); bodies up to 64 MiB for magpie; inject the model credential; delete `/x/claude`, `/x/tools/mcp`,
   `/x/sent-files`.
2. **The supervisor's hot path.** Every FUSE write, command poll, report and heartbeat goes through the gateway, so for
   `magpie` and `hermit` `/v1/sandbox/**`: no `egress_log` row per request (an aggregate counter per sandbox and route in
   `/metrics` instead; today one row per request would mean tens of thousands of rows per active turn-hour against
   4,783 rows in total); the sandbox token checked from an in-memory cache (at most 1 s, platform 17.2), never a
   database read per request; bodies streamed both ways without buffering; keep-alive connection pools to magpie and
   hermit; no allowlist or budget logic on these names; added latency p99 ≤ 5 ms (measured by gate item 6 and the
   conformance suite).
3. **Drain on SIGTERM** (fixes A16.F8, where a 120 s drain cut model streams mid-message): stop accepting new
   connections; answer every new request on a kept-alive connection with `Connection: close`; let in-flight requests
   finish: model streams and CONNECT tunnels up to 600 s, fixed-name requests (25 s long-polls, cc writes) up to 60 s;
   `terminationGracePeriodSeconds` = preStop 10 + 600 + 15 = 625 s. The `tools` route timeout stays at or above tools'
   own 150 s drain and 120 s upstream cap (180 s), so egress never cuts a tools call that tools would finish (A16.F9).
4. Platform's gateway conformance (8.2) adds: a 60 s model SSE stream and a 25 s long-poll survive a SIGTERM of the
   replica carrying them (gate item 12 repeats it on real hosts).

tools (platform 17.2): accept the gateway's bearer and take the owner from `Aviary-Sandbox-Person`, which tools sends to
yyc-mcp as `X-YYC-User` (A19.F9; yyc-mcp needs the email, which this header is). The MCP server name the model sees is
`tools`. **Decision recorded here for platform 17.2 (A19.F11): the one shared yyc-mcp bucket of 120 calls per minute
under the `yyc-ai` service identity is kept unchanged.** Splitting it per person would need a yyc-mcp change in the
news-stream repo for no current need (yyc-mcp's live 7-day log shows 35 calls through the service token, A19.F9, far
below 120 per minute); a 429 already reaches the model as a
tool error, and `tool_calls` records it, so the split can be made when 429s appear there.

pigeon: render live frames once the supervisor sends them (pigeon 8.7 says how; not in this release); sentences for
`run.oom`, `cc.write_stuck` and `sandbox.lost` (pigeon 7.9). Nothing for the MCP rename (pigeon 7.8 maps MCP names
without the server part). `yyc.sandbox_status` is a frozen wire constant of the shipped app fork, not YYC behaviour
(pigeon 7.6.5); the supervisor knows nothing of it.

platform:
1. 3.10 rows: R26 (cc write retry, as magpie 21.3), R28 (snapshot 501, clone only), R29 (`link` → EPERM), R30
   (origin `person | agent | import`), and three new ones: **R32** the supervisor ↔ hermit contract is section 15's
   shapes (`hermit-sandbox` and `supervisor-control` surfaces); **R33** the supervisor calls the fixed names directly
   with the sandbox token in `Proxy-Authorization` and never uses the proxy in `SandboxSelf.env`, which is the agent's
   (R13 restated); **R34** the gateway drain rules of gateway item 3.
2. 17.2 egress gains gateway items 2 and 3 and the tools decision above; 17.2 tools names the header.
3. 8.2 gains gateway item 4.
4. 7.8 E2 drops "live frames precede the durable row" for this release (the supervisor sends none, 8.3); E10 holds as
   written (crash transparency, 9.9).

cutover: 17.4 "supervisor: no change" holds for the switch itself, with one addition: a supervisor whose
conversation is still migrating gets `409 conversation_migrating` on its lease and waits, unready, retrying every 30 s
(4.3 step 5), so a sandbox started early never writes into a conversation being imported.

---

## 24. Decisions taken in this spec

1. One long-lived Claude Code process per sandbox across turns, owned by a keeper (PID 1) rather than by the
   supervisor, so supervisor restarts are invisible; `--resume` only when a process starts.
2. The keeper owns Claude Code's stdin pipe (with a `msg_id` dedupe ring) and the `/dev/fuse` fd; stdout goes to a
   file; FUSE requests are read straight into a memory-mapped 64-slot journal, so a supervisor crash loses none and a
   prompt is never delivered twice. Crash transparency is kept rather than simplified away (section 27, review point 21).
3. History events come only from transcript writes (magpie classifies), each write carrying its turn; token-level
   live frames are specified against magpie's existing route but ship in a later release, after pigeon renders them.
4. The cc write retry rule shared with magpie (R26): unbounded under general unavailability; one op stuck for 10 min
   while magpie answers other calls → EIO and `cc.write_stuck`, with a resync from the write-through cache.
4a. Reports are never dropped; the heartbeat's `lastTurn` lets magpie reconcile anything lost.
4b. An unforced release (idle, pool, soft lifetime) is refused `409 sandbox_busy` while busy, and a received
    `start_turn` already counts as busy; only hermit's forced releases end a running turn, as `sandbox.lost`.
4c. On `not_current_sandbox` or `lease_lost` the supervisor waits unready and never restarts in a loop.
4d. The supervisor reaches the fixed names directly with the sandbox token; `SandboxSelf.env` is the agent's
    environment only.
5. `--setting-sources=` (none) plus `allowManagedHooksOnly` and a refused set of user-config paths; `cleanupPeriodDays`
   36500.
6. `EnterPlanMode`, `ExitPlanMode` disallowed; `AskUserQuestion` offered (Deyao, 2026-11-05), parked when unanswered (DECISIONS 17); the generic ask relay remains for
   anything else that asks.
7. A send during background work stops the tasks first (the agreed supersede), although the long-lived process would
   allow keeping them; changing that later is a one-line policy change plus a product decision.
8. The 30-minute hold timer survives only as a logged silence ceiling; `ScheduleWakeup` is honoured at any delay
   within the sandbox's life.
9. `--session-id <conversation_id>` for new conversations; imported ones keep their ids.
10. Model ids from the gateway's `GET /v1/aviary/models`; instructions and title prompt from hermit's descriptor;
    nothing model- or deployment-specific in the supervisor.
11. The MCP server is named `tools` (tool names `mcp__tools__*`).
12. A user-space memory guard is the OOM protection that works under gVisor; scores and cgroup limits are set too and
    take effect on kernels that enforce them; same code everywhere.
13. :7070 serves hermit only (health, state, lifecycle, a bounded debug exec); no shell, no files-panel exec.
14. Claude Code moves to `/opt/claude-code`, root-owned, run by absolute path.
15. Configuration changes reach a running sandbox at its next idle point by a graceful generation restart.
16. The content cache stays on the migrated scratch disk, capped at 256 MiB plus the main transcript, rather than on a
    path migration skips (an open skipped file would fail gVisor's restore check).
17. The sandbox stays one trust domain (sudo unchanged); the guarantees that must hold against the agent are magpie's
    and the gateway's, and this spec says so where it makes a claim (6.3).
18. Claude Code's pin moves only on a green `claude-code-gate`; 9.4 is rewritten from the syscall trace of the real
    binary.

---

## 25. Coverage table

Every inventory feature whose `target_home` names the supervisor (147).

| Id | Feature | Section | Disposition |
|---|---|---|---|
| A0.F15 | Per-person CloudCLI process spawn | 4.5, 9.1 | dropped: no per-person process; one supervisor per sandbox, ~/.claude per conversation |
| A0.F19 | Running-chat probe | 11.3, 15.3 | supervisor reports run state to magpie and hermit; drains need no probe |
| A0.F20 | Idle stop sweeper | 15.3, 15.5 | hermit idle release from activity reports; supervisor refuses release while busy |
| A0.F25 | Peer holder discovery (rollout-safe owner) | 4.1, 9.5 | dropped: lease gives one writer; no rollout owner needed |
| A0.F30 | Graceful drain on SIGTERM / SIGINT | 4.7, 15.5, 19, 23 | the supervisor's SIGTERM is a forced orderly release; pigeon and magpie rollouts never touch a turn (no process outside the sandbox holds it); a gateway (egress) rollout drains model streams up to 600 s and fixed-name requests up to 60 s, and anything cut is retried with the same `opId` (gateway item 3) |
| A1.F8 | 'Preparing the workspace' status frames and Stop while preparing | 10.2, 7.5 | preparing is magpie/hermit; a start_turn canceled before delivery never reaches Claude Code |
| A1.F9 | Running Claude Code inside the sandbox (/proc protocol) | 4, 6, 7 | replaced: keeper-owned detached Claude Code driven over stream-json |
| A1.F10 | Sandbox keep-alive while Claude Code runs | 15.3 | activity reports replace touch |
| A1.F11 | Short commands in the sandbox (runInSandbox) | 14, 15.4 | titles go to model directly; bounded /v1/exec for ops |
| A1.F12 | Fixed model: one instance model, no picker, no effort | 6.2, 17.3 | ANTHROPIC_MODEL from the gateway model endpoint |
| A1.F14 | Permission mode and settings in the sandbox | 6.1, 6.4 | bypassPermissions, empty setting sources, managed settings constant |
| A1.F15 | Post-turn hold for background work (30 min ceiling) | 7.7 | hold is a reported state; silence ceiling 30 min, logged |
| A1.F16 | Conversation titles from the title model (inside the sandbox) | 14 | supervisor executes the title call via model |
| A1.F18 | Conversation list, details, running list | 11.1, 11.3 | running state reported; list is magpie |
| A1.F26 | Uploads/attachments staging into /workspace/uploads | 12.1 | blobs materialised into /workspace/uploads |
| A1.F27 | Images: model-ready copy + original saved | 12.1, 12.2 | model copy as image block, original to the workspace |
| A1.F29 | File download / inline preview (/files/raw) with symlink-safe open | 18 | not the supervisor: live workspace reads via hermit, snapshots via magpie |
| A1.F36 | Run registry, seq numbering, replay, single-run lock | 7.5, 8 | live run owned by supervisor; seq by magpie |
| A1.F37 | Provider-id mapping and transcript path at first sight | 6.1, 10.2 | --session-id/--resume; transcript_path reported |
| A1.F42 | Edit an already-sent message (chat.edit-send) | 6.1, 24 | dropped (no client; magpie 21.1 decision 9); no --resume-session-at |
| A1.F43 | Abort / stop-task / permission response | 7.6, 7.8, 10 | interrupt, stop_tasks, answer_ask commands |
| A1.F47 | Health endpoint and router-driven process lifecycle | 15.4 | `/v1/health` (bearer); turns survive pigeon and magpie restarts and, with the drain rules of 23, gateway rollouts |
| A2.F23 | send_agent_message_request (send) | 10.2, 12 | start_turn command |
| A2.F24 | cancel_agent_request (stop) | 7.8, 10.3 | interrupt/kill commands, local and bounded |
| A2.F31 | Turn lifecycle: turn_started / turn_completed / turn_failed / turn_canceled | 7.5, 11.1 | authoritative run state reported, incl. agent-started turns |
| A2.F34 | Hub as in-process CloudCLI chat client | 8, 10 | dropped: REST commands and events replace the hub client |
| A3.F1 | POST /internal/cc/touch: mark a sandbox in use | 15.3 | activity to hermit |
| A3.F11 | Sandbox lifecycle rules and limits | 15.5 | `sandbox_busy` refusal of unforced releases; hermit policy |
| A3.F14 | Sandbox env: no secrets in a sandbox | 6.2, 6.3, 15.2 | no secrets in env; per-key fetch |
| A3.F15 | Claude Code environment in the sandbox (claudeCodeEnv) | 6.2 | constant env, model facts from gateway |
| A3.F16 | Managed CLAUDE.md = instance system prompt + SANDBOX_RULES | 6.4, 15.1 | instructions text from hermit descriptor, written by supervisor |
| A3.F17 | Sent-file snapshots hook (PreToolUse on SendUserFile) | 13 | hook -> local socket -> magpie |
| A3.F18 | JuiceFS mounts for /workspace and ~/.claude | 9 | ~/.claude FUSE to magpie; /workspace stays hermit |
| A3.F56 | Sandbox agent client (health) | 15.4 | /v1/health |
| A5.F0 | Sandbox image base and toolchain | 3 | image stays hermit's; supervisor binaries added |
| A5.F2 | Docker inside the sandbox (on demand) | 4.6 | docker start socket moved to supervisor |
| A5.F3 | Claude Code install + managed config dir | 3, 6.4 | /opt/claude-code root-owned; managed files by supervisor |
| A5.F7 | sbx-init: managed Claude Code files from env | 4.3, 6.4, 15.1 | managed files from hermit REST, not env |
| A5.F8 | sbx-init: /etc/hosts and apt proxy | 4.3 | written at first start |
| A5.F9 | sbx-init: start agent unprivileged | 6.1 | spawn as uid 1000 with today's capability rules |
| A5.F10 | Agent HTTP API on :7070 | 15.4, 18 | replaced by the :7070 hermit API |
| A5.F11 | Exec token auth | 15.4 | control-token hashes from descriptor |
| A5.F12 | /proc exec protocol (process start, stream, signal, kill) | 18 | replaced: no process transport |
| A5.F13 | Process lifetime tied to the connection | 4.1, 4.5 | fixed: Claude Code outlives connections and supervisor restarts |
| A5.F14 | Claude Code launch spec from the UI | 6.1, 6.2 | argv/env built by supervisor |
| A5.F15 | Short commands in the sandbox (runInSandbox) | 14, 15.4 | title via model; /v1/exec for ops |
| A5.F18 | Sandbox environment contents (no secrets) | 6.2, 6.3 | env applied; private names stripped |
| A5.F32 | OCI bundle per sandbox | 3, 23 | bundle args become supervisor-keeper (hermit) |
| A5.F43 | sandboxd fixed memory budget | 5.4, 20 | bounded supervisor memory, soak-tested |
| A5.F44 | cgroup layout and OOM protection | 5 | scores, agent cgroup, user-space guard |
| A5.F49 | Live migration invariants (what must survive) | 16 | in-sandbox FUSE checkpointable; nothing on host |
| A5.F50 | Logging | 21.1 | JSON logs to the console log |
| A6.F12 | Route `claude` (model route to OpenRouter) | 6.2, 17 | ANTHROPIC_BASE_URL=http://model |
| A6.F39 | Sandbox env pointing at egress (proxy, mirrors, Claude Code, MCP, hooks) | 17, 23 | proxy env from hermit; fixed names through the gateway |
| A7.F6 | config.json: models, defaultModel, titleModel, titleReasoning, systemPromptFile | 17.3, 14 | model and title ids from the gateway |
| A7.F11 | Managed CLAUDE.md / system prompt | 6.4, 15.1 | hermit content, supervisor writes |
| A7.F12 | ui Deployment rollout (surge-first, topology spread, drain) | 19 | turns survive pigeon rollouts; no chat-drain wait |
| A7.F13 | ui per-user process placement (rendezvous hashing + peer holds) | 9.5 | dropped; lease is the single-writer guarantee |
| A7.F32 | JuiceFS volume yyc-ai via the CSI driver | 9, 23 | ~/.claude off JuiceFS; /workspace stays |
| A7.F47 | jarvis3 sandboxd workflow (host daemon release) | 3 | supervisor released from aviary CI; sandboxd stays hermit |
| A8.F0 | Per-user ~/.claude folder on JuiceFS (shared by all of a user's conversations and sandboxes) | 9.1 | per-conversation view; local scope per sandbox |
| A8.F1 | Sandbox bind mounts of ~/.claude and /workspace | 9, 3 | FUSE replaces the bind mount |
| A8.F2 | Host JuiceFS mount (sbx-jfs.service) | 23 | host JuiceFS only for /workspace |
| A8.F5 | Transcript file projects/-workspace/<sessionId>.jsonl | 9.1, 8.1 | transcript in magpie, presented as a file |
| A8.F8 | Claude Code transcript append path | 9.4, 9.5 | O_APPEND writes acked after store |
| A8.F9 | Claude Code in-place transcript rewrites (tombstone removal, compaction) | 9.3, 9.4 | truncate, pwrite, rename-over supported |
| A8.F10 | Resume reads the whole transcript | 4.5, 9.7 | long-lived process; resume from local cache |
| A8.F11 | Saved tool outputs projects/-workspace/<sessionId>/tool-results/ | 9.1 | magpie scope |
| A8.F12 | Subagent and workflow transcripts (supported by the UI, absent today) | 9.3 | LINK -> EPERM fallback; magpie scope |
| A8.F13 | Live-session registry sessions/<pid>.json and <pid>.<sha256>.key | 9.1 | local scope |
| A8.F14 | Shell snapshots shell-snapshots/snapshot-bash-<ms>-<rand>.sh | 9.1 | local scope |
| A8.F15 | session-env/<sessionId>/ | 9.1 | local scope |
| A8.F16 | .claude.json backups backups/.claude.json.backup.<ms> | 9.1 | local scope |
| A8.F17 | Retention sweep (.last-cleanup, cleanupPeriodDays) | 6.4, 9.4 | cleanupPeriodDays 36500 + magpie EPERM |
| A8.F18 | Auto-memory dir projects/-workspace/memory/ | 9.1 | magpie scope, per conversation |
| A8.F28 | Managed Claude Code config written outside ~/.claude | 6.4, 9.1 | managed files; user config paths refused |
| A8.F30 | Migration and checkpoint durability of ~/.claude writes | 9.2, 9.5, 16 | ack after store; stable size/mtime |
| A8.F31 | Concurrent writers to one ~/.claude | 9.1 | no shared files across sandboxes |
| A9.F0 | Streaming-input prompt that keeps stdin open (held prompt stream) | 4.5 | no held stdin needed |
| A9.F1 | BG_WAIT_CEILING_MS (30 min) and CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS | 6.2, 7.7 | ceiling 5 s for release; silence ceiling 30 min |
| A9.F2 | Which tool calls count as background work (static rule) | 7.7 | static rule kept for tools without task events |
| A9.F3 | Background task tracker (task events) | 7.7, 11.4 | tracker in supervisor, reported to magpie |
| A9.F4 | Hold decision at each `result` | 7.5, 7.7 | held when any task outstanding (both sources combined) |
| A9.F5 | CLI-initiated follow-up turns (task completion, Monitor events, wake-ups) | 7.5, 11.2 | agent-started turns (`origin: agent`) get explicit boundaries |
| A9.F6 | Release on stop of the last outstanding task | 7.7 | release on last task end |
| A9.F7 | New send supersedes a hold (releaseHeldSession and the addSession supersede) | 7.7 | supersede: stop_task each, then message |
| A9.F8 | Abort and stop-task during a hold | 7.8, 10.6 | stop_tasks command |
| A9.F9 | Running list during a hold: GET /api/providers/sessions/running | 11.1, 11.3 | held state and tasks reported |
| A9.F14 | Live task_status frames | 11.4 | task.status events |
| A9.F15 | Router idle sweep, LRU eviction and drain vs holds | 15.5, 19 | no router: a hold is a busy state, so idle release is refused `sandbox_busy`; pigeon/magpie rollouts never touch the process |
| A9.F16 | Sandbox keep-alive /internal/cc/touch during a hold | 15.3 | activity incl. held |
| A9.F18 | Sandbox agent kills the CLI when the proc connection drops | 4.1 | fixed: no connection kills the agent |
| A9.F20 | No logs for hold start/end | 21.1 | hold start/end logged |
| A10.F19 | GET /api/providers/sessions/running | 11.3, 15.3, 15.4 | information kept in reports; route gone |
| A10.F22 | Legacy /ws chat socket | 18 | no /ws; nothing in the sandbox |
| A10.F25 | GET /health on each child | 15.4 | /v1/health |
| A10.F27 | Child shutdown + no-op close calls | 15.5 | graceful stop sequence |
| A11.F0 | Full argv the SDK builds for a YYC (Paseo) turn | 6.1 | argv built by supervisor |
| A11.F1 | /proc framed process transport | 18 | gone |
| A11.F2 | Process lifetime is tied to the ui pod's socket | 4.1 | fixed |
| A11.F3 | stdin control_request initialize | 7.2 | initialize without hooks |
| A11.F4 | stdin user message | 12.2, 10.2 | user message from start_turn |
| A11.F5 | Held stdin and the background-work hold | 4.5, 7.7 | no hold timer for stdin |
| A11.F6 | stdin control_request interrupt (chat.abort, Paseo cancel) | 7.8 | interrupt with 15 s grace then kill |
| A11.F7 | stdin control_request stop_task (chat.stop-task) | 7.7, 10.6 | stop_task |
| A11.F8 | stdout control_request can_use_tool → canUseTool → permission_request | 7.6 | ask relay; bypass allows others |
| A11.F9 | stdout control_request hook_callback (Notification hook) | 7.2 | Notification hook dropped |
| A11.F10 | Other stdout control traffic the SDK handles itself | 7.3 | elicitation decline, dialog cancel, errors for others |
| A11.F11 | AskUserQuestion / ExitPlanMode in YYC (bypassPermissions) | 6.1, 7.6 | plan mode disallowed; AskUserQuestion relayed and parked when unanswered (DECISIONS 17) |
| A11.F12 | settingSources project,user,local: model-writable settings, hooks and CLAUDE.md | 6.1, 6.4 | empty setting sources, allowManagedHooksOnly |
| A11.F13 | `claude` resolved on a user-writable PATH | 3, 6.1 | absolute root-owned path |
| A11.F14 | Environment forwarded through X-Proc | 6.2 | constant env; hermit env passed through |
| A11.F15 | Reasoning/effort for the YYC model | 6.1 | no effort flags; gateway applies reasoning |
| A11.F16 | Sandbox touch while Claude Code runs | 15.3 | activity reports |
| A11.F17 | Sandbox acquisition before spawn (preparing status) | 10.2 | start_turn waits harmlessly while hermit prepares |
| A11.F18 | Title call through /proc (runInSandbox) | 14 | title via model |
| A11.F19 | Claude Code transcript lines the supervisor/FUSE must carry (2.1.304) | 9, 8.1 | byte-exact through FUSE |
| A14.F0 | Hub chat client bootstrapped from the first Paseo connection | 10 | REST command queue instead of a fake socket |
| A14.F5 | Live event follow (chat.subscribe from the hub) | 8 | transcript writes are the event source; live frames in a later release (8.3) |
| A14.F6 | Send through the hub (optimistic user row + chat.send) | 10.2 | supervisor runs the admitted turn |
| A14.F9 | Turn lifecycle events (started/completed/failed/canceled) | 7.5, 11.1 | authoritative run state |
| A14.F10 | Concurrent sends from two sockets | 7.5 | one process, pending_inputs; race handled |
| A15.F17 | In-sandbox agent version skew (ui as client of old sandboxes) | 21.3 | versioned contract; version reported |
| A16.F0 | Survival: ui pod drain longer than JARVIS_UI_DRAIN_SECONDS (900 s) | 19 | fixed |
| A16.F1 | Survival: CloudCLI child crash or V8 heap OOM (--max-old-space-size 512) | 19 | fixed |
| A16.F2 | Survival: LRU eviction / idle stop | 19 | dropped |
| A16.F5 | Survival: janitor max lifetime (maxLifetimeHours 8) reached mid-turn | 15.5 | unforced release refused `sandbox_busy`; hard-lifetime release forced and ends the turn cleanly (`sandbox.lost`) |
| A16.F6 | Survival: sandbox host loss | 11.3, 19 | heartbeats; hermit lost event ends the turn |
| A16.F7 | Survival: live migration mid-stream | 16.1 | invisible; nothing to reattach |
| A16.F12 | Survival: sbx-jfs restart on the sandbox host (juicefs crash) and EDQUOT | 9.5, 9.10 | writes block, never ENOTCONN |
| A16.F13 | Survival: network blip on the /proc TCP connection | 19 | fixed |
| A16.F14 | Stop (chat.abort) during a dead /proc link | 7.8, 19 | local interrupt; magpie bounds Stop at 30 s |
| A16.F17 | Drain and grace budget per role | 19 | no ui bound |
| A17.F13 | Claude Code per-user leftovers in ~/.claude | 9.1 | per conversation in magpie; local dies with sandbox |
| A18.F2 | Per-conversation /workspace quota 1 GiB / 1,000 inodes | 15.1 | hermit enforces; quota in descriptor and instructions |
| A18.F20 | AWS host sbx-jfs.service | 9, 23 | ~/.claude no longer on host JuiceFS |
| A18.F22 | Live migration relies on no-writeback | 16.1 | no host writeback involved for ~/.claude |
| A19.F17 | Postgres network access for YYC AI pods and sandbox hosts | 23 | host-to-Postgres only for /workspace |
| A20.F1 | Jarvis-mode server tests (fork server in Jarvis mode) | 22 | stop-while-preparing ported |
| A20.F3 | Title generation tests | 22 | title tests ported |
| A20.F5 | Image attachment tests | 22 | image attachment tests ported |
| A20.F9 | Core CloudCLI server tests YYC relies on (npm test, not in any CI) | 22 | hold/stop-task tests ported |
| A20.F10 | Local end-to-end harness (no cloud, no model cost) | 22 | fake hermit + supervisor command queue |
| A20.F11 | jarvis3 chat test suite (web, egress, tools, sandbox agent, scaler) | 22 | sandbox.test /proc suite replaced by supervisor CI |
| A20.F12 | chat.yml image smoke test | 22 | image smoke gains supervisor checks |
| A20.F13 | sandboxd Rust unit tests + soak test | 5.4, 22 | RSS-flat soak adopted |
| A20.F22 | Measured baselines (acceptance numbers) | 22 | supervisor memory and latency baselines |

Related features whose `target_home` is egress or tools but whose fix the supervisor depends on (no other spec homes
them; platform 17.2 is asked to, section 23):

| Id | Feature | Section | Disposition |
|---|---|---|---|
| A16.F8 | Survival: egress rollout (120 s drain) cutting CONNECT tunnels and the model's SSE stream | 19, 23 gateway item 3 | egress drain: `Connection: close`, model streams and tunnels up to 600 s, fixed names up to 60 s, grace 625 s; conformance check |
| A16.F9 | Survival: tools rollout mid tools/call | 19, 23 gateway item 3 | egress's tools route timeout ≥ 180 s, above tools' own 150 s drain |
| A19.F9 | YYC AI tools → yyc-mcp upstream (service token + X-YYC-User) | 6.4, 23 tools | tools maps `Aviary-Sandbox-Person` (lowercased email) to `X-YYC-User` |
| A19.F11 | Shared rate limit for all YYC AI staff on yyc-mcp | 23 tools | decision: the shared 120 calls/min bucket is kept unchanged |

---

## 26. Review round 1: where each point landed

| # | Point (severity) | Resolution in this spec |
|---|---|---|
| 1 | Egress rollout cuts model streams and fixed-name calls; A0.F30 claim wrong; A16.F8/F9 unhomed (major) | 19 gains a gateway-rollout row; A0.F30's disposition corrected (25); egress drain rules (`Connection: close`, 600 s for model streams and tunnels, 60 s for fixed names, grace 625 s), the tools route timeout ≥ 180 s and a conformance check asked of platform 17.2/8.2 (23 gateway items 3-4, platform R34); A16.F8, F9 listed as related rows (25); gate item 12 |
| 2 | `X-YYC-User` wording; owner header unnamed; yyc-mcp bucket undecided (minor) | 6.4 and 23: the gateway sets `Aviary-Sandbox-Person` (lowercased email), tools maps it to `X-YYC-User`; the shared 120/min bucket is kept unchanged (decision in 23 tools, rows A19.F9, F11 in 25) |
| 3 | hermit ↔ supervisor :7070 contract mismatch (blocker) | 15.4: `GET /v1/health` with the bearer → `{ok, ready, protocol, version, bootId, busy, state}`, ready only on `ready: true`; `POST /v1/lifecycle/release {reason, code, force, deadlineMs}` → `200 {released}` \| `409 sandbox_busy`; the supervisor's behaviour depends only on `force` (reason is hermit's free text, code its release code); hermit's changes in 23 hermit items 1-2 |
| 4 | `SandboxSelf` and activity shapes differ (blocker) | 15.1 and 15.3 are the one camelCase `SandboxSelf`, `ActivityReport`, `ActivityReply` of `hermit-api`, including `title.system`, `tokenSha256` as a list, `env` applied by the supervisor (6.2; `env_private` gone), lifecycle values, `etag`/304, and the reply's `release`, which the supervisor acts on; R13 restated as R33 (23 platform) |
| 5 | Routes and fields the supervisor uses that magpie lacks (blocker) | magpie's review round added them (magpie 7.5, 8.4, 8.5, 8.9, 16.2); this spec now uses magpie's names (`askId`, `toolUseId`, `origin: agent`, `lastUuid`, `lastTurn`, `turnId`, camelCase, `waitMs`); `sandbox.lost` as a report code is the one new ask (23 magpie item 8) |
| 6 | Two owners stage uploads (major) | 12.1: the supervisor is the only writer; hermit to drop the staging sentence and the route (23 hermit item 4) |
| 7 | Docker socket path; tini (major) | 3 and 23 hermit item 5: `/run/supervisor/docker.sock`, keeper as PID 1 without tini (R12) |
| 8 | Live frames have no magpie route or pigeon consumer; 404 retry (major) | magpie 7.5 now has the route; the supervisor's sender ships only after pigeon renders live frames (8.3, `--include-partial-messages` dropped from 6.1); 404 or `409 turn_not_active` → stop sending for the turn, never retried; platform E2 to drop its live-frame assertion (23 platform item 4) |
| 9 | `409 BUSY` ignored by hermit; admission races (major) | 15.5: unforced releases are refused and change nothing; a received `start_turn` already counts as busy (10.1); hermit treats 409 as not released, forces only hard lifetime/deleted/offboarded/not allowed/operator, destroys only after the answer or the deadline, and reserves a sandbox at `PUT` (23 hermit item 2); magpie re-acquires on a `sandbox_released` event before `start_turn` was acked (23 magpie item 10) |
| 10 | Watchdog kills healthy turns after a magpie outage (major) | magpie 8.8 now measures silence from `availableSince` and asks hermit first; 9.5 item 4 and 19's magpie row describe the supervisor's side |
| 11 | Dropped end reports leave turns running for ever (major) | 11: no report is ever dropped while the lease is valid; the heartbeat carries `lastTurn` (from `run.json`) for magpie's reconciliation (11.3, magpie 8.5) |
| 12 | A crash after the stdin write sends the prompt twice (major) | 10.2 step 5: `written` with `msgId` recorded before the keeper's `STDIN`; the keeper's dedupe ring answers `ALREADY` (4.2); a dead generation at replay fails the turn instead of re-sending; gate item 4 and unit tests |
| 13 | Retry forever vs EIO after 10 min (major) | one rule in both specs (9.5 item 4, magpie 6.12, R26): unbounded under general unavailability; one op stuck 10 min while heartbeats succeed → EIO, `cc.write_stuck`, generation ended, resync on the next lease |
| 14 | Activity cadence, health and release shapes; needless releases on restart (major) | 15.3 every 15 s with `busy` from `state`; 15.4 health semantics (`ok, ready: false` = starting; no single failed check is death); release `reason` free text plus `code`; `PUT`'s two-checks rule and the 90 s busy rule asked of hermit (23 hermit item 2) |
| 15 | Lease granted unconditionally; restart loop (minor) | magpie 6.9 grants only to hermit's current sandbox; 4.3 step 5 and 9.5 item 7: on `not_current_sandbox` or `lease_lost` the supervisor waits unready, retrying every 30 s, never exits (exit code 75 removed) |
| 16 | Late lines credited to the wrong turn (minor) | 8.1: `turnId` on every write (the turn when the request was read); 8.2: no 5 s cap, `lastUuid` with `409 transcript_pending`; magpie refuses a new turn while one runs |
| 17 | Streaming specified only on the supervisor side; Paseo merge risk (major) | as 8: shipped without `--include-partial-messages`; the later release waits for pigeon's replace-not-merge mapping and its real-reducer contract test (8.3) |
| 18 | Egress on the synchronous write path, `egress_log` volume, unspecified latency (major) | 23 gateway item 2: no per-request log rows for `/v1/sandbox/**`, in-memory token cache, streamed bodies, keep-alive pools, p99 ≤ 5 ms added; gate item 6's end-to-end target |
| 19 | Acknowledged writes wait for bucket PUTs (major) | magpie 5.2 stages new blob bytes in Postgres; 9.5 step 2 and the latency budget say so; bucket outages no longer block writes (9.10) |
| 20 | Hot path on a single-instance shared Postgres; offset mismatch a dead end (major) | magpie 18.2's own synchronous cluster (decided by Deyao, decision 2); 9.5 item 6 is now the repair from the write-through cache |
| 21 | Journal crash window between read and journal; too much machinery (major) | window closed: requests are read straight into the memory-mapped journal and `opId`s are derived from `unique` (9.3, 9.9), proven by fault injection (gate item 4); the simplification is not taken (section 27, review point 21) but is the stated fallback if the gate fails |
| 22 | Claude Code 2.1.304 is a Bun binary; sync writes stall the event loop; syscalls not inventoried (major) | 3 and 9.4 say what the binary is and what a blocked write costs; gate items 9 (strace inventory) and 10 (latency injection); contingency: non-history paths to local scope, never write-behind (section 27, review point 22) |
| 23 | Set-aside on relocation; scope table tied to Claude Code internals (major) | magpie 6.2 keys the main transcript by path; 9.1 lists `*.jsonl.superseded-*`; `claude-code-gate` CI before any pin move (9.1, 22) |
| 24 | User-space guard too slow for a fast allocator (major) | 5.1: hermit's YYC sandboxd sets host `memory.high` below `memory.max` (23 hermit item 6); gate item 11 (2 GiB/s) |
| 25 | Three contradictions: retry, link errno, snapshot (minor) | R26 (9.5), R29 (9.3), R28 (16.3: YYC snapshot stays 501, shares clone only); rows asked of platform 3.10 (23 platform item 1) |
| 26 | 1 GiB cache inflates migration (minor) | 9.7: 256 MiB plus the main transcript, kept on the migrated disk; gate item 3 measures the pause with a full cache; the excluded-path alternative is not taken because gVisor's restore would fail on an open excluded file (9.7) |
| 27 | MCP rename and `yyc.sandbox_status` (minor) | 6.4: no tool policy keyed by MCP names, history keeps `mcp__yyc__*` as data, pigeon's mapper ignores the server part; gate item 13; `yyc.sandbox_status` is pigeon's frozen wire constant (23 pigeon) |
| 28 | Passwordless sudo undercuts "OOM-protected" and "protected transcript" (minor) | 6.3 states the one-trust-domain caveat next to each claim (also 5.3); magpie keeps events immutable and gets an operator restore route (23 magpie item 9); narrowing sudo not taken (section 27, review point 28) |

## 27. Rejected review points

1. **Point 21's simplification** (kill Claude Code on any unclean supervisor exit, drop the journal, `PUT_FD`/`GET_FD`
   and node-id persistence, end the turn `supervisor.restarted`). Not taken, because the agreed design is a supervisor
   "restarted by a keeper" that "runs Claude Code detached", and platform's E10 asserts that a `kill -9` of the
   supervisor mid-turn lets the turn complete. The crash window the point found is real and is closed by its own
   alternative: the request is read directly into the journal's shared mapping, so there is no instant at which it
   exists only in the dead process (9.9), and the `opId` needs no record because it is derived from the request's
   `unique`. The fault-injection test it asked for is gate item 4. If that gate cannot pass on the gVisor fork, the
   simplification becomes the design and this section records it.
2. **Point 22's "bounded write-behind for non-main-transcript paths"**. Not taken: Deyao's design says a write returns
   once magpie stored it, and a write-behind would acknowledge bytes magpie does not have. The same goal (keep the
   event loop from stalling on paths that are not history) is met, if gate item 10 shows the stall, by moving
   `todos/`, `file-history/`, `plans/` and `tasks/` to local scope (9.4), which changes where they live, not what an
   acknowledgement means.
3. **Point 28's narrowing of sudoers to the apt wrapper and the docker start.** Not taken: `sudo apt-get install` of an
   arbitrary package runs its maintainer scripts as root, so sudo for apt is root, and the narrowing would give no
   boundary while breaking whatever else the agent does with sudo today (A5.F4, A5.F9). The point's other two parts
   are adopted: the caveat next to each claim (6.3) and magpie-side protection with an operator restore (23 magpie
   item 9).
4. **Point 26's first option** (cache on a path migration skips, revalidated after `resumed migrate`). Not taken in
   favour of its second option (a smaller budget, measured): the supervisor holds cache files open, and gVisor's
   restore checks every open host file against the checkpoint (A8.F30), so an excluded path would fail restores.
5. **Point 3's "one shared enum" for release reasons**, taken in point 14's form instead: `reason` stays hermit's free
   text (its 13.13 list grows with operator notes), `code` is the enum, and the supervisor's behaviour depends only on
   `force`. One enum the supervisor branches on would put hermit's policy vocabulary into the supervisor.
