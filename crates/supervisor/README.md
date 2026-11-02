# supervisor

The in-sandbox supervisor and its keeper (supervisor spec; platform R12, R32-R35). One supervisor per sandbox, one
sandbox per conversation. Built in three packages: WP-21 (this state of the crate: the process model, the lifecycle,
hermit's side), WP-22 (the `~/.claude` filesystem), WP-23 (the Claude Code driver).

## Binaries

| Binary | What |
|---|---|
| `supervisor-keeper` (`src/bin/keeper.rs`) | PID 1 (the image's entrypoint, no arguments, no environment read). Starts `supervisor run` with its own environment and restarts it (0.1, 0.2, 0.5, 1, 2, 5 s; reset after 60 s up); is the parent of everything the supervisor starts; owns their stdin pipes; keeps the `/dev/fuse` fd; reaps; remembers the last 64 exits; on SIGTERM waits 20 s for the supervisor, then SIGKILLs everything. One thread, `poll(2)`, every buffer allocated before the loop. |
| `supervisor` (`src/bin/supervisor.rs`) | `supervisor run` (the start sequence of 4.3), `supervisor --version`, and `sandbox-secret <KEY>` when called by that name (a symlink). |

## Modules

| Module | Spec |
|---|---|
| `limits` | 20: every constant, nothing configurable |
| `keeper::{proto, server, watch}` and `keeper::Client` | 4.1, 4.2: the protocol (SPAWN, STDIN/STDIN_MORE, CLOSE_STDIN, SIGNAL, PUT_FD, GET_FD, EXITS, STATUS), the keeper, the async exit stream |
| `state` | 4.4: `run.json`, atomic, at once on a state change, at most every 100 ms otherwise |
| `memguard` | 5.2, 5.3: the agent cgroup (v2 or v1) and the user-space guard |
| `docker` | 4.6: dockerd on demand |
| `boot` | DESIGN 12 D1, platform R32, 6.2: the sandbox token from `HTTP_PROXY`, the fixed-names client through that proxy, the child environment |
| `hermit::{Hermit, activity, control, exec}` | 15.1-15.4: the descriptor, activity reports, `:7070`, `/v1/exec` |
| `lifecycle` | 15.5: the state machine, admission, release, prepare, resumed, clock-jump detection; the `Agent` and `Filesystem` seams |
| `local` | 15.2: `agent.sock` secrets, `sandbox-secret` |
| `magpie` | 4.3 step 5: the lease at start (WP-22 adds the rest of the client) |
| `start` | 4.3, 4.7: the start sequence, `/etc/hosts` and apt's proxy file, the re-attach check, SIGTERM |

## The seams for WP-22 and WP-23

`lifecycle::Agent` (the Claude Code driver) and `lifecycle::Filesystem` (`~/.claude`) are what the start sequence
and the lifecycle call: `Filesystem::mount` at step 6, `Agent::attach` at step 7 with an `AttachContext` (the prior
`run.json`, what `start::reattach` found of its generation, the lease, the descriptor, the persisted state, the keeper
client and exit stream, the boot environment, the agent cgroup), and the release, pause and resume hooks. A
`start_turn` goes through `Lifecycle::admit` before anything else. Until those packages land, the binary runs with
`IdleAgent` and `UnbuiltFilesystem`, which refuses to mount: such a sandbox answers health `ready: false` for ever
rather than claim a `~/.claude` it does not have.

## Tests

- Keeper protocol (`src/keeper/tests.rs`): spawn with an exact environment, stdin written whole and at most once per
  `msg_id` (the 16-entry ring), an incomplete message discarded (small and spooled), a message Claude Code has not
  read holding up nothing else, a child exiting mid-message, the fd store across clients, the exits ring, signals to
  children only, the supervisor's restart backoff, the SIGTERM stop, `boot_id` kept.
- Lifecycle (`src/lifecycle.rs`): not ready until descriptor, lease, mount and workspace; unforced release and pause
  refused while busy or with an ask open (nothing changed); a forced release ends the turn first; a second release
  waits for the first; a `start_turn` racing a release is either refused or makes it busy; a blocked write refuses an
  unforced pause; clock jumps.
- Memory guard on a fake `/proc` (`src/memguard.rs`), `run.json` (`src/state.rs`), exec (`src/hermit/exec.rs`), dockerd
  with a stand-in daemon (`src/docker.rs`, as root).
- Start sequence (`src/start/tests.rs`) through fake hermit and fake magpie behind `test-gateway`, the sandbox token in
  `HTTP_PROXY`: ready, reported, 401/503, not ready without the lease, release refused while busy, a forced release
  exiting 0, a pending release in the activity reply; the re-attach check against a real keeper.
- `tests/soak.rs`: the supervisor as a child process under `:7070`, exec and socket load for 20 s, RSS under 64 MiB and
  flat after warm-up (debug build: 23 MiB peak); `soak_long` (1 h, ignored) for `nightly.yml`.

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
   package runs the 20 s soak in `ci.yml` and provides `soak_long`. The keeper's 4 MiB RSS needs it as PID 1 with root
   (its fixed paths), so it is left to the image smoke test (WP-24).
