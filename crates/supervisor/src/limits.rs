//! Every constant of supervisor spec section 20, and nothing else configurable: the same values in every deployment.
//! The constants of the parts other packages build (FUSE, the Claude Code driver) are here too, so the one table
//! stays in one file.

use std::time::Duration;

const MIB: u64 = 1024 * 1024;

// memory (5.3, 5.4)

/// The high watermark below the limit: the guard kills at `L - MEM_RESERVE`.
pub const MEM_RESERVE: u64 = 128 * MIB;
/// The guard's sampling interval.
pub const MEM_SAMPLE: Duration = Duration::from_millis(500);
/// The sampling interval above [`MEM_FAST_FRACTION`] of the limit.
pub const MEM_SAMPLE_FAST: Duration = Duration::from_millis(100);
/// Usage above this fraction of `L` (in percent) samples at [`MEM_SAMPLE_FAST`].
pub const MEM_FAST_PERCENT: u64 = 80;
/// A process smaller than this is never the guard's victim (Claude Code's main process is chosen instead).
pub const MIN_VICTIM_RSS: u64 = 32 * MIB;
/// The wait after a kill before the next kill decision.
pub const KILL_COOLDOWN: Duration = Duration::from_secs(2);
/// The supervisor's RSS budget, checked by the soak test (target 40 MiB).
pub const SUPERVISOR_RSS_BUDGET: u64 = 64 * MIB;
pub const SUPERVISOR_RSS_TARGET: u64 = 40 * MIB;
/// The keeper's RSS budget.
pub const KEEPER_RSS_BUDGET: u64 = 4 * MIB;
/// The agent cgroup's limit below `L` (5.2): the same reserve as the guard.
pub const AGENT_CGROUP_RESERVE: u64 = MEM_RESERVE;

// FUSE (9; built by WP-22)

pub const FUSE_MAX_WRITE: u32 = 1024 * 1024;
pub const FUSE_MAX_PAGES: u16 = 256;
pub const FUSE_SLOTS: usize = 64;
pub const FUSE_ATTR_VALID: Duration = Duration::from_secs(1);
/// Journal slots, each `FUSE_MAX_WRITE + 8 KiB`, `MAP_SHARED`.
pub const JOURNAL_SLOTS: usize = 64;
pub const JOURNAL_SLOT_BYTES: usize = 1024 * 1024 + 8 * 1024;

// magpie (9.5, 10, 11)

pub const MAGPIE_CONCURRENCY: usize = 16;
pub const MAGPIE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
pub const CC_WRITE_RETRY_FIRST: Duration = Duration::from_millis(200);
pub const CC_WRITE_RETRY_MAX: Duration = Duration::from_secs(10);
/// One op failing while heartbeats succeed: EIO and `cc.write_stuck` after this long.
pub const STUCK_OP: Duration = Duration::from_mins(10);
pub const REPORT_RETRY_FIRST: Duration = Duration::from_millis(500);
pub const REPORT_RETRY_MAX: Duration = Duration::from_secs(10);
/// The lease retry on `not_current_sandbox` / `conversation_migrating` (no exit).
pub const LEASE_RETRY_NOT_CURRENT: Duration = Duration::from_secs(30);
/// The lease retry on 503 and transport errors: [`LEASE_RETRY_FIRST`] doubling to [`LEASE_RETRY_MAX`].
pub const LEASE_RETRY_FIRST: Duration = Duration::from_millis(250);
pub const LEASE_RETRY_MAX: Duration = Duration::from_secs(10);
pub const COMMAND_WAIT_MS: u32 = 25_000;
pub const COMMAND_REQUEST_TIMEOUT: Duration = Duration::from_secs(40);
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);

// hermit (15)

/// The activity cadence's ceiling: the supervisor reports every `min(activityEverySeconds, 15)` s and on change.
pub const ACTIVITY_EVERY_MAX: Duration = Duration::from_secs(15);
/// The descriptor read at start: [`DESCRIPTOR_RETRY_FIRST`] doubling to [`DESCRIPTOR_RETRY_MAX`], forever (4.3 step 2).
pub const DESCRIPTOR_RETRY_FIRST: Duration = Duration::from_millis(250);
pub const DESCRIPTOR_RETRY_MAX: Duration = Duration::from_secs(2);
/// The descriptor is re-read at start, at each `start_turn` and this often.
pub const DESCRIPTOR_REFRESH: Duration = Duration::from_mins(10);
/// A failed activity report is retried at this pace (events kept, at most 32).
pub const ACTIVITY_RETRY: Duration = Duration::from_secs(2);
/// The per-attempt timeout of the supervisor's calls to hermit.
pub const HERMIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Pooled connections per fixed name, and the connect timeout (17.1).
pub const POOL_PER_NAME: usize = 8;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// Claude Code (7; driven by WP-23)

pub const INIT_WAIT: Duration = Duration::from_secs(30);
/// An interrupt that has not ended the turn after this long becomes SIGTERM, then [`KILL_AFTER_TERM`], SIGKILL.
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(15);
/// A graceful generation end: close stdin, wait [`STDIN_CLOSE_GRACE`], SIGTERM the group, wait
/// [`KILL_AFTER_TERM`], SIGKILL.
pub const STDIN_CLOSE_GRACE: Duration = Duration::from_secs(5);
pub const KILL_AFTER_TERM: Duration = Duration::from_secs(3);
pub const HOLD_SILENCE: Duration = Duration::from_mins(30);
pub const WAKEUP_SLACK: Duration = Duration::from_mins(2);
pub const SUPERSEDE_WAIT: Duration = Duration::from_secs(5);
pub const CANCEL_HOLD_WAIT: Duration = Duration::from_secs(10);
pub const ASK_TIMEOUT: Duration = Duration::from_mins(30);
/// Crash loop: [`CRASH_LOOP_EXITS`] exits within [`CRASH_LOOP_WINDOW`] refuse new starts for [`CRASH_LOOP_PAUSE`].
pub const CRASH_LOOP_EXITS: usize = 3;
pub const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60);
pub const CRASH_LOOP_PAUSE: Duration = Duration::from_mins(5);
pub const CACHE_BUDGET: u64 = 256 * MIB;
/// A generation's stdout is rotated at an idle point once over this size.
pub const STDOUT_ROTATE: u64 = 1024 * MIB;
pub const STDOUT_READ_BUFFER: usize = 64 * 1024;
pub const STDOUT_PARSE_MAX: usize = 64 * 1024 * 1024;
pub const STDERR_LINES_PER_SECOND: u32 = 100;
pub const STDERR_TAIL: usize = 4 * 1024;
pub const HOOK_BUDGET: Duration = Duration::from_secs(110);
pub const TITLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const TITLE_MAX_BYTES: usize = 1024 * 1024;

// run state (4.4)

/// `run.json` is written at most this often, and at once on a state change.
pub const RUN_STATE_THROTTLE: Duration = Duration::from_millis(100);

// /v1/exec (15.4)

pub const EXEC_UID: u32 = 1000;
pub const EXEC_GID: u32 = 1000;
pub const EXEC_TIMEOUT_MAX: Duration = Duration::from_secs(60);
pub const EXEC_IO_MAX: usize = 1024 * 1024;
pub const EXEC_CONCURRENCY: usize = 2;

// dockerd (4.6)

pub const DOCKERD_START_WAIT: Duration = Duration::from_secs(60);

// keeper (4.2)

/// The supervisor's restart delays; the last one repeats, and the sequence starts again after
/// [`KEEPER_BACKOFF_RESET`] of uptime.
pub const KEEPER_BACKOFF: [Duration; 6] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
pub const KEEPER_BACKOFF_RESET: Duration = Duration::from_secs(60);
/// On SIGTERM to PID 1 the keeper waits this long for the supervisor's graceful exit, then SIGKILLs everything.
pub const KEEPER_STOP_GRACE: Duration = Duration::from_secs(20);
/// The stdin dedupe ring: the last 16 `msg_id`s per child.
pub const KEEPER_STDIN_RING: usize = 16;
/// The largest keeper packet.
pub const KEEPER_PACKET: usize = 64 * 1024;
/// The exits the keeper remembers.
pub const KEEPER_EXITS: usize = 64;

// lifecycle (15.5)

/// A wall-clock jump of more than this across one tick (REALTIME advancing beyond MONOTONIC) means a restore.
pub const CLOCK_JUMP: Duration = Duration::from_secs(30);
/// The clock-jump check's tick.
pub const CLOCK_TICK: Duration = Duration::from_secs(1);
/// The deadline of a lifecycle call that names none.
pub const PREPARE_DEADLINE_DEFAULT: Duration = Duration::from_secs(10);
