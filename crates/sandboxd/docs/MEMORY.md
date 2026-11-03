# sandboxd's memory bound

sandboxd is the agent on every sandbox host (and on the gateway): `crates/sandboxd` (Rust, one static musl binary).
Moved here from jarvis3 `chat/sandboxd/daemon` (2026-11-02, jarvis3 commit 52c9b8c) with this note.
It replaced the Node sandboxd (jarvis3 `chat/sandboxd/src/*.ts`, 2026-10-21) with the same wire protocol, because
Deyao (2026-10-21): "I don't want the controller to crash, so something with a provable memory upper bound is good."
On a sandbox host every MiB is a sandbox's: sandboxd must never grow, swap or OOM.

**The bound (defaults, `sandboxd budget` prints it for a host.json): resident memory ≤ 31.5 MiB, cgroup charge
≤ 44.0 MiB.** Measured: 29.3-30.8 MiB RSS, flat to the byte, idle and under load:

| Where | Node sandboxd (RSS) | Rust sandboxd (RSS) |
|---|---|---|
| soak test, same load (this machine) | 88.1 MiB idle, 153.2 MiB peak | 29.30-29.31 MiB the whole run |
| m7i.xlarge, 6 sandboxes of 1 GiB, a busy sandbox migrated A→B→A, restart under them | – | 29.41-29.43 MiB; its cgroup ≤ 29.4 MiB |
| production hub (a small Hetzner VM, 2026-10-21) / gateway (k3s node) | 105 MiB / 109.7 MiB (cgroup 93-99 MiB) | gateway 30.7 MiB (cgroup 29.3 MiB) |
| production AWS host after a live migration onto it | – | 30.85 MiB, `stray`/`overflow`/`failed` 0 |

What follows: how the bound is built (the budget), how it is enforced (four mechanisms), how it is checked (tests),
and what it does not cover.

## The budget

Every collection, buffer and thread is sized at start from host.json `limits` (defaults in brackets) and never grows:

| `limits` key | default | what it bounds |
|---|---|---|
| `maxConnections` | 16 | API worker threads = TLS connections served at once (more wait in the kernel's accept queue, backlog 128) |
| `maxSandboxes` | 24 | sandbox entries on the host (an m7i.xlarge holds 15 sandboxes of 1 GiB) |
| `maxMigrations` | 4 | migrations in flight on the host, each direction (receive slots, concurrent sends) |
| `tlsArenaKiB` | 512 | heap per API worker, for rustls' per-connection state (min. 400) |
| `maxBodyKiB` | 64 | largest request body |
| `imageConnections` | 2 | the image server's connections (hub/gateway only) |
| `helperMemoryMaxMiB` | 384 | cgroup cap of the commands sandboxd runs (not part of sandboxd's bound) |
| `cgroups`, `mlock` | true | below |

| Item | Formula | Default |
|---|---|---:|
| binary (text + data) | static musl build, 1.6 MB measured, budgeted | 4.00 MiB |
| boot arena (.bss) | the start-up heap: TLS config, thread handles (73 KiB used) | 0.50 MiB |
| TLS arenas | `maxConnections` × `tlsArenaKiB` | 8.00 MiB |
| API worker buffers | `maxConnections` × (head 16 KiB + body `maxBody`+2 KiB + response 64 KiB+`maxSandboxes`×512 B + scratch 256 KiB + 64 KiB) | 7.47 MiB |
| image server buffers | `imageConnections` × (64 + 16 KiB), hub/gateway only | 0 |
| background + migration buffers | 256 + 64 KiB + `maxMigrations` × (64 + 16 KiB) | 0.62 MiB |
| tables | (`maxSandboxes` + `maxConnections` + `maxMigrations` + 4) sandbox slots × 20.8 KiB + migration slots | 0.92 MiB |
| thread stacks | `maxConnections` × 512 KiB + `maxMigrations` (+ image) × 256 KiB + start/background 512 KiB + main 256 KiB | 9.75 MiB |
| **resident bound (RSS)** | sum of the above | **31.51 MiB** |
| socket buffers (kernel, charged to the cgroup) | (`maxConnections` + image + 2 listeners) × 4 × 64 KiB (SO_RCVBUF/SO_SNDBUF fixed: no autotuning) | 4.50 MiB |
| other kernel memory | page tables, 16 KiB kernel stack per thread, fds, pipes, page cache of the small files it writes | 8.00 MiB |
| **cgroup bound** | | **44.01 MiB** |

## How it is enforced

1. **One fixed heap, never the OS.** The global allocator (`src/alloc.rs`) serves from arenas only: the boot arena (a
   static array) and one arena per API worker, carved from a single region mapped at start (`MAP_POPULATE`). It never
   calls mmap/brk afterwards. An exhausted worker arena falls back to the rest of the boot arena (counted as
   `overflow`); past that an allocation returns null (counted as `failed`) instead of growing.
2. **sandboxd's own code does not allocate after start.** HTTP parsing, JSON (a validating scanner without
   recursion; nesting over 64 is refused), command lines (fork + execve with argv/envp built in a stack buffer),
   command output (read through `poll` into a fixed buffer; the rest is drained and dropped), files, logs (one 4 KiB
   stack buffer per line, written straight to stdout) all work in fixed buffers. Every buffer is either on a
   fixed-size stack or in the region. Bigger input than a buffer is an error (413, 500 "too large"), never growth.
   The allocator counts any allocation made outside rustls after start (`stray`); the soak test requires 0 (and
   `--features audit` aborts on the first one, for a debugger's backtrace: that is how the two std calls that
   allocated, `env::var_os` and an `io::Error`'s Display, were found and replaced).
3. **rustls is the only heap user, and it is fenced.** Each worker serves one connection at a time on its own arena;
   the TLS state is created, used and dropped there, so the arena is empty between connections (checked after each
   connection; logged if not). Before every call into rustls, the worker checks that its arena has 200 KiB free (more
   than one call can allocate: rustls joins a handshake message of at most 64 KiB and parses it; measured worst case
   113 KiB for a 60 KiB client certificate chain); if not, that connection is closed (`IoErr::Budget`), never the
   process. No session cache, no tickets: nothing in TLS outlives a connection. Panics in a request (bugs) are
   caught per connection (`catch_unwind`) and close that connection only.
4. **Fixed capacity everywhere.** No unbounded queue: a worker accepts its next connection only when free (the kernel
   queue is bounded by the backlog), slots for sandboxes, per-name operation locks, migrations and image pulls are
   fixed tables (full = 503), no threads are spawned after start (22 threads with the defaults), no recursion on input,
   no logs kept in memory. Child processes are bounded by the threads that wait for them (one per API worker, one per
   migration slot, two per image pull) and live in their own cgroup.

And then the OS makes the bound hard:

- **mlockall(MCL_CURRENT | MCL_FUTURE)** after start: every page of the binary, arenas, buffers and stacks is
  resident and never swapped or paged out. So RSS is the budget from the first second (flat), and memory can't
  "appear" later under load.
- **cgroups** (unit `Delegate=yes`, `DelegateSubgroup=daemon`): sandboxd runs in `<unit>/daemon` with
  `memory.min` = `memory.max` − 8 MiB = the cgroup bound (`memory.swap.max` 0), and every command it runs in
  `<unit>/helpers` (`memory.max` = `helperMemoryMaxMiB`). A helper that runs out (a crane pull, a runsc CLI) is killed
  by its own cgroup's OOM killer and that request fails; sandboxd's reserve can't be taken by anything else. The unit
  keeps the reservation of its own (MemoryMin 160M, also on system.slice so it protects; Max 512M for both).
- Sockets get fixed 64 KiB buffers (kernel memory charged to sandboxd's cgroup would otherwise autotune to MiBs).

Why threads and not async: a fixed pool of blocking threads (one per connection, one per migration slot) makes every
bound a product of two configured numbers, and there is nothing to queue: a connection waits in the kernel until a
worker is free. tokio would add a runtime with its own allocations and unbounded task spawning that we would have to
fence with semaphores anyway. The cost is 512 KiB of locked stack per connection, which is in the table.

Dependencies (all permissive): rustls 0.23 (Apache-2.0/ISC/MIT) with ring 0.17 (Apache-2.0 AND ISC), rustls-webpki
(ISC), rustls-pki-types, untrusted, subtle (BSD-3), zeroize, once_cell (MIT/Apache-2.0), libc, linked_list_allocator
(MIT/Apache-2.0). No tokio, no serde.

## How it is checked

- `cargo test` (aviary `ci.yml`, job `test`): JSON (validation, no recursion on 10 000 nested brackets), spec checks, nft
  rules and names identical to the Node code (sha256 names), bundle, Node state files read back, command runner
  (bounded output, timeouts, a grandchild holding the pipe), and **arena exhaustion**: a server thread on a 256 KiB
  arena accepts a normal client, refuses one with a ~60 KiB certificate chain (`IoErr::Budget`, arena back to 0, no
  crash), then serves the next normal client; the allocator returns from the boot arena past a worker arena and null
  past both (counted, no abort).
- `tests/soak.py BINARY` (aviary `ci.yml`, job `sandboxd-soak`; in jarvis3 also by hand with `--node ../src/main.ts`
  for the comparison): two daemons (127.0.0.1,
  127.0.0.2) with stand-ins for runsc/ip/nft/wg (`tests/fake/`), mlockall on, then
  - every endpoint, 403 for a client certificate with another CN, a migration A → B, a send that won't converge
    (409, still there), restart recovery from the state files;
  - **soak** (60 s locally, 45 s in CI): 6 sandboxes on A and a 20-second migration A ↔ B in flight all the time,
    while 32 clients (twice `maxConnections`) hammer every endpoint (create/delete with 56 KiB bodies, 200 KiB bodies,
    10 000-deep JSON, peers, images, receive polls, intruder certificates) and 4 hostile clients send garbage,
    half handshakes, a ClientHello claiming 64 KiB, and hold silent connections;
  - RSS sampled every 250 ms: **29.30-29.31 MiB on both daemons for the whole run** (bound 31.51 MiB); allocator:
    `stray` 0, `overflow` 0, `failed` 0, TLS arena peak 51 KiB of 512 KiB, API stack peak 236 KiB of 512 KiB,
    other threads 144 KiB of 256 KiB (stacks are painted at start; GET /v1/host reports the high-water marks).
  - The Node sandboxd under the same load (`--node`): 88.1 MiB idle, 153.2 MiB peak.
- In production, GET /v1/host has `sandboxd.memory` (budget, rss, arena use and peaks, stack peaks, stray, overflow,
  failed) and `sandboxd.limits`.

## Not covered by this bound

- **runsc / gVisor** (Go): each sandbox's sentry, gofer and its memory are in the sandbox's own cgroup
  `/sbx/<name>` (memory.max = the sandbox's limit, 1 GiB; pids 4096), under `/sbx` capped at MemTotal −
  reserveMemory (`sbx-cgroup`). A sandbox over its limit dies alone (HOST.md "Capacity").
- **The commands sandboxd runs** (runsc CLI calls incl. `checkpoint --migrate-to` and `restore --migrate-listen` until
  the restore joins the sandbox's cgroup, crane, tar, curl, ip, nft, wg) and runsc's per-sandbox `runsc-fd-parking`
  sidecars (~8 KiB each): in `<unit>/helpers`, capped at `helperMemoryMaxMiB`.
- **The scratch copy of a migration** (HOST.md "Scratch on migration"): rsync on the source and the
  target's `sandboxd migrate-relay` (the same binary, its own process: a 1 MiB relay buffer and normal sockets) with
  its `rsync --daemon` children run in `<unit>/helpers`; sandboxd itself only waits for them.
- **Migration images** on the tmpfs `/run/sbx/ckpt`: charged to the receiving sandbox's cgroup (the restore joins it
  before it receives).
- **The kernel** itself (and memory of the netns/veth/nft objects per sandbox): the host reserve (`reserveMemory`) is
  what's left for it, the system's daemons and the unit; the controller never places more sandbox memory than
  MemTotal − reserve.
- Page cache sandboxd causes (its state files, logs) is charged to it but reclaimable; it is inside the 8 MiB kernel
  allowance and can't cause an OOM.

## Rollout (2026-10-21)

- Staging: two m7i.xlarge from the new AMI (jarvis3 release `sandboxd-954535256795`): the numbers
  above; migration A→B 1.4 s (final copy 51 ms, restore 83 ms), back 0.9 s (85 / 41 ms), the 300 MiB writer kept its
  PID and counted on; sandboxd restarted under 6 running sandboxes (same sentry PIDs); gateway mode on one of them
  (egress targets read back from the live nft table after deleting the file); image server 449 MB streamed.
- Gateway (a k3s node): jarvis3's `gateway.sh` with the release; a probe of a live AWS sandbox's agent through
  the gateway every 0.2 s during the swap: 45/45 answered; nft table byte-identical before/after; the controller's
  next calls (peers) went to the Rust daemon.
- AWS: launch template version 4 = the new AMI. The live sandbox on the last Node host was moved by retiring that
  host: the scaler launched a Rust host (active 29.6 s after launch; Node hosts: 40-47 s) and live-migrated it Node →
  Rust (5.6 s, 2 rounds, final copy 32 ms, pause 579 ms); the probe saw 1 of 711 answers missing (the switch).
  Then template version 5 = a new AMI (jarvis3 release `sandboxd-570eeac5d472`, with runsc `--allow-suid`); the older
  AMIs (incl. the last Node one) were pruned.

## Changes in behaviour (all additive)

- GET /v1/host also returns `sandboxd` (implementation, version, memory, limits); 400 instead of 500 for a bad spec;
  503 when a fixed table is full (`maxSandboxes`, `maxMigrations`, operations at once); 411 for chunked bodies.
- The hub/gateway keeps the egress targets across restarts (`/var/lib/sbx/egress-targets`; at the first start after
  the Node sandboxd they are read back from the live nft table), so an upgrade doesn't cut sandboxes' egress until
  the controller's next push. A burst host only ensures the hub peer at start instead of dropping its other peers.
- State files are written atomically (temporary file + rename), same format (the Node daemon's files are read).

## Since the move to aviary (2026-11-02)

- Built in the aviary workspace with its own Cargo profile `sandboxd` (`cargo build --profile sandboxd -p sandboxd`):
  the same settings as before (opt-level s, fat LTO, one codegen unit, strip) and `panic = "unwind"`, which the
  per-request `catch_unwind` needs; the workspace release profile aborts on panic, so sandboxd is never built with it.
- The workspace lints apply (clippy pedantic, `unsafe_code` allowed per module with a `// SAFETY:` comment on every
  block, checked by `clippy::undocumented_unsafe_blocks`); the environment is read only in `src/env.rs`.
- The client certificate CN sandboxd serves is `hermit` (was `sandbox-controller`), matching `fabric/host/make-certs.sh`
  (hermit spec 14.5). Nothing else in the wire protocol changed; `sandboxd-api` holds it as types, and
  `crates/sandboxd-api/tests/live.rs` parses every answer of the soak's functional part with them.
- Measured in the aviary soak (45 s, build runner): RSS 29.37-29.38 MiB on both daemons, bound 31.52 MiB, `stray`,
  `overflow`, `failed` 0; binary 1.72 MB.
