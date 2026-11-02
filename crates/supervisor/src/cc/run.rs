//! The run state machine and the driver (supervisor 4.5, 7, 8.2, 10, 11): one Claude Code generation lives across
//! turns; magpie's commands start, interrupt and kill turns, answer asks, stop tasks and make titles; Claude Code's
//! stdout moves the run state and produces the reports.
//!
//! ```text
//!             start_turn delivered (user message written)
//!   idle ─────────────────────────────────────────────────────> running(person)
//!    ^  ^                                                          │  │
//!    │  │ last result, no tasks                                    │  │ interrupt cmd
//!    │  └──────────────────────────────────────────────────────────┘  v
//!    │                 last result, tasks outstanding           stopping ──(result, or 15 s → kill gen)──> idle/held
//!    │                         │
//!    │                         v
//!    │  tasks all ended      held ───(start_turn: stop_task each, then message)──> running(person)
//!    └────────────────────── held ───(Claude Code output without a pending input)──> running(agent)
//!    idle ───(Claude Code output without a pending input, e.g. a wake-up)──> running(agent)
//!    running(agent) ──(result)──> idle | held
//!    any ──(generation exits)──> idle (turn failed run.exited / run.oom)
//! ```
//!
//! The state lives behind one lock that is never held across an `await`: stdout lines, exits, commands and the
//! one-second tick change it and hand back what to do (stdin lines, reports, signals), which runs after the lock is
//! released. Everything a restarted supervisor needs is put into `run.json` together with the stdout offset of the
//! line that caused it, so the next supervisor resumes the stdout file exactly there.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use gateway_api::models::Models;
use hermit_api::sandbox_surface::{ActivityEvent, ActivityEventKind, ActivityState};
use magpie_api::errors::DataCode;
use magpie_api::events::{TaskEvent, TaskState as WireTaskState};
use magpie_api::sandbox::{
    AckResult, AgentTurnRequest, AnswerAsk, AskOutcome, AskReport, AskResolvedReport, Command, CommandBody,
    CommandsQuery, Heartbeat, HeartbeatLastTurn, HeartbeatState, KillReason, ReportError, ReportedState, ReportedTask,
    StartTurn, SupervisorFacts, TaskReportEvent, TurnStateReport,
};
use magpie_api::turns::TurnOutcome;
use serde_json::{Value, json};
use sha2::Digest as _;
use supervisor_api::control::{Generation, TurnOrigin};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

use crate::boot::BootEnv;
use crate::cc::control::{self, Answer};
use crate::cc::launch::{self, Launch, Session};
use crate::cc::report::{Item, Reporter};
use crate::cc::stream::{Line, Out, ResultMsg, Tail, TaskKind, TaskMsg};
use crate::cc::tasks::{Change as TaskChange, Tracker};
use crate::cc::{Transcript, managed, prompt};
use crate::hermit::Hermit;
use crate::hermit::activity::Activity;
use crate::keeper::{self, ExitRecord, StdinOutcome};
use crate::lifecycle::{Agent, AttachContext, BoxFuture, Lifecycle, RunStatus};
use crate::limits::{
    ASK_TIMEOUT, CANCEL_HOLD_WAIT, COMMAND_REQUEST_TIMEOUT, COMMAND_WAIT_MS, CRASH_LOOP_EXITS, CRASH_LOOP_PAUSE,
    CRASH_LOOP_WINDOW, HEARTBEAT_EVERY, HOLD_SILENCE, INIT_WAIT, INTERRUPT_GRACE, KILL_AFTER_TERM, STDERR_TAIL,
    STDIN_CLOSE_GRACE, STDOUT_ROTATE, SUPERSEDE_WAIT,
};
use crate::model::Model;
use crate::state::{AskState, Change, Delivered, GenState, LastTurn, Persisted, RunPart, RunState};
use crate::uploads::{self, Uploads};

/// The namespace of the stdin `msg_id`s: `msg_id` = the first 16 bytes of SHA-256(namespace ‖ `boot_id` ‖ "stdin" ‖
/// `cmdSeq`), so a restarted supervisor derives the same id for the same command (10.2 step 5).
pub const SUPERVISOR_OP_NS: Uuid = Uuid::from_u128(0x6a1f_3c2e_9b4d_4e7a_8c55_0d2b_71e4_a903);
/// The driver's tick (interrupt grace, asks, the hold).
const TICK: Duration = Duration::from_secs(1);
/// How long a spawn waits for the models route before the turn fails (magpie's `starting` watchdog is 120 s).
const MODELS_WAIT: Duration = Duration::from_secs(110);
/// Acked commands kept in `run.json` beyond the unacked ones.
const DELIVERED_KEPT: usize = 64;
/// The poll's pause after a failed long-poll.
const POLL_RETRY: Duration = Duration::from_secs(1);

/// Where the driver's files are and who Claude Code runs as (the sandbox's in production; a test directory in tests).
#[derive(Clone, Debug)]
pub struct DriverConfig {
    pub launch: Launch,
    pub managed_dir: PathBuf,
    pub gen_dir: PathBuf,
    pub uploads: Uploads,
}

impl DriverConfig {
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            launch: Launch::sandbox(),
            managed_dir: managed::MANAGED_DIR.into(),
            gen_dir: crate::state::GEN_DIR.into(),
            uploads: Uploads::sandbox(),
        }
    }
}

/// The run's state (7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Idle,
    Starting,
    Running,
    Stopping,
    Held,
}

impl RunKind {
    fn activity(self) -> ActivityState {
        match self {
            RunKind::Idle => ActivityState::Idle,
            RunKind::Starting => ActivityState::Starting,
            RunKind::Running => ActivityState::Running,
            RunKind::Stopping => ActivityState::Stopping,
            RunKind::Held => ActivityState::Held,
        }
    }
    fn as_str(self) -> &'static str {
        self.activity().as_str()
    }
    fn parse(s: &str) -> Self {
        match s {
            "starting" => RunKind::Starting,
            "running" => RunKind::Running,
            "stopping" => RunKind::Stopping,
            "held" => RunKind::Held,
            _ => RunKind::Idle,
        }
    }
}

/// Why the supervisor ended a generation (its exit is then not a crash).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Why {
    /// The interrupt did not end the turn in 15 s.
    Interrupt,
    /// magpie's `kill`.
    Kill { watchdog: bool },
    /// The memory guard.
    Oom,
    /// A release.
    Release,
    /// An idle-point end (configuration change, stdout rotation, another session).
    Graceful,
    /// Initialisation did not complete.
    Init,
    /// The lease was lost (9.5 item 7).
    LeaseLost,
    /// One transcript write kept failing (9.5 item 4).
    Stuck(String),
    /// The cache disagrees with magpie's bytes.
    Forked(String),
}

/// An open turn.
#[derive(Clone, Debug)]
struct Turn {
    id: Uuid,
    origin: TurnOrigin,
    started: Instant,
    interrupt_sent: Option<OffsetDateTime>,
    /// uuids of the main-thread messages stdout showed during the turn (8.2).
    uuids: Vec<String>,
    usage: Option<magpie_api::sandbox::Usage>,
}

impl Turn {
    fn new(id: Uuid, origin: TurnOrigin) -> Self {
        Self {
            id,
            origin,
            started: Instant::now(),
            interrupt_sent: None,
            uuids: Vec::new(),
            usage: None,
        }
    }
}

/// A live generation.
struct Gen {
    n: u32,
    pid: i32,
    since: u64,
    stdout: PathBuf,
    stderr: PathBuf,
    offset: u64,
    session_id: String,
    initialized: bool,
    started_at: OffsetDateTime,
    why: Option<Why>,
    writer: mpsc::UnboundedSender<Write>,
    /// The descriptor `etag` its managed files came from.
    etag: String,
}

/// One stdin message for the generation's writer.
struct Write {
    msg_id: [u8; keeper::proto::MSG_ID],
    bytes: Vec<u8>,
    done: Option<oneshot::Sender<Result<StdinOutcome, String>>>,
}

/// An open ask (7.6).
#[derive(Clone, Debug)]
#[allow(clippy::struct_field_names)]
struct Ask {
    ask_id: String,
    cli_request_id: String,
    tool_name: String,
    tool_use_id: Option<String>,
    since: OffsetDateTime,
    since_mono: Instant,
}

/// The state behind the lock.
struct St {
    run: RunKind,
    turn: Option<Turn>,
    /// A person's turn written while an agent turn ran (7.5): it becomes the turn at the agent turn's result.
    queued: Option<Turn>,
    /// The turn a `start_turn` is preparing (state `starting`).
    starting: Option<Uuid>,
    /// An interrupt or kill arrived for the turn being prepared.
    cancel_starting: Option<Uuid>,
    pending_inputs: u32,
    live: Option<Gen>,
    last_gen: u32,
    tracker: Tracker,
    asks: Vec<Ask>,
    delivered: BTreeMap<u64, Delivered>,
    cmd_after: u64,
    last_turn: Option<LastTurn>,
    last_output: OffsetDateTime,
    idle_since: Option<OffsetDateTime>,
    crashes: VecDeque<Instant>,
    failing_until: Option<Instant>,
    control_waiters: HashMap<String, oneshot::Sender<Result<(), String>>>,
    /// The descriptor `etag` the managed files were written from.
    etag: String,
    /// The release reason, once a forced release ends the turn.
    release: Option<String>,
    logged_types: HashSet<String>,
    msg_seq: u64,
    /// The command loop is stopped (a pause or a release).
    quiesced: bool,
}

impl St {
    fn new() -> Self {
        Self {
            run: RunKind::Idle,
            turn: None,
            queued: None,
            starting: None,
            cancel_starting: None,
            pending_inputs: 0,
            live: None,
            last_gen: 0,
            tracker: Tracker::default(),
            asks: Vec::new(),
            delivered: BTreeMap::new(),
            cmd_after: 0,
            last_turn: None,
            last_output: OffsetDateTime::now_utc(),
            idle_since: Some(OffsetDateTime::now_utc()),
            crashes: VecDeque::new(),
            failing_until: None,
            control_waiters: HashMap::new(),
            etag: String::new(),
            release: None,
            logged_types: HashSet::new(),
            msg_seq: 0,
            quiesced: false,
        }
    }

    fn status(&self) -> RunStatus {
        let state = if self.starting.is_some() && self.run == RunKind::Idle {
            ActivityState::Starting
        } else {
            self.run.activity()
        };
        RunStatus {
            state,
            turn_id: self.turn.as_ref().map(|t| t.id).or(self.starting),
            origin: self.turn.as_ref().map(|t| t.origin),
            tasks: u32::try_from(self.tracker.len()).unwrap_or(u32::MAX),
            asks: u32::try_from(self.asks.len()).unwrap_or(u32::MAX),
            last_output_at: Some(self.last_output.into()),
            idle_since: self.idle_since.map(Into::into),
        }
    }

    fn set_run(&mut self, k: RunKind) {
        if k == RunKind::Idle && self.run != RunKind::Idle {
            self.idle_since = Some(OffsetDateTime::now_utc());
        } else if k != RunKind::Idle {
            self.idle_since = None;
        }
        self.run = k;
    }
}

/// What to do once the lock is released.
enum Fx {
    Report(Item),
    Task(TaskReportEvent),
    Stdin(Vec<u8>),
    SetTurn(Option<Uuid>),
    /// SIGTERM the group, 3 s, SIGKILL.
    Terminate(i32),
    Activity(ActivityEvent),
    IdlePoint,
    MainTranscript(String),
}

/// What the start sequence handed over at attach.
struct Ctx {
    persisted: Arc<Persisted>,
    keeper: keeper::Client,
    exits: Arc<keeper::watch::Exits>,
    boot: BootEnv,
    cgroup: Option<String>,
    lifecycle: Weak<Lifecycle>,
    activity: Option<Arc<Activity>>,
    boot_id: String,
    restarts: u32,
    conversation: Uuid,
    /// The runtime, for the filesystem's hooks (called from its threads).
    rt: tokio::runtime::Handle,
}

/// The Claude Code driver.
pub struct Driver {
    cfg: DriverConfig,
    model: Model,
    hermit: Hermit,
    transcript: Arc<dyn Transcript>,
    reporter: Arc<Reporter>,
    status: watch::Sender<RunStatus>,
    st: Mutex<St>,
    changed: watch::Sender<u64>,
    ctx: OnceLock<Ctx>,
    /// One `start_turn` at a time (another command runs beside it).
    turn_lane: tokio::sync::Mutex<()>,
    /// One spawn at a time.
    gen_lane: tokio::sync::Mutex<()>,
    poll: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Commands in execution in this process.
    inflight: Mutex<HashSet<u64>>,
    /// Every task the driver started (tests end a driver for good with [`Driver::abort_all`]).
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    me: Weak<Driver>,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver").finish_non_exhaustive()
    }
}

fn origin_str(o: TurnOrigin) -> &'static str {
    match o {
        TurnOrigin::Person => "person",
        TurnOrigin::Agent => "agent",
        TurnOrigin::Import => "import",
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn wire(t: OffsetDateTime) -> magpie_api::Timestamp {
    t.into()
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stdin `msg_id` of command `seq` (10.2 step 5).
#[must_use]
pub fn msg_id(boot_id: &str, seq: u64) -> [u8; 16] {
    let mut h = sha2::Sha256::new();
    h.update(SUPERVISOR_OP_NS.as_bytes());
    h.update(boot_id.as_bytes());
    h.update(b"stdin");
    h.update(seq.to_be_bytes());
    let d = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&d[..16]);
    id
}

fn hex(b: &[u8]) -> String {
    crate::sent_files::hex(b)
}

fn unhex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn error(code: DataCode, message: impl Into<String>) -> ReportError {
    ReportError {
        code,
        message: message.into(),
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Completed,
    Failed(ReportError),
    Canceled,
}

fn outcome_of_result(r: &ResultMsg, turn: &Turn, release: Option<&String>) -> Outcome {
    if let Some(reason) = release {
        return Outcome::Failed(error(DataCode::SandboxLost, format!("released: {reason}")));
    }
    if turn.interrupt_sent.is_some() {
        return Outcome::Canceled;
    }
    if r.subtype == "success" && !r.is_error {
        return Outcome::Completed;
    }
    let text: String = r.result.as_deref().unwrap_or("").chars().take(1000).collect();
    Outcome::Failed(error(DataCode::RunError, format!("{}: {text}", r.subtype)))
}

fn signal_name(sig: i32) -> String {
    match sig {
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGBUS => "SIGBUS".into(),
        other => format!("signal {other}"),
    }
}

fn exit_text(exit: Option<&ExitRecord>) -> String {
    match exit {
        Some(ExitRecord { code: Some(c), .. }) => format!("exit {c}"),
        Some(ExitRecord { signal: Some(s), .. }) => format!("signal {}", signal_name(*s)),
        _ => "exited".into(),
    }
}

fn outcome_of_exit(
    why: Option<&Why>,
    turn: &Turn,
    release: Option<&String>,
    exit: Option<&ExitRecord>,
    tail: &str,
) -> Outcome {
    if let Some(reason) = release {
        return Outcome::Failed(error(DataCode::SandboxLost, format!("released: {reason}")));
    }
    match why {
        Some(Why::Oom) => return Outcome::Failed(error(DataCode::RunOom, "the memory guard killed Claude Code")),
        Some(Why::Kill { watchdog: true }) => {
            return Outcome::Failed(error(DataCode::RunError, "killed by magpie's watchdog"));
        }
        Some(Why::Interrupt | Why::Kill { watchdog: false }) => return Outcome::Canceled,
        Some(Why::LeaseLost) => return Outcome::Failed(error(DataCode::SandboxLost, "the lease was lost")),
        Some(Why::Stuck(m)) => return Outcome::Failed(error(DataCode::CcWriteStuck, m.clone())),
        Some(Why::Forked(m)) => return Outcome::Failed(error(DataCode::RunError, format!("history forked: {m}"))),
        _ => {}
    }
    if turn.interrupt_sent.is_some() {
        return Outcome::Canceled;
    }
    let mut msg = exit_text(exit);
    if !tail.is_empty() {
        msg.push_str(": ");
        msg.push_str(tail);
    }
    Outcome::Failed(error(DataCode::RunExited, msg))
}

fn task_state(m: &TaskMsg) -> WireTaskState {
    match m.status.as_deref() {
        Some("completed") => WireTaskState::Completed,
        Some("failed") => WireTaskState::Failed,
        Some("killed" | "stopped") => WireTaskState::Stopped,
        None if m.kind == TaskKind::Notification => WireTaskState::Completed,
        _ => WireTaskState::Running,
    }
}

fn reported_tasks(t: &Tracker) -> Vec<ReportedTask> {
    t.tasks()
        .iter()
        .map(|t| ReportedTask {
            task_id: t.task_id.clone(),
            tool_use_id: t.tool_use_id.clone(),
            kind: t.kind.clone(),
            description: Some(t.description.clone()),
            started_at: Some(wire(t.started_at)),
        })
        .collect()
}

/// The last 4 KiB of a generation's stderr.
fn stderr_tail(path: &std::path::Path) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = len.saturating_sub(STDERR_TAIL as u64);
    let _ = f.seek(SeekFrom::Start(from));
    let mut b = Vec::new();
    let _ = f.take(STDERR_TAIL as u64).read_to_end(&mut b);
    String::from_utf8_lossy(&b).trim().to_owned()
}

impl Driver {
    #[must_use]
    pub fn new(
        cfg: DriverConfig,
        magpie: magpie_api::client::Client,
        model: Model,
        hermit: Hermit,
        transcript: Arc<dyn Transcript>,
    ) -> Arc<Self> {
        let reporter = Reporter::new(magpie, transcript.clone());
        Arc::new_cyclic(|me| Self {
            cfg,
            model,
            hermit,
            transcript,
            reporter,
            status: watch::channel(RunStatus::idle()).0,
            st: Mutex::new(St::new()),
            changed: watch::channel(0).0,
            ctx: OnceLock::new(),
            turn_lane: tokio::sync::Mutex::new(()),
            gen_lane: tokio::sync::Mutex::new(()),
            poll: Mutex::new(None),
            inflight: Mutex::new(HashSet::new()),
            tasks: Mutex::new(Vec::new()),
            me: me.clone(),
        })
    }

    fn arc(&self) -> Arc<Self> {
        self.me
            .upgrade()
            .unwrap_or_else(|| unreachable!("the driver is alive while its method runs"))
    }

    fn track(&self, h: tokio::task::AbortHandle) {
        let mut t = lock(&self.tasks);
        t.retain(|a| !a.is_finished());
        t.push(h);
    }

    /// Spawns a task of the driver's.
    fn go(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        let h = tokio::spawn(f);
        self.track(h.abort_handle());
    }

    /// Ends every task the driver started (a test's stand-in for a supervisor that died).
    #[cfg(test)]
    pub(crate) fn abort_all(&self) {
        self.stop_poll();
        for a in lock(&self.tasks).drain(..) {
            a.abort();
        }
    }

    fn ctx(&self) -> Option<&Ctx> {
        self.ctx.get()
    }

    /// The reporter (tests inspect it).
    #[must_use]
    pub fn reporter(&self) -> &Arc<Reporter> {
        &self.reporter
    }

    // ---- state, persistence, effects ----

    /// Runs `f` under the lock, persists, publishes the status, then runs the effects.
    fn with<R>(&self, change: Change, f: impl FnOnce(&mut St, &mut Vec<Fx>) -> R) -> R {
        let mut fx = Vec::new();
        let r = {
            let mut st = lock(&self.st);
            let r = f(&mut st, &mut fx);
            self.save(&st, change);
            let s = st.status();
            self.status.send_if_modified(|old| {
                let differs = *old != s;
                if differs {
                    *old = s;
                }
                differs
            });
            r
        };
        self.changed.send_modify(|n| *n += 1);
        self.run_fx(fx);
        r
    }

    fn save(&self, st: &St, change: Change) {
        let Some(ctx) = self.ctx() else { return };
        ctx.persisted.update(change, |s: &mut RunState| {
            s.cmd_after = st.cmd_after;
            s.msg_seq = st.msg_seq;
            s.generation = st.live.as_ref().map(|g| GenState {
                n: g.n,
                pid: g.pid,
                started_at: rfc3339(g.started_at),
                session_id: Some(g.session_id.clone()),
                stdout: g.stdout.to_string_lossy().into_owned(),
                stdout_offset: g.offset,
                initialized: g.initialized,
            });
            let t = st.turn.as_ref();
            s.run = RunPart {
                state: st.run.as_str().to_owned(),
                turn_id: t.map(|t| t.id.to_string()),
                origin: t.map(|t| origin_str(t.origin).to_owned()),
                pending_inputs: st.pending_inputs,
                interrupt_sent_at: t.and_then(|t| t.interrupt_sent).map(rfc3339),
                pending_uuids: t.map(|t| t.uuids.clone()).unwrap_or_default(),
                last_uuid: t.and_then(|t| t.uuids.last().cloned()),
                queued_turn_id: st.queued.as_ref().map(|q| q.id.to_string()),
            };
            s.last_turn.clone_from(&st.last_turn);
            s.tasks = st.tracker.saved();
            s.asks = st
                .asks
                .iter()
                .map(|a| AskState {
                    ask_id: a.ask_id.clone(),
                    cli_request_id: a.cli_request_id.clone(),
                    tool_name: a.tool_name.clone(),
                    since: rfc3339(a.since),
                })
                .collect();
            s.delivered = st.delivered.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        });
    }

    fn run_fx(&self, fx: Vec<Fx>) {
        for f in fx {
            match f {
                Fx::Report(item) => self.reporter.push(item),
                Fx::Task(ev) => self.reporter.task_event(ev),
                Fx::Stdin(bytes) => {
                    let w = lock(&self.st).live.as_ref().map(|g| g.writer.clone());
                    if let Some(w) = w {
                        let _ = w.send(Write {
                            msg_id: keeper::new_msg_id(),
                            bytes,
                            done: None,
                        });
                    }
                }
                Fx::SetTurn(t) => self.transcript.set_turn(t),
                Fx::MainTranscript(p) => self.transcript.set_main_transcript(Some(p)),
                Fx::Terminate(pid) => {
                    if let Some(ctx) = self.ctx() {
                        let k = ctx.keeper.clone();
                        self.go(async move {
                            let k2 = k.clone();
                            let _ = tokio::task::spawn_blocking(move || k2.signal(-pid, libc::SIGTERM)).await;
                            tokio::time::sleep(KILL_AFTER_TERM).await;
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = k.signal(-pid, libc::SIGKILL);
                                k.signal(pid, libc::SIGKILL)
                            })
                            .await;
                        });
                    }
                }
                Fx::Activity(e) => {
                    if let Some(a) = self.ctx().and_then(|c| c.activity.clone()) {
                        a.event(e);
                    }
                }
                Fx::IdlePoint => {
                    let me = self.arc();
                    self.go(async move { me.idle_point().await });
                }
            }
        }
    }

    /// Waits until `f` holds, or `deadline`; whether it holds.
    async fn wait_until(&self, deadline: Option<Instant>, f: impl Fn(&St) -> bool) -> bool {
        let mut rx = self.changed.subscribe();
        loop {
            if f(&lock(&self.st)) {
                return true;
            }
            match deadline {
                Some(d) => {
                    if tokio::time::timeout_at(d, rx.changed()).await.is_err() {
                        return f(&lock(&self.st));
                    }
                }
                None => {
                    let _ = rx.changed().await;
                }
            }
        }
    }

    // ---- turn ends ----

    /// Ends the open turn with `outcome` (the run goes `held` or `idle`), and queues its end report. `wait`: the
    /// turn's uuids must be stored first (8.2); not for a generation that exited (it writes nothing more).
    #[allow(clippy::needless_pass_by_value)] // an outcome is consumed by the turn's end
    fn end_turn_now(&self, st: &mut St, fx: &mut Vec<Fx>, outcome: Outcome, wait: bool, exit_code: Option<i32>) {
        let Some(turn) = st.turn.take() else { return };
        let held = !st.tracker.is_empty() && st.live.is_some();
        let (state, err, out) = match &outcome {
            Outcome::Completed => (ReportedState::Completed, None, TurnOutcome::Completed),
            Outcome::Failed(e) => (ReportedState::Failed, Some(e.clone()), TurnOutcome::Failed),
            Outcome::Canceled => (ReportedState::Canceled, None, TurnOutcome::Canceled),
        };
        let ms = u64::try_from(turn.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(turn = %turn.id, origin = origin_str(turn.origin), outcome = ?out, ms, held, "turn ended");
        let report = TurnStateReport {
            state,
            at: wire(now()),
            provider_session_id: None,
            transcript_path: None,
            exit_code,
            error: err.clone(),
            held: Some(held),
            tasks: held.then(|| reported_tasks(&st.tracker)),
            last_uuid: if wait { turn.uuids.last().cloned() } else { None },
            usage: turn.usage,
            duration_ms: Some(ms),
        };
        let id = turn.id;
        let me = self.me.clone();
        let last = LastTurn {
            id: id.to_string(),
            outcome: match out {
                TurnOutcome::Completed => "completed".into(),
                TurnOutcome::Failed => "failed".into(),
                TurnOutcome::Canceled => "canceled".into(),
            },
            error: err.and_then(|e| serde_json::to_value(e).ok()),
        };
        fx.push(Fx::Report(Item::State {
            turn: id,
            report,
            wait: if wait { turn.uuids } else { Vec::new() },
            done: Some(Box::new(move |ok| {
                if ok && let Some(d) = me.upgrade() {
                    d.with(Change::State, |st, _| st.last_turn = Some(last));
                }
            })),
        }));
        if let Some(q) = st.queued.take() {
            // the person's turn written during an agent turn is now the turn
            st.turn = Some(q);
            fx.push(Fx::SetTurn(st.turn.as_ref().map(|t| t.id)));
            st.set_run(RunKind::Running);
            return;
        }
        st.pending_inputs = 0;
        fx.push(Fx::SetTurn(None));
        if held {
            tracing::info!(tasks = st.tracker.len(), "hold started");
            st.set_run(RunKind::Held);
        } else {
            st.set_run(RunKind::Idle);
            fx.push(Fx::IdlePoint);
        }
    }

    /// Opens an agent-started turn (7.5, 11.2).
    fn open_agent_turn(st: &mut St, fx: &mut Vec<Fx>) {
        let id = Uuid::now_v7();
        tracing::info!(turn = %id, origin = "agent", "turn started");
        st.turn = Some(Turn::new(id, TurnOrigin::Agent));
        st.set_run(RunKind::Running);
        fx.push(Fx::SetTurn(Some(id)));
        fx.push(Fx::Report(Item::AgentTurn(AgentTurnRequest {
            turn_id: id,
            origin: magpie_api::turns::TurnOrigin::Agent,
            at: wire(now()),
        })));
    }

    fn hold_check(st: &mut St, fx: &mut Vec<Fx>, reason: &str) {
        if st.run == RunKind::Held && st.tracker.is_empty() && st.pending_inputs == 0 && st.turn.is_none() {
            tracing::info!(reason, "hold ended");
            st.set_run(RunKind::Idle);
            fx.push(Fx::IdlePoint);
        }
    }

    // ---- stdout ----

    #[allow(clippy::too_many_lines)] // one arm per stdout message of 7.4
    fn on_line(&self, gen_n: u32, line: Line) {
        self.with(Change::Progress, |st, fx| {
            let Some(g) = st.live.as_mut() else { return };
            if g.n != gen_n {
                return;
            }
            g.offset = line.end;
            let gen_key = format!("{gen_n}:{}", line.end);
            if !matches!(line.out, Out::KeepAlive | Out::Unparsed) {
                st.last_output = now();
            }
            // an assistant message, a stream event, or a task-notification wake-up opens an agent turn; a bare
            // `user` line (a tool result, the interrupt marker) does not
            let opens = match &line.out {
                Out::Message {
                    assistant,
                    task_notification,
                    ..
                } => *assistant || task_notification.is_some(),
                other => other.is_turn_output(),
            };
            if opens && st.pending_inputs == 0 && st.turn.is_none() && matches!(st.run, RunKind::Idle | RunKind::Held) {
                Self::open_agent_turn(st, fx);
            }
            match line.out {
                Out::Init { session_id, model } => {
                    tracing::info!(gen = gen_n, model = ?model, "claude initialised");
                    if let (Some(sid), Some(g)) = (session_id, st.live.as_mut())
                        && sid != g.session_id
                    {
                        tracing::warn!(expected = %g.session_id, reported = %sid, "claude reports another session id");
                        g.session_id = sid;
                    }
                }
                Out::Message {
                    uuid,
                    main,
                    tool_uses,
                    task_notification,
                    ..
                } => {
                    if let Some(text) = task_notification {
                        st.tracker.on_notification(&text);
                    }
                    if main {
                        let t = now();
                        for tu in &tool_uses {
                            st.tracker.on_tool_use(tu, t);
                        }
                        if let (Some(u), Some(turn)) = (uuid, st.turn.as_mut())
                            && !turn.uuids.contains(&u)
                        {
                            turn.uuids.push(u);
                        }
                    }
                }
                Out::StreamEvent | Out::KeepAlive => {}
                Out::Task(m) => {
                    let change = st.tracker.on_task(&m, now());
                    fx.push(Fx::Task(TaskReportEvent {
                        id: gen_key,
                        task_id: m.task_id.clone(),
                        tool_use_id: m.tool_use_id.clone(),
                        event: match m.kind {
                            TaskKind::Started => TaskEvent::Started,
                            TaskKind::Updated => TaskEvent::Updated,
                            TaskKind::Notification => TaskEvent::Notification,
                            TaskKind::Progress => TaskEvent::Progress,
                        },
                        status: task_state(&m),
                        description: m.description.clone(),
                        summary: m.summary.clone(),
                        output_file: m.output_file.clone(),
                        at: wire(now()),
                    }));
                    if change == TaskChange::Removed {
                        Self::hold_check(st, fx, "tasks done");
                    }
                }
                Out::Result(r) => self.on_result(st, fx, &r),
                Out::ControlRequest {
                    request_id,
                    subtype,
                    request,
                } => match control::answer(&request_id, &subtype, &request) {
                    Answer::Now(bytes) => fx.push(Fx::Stdin(bytes)),
                    Answer::Ask {
                        tool_name,
                        tool_use_id,
                        input,
                    } => {
                        let ask_id = Uuid::new_v4().to_string();
                        tracing::info!(ask = %ask_id, tool = %tool_name, "ask opened");
                        st.asks.push(Ask {
                            ask_id: ask_id.clone(),
                            cli_request_id: request_id,
                            tool_name: tool_name.clone(),
                            tool_use_id: tool_use_id.clone(),
                            since: now(),
                            since_mono: Instant::now(),
                        });
                        fx.push(Fx::Report(Item::AskOpen(AskReport {
                            ask_id,
                            tool_name,
                            tool_use_id,
                            input,
                        })));
                    }
                },
                Out::ControlResponse { request_id, ok, error } => {
                    if let Some(w) = st.control_waiters.remove(&request_id) {
                        let _ = w.send(if ok { Ok(()) } else { Err(error.unwrap_or_default()) });
                    }
                }
                Out::ControlCancel { request_id } => {
                    if let Some(i) = st.asks.iter().position(|a| a.cli_request_id == request_id) {
                        let a = st.asks.remove(i);
                        fx.push(Fx::Report(Item::AskResolved(
                            a.ask_id,
                            AskResolvedReport {
                                outcome: AskOutcome::Canceled,
                            },
                        )));
                    }
                }
                Out::Logged(kind) => tracing::debug!(kind = %kind, "claude system message"),
                Out::Other(kind) => {
                    if st.logged_types.insert(kind.clone()) {
                        tracing::info!(kind = %kind, "unknown stdout message type: ignored");
                    }
                }
                Out::Unparsed => {
                    if st.logged_types.insert("<unparsed>".into()) {
                        tracing::info!("a stdout line that does not parse (or is over the parse limit): ignored");
                    }
                }
            }
        });
    }

    fn on_result(&self, st: &mut St, fx: &mut Vec<Fx>, r: &ResultMsg) {
        let Some(turn) = st.turn.as_mut() else {
            st.pending_inputs = st.pending_inputs.saturating_sub(1);
            return;
        };
        if let Some(u) = r.usage {
            turn.usage = Some(u);
        }
        if turn.origin == TurnOrigin::Agent && st.queued.is_some() {
            // the agent turn's own result; the person's message is answered by the next one (7.5)
            let outcome = outcome_of_result(r, turn, st.release.as_ref());
            self.end_turn_now(st, fx, outcome, true, None);
            return;
        }
        st.pending_inputs = st.pending_inputs.saturating_sub(1);
        if st.pending_inputs == 0 {
            let outcome = outcome_of_result(r, turn, st.release.as_ref());
            self.end_turn_now(st, fx, outcome, true, None);
        }
    }

    fn on_exit(&self, gen_n: u32, exit: Option<&ExitRecord>) {
        self.with(Change::State, |st, fx| {
            let Some(g) = st.live.take_if(|g| g.n == gen_n) else {
                return;
            };
            let tail = stderr_tail(&g.stderr);
            tracing::info!(gen = gen_n, code = ?exit.and_then(|e| e.code), signal = ?exit.and_then(|e| e.signal), why = ?g.why, "claude exited");
            fx.push(Fx::Activity(ActivityEvent {
                kind: ActivityEventKind::GenerationExit,
                at: now().into(),
                pid: u32::try_from(g.pid).ok(),
                comm: Some("claude".into()),
                rss: None,
                exit_code: exit.and_then(|e| e.code),
                signal: exit.and_then(|e| e.signal),
            }));
            for (_, w) in st.control_waiters.drain() {
                let _ = w.send(Err("Claude Code exited".into()));
            }
            if g.why.is_none() {
                let t = Instant::now();
                st.crashes.push_back(t);
                while st.crashes.front().is_some_and(|c| t.duration_since(*c) > CRASH_LOOP_WINDOW) {
                    st.crashes.pop_front();
                }
                if st.crashes.len() >= CRASH_LOOP_EXITS {
                    tracing::warn!("Claude Code exits in a loop: new turns refused for 5 min");
                    st.failing_until = Some(t + CRASH_LOOP_PAUSE);
                }
            }
            let code = exit.and_then(|e| e.code);
            while let Some(turn) = st.turn.clone() {
                let o = outcome_of_exit(g.why.as_ref(), &turn, st.release.as_ref(), exit, &tail);
                self.end_turn_now(st, fx, o, false, code);
            }
            st.tracker.clear();
            st.pending_inputs = 0;
            for a in st.asks.drain(..) {
                fx.push(Fx::Report(Item::AskResolved(
                    a.ask_id,
                    AskResolvedReport {
                        outcome: AskOutcome::Canceled,
                    },
                )));
            }
            if st.run != RunKind::Idle {
                st.set_run(RunKind::Idle);
            }
        });
    }

    // ---- the tick ----

    fn tick(&self) {
        self.with(Change::State, |st, fx| {
            let t = now();
            let mono = Instant::now();
            // interrupt grace
            if let (Some(turn), Some(g)) = (st.turn.as_ref(), st.live.as_mut())
                && let Some(sent) = turn.interrupt_sent
                && t - sent > INTERRUPT_GRACE
                && g.why.is_none()
            {
                tracing::warn!(turn = %turn.id, "interrupt forced kill");
                g.why = Some(Why::Interrupt);
                fx.push(Fx::Terminate(g.pid));
            }
            // asks
            let expired: Vec<Ask> = st
                .asks
                .iter()
                .filter(|a| mono.duration_since(a.since_mono) > ASK_TIMEOUT)
                .cloned()
                .collect();
            for a in expired {
                st.asks.retain(|x| x.ask_id != a.ask_id);
                fx.push(Fx::Stdin(control::deny(
                    &a.cli_request_id,
                    "No answer was given.",
                    a.tool_use_id.as_deref(),
                )));
                fx.push(Fx::Report(Item::AskResolved(
                    a.ask_id,
                    AskResolvedReport {
                        outcome: AskOutcome::Canceled,
                    },
                )));
            }
            // pseudo-tasks and the silence ceiling
            if st.tracker.expire(t) > 0 {
                Self::hold_check(st, fx, "tasks done");
            }
            if st.run == RunKind::Held && t - st.last_output > HOLD_SILENCE && !st.tracker.until_ahead(t) {
                let tasks: Vec<String> = st.tracker.tasks().iter().map(|x| x.task_id.clone()).collect();
                tracing::warn!(tasks = ?tasks, reason = "silence ceiling", "hold ended");
                for id in st.tracker.event_ids() {
                    fx.push(Fx::Stdin(control::stop_task(&format!("stop-{}", Uuid::new_v4()), &id)));
                }
                st.tracker.clear();
                st.set_run(RunKind::Idle);
                fx.push(Fx::IdlePoint);
            }
        });
    }

    // ---- generations ----

    async fn keeper_call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&keeper::Client) -> Result<T, keeper::KeeperError> + Send + 'static,
    ) -> Result<T, String> {
        let Some(ctx) = self.ctx() else {
            return Err("not attached".into());
        };
        let k = ctx.keeper.clone();
        tokio::task::spawn_blocking(move || f(&k))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    /// The models, waiting for the route (a spawn is part of starting a turn).
    async fn models(&self) -> Result<Models, String> {
        let deadline = Instant::now() + MODELS_WAIT;
        let mut delay = Duration::from_millis(500);
        loop {
            match self.model.models().await {
                Ok(m) => return Ok(m),
                Err(e) => {
                    if Instant::now() + delay > deadline {
                        return Err(format!("the models route is unavailable: {e}"));
                    }
                    tracing::info!(error = %e, "models route unavailable: retried");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            }
        }
    }

    /// The live generation for `session`, spawned (and initialised) if there is none or it runs another session.
    async fn ensure_gen(&self, session: &Session) -> Result<u32, String> {
        let _lane = self.gen_lane.lock().await;
        let live = {
            let st = lock(&self.st);
            st.live.as_ref().map(|g| (g.n, g.session_id.clone(), g.initialized))
        };
        if let Some((n, sid, init)) = live {
            if sid == session.id() {
                if init {
                    return Ok(n);
                }
                // a generation spawned by another caller: wait for its initialisation
                let ok = self
                    .wait_until(Some(Instant::now() + INIT_WAIT), |s| {
                        s.live.as_ref().is_none_or(|g| g.n != n || g.initialized)
                    })
                    .await;
                let still = lock(&self.st).live.as_ref().is_some_and(|g| g.n == n && g.initialized);
                if ok && still {
                    return Ok(n);
                }
            } else {
                tracing::info!(live = %sid, wanted = %session.id(), "another session: generation ended");
                self.end_generation(
                    Why::Graceful,
                    Instant::now() + STDIN_CLOSE_GRACE + KILL_AFTER_TERM + Duration::from_secs(2),
                )
                .await;
            }
        }
        if let Some(until) = lock(&self.st).failing_until
            && Instant::now() < until
        {
            return Err("Claude Code would not start".into());
        }
        self.spawn(session).await
    }

    async fn spawn(&self, session: &Session) -> Result<u32, String> {
        let Some(ctx) = self.ctx() else {
            return Err("not attached".into());
        };
        let models = self.models().await?;
        let descriptor = self.hermit.descriptor().ok_or("no descriptor")?;
        let env = launch::env(&ctx.boot, &descriptor.env, &models);
        let n = {
            let mut st = lock(&self.st);
            st.last_gen += 1;
            st.last_gen
        };
        let dir = self.cfg.gen_dir.join(n.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| format!("generation directory: {e}"))?;
        let stdout = dir.join("stdout.ndjson");
        let stderr = dir.join("stderr.log");
        let spec = self.cfg.launch.spec(
            session,
            env,
            stdout.to_string_lossy().into_owned(),
            stderr.to_string_lossy().into_owned(),
            ctx.cgroup.clone(),
        );
        let spawned = self.keeper_call(move |k| k.spawn_tracked(&spec)).await?;
        tracing::info!(gen = n, pid = spawned.pid, session = %session.id(), resume = matches!(session, Session::Resume(_)), "claude started");
        let (tx, rx) = mpsc::unbounded_channel();
        let (init_tx, init_rx) = oneshot::channel();
        let init_id = format!("init-{n}");
        self.with(Change::State, |st, _| {
            st.live = Some(Gen {
                n,
                pid: spawned.pid,
                since: spawned.since,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                offset: 0,
                session_id: session.id().to_owned(),
                initialized: false,
                started_at: now(),
                why: None,
                writer: tx.clone(),
                etag: descriptor.etag.clone(),
            });
            st.control_waiters.insert(init_id.clone(), init_tx);
        });
        ctx.persisted.flush().await;
        self.start_gen_tasks(n, spawned.pid, spawned.since, stdout, stderr, 0, rx);
        let _ = tx.send(Write {
            msg_id: keeper::new_msg_id(),
            bytes: control::initialize(&init_id),
            done: None,
        });
        let r = tokio::time::timeout(INIT_WAIT, init_rx).await;
        match r {
            Ok(Ok(Ok(()))) => {
                self.with(Change::State, |st, _| {
                    if let Some(g) = st.live.as_mut().filter(|g| g.n == n) {
                        g.initialized = true;
                    }
                });
                Ok(n)
            }
            other => {
                let why = match other {
                    Ok(Ok(Err(e))) => format!("Claude Code refused to initialise: {e}"),
                    Ok(Err(_)) => "Claude Code exited before it initialised".into(),
                    _ => "Claude Code did not initialise within 30 s".into(),
                };
                tracing::warn!(gen = n, why = %why, "claude not initialised");
                self.kill_gen(n, Why::Init).await;
                Err(why)
            }
        }
    }

    /// The generation's tail, exit watch, stderr copy and stdin writer.
    #[allow(clippy::too_many_arguments)]
    fn start_gen_tasks(
        &self,
        n: u32,
        pid: i32,
        since: u64,
        stdout: PathBuf,
        stderr: PathBuf,
        offset: u64,
        mut rx: mpsc::UnboundedReceiver<Write>,
    ) {
        let Some(ctx) = self.ctx() else { return };
        // the writer: one message at a time, in order
        let k = ctx.keeper.clone();
        self.go(async move {
            while let Some(w) = rx.recv().await {
                let k = k.clone();
                let r =
                    tokio::task::spawn_blocking(move || k.stdin(pid, w.msg_id, &w.bytes).map_err(|e| e.to_string()))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r);
                if let Err(e) = &r {
                    tracing::debug!(gen = n, error = %e, "stdin write failed");
                }
                if let Some(d) = w.done {
                    let _ = d.send(r);
                }
            }
        });
        let me = self.arc();
        let exits = ctx.exits.clone();
        self.go(async move {
            let mut tail = Tail::new(stdout.clone(), offset);
            let mut err_tail = StderrCopy::new(stderr, n);
            let mut notify = crate::cc::stream::Watch::new(&stdout);
            let exit = exits.wait(pid, since);
            tokio::pin!(exit);
            loop {
                let exited = tokio::select! {
                    e = &mut exit => Some(e),
                    () = notify.wait() => None,
                };
                match tail.read().await {
                    Ok(lines) => {
                        for l in lines {
                            me.on_line(n, l);
                        }
                    }
                    Err(e) => tracing::warn!(gen = n, error = %e, "stdout read failed"),
                }
                err_tail.copy().await;
                if let Some(e) = exited {
                    let rec = (e.name != keeper::watch::LOST).then_some(e);
                    me.on_exit(n, rec.as_ref());
                    return;
                }
            }
        });
    }

    /// SIGKILLs generation `n`'s group at once and waits (at most 5 s) for its exit to be handled.
    async fn kill_gen(&self, n: u32, why: Why) {
        let pid = self.with(Change::State, |st, _| {
            st.live.as_mut().filter(|g| g.n == n).map(|g| {
                g.why.get_or_insert(why);
                g.pid
            })
        });
        let Some(pid) = pid else { return };
        let _ = self
            .keeper_call(move |k| {
                let _ = k.signal(-pid, libc::SIGKILL);
                k.signal(pid, libc::SIGKILL)
            })
            .await;
        self.wait_until(Some(Instant::now() + Duration::from_secs(5)), |s| {
            s.live.as_ref().is_none_or(|g| g.n != n)
        })
        .await;
    }

    /// Ends the generation gracefully: close stdin, 5 s, SIGTERM its group, 3 s, SIGKILL; by `deadline`.
    async fn end_generation(&self, why: Why, deadline: Instant) {
        let g = self.with(Change::State, |st, _| {
            st.live.as_mut().map(|g| {
                g.why.get_or_insert(why);
                (g.n, g.pid)
            })
        });
        let Some((n, pid)) = g else { return };
        let gone = move |s: &St| s.live.as_ref().is_none_or(|g| g.n != n);
        let _ = self.keeper_call(move |k| k.close_stdin(pid)).await;
        let first = (Instant::now() + STDIN_CLOSE_GRACE).min(deadline.checked_sub(KILL_AFTER_TERM).unwrap_or(deadline));
        if self.wait_until(Some(first), gone).await {
            return;
        }
        let _ = self.keeper_call(move |k| k.signal(-pid, libc::SIGTERM)).await;
        let second = (Instant::now() + KILL_AFTER_TERM).min(deadline);
        if self.wait_until(Some(second), gone).await {
            return;
        }
        let _ = self
            .keeper_call(move |k| {
                let _ = k.signal(-pid, libc::SIGKILL);
                k.signal(pid, libc::SIGKILL)
            })
            .await;
        self.wait_until(Some(Instant::now() + Duration::from_secs(2)), gone)
            .await;
    }

    /// At an idle point: old generation files go; a changed configuration or an oversized stdout ends the
    /// generation so the next turn resumes with the new instructions (6.4, 4.5).
    async fn idle_point(&self) {
        // a start_turn in progress owns the generation (its step 1 applies a configuration change itself)
        let Ok(_lane) = self.turn_lane.try_lock() else {
            return;
        };
        let (cur, end) = {
            let st = lock(&self.st);
            let idle = st.run == RunKind::Idle && st.turn.is_none() && st.tracker.is_empty() && st.starting.is_none();
            let cur = st.live.as_ref().map_or(st.last_gen, |g| g.n);
            let end = st
                .live
                .as_ref()
                .filter(|g| {
                    idle && (g.etag != st.etag || std::fs::metadata(&g.stdout).map_or(0, |m| m.len()) > STDOUT_ROTATE)
                })
                .map(|g| g.n);
            (cur, end)
        };
        if let Ok(rd) = std::fs::read_dir(&self.cfg.gen_dir) {
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok())
                    && n < cur
                {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        if let Some(n) = end {
            tracing::info!("configuration changed or stdout rotated: generation ended at an idle point");
            let _gen = self.gen_lane.lock().await;
            if lock(&self.st).live.as_ref().is_some_and(|g| g.n == n) {
                self.end_generation(
                    Why::Graceful,
                    Instant::now() + STDIN_CLOSE_GRACE + KILL_AFTER_TERM + Duration::from_secs(2),
                )
                .await;
            }
        }
    }

    /// Writes the managed files from the descriptor if its `etag` differs from what they hold.
    fn apply_descriptor(&self, d: &hermit_api::sandbox_surface::SandboxSelf) {
        let differs = lock(&self.st).etag != d.etag;
        if differs {
            match managed::write(&self.cfg.managed_dir, &d.claude.managed_claude_md) {
                Ok(()) => {
                    self.with(Change::State, |st, _| st.etag.clone_from(&d.etag));
                }
                Err(e) => tracing::error!(error = %e, "managed files not written"),
            }
        }
    }

    // ---- commands ----

    async fn poll_loop(self: Arc<Self>) {
        let mut after = {
            let st = lock(&self.st);
            let low = st
                .delivered
                .iter()
                .find(|(_, d)| d.state != "acked")
                .map(|(k, _)| k.saturating_sub(1));
            low.map_or(st.cmd_after, |l| l.min(st.cmd_after))
        };
        loop {
            let q = CommandsQuery {
                after,
                wait_ms: Some(u64::from(COMMAND_WAIT_MS)),
            };
            let r = tokio::time::timeout(COMMAND_REQUEST_TIMEOUT, self.reporter.calls().commands(&q)).await;
            match r {
                Ok(Ok(resp)) => {
                    for c in resp.commands {
                        after = after.max(c.cmd_seq);
                        self.dispatch(c);
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "command poll failed");
                    tokio::time::sleep(POLL_RETRY).await;
                }
                Err(_) => {}
            }
        }
    }

    fn start_poll(&self) {
        let mut p = lock(&self.poll);
        if p.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let h = tokio::spawn(self.arc().poll_loop());
        self.track(h.abort_handle());
        *p = Some(h);
    }

    fn stop_poll(&self) {
        if let Some(h) = lock(&self.poll).take() {
            h.abort();
        }
    }

    fn ack(&self, seq: u64, result: AckResult) {
        let me = self.me.clone();
        self.reporter.push(Item::Ack {
            seq,
            result,
            done: Some(Box::new(move |ok| {
                if let Some(d) = me.upgrade() {
                    lock(&d.inflight).remove(&seq);
                    if ok {
                        d.with(Change::State, |st, _| {
                            st.delivered.insert(
                                seq,
                                Delivered {
                                    state: "acked".into(),
                                    ..Delivered::default()
                                },
                            );
                            prune(&mut st.delivered);
                        });
                    }
                }
            })),
        });
    }

    fn dispatch(&self, c: Command) {
        let seq = c.cmd_seq;
        let state = lock(&self.st).delivered.get(&seq).map(|d| d.state.clone());
        if state.as_deref() == Some("acked") {
            if lock(&self.inflight).insert(seq) {
                self.ack(seq, AckResult::default());
            }
            return;
        }
        if !lock(&self.inflight).insert(seq) {
            return;
        }
        self.with(Change::State, |st, _| {
            st.cmd_after = st.cmd_after.max(seq);
            st.delivered.entry(seq).or_insert_with(|| Delivered {
                state: "received".into(),
                ..Delivered::default()
            });
        });
        let me = self.arc();
        self.go(async move {
            if let Some(ctx) = me.ctx() {
                ctx.persisted.flush().await;
            }
            me.execute(c).await;
        });
    }

    async fn execute(&self, c: Command) {
        let seq = c.cmd_seq;
        match c.body {
            CommandBody::StartTurn(s) => {
                let Some(turn) = c.turn_id else {
                    self.ack(seq, err_ack("start_turn without a turnId"));
                    return;
                };
                self.start_turn(seq, turn, s).await;
            }
            CommandBody::Interrupt(_) => self.interrupt(seq, c.turn_id).await,
            CommandBody::Kill(k) => self.kill(seq, c.turn_id, k.reason).await,
            CommandBody::AnswerAsk(a) => self.answer_ask(seq, &a),
            CommandBody::Title(t) => {
                let system = self
                    .hermit
                    .descriptor()
                    .map(|d| d.title.system.clone())
                    .unwrap_or_default();
                let r = self.model.title(&system, &t).await;
                self.ack(seq, r);
            }
            CommandBody::StopTasks(_) => {
                let n = self.stop_tasks(Instant::now() + CANCEL_HOLD_WAIT, "stopped").await;
                self.ack(
                    seq,
                    AckResult {
                        stopped: Some(n),
                        ..AckResult::default()
                    },
                );
            }
            CommandBody::Unknown => self.ack(seq, err_ack("unsupported")),
        }
    }

    /// Stops every task (7.7): `stop_task` for each event task, waits for their notifications by `deadline`, drops
    /// the pseudo-tasks; the run leaves the hold. How many were stopped.
    async fn stop_tasks(&self, deadline: Instant, reason: &str) -> u32 {
        let (ids, n) = {
            let st = lock(&self.st);
            (
                st.tracker.event_ids(),
                u32::try_from(st.tracker.len()).unwrap_or(u32::MAX),
            )
        };
        self.with(Change::State, |_, fx| {
            for id in &ids {
                fx.push(Fx::Stdin(control::stop_task(&format!("stop-{}", Uuid::new_v4()), id)));
            }
        });
        self.wait_until(Some(deadline), |s| !s.tracker.has_event_tasks()).await;
        self.with(Change::State, |st, fx| {
            st.tracker.clear();
            if st.run == RunKind::Held && st.turn.is_none() {
                tracing::info!(reason, "hold ended");
                st.set_run(RunKind::Idle);
                fx.push(Fx::IdlePoint);
            }
        });
        n
    }

    #[allow(clippy::too_many_lines)] // the steps of 10.2 in order
    async fn start_turn(&self, seq: u64, turn_id: Uuid, s: StartTurn) {
        let _lane = self.turn_lane.lock().await;
        let Some(ctx) = self.ctx() else { return };
        let conversation = ctx.conversation;
        let delivered = lock(&self.st).delivered.get(&seq).cloned().unwrap_or_default();
        if delivered.state == "running" {
            self.ack(
                seq,
                AckResult {
                    delivered: Some(true),
                    ..AckResult::default()
                },
            );
            return;
        }
        // the one door: busy at once, or refused because a release or pause has begun
        let admitted = ctx.lifecycle.upgrade().is_some_and(|l| {
            l.admit(|| {
                self.with(Change::State, |st, _| st.starting = Some(turn_id));
            })
        });
        if !admitted {
            tracing::info!(turn = %turn_id, "start_turn not admitted: a release or pause has begun");
            lock(&self.inflight).remove(&seq);
            return;
        }
        let fail = |me: &Self, msg: String| {
            tracing::warn!(turn = %turn_id, error = %msg, "turn failed before it started");
            me.with(Change::State, |st, fx| {
                st.starting = None;
                fx.push(Fx::Report(Item::State {
                    turn: turn_id,
                    report: end_report(ReportedState::Failed, Some(error(DataCode::RunError, msg.clone()))),
                    wait: Vec::new(),
                    done: None,
                }));
            });
            me.ack(seq, err_ack(&msg));
        };
        let session = match &s.resume.provider_session_id {
            Some(id) => Session::Resume(id.clone()),
            None => Session::New(conversation.to_string()),
        };
        if delivered.state == "written" {
            // a restart between the record and the report: the same message again, with the same msg_id (10.2)
            let alive = lock(&self.st).live.as_ref().map(|g| g.n);
            if alive.is_none() || alive != delivered.generation {
                self.with(Change::State, |st, fx| {
                    st.starting = None;
                    fx.push(Fx::Report(Item::State {
                        turn: turn_id,
                        report: end_report(
                            ReportedState::Failed,
                            Some(error(
                                DataCode::RunExited,
                                "Claude Code exited before the prompt was answered",
                            )),
                        ),
                        wait: Vec::new(),
                        done: None,
                    }));
                });
                self.ack(seq, err_ack("Claude Code exited"));
                return;
            }
        }
        // 1: the descriptor (a change is applied at the next idle point)
        match self.hermit.refresh().await {
            Ok(d) => {
                self.apply_descriptor(&d);
                let end = {
                    let st = lock(&self.st);
                    st.turn.is_none() && st.tracker.is_empty() && st.live.as_ref().is_some_and(|g| g.etag != st.etag)
                };
                if end {
                    tracing::info!("configuration changed: generation ended before the turn");
                    self.end_generation(
                        Why::Graceful,
                        Instant::now() + STDIN_CLOSE_GRACE + KILL_AFTER_TERM + Duration::from_secs(2),
                    )
                    .await;
                }
            }
            Err(e) => tracing::info!(error = %e, "descriptor not re-read"),
        }
        // 2: a hold is superseded (an agent turn just takes the message, 7.5)
        if lock(&self.st).run == RunKind::Held {
            self.stop_tasks(Instant::now() + SUPERSEDE_WAIT, "supersede").await;
        }
        // 3: uploads
        let mut items = Vec::new();
        for f in &s.files {
            items.push(uploads::Item {
                workspace_path: f.workspace_path.clone(),
                sha: f.blob.clone(),
            });
        }
        for i in &s.images {
            items.push(uploads::Item {
                workspace_path: i.workspace_path.clone(),
                sha: i.blob.clone(),
            });
        }
        let paths = match self.cfg.uploads.materialise(&self.reporter.calls(), &items).await {
            Ok(p) => p,
            Err(e) => return fail(self, e),
        };
        let mut images = Vec::new();
        for i in &s.images {
            match self.fetch_blob(&i.model_blob).await {
                Ok(bytes) => images.push(prompt::ImageBlock {
                    media_type: i.model_mime.clone(),
                    bytes,
                }),
                Err(e) => return fail(self, e),
            }
        }
        let files: Vec<prompt::Attached> = s
            .files
            .iter()
            .zip(&paths)
            .map(|(f, p)| prompt::Attached {
                path: p.clone(),
                name: Some(f.name.clone()),
            })
            .collect();
        let shown: Vec<prompt::Attached> = s
            .images
            .iter()
            .zip(&paths[s.files.len()..])
            .map(|(i, p)| prompt::Attached {
                path: p.clone(),
                name: i.workspace_path.rsplit('/').next().map(str::to_owned),
            })
            .collect();
        let text = prompt::text(&s.text, &files, &shown);
        let line = prompt::user_line(&prompt::content(text, &images), &s.sent_at.to_wire());
        drop(images);
        // 4: a generation
        let n = match self.ensure_gen(&session).await {
            Ok(n) => n,
            Err(e) => return fail(self, e),
        };
        if lock(&self.st).cancel_starting == Some(turn_id) {
            self.with(Change::State, |st, fx| {
                st.starting = None;
                st.cancel_starting = None;
                fx.push(Fx::Report(Item::State {
                    turn: turn_id,
                    report: end_report(ReportedState::Canceled, None),
                    wait: Vec::new(),
                    done: None,
                }));
            });
            self.ack(
                seq,
                AckResult {
                    delivered: Some(false),
                    ..AckResult::default()
                },
            );
            return;
        }
        // 5: the record before the keeper has the message
        let (id, after, writer) = {
            let id = if delivered.state == "written" {
                delivered
                    .msg_id
                    .as_deref()
                    .and_then(unhex)
                    .unwrap_or_else(|| msg_id(&ctx.boot_id, seq))
            } else {
                msg_id(&ctx.boot_id, seq)
            };
            let after = self.with(Change::State, |st, _| {
                let after = if delivered.state == "written" {
                    delivered.pending_inputs_after.unwrap_or(st.pending_inputs + 1)
                } else {
                    st.pending_inputs + 1
                };
                st.msg_seq += 1;
                st.delivered.insert(
                    seq,
                    Delivered {
                        state: "written".into(),
                        msg_id: Some(hex(&id)),
                        generation: Some(n),
                        pending_inputs_after: Some(after),
                    },
                );
                after
            });
            ctx.persisted.flush().await;
            let writer = lock(&self.st)
                .live
                .as_ref()
                .filter(|g| g.n == n)
                .map(|g| g.writer.clone());
            (id, after, writer)
        };
        let (tx, rx) = oneshot::channel();
        let sent = writer.is_some_and(|w| {
            w.send(Write {
                msg_id: id,
                bytes: line,
                done: Some(tx),
            })
            .is_ok()
        });
        let written = if sent {
            rx.await.unwrap_or_else(|_| Err("writer gone".into()))
        } else {
            Err("Claude Code exited".into())
        };
        if let Err(e) = written {
            tracing::warn!(turn = %turn_id, error = %e, "prompt not delivered");
            self.with(Change::State, |st, fx| {
                st.starting = None;
                fx.push(Fx::Report(Item::State {
                    turn: turn_id,
                    report: end_report(ReportedState::Failed, Some(error(DataCode::RunExited, e.clone()))),
                    wait: Vec::new(),
                    done: None,
                }));
            });
            self.ack(seq, err_ack(&e));
            return;
        }
        // 6: running, then the ack
        self.with(Change::State, |st, fx| {
            st.starting = None;
            st.pending_inputs = after;
            let sid = st
                .live
                .as_ref()
                .map_or_else(|| session.id().to_owned(), |g| g.session_id.clone());
            let agent_running = st.turn.as_ref().is_some_and(|t| t.origin == TurnOrigin::Agent);
            let already = st.turn.as_ref().is_some_and(|t| t.id == turn_id)
                || st.queued.as_ref().is_some_and(|t| t.id == turn_id);
            if !already {
                let t = Turn::new(turn_id, TurnOrigin::Person);
                if agent_running {
                    st.queued = Some(t);
                } else {
                    st.turn = Some(t);
                    st.set_run(RunKind::Running);
                    fx.push(Fx::SetTurn(Some(turn_id)));
                }
            }
            tracing::info!(turn = %turn_id, origin = "person", "turn started");
            fx.push(Fx::MainTranscript(format!("projects/-workspace/{sid}.jsonl")));
            st.delivered.insert(
                seq,
                Delivered {
                    state: "running".into(),
                    ..st.delivered.get(&seq).cloned().unwrap_or_default()
                },
            );
            fx.push(Fx::Report(Item::State {
                turn: turn_id,
                report: TurnStateReport {
                    provider_session_id: Some(sid.clone()),
                    transcript_path: Some(format!("projects/-workspace/{sid}.jsonl")),
                    ..end_report(ReportedState::Running, None)
                },
                wait: Vec::new(),
                done: None,
            }));
        });
        self.ack(
            seq,
            AckResult {
                delivered: Some(true),
                ..AckResult::default()
            },
        );
    }

    async fn fetch_blob(&self, sha: &str) -> Result<Vec<u8>, String> {
        let res = self
            .reporter
            .calls()
            .sandbox_blob_get(
                sha,
                &magpie_api::uploads::SandboxBlobQuery {
                    as_: Some(magpie_api::uploads::SandboxBlobAs::Raw),
                },
            )
            .await
            .map_err(|e| format!("blob {sha}: {e}"))?;
        let b = res.bytes().await.map_err(|e| format!("blob {sha}: {e}"))?;
        Ok(b.to_vec())
    }

    async fn interrupt(&self, seq: u64, turn_id: Option<Uuid>) {
        let target = self.with(Change::State, |st, fx| {
            if turn_id.is_some() && st.starting == turn_id {
                st.cancel_starting = turn_id;
                return turn_id;
            }
            let turn = st.turn.as_mut().filter(|t| turn_id.is_none_or(|id| id == t.id))?;
            if turn.interrupt_sent.is_none() {
                turn.interrupt_sent = Some(now());
                tracing::info!(turn = %turn.id, "interrupt sent");
                fx.push(Fx::Stdin(control::interrupt(&format!("intr-{}", Uuid::new_v4()))));
            }
            let id = turn.id;
            st.set_run(RunKind::Stopping);
            for a in st.asks.drain(..) {
                fx.push(Fx::Stdin(control::deny(
                    &a.cli_request_id,
                    "The turn was stopped.",
                    a.tool_use_id.as_deref(),
                )));
                fx.push(Fx::Report(Item::AskResolved(
                    a.ask_id,
                    AskResolvedReport {
                        outcome: AskOutcome::Canceled,
                    },
                )));
            }
            Some(id)
        });
        if let Some(id) = target {
            // the outcome first: the grace kill bounds the wait
            self.wait_until(None, |s| {
                s.turn.as_ref().is_none_or(|t| t.id != id) && s.starting != Some(id)
            })
            .await;
        }
        self.ack(seq, AckResult::default());
    }

    async fn kill(&self, seq: u64, turn_id: Option<Uuid>, reason: KillReason) {
        let watchdog = reason == KillReason::Watchdog;
        let target = {
            let st = lock(&self.st);
            if turn_id.is_some() && st.starting == turn_id {
                None
            } else {
                st.turn
                    .as_ref()
                    .filter(|t| turn_id.is_none_or(|id| id == t.id))
                    .and_then(|_| st.live.as_ref().map(|g| g.n))
            }
        };
        if turn_id.is_some() && lock(&self.st).starting == turn_id {
            self.with(Change::State, |st, _| st.cancel_starting = turn_id);
            let id = turn_id;
            self.wait_until(None, |s| s.starting != id).await;
        }
        if let Some(n) = target {
            tracing::info!(turn = ?turn_id, watchdog, "kill");
            self.kill_gen(n, Why::Kill { watchdog }).await;
        }
        self.ack(seq, AckResult::default());
    }

    fn answer_ask(&self, seq: u64, a: &AnswerAsk) {
        let found = self.with(Change::State, |st, fx| {
            let i = st.asks.iter().position(|x| x.ask_id == a.ask_id)?;
            let ask = st.asks.remove(i);
            let line = if a.allow {
                let input = a.updated_input.clone().map_or_else(|| json!({}), Value::Object);
                control::allow(&ask.cli_request_id, &input, ask.tool_use_id.as_deref())
            } else {
                control::deny(
                    &ask.cli_request_id,
                    a.message.as_deref().unwrap_or("The person declined."),
                    ask.tool_use_id.as_deref(),
                )
            };
            fx.push(Fx::Stdin(line));
            fx.push(Fx::Report(Item::AskResolved(
                ask.ask_id,
                AskResolvedReport {
                    outcome: AskOutcome::Answered,
                },
            )));
            Some(())
        });
        if found.is_some() {
            self.ack(seq, AckResult::default());
        } else {
            self.ack(seq, err_ack("no_such_ask"));
        }
    }

    // ---- the filesystem's hooks (9.5) ----

    fn kill_from_hook(&self, why: Why) {
        let Some(rt) = self.ctx().map(|c| c.rt.clone()) else {
            return;
        };
        let n = lock(&self.st).live.as_ref().map(|g| g.n);
        if let Some(n) = n {
            let me = self.arc();
            rt.spawn(async move { me.kill_gen(n, why).await });
        }
    }

    /// `lease_lost` (9.5 item 7): the generation is killed; its turn's reports are given up with the lease.
    pub fn lease_lost(&self) {
        tracing::warn!("lease lost: the generation is killed");
        self.kill_from_hook(Why::LeaseLost);
    }

    /// One transcript write kept failing for 10 min (9.5 item 4): the turn fails `cc.write_stuck`, the generation
    /// ends.
    pub fn write_stuck(&self, what: &str, message: &str) {
        tracing::error!(what, message, "cc write stuck: the generation is killed");
        self.kill_from_hook(Why::Stuck(format!("{what}: {message}")));
    }

    /// The cache disagrees with magpie's bytes: a forked history must not continue.
    pub fn forked(&self, message: &str) {
        tracing::error!(message, "history forked: the generation is killed");
        self.kill_from_hook(Why::Forked(message.to_owned()));
    }

    // ---- loops ----

    async fn tick_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(TICK).await;
            self.tick();
        }
    }

    fn heartbeat(&self) -> Heartbeat {
        let st = lock(&self.st);
        let ctx = self.ctx();
        Heartbeat {
            turn_id: st.turn.as_ref().map(|t| t.id),
            state: match st.run {
                RunKind::Running | RunKind::Starting => HeartbeatState::Running,
                RunKind::Stopping => HeartbeatState::Stopping,
                RunKind::Held => HeartbeatState::Held,
                RunKind::Idle => HeartbeatState::Idle,
            },
            tasks: u32::try_from(st.tracker.len()).unwrap_or(u32::MAX),
            last_turn: st.last_turn.as_ref().and_then(|l| {
                Some(HeartbeatLastTurn {
                    id: Uuid::parse_str(&l.id).ok()?,
                    outcome: match l.outcome.as_str() {
                        "completed" => TurnOutcome::Completed,
                        "failed" => TurnOutcome::Failed,
                        _ => TurnOutcome::Canceled,
                    },
                    error: l.error.clone().and_then(|e| serde_json::from_value(e).ok()),
                })
            }),
            generation: u64::from(st.live.as_ref().map_or(st.last_gen, |g| g.n)),
            supervisor: SupervisorFacts {
                version: crate::hermit::version(),
                boot_id: ctx.map(|c| c.boot_id.clone()).unwrap_or_default(),
                restarts: ctx.map_or(0, |c| c.restarts),
            },
            cc: self.transcript.facts(),
        }
    }

    async fn heartbeat_loop(self: Arc<Self>) {
        loop {
            let hb = self.heartbeat();
            match self.reporter.calls().heartbeat(&hb).await {
                Ok(()) => self.transcript.heartbeat_ok(),
                Err(e) => tracing::debug!(error = %e, "heartbeat failed"),
            }
            tokio::time::sleep(HEARTBEAT_EVERY).await;
        }
    }

    /// Restores the state of a previous supervisor of this boot (4.3 step 8).
    fn restore(&self, prior: &RunState, alive: Option<(u32, i32)>, exit: Option<&ExitRecord>) {
        self.with(Change::State, |st, fx| {
            st.cmd_after = prior.cmd_after;
            st.msg_seq = prior.msg_seq;
            st.last_turn.clone_from(&prior.last_turn);
            st.delivered = prior
                .delivered
                .iter()
                .filter_map(|(k, v)| Some((k.parse::<u64>().ok()?, v.clone())))
                .collect();
            st.last_gen = prior.generation.as_ref().map_or(0, |g| g.n);
            let turn = prior
                .run
                .turn_id
                .as_deref()
                .and_then(|t| Uuid::parse_str(t).ok())
                .map(|id| Turn {
                    id,
                    origin: match prior.run.origin.as_deref() {
                        Some("agent") => TurnOrigin::Agent,
                        _ => TurnOrigin::Person,
                    },
                    started: Instant::now(),
                    interrupt_sent: prior.run.interrupt_sent_at.as_deref().map(|_| now()),
                    uuids: prior.run.pending_uuids.clone(),
                    usage: None,
                });
            let queued = prior
                .run
                .queued_turn_id
                .as_deref()
                .and_then(|t| Uuid::parse_str(t).ok())
                .map(|id| Turn::new(id, TurnOrigin::Person));
            if let (Some((n, pid)), Some(g)) = (alive, prior.generation.as_ref()) {
                {
                    st.live = Some(Gen {
                        n,
                        pid,
                        since: self.ctx().map_or(0, |c| c.exits.origin()),
                        stdout: PathBuf::from(&g.stdout),
                        stderr: PathBuf::from(&g.stdout).with_file_name("stderr.log"),
                        offset: g.stdout_offset,
                        session_id: g.session_id.clone().unwrap_or_default(),
                        initialized: g.initialized,
                        started_at: OffsetDateTime::parse(
                            &g.started_at,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .unwrap_or_else(|_| now()),
                        why: None,
                        // replaced when the writer starts
                        writer: mpsc::unbounded_channel().0,
                        etag: String::new(),
                    });
                    st.turn = turn;
                    st.queued = queued;
                    st.pending_inputs = prior.run.pending_inputs;
                    st.tracker = Tracker::from_saved(&prior.tasks);
                    st.asks = prior
                        .asks
                        .iter()
                        .map(|a| Ask {
                            ask_id: a.ask_id.clone(),
                            cli_request_id: a.cli_request_id.clone(),
                            tool_name: a.tool_name.clone(),
                            tool_use_id: None,
                            since: now(),
                            since_mono: Instant::now(),
                        })
                        .collect();
                    st.run = RunKind::parse(&prior.run.state);
                    if st.run == RunKind::Starting {
                        st.run = RunKind::Idle;
                    }
                    st.idle_since = (st.run == RunKind::Idle).then(now);
                }
            } else {
                {
                    // the generation died while no supervisor watched: its turn failed (7.9)
                    let tail = prior
                        .generation
                        .as_ref()
                        .map(|g| stderr_tail(&PathBuf::from(&g.stdout).with_file_name("stderr.log")))
                        .unwrap_or_default();
                    for t in [turn, queued].into_iter().flatten() {
                        st.turn = Some(t.clone());
                        let o = outcome_of_exit(None, &t, None, exit, &tail);
                        self.end_turn_now(st, fx, o, false, exit.and_then(|e| e.code));
                    }
                    for a in &prior.asks {
                        fx.push(Fx::Report(Item::AskResolved(
                            a.ask_id.clone(),
                            AskResolvedReport {
                                outcome: AskOutcome::Canceled,
                            },
                        )));
                    }
                    st.set_run(RunKind::Idle);
                }
            }
        });
    }
}

fn prune(d: &mut BTreeMap<u64, Delivered>) {
    let acked: Vec<u64> = d.iter().filter(|(_, v)| v.state == "acked").map(|(k, _)| *k).collect();
    if acked.len() > DELIVERED_KEPT {
        for k in &acked[..acked.len() - DELIVERED_KEPT] {
            d.remove(k);
        }
    }
}

fn err_ack(e: &str) -> AckResult {
    AckResult {
        error: Some(e.to_owned()),
        ..AckResult::default()
    }
}

fn end_report(state: ReportedState, err: Option<ReportError>) -> TurnStateReport {
    TurnStateReport {
        state,
        at: wire(now()),
        provider_session_id: None,
        transcript_path: None,
        exit_code: None,
        error: err,
        held: None,
        tasks: None,
        last_uuid: None,
        usage: None,
        duration_ms: None,
    }
}

/// Copies a generation's stderr into the log as `claude[g]:` lines, at most 100 lines a second.
struct StderrCopy {
    path: PathBuf,
    offset: u64,
    n: u32,
    window: Instant,
    lines: u32,
}

impl StderrCopy {
    fn new(path: PathBuf, n: u32) -> Self {
        Self {
            path,
            offset: 0,
            n,
            window: Instant::now(),
            lines: 0,
        }
    }

    async fn copy(&mut self) {
        use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
        let Ok(mut f) = tokio::fs::File::open(&self.path).await else {
            return;
        };
        if f.seek(std::io::SeekFrom::Start(self.offset)).await.is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.take(256 * 1024).read_to_end(&mut buf).await.is_err() {
            return;
        }
        let Some(last) = buf.iter().rposition(|b| *b == b'\n') else {
            return;
        };
        self.offset += last as u64 + 1;
        for l in buf[..last].split(|b| *b == b'\n') {
            if self.window.elapsed() >= Duration::from_secs(1) {
                self.window = Instant::now();
                self.lines = 0;
            }
            self.lines += 1;
            if self.lines <= crate::limits::STDERR_LINES_PER_SECOND {
                tracing::info!(gen = self.n, "claude[{}]: {}", self.n, String::from_utf8_lossy(l));
            }
        }
    }
}

impl Agent for Driver {
    fn attach(&self, ctx: AttachContext) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let k = ctx.keeper.clone();
            let restarts = tokio::task::spawn_blocking(move || k.status().map_or(0, |s| s.supervisor_restarts))
                .await
                .unwrap_or(0);
            self.reporter.set_lease(ctx.lease.lease_id);
            let conversation = ctx.lease.conversation_id;
            let first = ctx.prior.is_none();
            let _ = self.ctx.set(Ctx {
                persisted: ctx.persisted.clone(),
                keeper: ctx.keeper.clone(),
                exits: ctx.exits.clone(),
                boot: ctx.boot.clone(),
                cgroup: ctx.cgroup.as_ref().map(|p| p.to_string_lossy().into_owned()),
                lifecycle: ctx.lifecycle.clone(),
                activity: ctx.activity.clone(),
                boot_id: ctx.persisted.read(|s| s.boot_id.clone()),
                restarts,
                conversation,
                rt: tokio::runtime::Handle::current(),
            });
            // 4: the managed files (at every start: idempotent, and the descriptor may have changed)
            self.apply_descriptor(&ctx.descriptor);
            let alive = match &ctx.generation {
                crate::start::Reattach::Alive { n, pid } => Some((*n, *pid)),
                _ => None,
            };
            let exit = match &ctx.generation {
                crate::start::Reattach::Exited { exit, .. } => exit.clone(),
                _ => None,
            };
            if let Some(prior) = &ctx.prior {
                self.restore(prior, alive, exit.as_ref());
            }
            // the live generation's loops, from the saved offset
            let resumed = {
                let mut st = lock(&self.st);
                let etag = st.etag.clone();
                st.live.as_mut().map(|g| {
                    let (tx, rx) = mpsc::unbounded_channel();
                    g.writer = tx;
                    g.etag = etag;
                    (g.n, g.pid, g.since, g.stdout.clone(), g.stderr.clone(), g.offset, rx)
                })
            };
            if let Some((n, pid, since, out, err, offset, rx)) = resumed {
                tracing::info!(gen = n, pid, offset, "claude re-attached");
                let turn = lock(&self.st).turn.as_ref().map(|t| t.id);
                self.transcript.set_turn(turn);
                self.start_gen_tasks(n, pid, since, out, err, offset, rx);
            }
            let me = self.arc();
            me.go(me.reporter.clone().run());
            me.go(me.reporter.clone().run_tasks());
            me.go(me.clone().tick_loop());
            me.go(me.clone().heartbeat_loop());
            self.start_poll();
            // pre-spawn (4.3 step 8) so the first turn does not pay for start-up and resume
            if alive.is_none() {
                let session = match &ctx.lease.provider_session_id {
                    Some(s) => Session::Resume(s.clone()),
                    None => Session::New(conversation.to_string()),
                };
                let me = self.arc();
                self.go(async move {
                    if let Err(e) = me.ensure_gen(&session).await {
                        tracing::warn!(error = %e, first, "pre-spawn failed");
                    }
                });
            }
        })
    }

    fn status(&self) -> watch::Receiver<RunStatus> {
        self.status.subscribe()
    }

    fn generation(&self) -> Option<Generation> {
        lock(&self.st).live.as_ref().map(|g| Generation {
            n: g.n,
            pid: u32::try_from(g.pid).unwrap_or(0),
            started_at: g.started_at.into(),
        })
    }

    fn claude_pid(&self) -> Option<i32> {
        lock(&self.st).live.as_ref().map(|g| g.pid)
    }

    fn end_turn(&self, reason: String, deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let open = self.with(Change::State, |st, fx| {
                st.release = Some(reason.clone());
                let open = st.turn.as_mut().map(|t| {
                    if t.interrupt_sent.is_none() {
                        t.interrupt_sent = Some(now());
                        fx.push(Fx::Stdin(control::interrupt(&format!("intr-{}", Uuid::new_v4()))));
                    }
                    t.id
                });
                if open.is_some() {
                    st.set_run(RunKind::Stopping);
                }
                for id in st.tracker.event_ids() {
                    fx.push(Fx::Stdin(control::stop_task(&format!("stop-{}", Uuid::new_v4()), &id)));
                }
                open
            });
            if open.is_some() {
                let ended = self.wait_until(Some(deadline), |s| s.turn.is_none()).await;
                if !ended {
                    let n = lock(&self.st).live.as_ref().map(|g| g.n);
                    if let Some(n) = n {
                        tracing::warn!("the interrupt did not end the turn in time: killed");
                        self.kill_gen(n, Why::Release).await;
                    }
                    // no generation (or it would not die): the turn is reported lost all the same
                    self.with(Change::State, |st, fx| {
                        if st.turn.is_some() {
                            let o = Outcome::Failed(error(DataCode::SandboxLost, format!("released: {reason}")));
                            self.end_turn_now(st, fx, o, false, None);
                        }
                    });
                }
            }
        })
    }

    fn stop(&self, deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stop_poll();
            self.with(Change::State, |st, _| st.quiesced = true);
            self.end_generation(Why::Release, deadline).await;
            if !self.reporter.drain(deadline).await {
                tracing::warn!("release: reports still unanswered at the deadline");
            }
            let hb = self.heartbeat();
            let _ = tokio::time::timeout_at(deadline, self.reporter.calls().heartbeat(&hb)).await;
        })
    }

    fn quiesce(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stop_poll();
            self.with(Change::State, |st, _| st.quiesced = true);
        })
    }

    fn resume(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.with(Change::State, |st, _| st.quiesced = false);
            if self.ctx().is_some() {
                self.start_poll();
            }
        })
    }

    fn oom_killed(&self) {
        self.with(Change::State, |st, _| {
            if let Some(g) = st.live.as_mut() {
                g.why = Some(Why::Oom);
            }
        });
    }

    fn tasks(&self) -> Vec<Value> {
        lock(&self.st)
            .tracker
            .tasks()
            .iter()
            .map(|t| {
                json!({"taskId": t.task_id, "toolUseId": t.tool_use_id, "type": t.kind,
                       "description": t.description, "nested": t.nested})
            })
            .collect()
    }

    fn send_user_file(
        &self,
        req: supervisor_api::local::SendUserFileRequest,
        fds: Vec<std::os::fd::OwnedFd>,
    ) -> BoxFuture<'_, Result<supervisor_api::local::SendUserFileReply, String>> {
        Box::pin(async move { crate::sent_files::send(self.reporter.calls(), req, fds).await })
    }

    fn asks(&self) -> Vec<Value> {
        lock(&self.st)
            .asks
            .iter()
            .map(|a| json!({"askId": a.ask_id, "toolName": a.tool_name, "since": rfc3339(a.since)}))
            .collect()
    }
}

#[cfg(test)]
mod tests;
