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
use crate::cc::report::{Item, Reporter, Wait};
use crate::cc::stream::{Line, Out, Partial, ResultMsg, Step, Tail, TaskKind, TaskMsg};
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
use crate::state::{AskState, Change, Delivered, EndingState, GenState, LastTurn, Persisted, RunPart, RunState};
use crate::uploads::{self, Uploads};

/// The namespace of the supervisor's derived ids: the stdin `msg_id` = `UUIDv5`(namespace, `boot_id` ‖ "stdin" ‖
/// `cmdSeq`) (10.2 step 5), so a restarted supervisor derives the same id for the same command.
pub const SUPERVISOR_OP_NS: Uuid = Uuid::from_u128(0x6a1f_3c2e_9b4d_4e7a_8c55_0d2b_71e4_a903);
/// The driver's tick (interrupt grace, asks, the hold).
const TICK: Duration = Duration::from_secs(1);
/// How long a spawn waits for the models route before the turn fails (magpie's `starting` watchdog is 120 s).
const MODELS_WAIT: Duration = Duration::from_secs(110);
/// Acked commands kept in `run.json` beyond the unacked ones.
const DELIVERED_KEPT: usize = 64;
/// The poll's pause after a failed long-poll.
const POLL_RETRY: Duration = Duration::from_secs(1);
/// How long removing a kept message waits for `run.json` to be on disk before it keeps the file and goes on.
const MESSAGE_DROP_FLUSH_MAX: Duration = Duration::from_secs(30);

/// Where the driver's files are and who Claude Code runs as (the sandbox's in production; a test directory in tests).
#[derive(Clone, Debug)]
pub struct DriverConfig {
    pub launch: Launch,
    pub managed_dir: PathBuf,
    pub gen_dir: PathBuf,
    pub uploads: Uploads,
    /// Claude Code's `~/.claude` (the main transcript is `projects/-workspace/<session>.jsonl` under it).
    pub claude_dir: PathBuf,
}

impl DriverConfig {
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            launch: Launch::sandbox(),
            managed_dir: managed::MANAGED_DIR.into(),
            gen_dir: crate::state::GEN_DIR.into(),
            uploads: Uploads::sandbox(),
            claude_dir: PathBuf::from(crate::boot::AGENT_HOME).join(".claude"),
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
    /// A release of a run that waited only on its questions (DECISIONS 17): its turn is parked, and the generation
    /// ends by SIGTERM with stdin left open (a closed stdin makes Claude Code fail the question and call the model).
    Park,
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
    /// A restarted supervisor found it still initialising: nothing ran on it, and a new one is started (its
    /// initialisation answer may have been read by the previous supervisor).
    Stale,
}

/// An open turn.
#[derive(Clone, Debug)]
struct Turn {
    id: Uuid,
    origin: TurnOrigin,
    started: Instant,
    /// When the interrupt was sent: the wall clock for `run.json`, the monotonic clock for the 15 s grace (a clock
    /// step must neither fire nor stretch it).
    interrupt_sent: Option<OffsetDateTime>,
    interrupt_mono: Option<Instant>,
    /// uuids of the main-thread messages stdout showed during the turn (8.2).
    uuids: Vec<String>,
    usage: Option<magpie_api::sandbox::Usage>,
    /// How long its questions waited for the person (not working time: DECISIONS 17).
    ask_wait: Duration,
    /// The person's message is with the writer (a turn is installed before its message is handed over, so stdout
    /// can never open an agent turn for its answer; an interrupt meanwhile is written right after the message).
    handed: bool,
}

impl Turn {
    fn new(id: Uuid, origin: TurnOrigin) -> Self {
        Self {
            id,
            origin,
            started: Instant::now(),
            interrupt_sent: None,
            interrupt_mono: None,
            uuids: Vec::new(),
            usage: None,
            ask_wait: Duration::ZERO,
            handed: true,
        }
    }

    /// Marks the interrupt sent; whether it was not yet.
    fn mark_interrupt(&mut self) -> bool {
        if self.interrupt_sent.is_some() {
            return false;
        }
        self.interrupt_sent = Some(now());
        self.interrupt_mono = Some(Instant::now());
        true
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
    /// The model it runs (`ANTHROPIC_MODEL`): the conversation's picked model, else the models route's default.
    model: String,
    /// Whether that is a picked model (`start_turn.model`) rather than the route's default.
    picked: bool,
    /// The thinking level it was last switched to ([`Driver::apply_thinking`]).
    thinking: Thinking,
}

impl Gen {
    /// Whether it runs the model a turn wants: the picked one, or with none picked the route's default.
    fn runs(&self, wanted: Option<&str>) -> bool {
        match wanted {
            Some(m) => self.model == m,
            None => !self.picked,
        }
    }
}

/// The messages being streamed (`stream_event`), one per thread (the main one, and each subagent's by its `Agent`
/// call): what a live frame needs beside the event itself (8.3).
#[derive(Default)]
struct PartialOut {
    threads: HashMap<Option<String>, PartialMessage>,
}

#[derive(Default)]
struct PartialMessage {
    id: String,
    /// The bytes of text so far in each block.
    lens: HashMap<u32, u64>,
}

impl PartialOut {
    /// The live frame of a `stream_event`, if it makes one (a block's start, its text, its end).
    fn frame(&mut self, p: Partial) -> Option<magpie_api::live::LiveFrame> {
        use magpie_api::live::FrameOp;
        let Partial {
            parent_tool_use_id,
            step,
        } = p;
        if let Step::Message { id } = step {
            self.threads.insert(
                parent_tool_use_id,
                PartialMessage {
                    id,
                    lens: HashMap::new(),
                },
            );
            return None;
        }
        let m = self.threads.get_mut(&parent_tool_use_id)?;
        let mut f = crate::cc::report::blank_frame();
        f.message_id = Some(m.id.clone());
        f.at = Some(magpie_api::Timestamp::now());
        match step {
            Step::Start { index, block, name } => {
                m.lens.insert(index, 0);
                (f.op, f.index, f.block, f.name) = (FrameOp::Start, Some(index), Some(block), name);
                f.parent_tool_use_id = parent_tool_use_id;
            }
            Step::Text { index, text } => {
                let len = m.lens.entry(index).or_insert(0);
                (f.op, f.index, f.offset) = (FrameOp::Delta, Some(index), Some(*len));
                *len += text.len() as u64;
                f.text = Some(text);
            }
            Step::Stop { index } => {
                m.lens.remove(&index);
                (f.op, f.index) = (FrameOp::Stop, Some(index));
            }
            Step::Message { .. } | Step::Other => return None,
        }
        Some(f)
    }
}

/// A turn that ended and whose end report magpie has not answered yet: the heartbeat goes on naming it (magpie
/// would otherwise take an idle heartbeat without it as a turn the sandbox forgot, magpie 8.5 rule 2).
/// The thinking level a generation is on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Thinking {
    /// Not known (a new generation, or one a restarted supervisor found): the next turn's is applied.
    Unknown,
    /// Switched to this level; `None`: none named, Claude Code's own.
    Level(Option<String>),
}

#[derive(Clone, Debug)]
struct Ending {
    id: Uuid,
    report: TurnStateReport,
    /// The uuids its end waits for (8.2), written by `generation`; `None`: the writes in flight only.
    uuids: Option<Vec<String>>,
    generation: u32,
    stopping: bool,
    last: LastTurn,
}

/// Why no generation was had for a turn.
enum GenError {
    /// The turn was interrupted or killed, or a release or pause began, while it waited.
    Canceled,
    Failed(String),
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
    /// The report lane of the turn it was opened in.
    lane: Option<Uuid>,
}

impl Ask {
    /// Its resolved report, in the lane it was opened in.
    fn resolved(&self, outcome: AskOutcome) -> Fx {
        Fx::ReportIn(
            self.lane,
            Item::AskResolved(self.ask_id.clone(), AskResolvedReport { outcome }),
        )
    }
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
    /// The images Claude Code's context holds since its last compaction: each main-thread image block read from
    /// stdout or written in a prompt (`None`: not counted yet, the next generation counts the transcript's).
    images: Option<u32>,
    /// The session `images` was counted for (another session's generation counts its own transcript).
    images_session: Option<String>,
    /// The descriptor's `claude.compactAtImages`.
    compact_at: Option<u32>,
    /// The image count a compaction was last asked for at: asked again only once more images have come (a
    /// compaction that fails is not repeated at every idle point).
    compact_tried: Option<u32>,
    live: Option<Gen>,
    last_gen: u32,
    tracker: Tracker,
    asks: Vec<Ask>,
    /// Since when a question has been open (the monotonic and the wall clock): no turn timer runs meanwhile.
    asks_since: Option<(Instant, OffsetDateTime)>,
    /// The main thread's tool calls without a result yet: a run whose open calls are all its questions waits only
    /// on the person (`asking`).
    open_tools: HashSet<String>,
    /// A parked turn whose `parked` report magpie has not answered: the heartbeat still names it.
    parking: Option<Uuid>,
    delivered: BTreeMap<u64, Delivered>,
    cmd_after: u64,
    last_turn: Option<LastTurn>,
    last_output: OffsetDateTime,
    /// The same on the monotonic clock (the hold's silence ceiling).
    last_output_mono: Instant,
    idle_since: Option<OffsetDateTime>,
    crashes: VecDeque<Instant>,
    failing_until: Option<Instant>,
    control_waiters: HashMap<String, oneshot::Sender<Result<(), String>>>,
    /// The messages being streamed, for the live frames.
    partial: PartialOut,
    /// The descriptor `etag` the managed files were written from.
    etag: String,
    /// The model the conversation's person picked, as the last `start_turn` said (`None`: the route's default): a
    /// generation is started on it, and one that runs another model ends before the next turn.
    model: Option<String>,
    /// The release reason, once a forced release ends the turn.
    release: Option<String>,
    logged_types: HashSet<String>,
    msg_seq: u64,
    /// The command loop is stopped (a pause or a release).
    quiesced: bool,
    /// Turns whose end report is not answered yet.
    ending: Vec<Ending>,
    /// How the last generation's initialisation went.
    init_verdict: Option<(u32, Result<(), String>)>,
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
            images: None,
            images_session: None,
            compact_at: None,
            compact_tried: None,
            live: None,
            last_gen: 0,
            tracker: Tracker::default(),
            asks: Vec::new(),
            asks_since: None,
            open_tools: HashSet::new(),
            parking: None,
            delivered: BTreeMap::new(),
            cmd_after: 0,
            last_turn: None,
            last_output: OffsetDateTime::now_utc(),
            last_output_mono: Instant::now(),
            idle_since: Some(OffsetDateTime::now_utc()),
            crashes: VecDeque::new(),
            failing_until: None,
            control_waiters: HashMap::new(),
            partial: PartialOut::default(),
            etag: String::new(),
            model: None,
            release: None,
            logged_types: HashSet::new(),
            msg_seq: 0,
            quiesced: false,
            ending: Vec::new(),
            init_verdict: None,
        }
    }

    /// Whether the preparation of `turn` (or a spawn for no turn) must stop: it was interrupted or killed, or a
    /// release or pause began.
    fn cancelled(&self, turn: Option<Uuid>) -> bool {
        self.release.is_some() || self.quiesced || turn.is_some_and(|t| self.cancel_starting == Some(t))
    }

    /// The run waits only on the person: questions are open, no tool call but theirs runs, no background task,
    /// nothing queued or being stopped (DECISIONS 17).
    fn asking(&self) -> bool {
        !self.asks.is_empty()
            && self.run == RunKind::Running
            && self.starting.is_none()
            && self.queued.is_none()
            && self.live.is_some()
            && self.tracker.is_empty()
            && self
                .turn
                .as_ref()
                .is_some_and(|t| t.interrupt_sent.is_none() && self.asks.iter().all(|a| a.lane == Some(t.id)))
            && self
                .open_tools
                .iter()
                .all(|id| self.asks.iter().any(|a| a.tool_use_id.as_deref() == Some(id.as_str())))
    }

    /// Keeps the question clock: when the last open question closes, its wait is added to the turn's (not working
    /// time) and the silence ceiling's clock moves past it, so neither counts the time the person took.
    fn sync_asks(&mut self) {
        match (self.asks.is_empty(), self.asks_since) {
            (false, None) => {
                let mono = self
                    .asks
                    .iter()
                    .map(|a| a.since_mono)
                    .min()
                    .unwrap_or_else(Instant::now);
                let wall = self.asks.iter().map(|a| a.since).min().unwrap_or_else(now);
                self.asks_since = Some((mono, wall));
            }
            (true, Some((opened, _))) => {
                self.asks_since = None;
                if let Some(t) = self.turn.as_mut() {
                    t.ask_wait += opened.elapsed();
                }
                // the silence counted is what came before the question
                let counted = opened.max(self.last_output_mono).duration_since(self.last_output_mono);
                self.last_output_mono = Instant::now().checked_sub(counted).unwrap_or(self.last_output_mono);
            }
            _ => {}
        }
    }

    /// The turn's working time so far: its time less the time its questions waited.
    fn worked(&self, turn: &Turn) -> Duration {
        let open = self.asks_since.map_or(Duration::ZERO, |(o, _)| o.elapsed());
        turn.started.elapsed().saturating_sub(turn.ask_wait + open)
    }

    fn status(&self) -> RunStatus {
        let asking = self.asking();
        let state = if self.starting.is_some() && self.run == RunKind::Idle {
            ActivityState::Starting
        } else if asking {
            ActivityState::Asking
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
            // waiting for a person is idle time for hermit, counted from the question
            idle_since: if asking {
                self.asks_since.map(|(_, wall)| wall.into())
            } else {
                self.idle_since.map(Into::into)
            },
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

/// Tests: how long the writer holds back the keeper's answer to a prompt (Claude Code answers in the meantime).
#[cfg(test)]
pub(crate) static STDIN_ACK_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What to do once the lock is released.
enum Fx {
    Report(Item),
    /// A report in a named lane (asks: their turn's).
    ReportIn(Option<Uuid>, Item),
    Task(TaskReportEvent),
    /// A live frame of the turn (8.3).
    Live(Uuid, magpie_api::live::LiveFrame),
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
    /// Tests: a kill lands while a prompt is with the writer, and its exit is handled after the failed write.
    #[cfg(test)]
    race_kill_with_prompt: std::sync::atomic::AtomicBool,
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

/// How long ago `m` was, in milliseconds (what `run.json` keeps of a monotonic instant).
fn elapsed_ms(m: Instant) -> u64 {
    u64::try_from(m.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The monotonic instant `ms` milliseconds ago (a restored timer goes on from where it was).
fn ago(ms: u64) -> Instant {
    Instant::now()
        .checked_sub(Duration::from_millis(ms))
        .unwrap_or_else(Instant::now)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stdin `msg_id` of command `seq` (10.2 step 5): `UUIDv5`(`SUPERVISOR_OP_NS`, `boot_id` ‖ "stdin" ‖ `seq` as
/// 8 big-endian bytes).
#[must_use]
pub fn msg_id(boot_id: &str, seq: u64) -> [u8; 16] {
    let mut name = Vec::with_capacity(boot_id.len() + 13);
    name.extend_from_slice(boot_id.as_bytes());
    name.extend_from_slice(b"stdin");
    name.extend_from_slice(&seq.to_be_bytes());
    *Uuid::new_v5(&SUPERVISOR_OP_NS, &name).as_bytes()
}

fn derived(boot_id: &str, what: &[u8], gen_n: u32, offset: u64) -> [u8; 32] {
    let mut h = sha2::Sha256::new();
    h.update(SUPERVISOR_OP_NS.as_bytes());
    h.update(boot_id.as_bytes());
    h.update(what);
    h.update(gen_n.to_be_bytes());
    h.update(offset.to_be_bytes());
    h.finalize().into()
}

/// The id of the agent turn opened by generation `gen_n`'s stdout line ending at `offset`: a `UUIDv7` with the
/// generation's start as its time and the rest derived from (`boot_id`, generation, offset), so the line read again
/// after a restart opens the same turn (4.5).
#[must_use]
pub fn agent_turn_id(boot_id: &str, gen_n: u32, offset: u64, gen_started: OffsetDateTime) -> Uuid {
    let d = derived(boot_id, b"agent-turn", gen_n, offset);
    let mut r = [0u8; 10];
    r.copy_from_slice(&d[..10]);
    let ms = u64::try_from(gen_started.unix_timestamp_nanos() / 1_000_000).unwrap_or(0);
    uuid::Builder::from_unix_timestamp_millis(ms, &r).into_uuid()
}

/// The image blocks in the transcript at `path` after its last `compact_boundary` line: each main-thread message's
/// own and its tool results', read as the stdout lines are ([`crate::cc::stream::parse`]).
///
/// # Errors
/// The file cannot be read.
pub fn count_images(path: &std::path::Path) -> std::io::Result<u32> {
    use std::io::BufRead as _;
    let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut line = Vec::new();
    let mut n: u32 = 0;
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 {
            return Ok(n);
        }
        match crate::cc::stream::parse(line.strip_suffix(b"\n").unwrap_or(&line)) {
            Out::Compacted => n = 0,
            Out::Message { main: true, images, .. } => n = n.saturating_add(images),
            _ => {}
        }
    }
}

/// The `askId` of the `can_use_tool` request on generation `gen_n`'s stdout line ending at `offset` (as
/// [`agent_turn_id`]: the same line gives the same ask).
#[must_use]
pub fn ask_id(boot_id: &str, gen_n: u32, offset: u64) -> Uuid {
    let d = derived(boot_id, b"ask", gen_n, offset);
    let mut r = [0u8; 16];
    r.copy_from_slice(&d[..16]);
    uuid::Builder::from_random_bytes(r).into_uuid()
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
            #[cfg(test)]
            race_kill_with_prompt: std::sync::atomic::AtomicBool::new(false),
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
        self.with_change(|st, fx| (f(st, fx), change))
    }

    /// As [`Driver::with`], with `f` saying how urgently the change is persisted.
    fn with_change<R>(&self, f: impl FnOnce(&mut St, &mut Vec<Fx>) -> (R, Change)) -> R {
        let mut fx = Vec::new();
        let r = {
            let mut st = lock(&self.st);
            let (r, change) = f(&mut st, &mut fx);
            st.sync_asks();
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
                etag: g.etag.clone(),
                model: g.model.clone(),
                picked: g.picked,
            });
            s.managed_etag.clone_from(&st.etag);
            let t = st.turn.as_ref();
            s.run = RunPart {
                state: st.run.as_str().to_owned(),
                turn_id: t.map(|t| t.id.to_string()),
                origin: t.map(|t| origin_str(t.origin).to_owned()),
                pending_inputs: st.pending_inputs,
                interrupt_sent_at: t.and_then(|t| t.interrupt_sent).map(rfc3339),
                interrupt_elapsed_ms: t.and_then(|t| t.interrupt_mono).map(elapsed_ms),
                pending_uuids: t.map(|t| t.uuids.clone()).unwrap_or_default(),
                last_uuid: t.and_then(|t| t.uuids.last().cloned()),
                queued_turn_id: st.queued.as_ref().map(|q| q.id.to_string()),
                handed: t.map(|t| t.handed),
                queued_interrupt_elapsed_ms: st.queued.as_ref().and_then(|q| q.interrupt_mono).map(elapsed_ms),
                queued_handed: st.queued.as_ref().map(|q| q.handed),
                open_tools: st.open_tools.iter().cloned().collect(),
                images: st.images,
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
                    tool_use_id: a.tool_use_id.clone(),
                    elapsed_ms: Some(elapsed_ms(a.since_mono)),
                    lane: a.lane.map(|l| l.to_string()),
                })
                .collect();
            s.ending = st
                .ending
                .iter()
                .map(|e| EndingState {
                    turn_id: e.id.to_string(),
                    report: serde_json::to_value(&e.report).unwrap_or_default(),
                    uuids: e.uuids.clone(),
                    generation: e.generation,
                    stopping: e.stopping,
                    last: e.last.clone(),
                })
                .collect();
            s.delivered = st.delivered.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        });
    }

    fn run_fx(&self, fx: Vec<Fx>) {
        for f in fx {
            match f {
                Fx::Report(item) => self.reporter.push(item),
                Fx::ReportIn(lane, item) => self.reporter.push_in(lane, item),
                Fx::Task(ev) => self.reporter.task_event(ev),
                Fx::Live(turn, frame) => self.reporter.live_frame(turn, frame),
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
    /// turn's uuids must be stored first (8.2); otherwise (a generation that exited writes nothing more) the
    /// filesystem writes already in flight are answered first.
    #[allow(clippy::needless_pass_by_value)] // an outcome is consumed by the turn's end
    fn end_turn_now(&self, st: &mut St, fx: &mut Vec<Fx>, outcome: Outcome, wait: bool, exit_code: Option<i32>) {
        let Some(turn) = st.turn.take() else { return };
        let held = !st.tracker.is_empty() && st.live.is_some();
        let (state, err, out) = match &outcome {
            Outcome::Completed => (ReportedState::Completed, None, TurnOutcome::Completed),
            Outcome::Failed(e) => (ReportedState::Failed, Some(e.clone()), TurnOutcome::Failed),
            Outcome::Canceled => (ReportedState::Canceled, None, TurnOutcome::Canceled),
        };
        st.open_tools.clear();
        let ms = u64::try_from(st.worked(&turn).as_millis()).unwrap_or(u64::MAX);
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
        let ending = Ending {
            id: turn.id,
            report,
            uuids: wait.then_some(turn.uuids),
            generation: st.live.as_ref().map_or(st.last_gen, |g| g.n),
            stopping: turn.interrupt_sent.is_some(),
            last: LastTurn {
                id: turn.id.to_string(),
                outcome: match out {
                    TurnOutcome::Completed => "completed".into(),
                    TurnOutcome::Failed => "failed".into(),
                    TurnOutcome::Canceled => "canceled".into(),
                },
                error: err.and_then(|e| serde_json::to_value(e).ok()),
            },
        };
        fx.push(Fx::Report(self.end_item(&ending)));
        st.ending.push(ending);
        if let Some(q) = st.queued.take() {
            // the person's turn written during an agent turn is now the turn; one stopped while it waited is
            // interrupted again now that Claude Code works on it (the 15 s grace runs from the first interrupt)
            if q.interrupt_sent.is_some() && q.handed {
                fx.push(Fx::Stdin(control::interrupt(&format!("intr-{}", Uuid::new_v4()))));
            }
            let stopping = q.interrupt_sent.is_some();
            st.turn = Some(q);
            fx.push(Fx::SetTurn(st.turn.as_ref().map(|t| t.id)));
            st.set_run(if stopping { RunKind::Stopping } else { RunKind::Running });
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

    /// The end report of `e`: once magpie answered it, the heartbeat stops naming the turn and names it as
    /// `lastTurn` instead.
    fn end_item(&self, e: &Ending) -> Item {
        let me = self.me.clone();
        let (id, last) = (e.id, e.last.clone());
        Item::State {
            turn: id,
            report: e.report.clone(),
            wait: match &e.uuids {
                Some(u) => Wait::Uuids {
                    uuids: u.clone(),
                    generation: e.generation,
                },
                None => Wait::Settle,
            },
            done: Some(Box::new(move |ok| {
                if let Some(d) = me.upgrade() {
                    d.with(Change::State, |st, _| {
                        st.ending.retain(|x| x.id != id);
                        if ok {
                            st.last_turn = Some(last);
                        }
                    });
                }
            })),
        }
    }

    /// Opens an agent-started turn (7.5, 11.2) under `id` (derived from the line that opened it, so a restarted
    /// supervisor that re-reads the line opens the same turn).
    fn open_agent_turn(st: &mut St, fx: &mut Vec<Fx>, id: Uuid) {
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

    fn on_line(&self, gen_n: u32, line: Line) {
        let boot = self.ctx().map(|c| c.boot_id.clone()).unwrap_or_default();
        self.with_change(|st, fx| ((), self.line_in(st, fx, gen_n, line, &boot)));
    }

    /// One stdout line under the lock; how urgently it is persisted: a line that changes the run, its tasks or its
    /// asks at once (4.4: a restart must not re-read it as new), any other with the throttled offset.
    #[allow(clippy::too_many_lines)] // one arm per stdout message of 7.4
    fn line_in(&self, st: &mut St, fx: &mut Vec<Fx>, gen_n: u32, line: Line, boot: &str) -> Change {
        let Some(g) = st.live.as_mut() else {
            return Change::Progress;
        };
        if g.n != gen_n {
            return Change::Progress;
        }
        g.offset = line.end;
        let gen_started = g.started_at;
        let gen_key = format!("{gen_n}:{}", line.end);
        if !matches!(line.out, Out::KeepAlive | Out::Unparsed) {
            st.last_output = now();
            st.last_output_mono = Instant::now();
        }
        let mut change = match &line.out {
            Out::Init { .. }
            | Out::Task(_)
            | Out::Result(_)
            | Out::Compacted
            | Out::ControlRequest { .. }
            | Out::ControlCancel { .. } => Change::State,
            Out::Message {
                tool_uses,
                task_notification,
                images,
                ..
            } if !tool_uses.is_empty() || task_notification.is_some() || *images > 0 => Change::State,
            _ => Change::Progress,
        };
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
            Self::open_agent_turn(st, fx, agent_turn_id(boot, gen_n, line.end, gen_started));
            change = Change::State;
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
                tool_results,
                task_notification,
                images,
                ..
            } => {
                if let Some(text) = task_notification {
                    st.tracker.on_notification(&text);
                }
                if main && images > 0 {
                    // a tool result's images stay in Claude Code's context, sent with every model call from now on
                    st.images = Some(st.images.unwrap_or(0).saturating_add(images));
                }
                if main {
                    let t = now();
                    for tu in &tool_uses {
                        st.tracker.on_tool_use(tu, t);
                        st.open_tools.insert(tu.id.clone());
                    }
                    for id in &tool_results {
                        st.open_tools.remove(id);
                    }
                    if let (Some(u), Some(turn)) = (uuid, st.turn.as_mut())
                        && !turn.uuids.contains(&u)
                    {
                        turn.uuids.push(u);
                    }
                }
            }
            Out::StreamEvent(p) => {
                if let Some(turn) = st.turn.as_ref().map(|t| t.id)
                    && let Some(frame) = st.partial.frame(p)
                {
                    fx.push(Fx::Live(turn, frame));
                }
            }
            Out::KeepAlive => {}
            Out::Compacted => {
                tracing::info!(images = st.images.unwrap_or(0), "claude compacted the conversation");
                st.images = Some(0);
                st.compact_tried = None;
            }
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
                    let ask_id = ask_id(boot, gen_n, line.end).to_string();
                    let ask_lane = st.turn.as_ref().map(|t| t.id);
                    tracing::info!(ask = %ask_id, tool = %tool_name, "ask opened");
                    st.asks.push(Ask {
                        ask_id: ask_id.clone(),
                        cli_request_id: request_id,
                        tool_name: tool_name.clone(),
                        tool_use_id: tool_use_id.clone(),
                        since: now(),
                        since_mono: Instant::now(),
                        lane: ask_lane,
                    });
                    fx.push(Fx::ReportIn(
                        ask_lane,
                        Item::AskOpen(AskReport {
                            ask_id,
                            tool_name,
                            tool_use_id,
                            input,
                        }),
                    ));
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
                    fx.push(a.resolved(AskOutcome::Canceled));
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
        change
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
            // an exit nobody asked for, or one after a failed initialisation, counts towards the crash loop (7.9;
            // the limits table's "3 exits in 60 s")
            if matches!(g.why, None | Some(Why::Init)) {
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
            st.open_tools.clear();
            st.pending_inputs = 0;
            for a in st.asks.drain(..) {
                fx.push(a.resolved(AskOutcome::Canceled));
            }
            if st.run != RunKind::Idle {
                st.set_run(RunKind::Idle);
            }
        });
        // end reports still waiting for this generation's lines stop waiting: it writes nothing more
        self.reporter.generation_ended(gen_n);
    }

    // ---- the tick ----

    fn tick(&self) {
        self.with(Change::State, |st, fx| {
            let t = now();
            let mono = Instant::now();
            // interrupt grace
            if let (Some(turn), Some(g)) = (st.turn.as_ref(), st.live.as_mut())
                && let Some(sent) = turn.interrupt_mono
                && mono.duration_since(sent) > INTERRUPT_GRACE
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
                    &ask_timeout_answer(),
                    a.tool_use_id.as_deref(),
                )));
                fx.push(a.resolved(AskOutcome::Canceled));
            }
            // pseudo-tasks and the silence ceiling
            if st.tracker.expire(t) > 0 {
                Self::hold_check(st, fx, "tasks done");
            }
            if st.run == RunKind::Held
                && st.asks.is_empty()
                && mono.duration_since(st.last_output_mono) > HOLD_SILENCE
                && !st.tracker.until_ahead(t)
            {
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

    /// Waits until the preparation of `turn` must stop ([`St::cancelled`]).
    async fn until_cancelled(&self, turn: Option<Uuid>) {
        self.wait_until(None, |s| s.cancelled(turn)).await;
    }

    /// The live generation for `session`, spawned (and initialised) if there is none or it runs another session.
    /// Every wait in it (another spawn, the models route, the initialisation) ends as soon as `turn` is interrupted
    /// or killed or a release begins; a generation already started then goes on initialising by itself.
    async fn ensure_gen(&self, session: &Session, turn: Option<Uuid>) -> Result<u32, GenError> {
        let _lane = tokio::select! {
            biased;
            () = self.until_cancelled(turn) => return Err(GenError::Canceled),
            l = self.gen_lane.lock() => l,
        };
        let live = {
            let st = lock(&self.st);
            st.live.as_ref().map(|g| (g.n, g.session_id.clone(), g.initialized))
        };
        if let Some((n, sid, init)) = live {
            if sid == session.id() {
                if init {
                    return Ok(n);
                }
                // initialising (a spawn whose turn was stopped meanwhile): wait for it
                self.wait_until(Some(Instant::now() + INIT_WAIT), |s| {
                    s.live.as_ref().is_none_or(|g| g.n != n || g.initialized) || s.cancelled(turn)
                })
                .await;
                if lock(&self.st).cancelled(turn) {
                    return Err(GenError::Canceled);
                }
                if lock(&self.st).live.as_ref().is_some_and(|g| g.n == n && g.initialized) {
                    return Ok(n);
                }
                // it never initialised: it ends before another starts on the same session
                tracing::warn!(gen = n, "a generation that did not initialise is killed");
                self.kill_gen(n, Why::Init).await;
            } else {
                tracing::info!(live = %sid, wanted = %session.id(), "another session: generation ended");
                self.end_generation(
                    Why::Graceful,
                    Instant::now() + STDIN_CLOSE_GRACE + KILL_AFTER_TERM + Duration::from_secs(2),
                )
                .await;
            }
        }
        let counted = {
            let st = lock(&self.st);
            st.images.is_some() && st.images_session.as_deref() == Some(session.id())
        };
        if !counted {
            // the first generation on this session here (a conversation resumed on a new sandbox, or another
            // session): what its transcript already holds counts
            let n = match session {
                Session::Resume(id) => self.transcript_images(id).await,
                Session::New(_) => 0,
            };
            self.with(Change::State, |st, _| {
                st.images = Some(n);
                st.images_session = Some(session.id().to_owned());
                st.compact_tried = None;
            });
        }
        if let Some(g) = lock(&self.st).live.as_ref() {
            // never two Claude Codes: one that would not end blocks the next
            return Err(GenError::Failed(format!(
                "the previous Claude Code (generation {}) would not end",
                g.n
            )));
        }
        if let Some(until) = lock(&self.st).failing_until
            && Instant::now() < until
        {
            return Err(GenError::Failed("Claude Code would not start".into()));
        }
        self.spawn(session, turn).await
    }

    /// Switches generation `n` to the conversation's thinking level (`start_turn.thinking`; absent: the level the
    /// models route names for the generation's model, else Claude Code's own for it) when it is not on it already:
    /// thinking off for `none` (`set_max_thinking_tokens` 0), else on at Claude Code's effort level of that name
    /// (`apply_flag_settings` `effortLevel`). Claude Code sends both with every model call (`thinking` present or
    /// not, `output_config.effort`), and the gateway applies the level.
    async fn apply_thinking(&self, n: u32, thinking: Option<&str>) -> Result<(), String> {
        let (model, applied) = {
            let st = lock(&self.st);
            match st.live.as_ref().filter(|g| g.n == n) {
                Some(g) => (g.model.clone(), g.thinking.clone()),
                None => return Err("Claude Code exited before the prompt was written".into()),
            }
        };
        let level: Option<String> = if let Some(l) = thinking {
            Some(l.to_owned())
        } else {
            let models = self.models().await?;
            models
                .models
                .iter()
                .find(|m| m.id == model)
                .and_then(|m| m.reasoning.clone())
        };
        if applied == Thinking::Level(level.clone()) {
            return Ok(());
        }
        tracing::info!(gen = n, thinking = ?level, "claude switched to the conversation's thinking level");
        match level.as_deref() {
            Some("none") => {
                self.control_call(n, |id| control::set_max_thinking_tokens(id, Some(0)))
                    .await?;
            }
            Some(l) => {
                self.control_call(n, |id| control::set_max_thinking_tokens(id, None))
                    .await?;
                self.control_call(n, |id| control::set_effort(id, l)).await?;
            }
            // back from thinking off to no level named: on again (the effort stays the last one set)
            None if applied == Thinking::Level(Some("none".to_owned())) => {
                self.control_call(n, |id| control::set_max_thinking_tokens(id, None))
                    .await?;
            }
            None => {}
        }
        self.with(Change::Progress, |st, _| {
            if let Some(g) = st.live.as_mut().filter(|g| g.n == n) {
                g.thinking = Thinking::Level(level);
            }
        });
        Ok(())
    }

    /// One control request to generation `n` and its answer (at most [`INIT_WAIT`]).
    async fn control_call(&self, n: u32, request: impl FnOnce(&str) -> Vec<u8>) -> Result<(), String> {
        let id = format!("ctl-{}", Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        let writer = {
            let mut st = lock(&self.st);
            let Some(w) = st.live.as_ref().filter(|g| g.n == n).map(|g| g.writer.clone()) else {
                return Err("Claude Code exited".into());
            };
            st.control_waiters.insert(id.clone(), tx);
            w
        };
        let _ = writer.send(Write {
            msg_id: keeper::new_msg_id(),
            bytes: request(&id),
            done: None,
        });
        match tokio::time::timeout(INIT_WAIT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("Claude Code exited".into()),
            Err(_) => {
                lock(&self.st).control_waiters.remove(&id);
                Err("Claude Code did not answer a control request within 30 s".into())
            }
        }
    }

    /// The models a generation starts with: the route's, with the conversation's picked model (`start_turn.model`)
    /// as the default when there is one; and whether there is.
    fn on_picked_model(&self, mut models: Models) -> (Models, bool) {
        let picked = lock(&self.st).model.clone();
        if let Some(m) = &picked {
            models.default.clone_from(m);
        }
        (models, picked.is_some())
    }

    #[allow(clippy::too_many_lines)] // one generation's start, step by step
    async fn spawn(&self, session: &Session, turn: Option<Uuid>) -> Result<u32, GenError> {
        let Some(ctx) = self.ctx() else {
            return Err(GenError::Failed("not attached".into()));
        };
        let (models, picked) = tokio::select! {
            biased;
            () = self.until_cancelled(turn) => return Err(GenError::Canceled),
            m = self.models() => self.on_picked_model(m.map_err(GenError::Failed)?),
        };
        let descriptor = self
            .hermit
            .descriptor()
            .ok_or_else(|| GenError::Failed("no descriptor".into()))?;
        let env = launch::env(&ctx.boot, &descriptor.env, &models);
        let n = {
            let mut st = lock(&self.st);
            if st.cancelled(turn) {
                // stopped, or a release or pause has begun: nothing new is started on this sandbox
                return Err(GenError::Canceled);
            }
            st.last_gen += 1;
            st.last_gen
        };
        let dir = self.cfg.gen_dir.join(n.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| GenError::Failed(format!("generation directory: {e}")))?;
        let stdout = dir.join("stdout.ndjson");
        let stderr = dir.join("stderr.log");
        let spec = self.cfg.launch.spec(
            session,
            descriptor.claude.project_settings,
            env,
            stdout.to_string_lossy().into_owned(),
            stderr.to_string_lossy().into_owned(),
            ctx.cgroup.clone(),
        );
        let spawned = self
            .keeper_call(move |k| k.spawn_tracked(&spec))
            .await
            .map_err(GenError::Failed)?;
        tracing::info!(gen = n, pid = spawned.pid, session = %session.id(), resume = matches!(session, Session::Resume(_)), model = %models.default, "claude started");
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
                // what the managed files hold now (written at attach and at each start's step 1)
                etag: st.etag.clone(),
                model: models.default.clone(),
                picked,
                thinking: Thinking::Unknown,
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
        // the initialisation is seen through by its own task, so a turn stopped meanwhile does not wait for it
        let me = self.arc();
        self.go(async move {
            let verdict = match tokio::time::timeout(INIT_WAIT, init_rx).await {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(e))) => Err(format!("Claude Code refused to initialise: {e}")),
                Ok(Err(_)) => Err("Claude Code exited before it initialised".into()),
                Err(_) => Err("Claude Code did not initialise within 30 s".into()),
            };
            if let Err(why) = &verdict {
                tracing::warn!(gen = n, why = %why, "claude not initialised");
                me.kill_gen(n, Why::Init).await;
            }
            me.with(Change::State, |st, _| {
                if verdict.is_ok()
                    && let Some(g) = st.live.as_mut().filter(|g| g.n == n)
                {
                    g.initialized = true;
                }
                st.init_verdict = Some((n, verdict));
            });
        });
        self.wait_until(None, |s| {
            s.init_verdict.as_ref().is_some_and(|(v, _)| *v == n) || s.cancelled(turn)
        })
        .await;
        let st = lock(&self.st);
        match &st.init_verdict {
            Some((v, Ok(()))) if *v == n => Ok(n),
            Some((v, Err(e))) if *v == n => Err(GenError::Failed(e.clone())),
            _ => Err(GenError::Canceled),
        }
    }

    fn stopped_message(st: &St) -> String {
        st.release
            .as_ref()
            .map_or_else(|| "the sandbox is pausing".into(), |r| format!("released: {r}"))
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
        #[cfg(test)]
        let me = self.arc();
        self.go(async move {
            while let Some(w) = rx.recv().await {
                #[cfg(test)]
                if w.done.is_some() && me.race_kill_with_prompt.load(std::sync::atomic::Ordering::SeqCst) {
                    me.hold_prompt_until_killed(n).await;
                }
                let k = k.clone();
                let r =
                    tokio::task::spawn_blocking(move || k.stdin(pid, w.msg_id, &w.bytes).map_err(|e| e.to_string()))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r);
                if let Err(e) = &r {
                    tracing::debug!(gen = n, error = %e, "stdin write failed");
                }
                #[cfg(test)]
                {
                    let ms = STDIN_ACK_DELAY_MS.load(std::sync::atomic::Ordering::SeqCst);
                    if ms > 0 && w.done.is_some() {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
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
                    #[cfg(test)]
                    if me.race_kill_with_prompt.load(std::sync::atomic::Ordering::SeqCst) {
                        // the failed prompt write is handled first
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    let rec = (e.name != keeper::watch::LOST).then_some(e);
                    me.on_exit(n, rec.as_ref());
                    return;
                }
            }
        });
    }

    /// Tests ([`Driver::race_kill_with_prompt`]): the prompt for generation `n` waits until a kill of `n` is
    /// recorded and its SIGKILL has landed (at most 10 s), so the write fails before the exit is handled.
    #[cfg(test)]
    async fn hold_prompt_until_killed(&self, n: u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !lock(&self.st)
                .live
                .as_ref()
                .is_some_and(|g| g.n == n && g.why.is_some())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
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
                let why = g.why.get_or_insert(why);
                (g.n, g.pid, matches!(why, Why::Park))
            })
        });
        let Some((n, pid, parked)) = g else { return };
        let gone = move |s: &St| s.live.as_ref().is_none_or(|g| g.n != n);
        // a parked turn's generation keeps its stdin: closed, Claude Code would answer the open question itself
        // ("permission stream closed") and call the model again (supervisor 7.6)
        if !parked {
            let _ = self.keeper_call(move |k| k.close_stdin(pid)).await;
            let first =
                (Instant::now() + STDIN_CLOSE_GRACE).min(deadline.checked_sub(KILL_AFTER_TERM).unwrap_or(deadline));
            if self.wait_until(Some(first), gone).await {
                return;
            }
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
            // the start_turn is still finishing its delivery while its turn has already ended (a quick answer): the
            // compaction check waits for it, the rest is that start_turn's
            let _lane = self.turn_lane.lock().await;
            self.compact_if_due();
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
            return;
        }
        self.compact_if_due();
    }

    /// At an idle point, with the descriptor's `claude.compactAtImages` reached: Claude Code is told to compact
    /// (`/compact`), as a turn of origin agent that ends at the compaction's own `result`. Every model call sends all
    /// the conversation's images again, and model hosts slow down or refuse past a few dozen; the compaction replaces
    /// the conversation so far, its images included, by a summary. A person's `start_turn` meanwhile is written
    /// behind it and answered after it, as during any agent turn (7.5).
    fn compact_if_due(&self) {
        let boot = self.ctx().map(|c| c.boot_id.clone()).unwrap_or_default();
        self.with(Change::State, |st, fx| {
            let (Some(at), Some(images)) = (st.compact_at, st.images) else {
                return;
            };
            let idle = st.run == RunKind::Idle
                && st.turn.is_none()
                && st.queued.is_none()
                && st.starting.is_none()
                && st.tracker.is_empty()
                && st.pending_inputs == 0
                && !st.quiesced
                && st.release.is_none();
            if !idle || images < at || st.compact_tried.is_some_and(|t| images <= t) {
                return;
            }
            let Some(g) = st.live.as_ref().filter(|g| g.initialized) else {
                return;
            };
            tracing::info!(images, at, "the conversation holds too many images: compacting");
            let id = agent_turn_id(&boot, g.n, g.offset, g.started_at);
            st.compact_tried = Some(images);
            // as a wake-up's turn, it is no pending input: it ends at its own `result`, and a person's message written
            // meanwhile is answered by the next one
            Self::open_agent_turn(st, fx, id);
            fx.push(Fx::Stdin(prompt::user_line(
                &Value::String("/compact".to_owned()),
                &rfc3339(now()),
            )));
        });
    }

    /// The image blocks of the main transcript since its last compaction: what a resumed Claude Code sends with
    /// every model call. A transcript that cannot be read counts as none.
    async fn transcript_images(&self, session: &str) -> u32 {
        let path = self.cfg.claude_dir.join(format!("projects/-workspace/{session}.jsonl"));
        let counted = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::task::spawn_blocking(move || count_images(&path)),
        )
        .await;
        match counted {
            Ok(Ok(Ok(n))) => n,
            Ok(Ok(Err(e))) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, "the transcript's images were not counted");
                }
                0
            }
            _ => {
                tracing::warn!("the transcript's images were not counted in 30 s");
                0
            }
        }
    }

    /// Writes the managed files from the descriptor if its `etag` differs from what they hold.
    fn apply_descriptor(&self, d: &hermit_api::sandbox_surface::SandboxSelf) {
        lock(&self.st).compact_at = d.claude.compact_at_images;
        let differs = lock(&self.st).etag != d.etag;
        if differs {
            match managed::write(&self.cfg.managed_dir, &d.claude) {
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
        self.ack_in(None, seq, result);
    }

    /// Acks `seq` in `lane` (a command about a turn: after that turn's reports).
    fn ack_in(&self, lane: Option<Uuid>, seq: u64, result: AckResult) {
        let me = self.me.clone();
        self.reporter.push_in(
            lane,
            Item::Ack {
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
            },
        );
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
            CommandBody::SendMessage(m) => self.send_message(seq, c.turn_id, &m.text).await,
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

    /// Where a `start_turn`'s message is kept between its `written` record and its delivery: a replay after a
    /// restart sends these bytes again (10.2) rather than building them anew.
    fn message_path(&self, n: u32, seq: u64) -> PathBuf {
        self.cfg.gen_dir.join(n.to_string()).join(format!("stdin-{seq}.ndjson"))
    }

    /// Keeps the message before its `written` record exists: a turn whose message cannot be kept is not started,
    /// so a replay always finds the bytes it must send again.
    async fn save_message(&self, n: u32, seq: u64, line: &[u8]) -> Result<(), String> {
        let path = self.message_path(n, seq);
        let bytes = line.to_vec();
        tokio::task::spawn_blocking(move || {
            let tmp = path.with_extension("tmp");
            let r = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &path));
            if r.is_err() {
                // a part-written file is not left until its generation directory is pruned
                let _ = std::fs::remove_file(&tmp);
            }
            r
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|r| r)
        .map_err(|e| format!("the message could not be kept for a replay: {e}"))
    }

    /// Removes the kept message once `run.json` no longer needs it: the state that makes a replay unnecessary
    /// (`running`, `failed`, or the turn's end) is on disk first, so a restart in between still finds the bytes.
    /// The wait is bounded: a `run.json` that cannot be written keeps the file (its generation directory is pruned
    /// at a later idle point) and the command is still acked, so the turn lane is never held for ever.
    async fn drop_message(&self, n: u32, seq: u64) {
        if let Some(ctx) = self.ctx()
            && tokio::time::timeout(MESSAGE_DROP_FLUSH_MAX, ctx.persisted.flush())
                .await
                .is_err()
        {
            tracing::warn!(seq, "run.json not written in time: the kept message stays");
            return;
        }
        let _ = tokio::fs::remove_file(self.message_path(n, seq)).await;
    }

    /// Records that `seq`'s message was never handed over (its end is reported): a replay before magpie answers
    /// the ack sends the same negative ack again rather than taking the message as answered.
    fn mark_failed(&self, seq: u64, err: Option<&str>) {
        self.with(Change::State, |st, _| {
            st.delivered.insert(
                seq,
                Delivered {
                    state: "failed".into(),
                    error: err.map(str::to_owned),
                    ..st.delivered.get(&seq).cloned().unwrap_or_default()
                },
            );
        });
    }

    /// Ends the preparation of `turn_id` that was interrupted, killed, or overtaken by a release or pause: nothing
    /// was written, and its end is reported (`canceled`, or `failed` `sandbox.lost`).
    fn stop_preparing(st: &mut St, fx: &mut Vec<Fx>, turn_id: Uuid) {
        st.starting = None;
        if st.cancel_starting == Some(turn_id) {
            st.cancel_starting = None;
        }
        let report = if st.release.is_some() || st.quiesced {
            end_report(
                ReportedState::Failed,
                Some(error(DataCode::SandboxLost, Self::stopped_message(st))),
            )
        } else {
            end_report(ReportedState::Canceled, None)
        };
        tracing::info!(turn = %turn_id, "turn stopped before its message was written");
        fx.push(Fx::Report(Item::state(turn_id, report)));
    }

    /// Steps 1 to 3 of 10.2 for a new turn: the descriptor, the hold, the uploads; the message.
    async fn prepare(&self, s: &StartTurn, replay: bool) -> Result<Vec<u8>, String> {
        // 1: the descriptor (a change is applied at the next idle point) and the conversation's picked model; 2: a
        // hold is superseded (an agent turn just takes the message, 7.5). A replay's turn already runs: none applies.
        if !replay {
            match self.hermit.refresh().await {
                Ok(d) => self.apply_descriptor(&d),
                Err(e) => tracing::info!(error = %e, "descriptor not re-read"),
            }
            // the model this turn runs on: a generation on another model (or on other managed files) ends here and
            // the turn resumes the conversation on a new one. While a turn or background tasks run, the generation
            // stays: the change waits for the next turn that finds it free.
            let end = self.with(Change::State, |st, _| {
                st.model.clone_from(&s.model);
                st.turn.is_none()
                    && st.tracker.is_empty()
                    && st
                        .live
                        .as_ref()
                        .is_some_and(|g| g.etag != st.etag || !g.runs(st.model.as_deref()))
            });
            if end {
                tracing::info!("configuration or model changed: generation ended before the turn");
                self.end_generation(
                    Why::Graceful,
                    Instant::now() + STDIN_CLOSE_GRACE + KILL_AFTER_TERM + Duration::from_secs(2),
                )
                .await;
            }
            if lock(&self.st).run == RunKind::Held {
                self.stop_tasks(Instant::now() + SUPERSEDE_WAIT, "supersede").await;
            }
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
        let blob_deadline = Instant::now() + uploads::BLOB_RETRY_FOR;
        let paths = self
            .cfg
            .uploads
            .materialise(&self.reporter.calls(), &items, blob_deadline)
            .await?;
        let mut images = Vec::new();
        for i in &s.images {
            images.push(prompt::ImageBlock {
                media_type: i.model_mime.clone(),
                bytes: uploads::blob(&self.reporter.calls(), &i.model_blob, blob_deadline).await?,
            });
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
        // a parked turn's resumption: its answers are the message (a resumed Claude Code no longer takes an answer to
        // its predecessor's question, supervisor 7.6)
        let text = if s.answers.is_empty() {
            prompt::text(&s.text, &files, &shown)
        } else {
            prompt::answers(&s.answers)
        };
        Ok(prompt::user_line(&prompt::content(text, &images), &s.sent_at.to_wire()))
    }

    #[allow(clippy::too_many_lines)] // the steps of 10.2 in order
    async fn start_turn(&self, seq: u64, turn_id: Uuid, s: StartTurn) {
        let _lane = self.turn_lane.lock().await;
        let Some(ctx) = self.ctx() else { return };
        let conversation = ctx.conversation;
        let delivered = lock(&self.st).delivered.get(&seq).cloned().unwrap_or_default();
        let delivered_ack = |me: &Self, yes: bool| {
            me.ack_in(
                Some(turn_id),
                seq,
                AckResult {
                    delivered: Some(yes),
                    ..AckResult::default()
                },
            );
        };
        if delivered.state == "running" {
            return delivered_ack(self, true);
        }
        if delivered.state == "failed" {
            // the message was never handed over: the same negative ack again
            return match delivered.error.as_deref() {
                Some(e) => self.ack_in(Some(turn_id), seq, err_ack(e)),
                None => delivered_ack(self, false),
            };
        }
        // a restart between the record and the report (10.2): the restored turn runs on that generation already
        let replay = delivered.state == "written";
        if replay {
            let (alive, current) = {
                let st = lock(&self.st);
                (
                    st.live.as_ref().map(|g| g.n),
                    st.turn.as_ref().is_some_and(|t| t.id == turn_id)
                        || st.queued.as_ref().is_some_and(|t| t.id == turn_id),
                )
            };
            if alive.is_none() || alive != delivered.generation {
                self.with(Change::State, |_, fx| {
                    fx.push(Fx::Report(Item::state(
                        turn_id,
                        end_report(
                            ReportedState::Failed,
                            Some(error(
                                DataCode::RunExited,
                                "Claude Code exited before the prompt was answered",
                            )),
                        ),
                    )));
                });
                return self.ack_in(Some(turn_id), seq, err_ack("Claude Code exited"));
            }
            if !current {
                // the restored turn already ended: Claude Code had the message and answered it
                return delivered_ack(self, true);
            }
        }
        // the one door: busy at once, or refused because a release or pause has begun (a replay's turn is installed
        // already: an interrupt or kill finds it there)
        let admitted = ctx.lifecycle.upgrade().is_some_and(|l| {
            l.admit(|| {
                if !replay {
                    self.with(Change::State, |st, _| st.starting = Some(turn_id));
                }
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
                if st.cancel_starting == Some(turn_id) {
                    st.cancel_starting = None;
                }
                let err = if st.release.is_some() {
                    error(DataCode::SandboxLost, Self::stopped_message(st))
                } else {
                    error(DataCode::RunError, msg.clone())
                };
                fx.push(Fx::Report(Item::state(
                    turn_id,
                    end_report(ReportedState::Failed, Some(err)),
                )));
            });
            me.ack_in(Some(turn_id), seq, err_ack(&msg));
        };
        // stopped while it was prepared: nothing is written (a replay's turn ends as any running turn does)
        let stopped = |me: &Self| {
            me.with(Change::State, |st, fx| {
                if replay {
                    st.starting = None;
                } else {
                    Self::stop_preparing(st, fx, turn_id);
                }
            });
            delivered_ack(me, false);
        };
        let session = match &s.resume.provider_session_id {
            Some(id) => Session::Resume(id.clone()),
            None => Session::New(conversation.to_string()),
        };
        // a replay sends the bytes it recorded: nothing is downloaded or renamed onto the workspace again (the
        // agent may have changed those files since)
        let kept = match (replay, delivered.generation) {
            (true, Some(g)) => tokio::fs::read(self.message_path(g, seq)).await.ok().map(|b| (g, b)),
            _ => None,
        };
        if replay && kept.is_none() {
            // unreachable unless the file was removed by hand: save_message comes before the record
            tracing::warn!(seq, "the replayed message was not kept: built again");
        }
        let (n, line) = if let Some(k) = kept {
            k
        } else {
            // 1-3, then 4: a generation; an interrupt, kill, release or pause ends the wait at once (7.8)
            let line = tokio::select! {
                biased;
                () = self.until_cancelled(Some(turn_id)) => {
                    self.cfg.uploads.sweep().await;
                    return stopped(self);
                }
                r = self.prepare(&s, replay) => match r {
                    Ok(l) => l,
                    Err(e) => return fail(self, e),
                },
            };
            match self.ensure_gen(&session, Some(turn_id)).await {
                Ok(n) => {
                    // the conversation's thinking level, before its message is written
                    if let Err(e) = self.apply_thinking(n, s.thinking.as_deref()).await {
                        return fail(self, e);
                    }
                    if !replay && let Err(e) = self.save_message(n, seq, &line).await {
                        return fail(self, e);
                    }
                    (n, line)
                }
                Err(GenError::Canceled) => return stopped(self),
                Err(GenError::Failed(e)) => return fail(self, e),
            }
        };
        // 5: the record, the pending input and the person's turn together, before the keeper has the message:
        // Claude Code's answer can never be read without the turn it belongs to (7.5), and a restart finds both
        let id = if replay {
            delivered
                .msg_id
                .as_deref()
                .and_then(unhex)
                .unwrap_or_else(|| msg_id(&ctx.boot_id, seq))
        } else {
            msg_id(&ctx.boot_id, seq)
        };
        let prompt_images = match crate::cc::stream::parse(line.strip_suffix(b"\n").unwrap_or(&line)) {
            Out::Message { images, .. } => images,
            _ => 0,
        };
        let prepared = self.with(Change::State, |st, fx| {
            if st.cancelled(Some(turn_id)) {
                // stopped (or released) while it was prepared: the message is never written
                if replay {
                    st.starting = None;
                } else {
                    Self::stop_preparing(st, fx, turn_id);
                }
                return Prepared::Stopped;
            }
            let Some(sid) = st.live.as_ref().filter(|g| g.n == n).map(|g| g.session_id.clone()) else {
                st.starting = None;
                fx.push(Fx::Report(Item::state(
                    turn_id,
                    end_report(
                        ReportedState::Failed,
                        Some(error(
                            DataCode::RunExited,
                            "Claude Code exited before the prompt was written",
                        )),
                    ),
                )));
                return Prepared::Gone;
            };
            st.starting = None;
            let current = st.turn.as_ref().is_some_and(|t| t.id == turn_id)
                || st.queued.as_ref().is_some_and(|t| t.id == turn_id);
            if replay && !current {
                // the restored turn ended meanwhile: Claude Code had the message and answered it
                return Prepared::Answered;
            }
            let after = if replay {
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
                    turn_id: Some(turn_id.to_string()),
                    error: None,
                },
            );
            if !replay {
                // the message's own image blocks join the context with it
                st.images = Some(st.images.unwrap_or(0).saturating_add(prompt_images));
            }
            if !current {
                st.pending_inputs = after;
                let mut t = Turn::new(turn_id, TurnOrigin::Person);
                t.handed = false;
                if st.turn.as_ref().is_some_and(|t| t.origin == TurnOrigin::Agent) {
                    st.queued = Some(t);
                } else {
                    st.turn = Some(t);
                    st.set_run(RunKind::Running);
                    fx.push(Fx::SetTurn(Some(turn_id)));
                }
                tracing::info!(turn = %turn_id, origin = "person", "turn started");
                fx.push(Fx::MainTranscript(format!("projects/-workspace/{sid}.jsonl")));
                fx.push(Fx::Report(Item::state(turn_id, running_report(&sid))));
            }
            Prepared::Write
        });
        match prepared {
            Prepared::Write => {}
            Prepared::Stopped => {
                self.drop_message(n, seq).await;
                return delivered_ack(self, false);
            }
            Prepared::Gone => {
                self.drop_message(n, seq).await;
                return self.ack_in(Some(turn_id), seq, err_ack("Claude Code exited"));
            }
            Prepared::Answered => {
                self.drop_message(n, seq).await;
                return delivered_ack(self, true);
            }
        }
        ctx.persisted.flush().await;
        // the message to the writer, under the lock: an interrupt that came meanwhile goes right after it
        let rx = self.with(Change::State, |st, fx| {
            if st.release.is_some() {
                // released before the hand-over: the message is never written
                let o = Outcome::Failed(error(DataCode::SandboxLost, Self::stopped_message(st)));
                if st.turn.as_ref().is_some_and(|t| t.id == turn_id) {
                    self.end_turn_now(st, fx, o, false, None);
                } else if st.queued.take_if(|q| q.id == turn_id).is_some() {
                    st.pending_inputs = st.pending_inputs.saturating_sub(1);
                    if let Outcome::Failed(e) = o {
                        fx.push(Fx::Report(Item::state(
                            turn_id,
                            end_report(ReportedState::Failed, Some(e)),
                        )));
                    }
                }
                return None;
            }
            let w = st.live.as_ref().filter(|g| g.n == n).map(|g| g.writer.clone())?;
            let queued = st.queued.as_ref().is_some_and(|q| q.id == turn_id);
            let t = [st.turn.as_mut(), st.queued.as_mut()]
                .into_iter()
                .flatten()
                .find(|t| t.id == turn_id)?;
            // a restored turn whose message was with the writer had its interrupt written then: not again. A
            // queued turn is interrupted once it is the turn (the agent turn had its own interrupt), not here.
            let was_handed = std::mem::replace(&mut t.handed, true) || queued;
            let (tx, rx) = oneshot::channel();
            w.send(Write {
                msg_id: id,
                bytes: line,
                done: Some(tx),
            })
            .ok()?;
            if t.interrupt_sent.is_some() && !was_handed {
                tracing::info!(turn = %turn_id, "interrupt sent");
                let _ = w.send(Write {
                    msg_id: keeper::new_msg_id(),
                    bytes: control::interrupt(&format!("intr-{}", Uuid::new_v4())),
                    done: None,
                });
            }
            Some(rx)
        });
        let Some(rx) = rx else {
            // killed or exited before the message was handed over: its end is reported already
            self.mark_failed(seq, None);
            self.drop_message(n, seq).await;
            return delivered_ack(self, false);
        };
        if let Err(e) = rx.await.unwrap_or_else(|_| Err("writer gone".into())) {
            tracing::warn!(turn = %turn_id, error = %e, "prompt not delivered");
            self.with(Change::State, |st, fx| {
                if st.live.as_ref().is_some_and(|g| g.n == n && g.why.is_some()) {
                    // the generation is being ended on purpose (a kill, an interrupt's forced end, the memory
                    // guard...) and that is why the write failed: its exit ends the turn with that reason (7.9),
                    // never as an exit nobody asked for
                } else if st.turn.as_ref().is_some_and(|t| t.id == turn_id) {
                    self.end_turn_now(
                        st,
                        fx,
                        Outcome::Failed(error(DataCode::RunExited, e.clone())),
                        false,
                        None,
                    );
                } else if st.queued.as_ref().is_some_and(|t| t.id == turn_id) {
                    st.queued = None;
                    st.pending_inputs = st.pending_inputs.saturating_sub(1);
                    fx.push(Fx::Report(Item::state(
                        turn_id,
                        end_report(ReportedState::Failed, Some(error(DataCode::RunExited, e.clone()))),
                    )));
                }
            });
            self.mark_failed(seq, Some(&e));
            self.drop_message(n, seq).await;
            return self.ack_in(Some(turn_id), seq, err_ack(&e));
        }
        // 6: delivered (the `running` report went with step 5), then the ack
        self.with(Change::State, |st, _| {
            st.delivered.insert(
                seq,
                Delivered {
                    state: "running".into(),
                    ..st.delivered.get(&seq).cloned().unwrap_or_default()
                },
            );
        });
        self.drop_message(n, seq).await;
        delivered_ack(self, true);
    }

    /// The turn an interrupt or kill for `turn_id` means: the current one (any, without an id), or the person's
    /// turn queued behind an agent turn (magpie already has it `running`).
    fn targets(st: &St, turn_id: Option<Uuid>) -> Option<Uuid> {
        if let Some(q) = st.queued.as_ref()
            && turn_id == Some(q.id)
        {
            return Some(q.id);
        }
        st.turn
            .as_ref()
            .filter(|t| turn_id.is_none_or(|id| id == t.id))
            .map(|t| t.id)
    }

    async fn interrupt(&self, seq: u64, turn_id: Option<Uuid>) {
        let target = self.with(Change::State, |st, fx| {
            if turn_id.is_some() && st.starting == turn_id {
                // being prepared: step 5 sees this and writes nothing
                st.cancel_starting = turn_id;
                return turn_id;
            }
            let id = Self::targets(st, turn_id)?;
            let queued = st.queued.as_ref().is_some_and(|q| q.id == id);
            // a queued turn: Claude Code works on the agent turn and takes the person's message next, so the
            // interrupt stops both (the queued one is interrupted again once it is the turn)
            let mut send = false;
            for t in [st.turn.as_mut(), if queued { st.queued.as_mut() } else { None }]
                .into_iter()
                .flatten()
            {
                if t.mark_interrupt() && t.handed {
                    send = true;
                }
            }
            if send {
                tracing::info!(turn = %id, queued, "interrupt sent");
                fx.push(Fx::Stdin(control::interrupt(&format!("intr-{}", Uuid::new_v4()))));
            }
            st.set_run(RunKind::Stopping);
            for a in st.asks.drain(..) {
                fx.push(Fx::Stdin(control::deny(
                    &a.cli_request_id,
                    "The turn was stopped.",
                    a.tool_use_id.as_deref(),
                )));
                fx.push(a.resolved(AskOutcome::Canceled));
            }
            Some(id)
        });
        if let Some(id) = target {
            // the outcome first: the grace kill bounds the wait
            self.wait_until(None, |s| {
                s.turn.as_ref().is_none_or(|t| t.id != id)
                    && s.queued.as_ref().is_none_or(|t| t.id != id)
                    && s.starting != Some(id)
            })
            .await;
        }
        self.ack_in(target, seq, AckResult::default());
    }

    async fn kill(&self, seq: u64, turn_id: Option<Uuid>, reason: KillReason) {
        let watchdog = reason == KillReason::Watchdog;
        let starting = self.with(Change::State, |st, _| {
            let s = turn_id.is_some() && st.starting == turn_id;
            if s {
                st.cancel_starting = turn_id;
            }
            s
        });
        if starting {
            // step 5 either sees the cancel (nothing written) or installs the turn, which is then killed below
            self.wait_until(None, |s| s.starting != turn_id).await;
        }
        let target = {
            let st = lock(&self.st);
            Self::targets(&st, turn_id).and_then(|id| st.live.as_ref().map(|g| (id, g.n)))
        };
        if let Some((id, n)) = target {
            tracing::info!(turn = %id, watchdog, "kill");
            self.kill_gen(n, Why::Kill { watchdog }).await;
        }
        self.ack_in(target.map(|(id, _)| id).or(turn_id), seq, AckResult::default());
    }

    /// `send_message` (a schedule's fire, or a person's message, while a turn runs, the person's or an agent turn):
    /// the message is written to
    /// the turn's generation at once, and Claude Code takes it in at its next tool result or when the model stops, as
    /// a message typed mid-turn.
    /// It never counts as a pending input: taken in mid-turn it is answered by the turn's own `result`; left for after
    /// that `result`, Claude Code answers it in a run of its own, which opens an agent turn as any output with no
    /// pending input does. Acked `delivered: false` when `turn_id` is not the running turn (magpie then
    /// sends it as a new turn); a repeat of a written one (a restarted supervisor) is acked again, never rewritten.
    async fn send_message(&self, seq: u64, turn_id: Option<Uuid>, text: &str) {
        // after the turn's own start_turn, which holds the lane while it writes the person's message
        let _lane = self.turn_lane.lock().await;
        let already = lock(&self.st)
            .delivered
            .get(&seq)
            .is_some_and(|d| d.state == "injected");
        if already {
            return self.ack(
                seq,
                AckResult {
                    delivered: Some(true),
                    ..AckResult::default()
                },
            );
        }
        let message = prompt::user_line(&Value::String(text.to_owned()), &rfc3339(now()));
        let lane = self.with(Change::State, |st, fx| {
            let t = st.turn.as_ref()?;
            let running = turn_id == Some(t.id)
                && t.handed
                && t.interrupt_sent.is_none()
                && st.live.is_some()
                && st.release.is_none();
            if !running {
                return None;
            }
            st.delivered.insert(
                seq,
                Delivered {
                    state: "injected".into(),
                    turn_id: Some(t.id.to_string()),
                    ..Delivered::default()
                },
            );
            fx.push(Fx::Stdin(message));
            Some(t.id)
        });
        match lane {
            Some(l) => {
                tracing::info!(turn = %l, "a message was written into the running turn");
                self.ack_in(
                    Some(l),
                    seq,
                    AckResult {
                        delivered: Some(true),
                        ..AckResult::default()
                    },
                );
            }
            None => self.ack(
                seq,
                AckResult {
                    delivered: Some(false),
                    ..AckResult::default()
                },
            ),
        }
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
            fx.push(ask.resolved(AskOutcome::Answered));
            Some(ask.lane)
        });
        match found {
            Some(lane) => self.ack_in(lane, seq, AckResult::default()),
            None => self.ack(seq, err_ack("no_such_ask")),
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
        // a turn whose end report is still on its way (waiting for its lines, 8.2, or for magpie) is still the turn:
        // magpie must neither take it for one the sandbox forgot nor end it before its last row (magpie 8.5)
        let ending = st.ending.first().filter(|_| st.turn.is_none());
        // a parked turn whose report is on its way is still the turn, so magpie never takes it for one the sandbox
        // forgot (DECISIONS 17)
        let parking = st.parking.filter(|_| st.turn.is_none() && ending.is_none());
        Heartbeat {
            turn_id: st.turn.as_ref().map(|t| t.id).or(ending.map(|e| e.id)).or(parking),
            state: match (ending, st.run) {
                _ if parking.is_some() => HeartbeatState::Running,
                (Some(e), _) if e.stopping => HeartbeatState::Stopping,
                (Some(_), _) | (None, RunKind::Running | RunKind::Starting) => HeartbeatState::Running,
                (None, RunKind::Stopping) => HeartbeatState::Stopping,
                (None, RunKind::Held) => HeartbeatState::Held,
                (None, RunKind::Idle) => HeartbeatState::Idle,
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

    /// Restores the state of a previous supervisor of this boot (4.3 step 8), before the stdout tail restarts: the
    /// turns `run.json` holds (a person's turn is saved together with its message's `written` record) are the
    /// turns again, so lines read again or answered meanwhile are credited to them; their `running` (or agent-turn)
    /// report is sent again, idempotent, in case the previous supervisor's queue still held it.
    #[allow(clippy::too_many_lines)] // one field of run.json after another
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
                .map(|id| {
                    let mut t = Turn::new(
                        id,
                        match prior.run.origin.as_deref() {
                            Some("agent") => TurnOrigin::Agent,
                            _ => TurnOrigin::Person,
                        },
                    );
                    t.uuids.clone_from(&prior.run.pending_uuids);
                    if prior.run.interrupt_sent_at.is_some() {
                        // the grace goes on from where it was (the monotonic clock does not survive the process)
                        t.interrupt_sent = Some(now());
                        t.interrupt_mono = Some(ago(prior.run.interrupt_elapsed_ms.unwrap_or(0)));
                    }
                    t.handed = prior.run.handed.unwrap_or(true);
                    t
                });
            let queued = prior
                .run
                .queued_turn_id
                .as_deref()
                .and_then(|t| Uuid::parse_str(t).ok())
                .map(|id| {
                    let mut t = Turn::new(id, TurnOrigin::Person);
                    if let Some(ms) = prior.run.queued_interrupt_elapsed_ms {
                        t.interrupt_sent = Some(now());
                        t.interrupt_mono = Some(ago(ms));
                    }
                    t.handed = prior.run.queued_handed.unwrap_or(true);
                    t
                });
            // ended turns whose end report the previous supervisor had not got answered: sent again (idempotent)
            for e in &prior.ending {
                let (Ok(id), Ok(report)) = (
                    Uuid::parse_str(&e.turn_id),
                    serde_json::from_value::<TurnStateReport>(e.report.clone()),
                ) else {
                    continue;
                };
                let ending = Ending {
                    id,
                    report,
                    uuids: e.uuids.clone(),
                    generation: e.generation,
                    stopping: e.stopping,
                    last: e.last.clone(),
                };
                fx.push(Fx::Report(self.end_item(&ending)));
                st.ending.push(ending);
            }
            if let (Some((n, pid)), Some(g)) = (alive, prior.generation.as_ref()) {
                let sid = g.session_id.clone().unwrap_or_default();
                st.live = Some(Gen {
                    n,
                    pid,
                    since: self.ctx().map_or(0, |c| c.exits.origin()),
                    stdout: PathBuf::from(&g.stdout),
                    stderr: PathBuf::from(&g.stdout).with_file_name("stderr.log"),
                    offset: g.stdout_offset,
                    session_id: sid.clone(),
                    initialized: g.initialized,
                    started_at: OffsetDateTime::parse(&g.started_at, &time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| now()),
                    why: None,
                    // replaced when the writer starts
                    writer: mpsc::unbounded_channel().0,
                    // the managed files it started with (a run.json without it: those on disk)
                    etag: if g.etag.is_empty() {
                        st.etag.clone()
                    } else {
                        g.etag.clone()
                    },
                    model: g.model.clone(),
                    picked: g.picked,
                    thinking: Thinking::Unknown,
                });
                // the model the conversation was on (the next start_turn says it again)
                st.model = g.picked.then(|| g.model.clone());
                self.reporter.generation_ended(n.saturating_sub(1));
                for t in [turn.as_ref(), queued.as_ref()].into_iter().flatten() {
                    fx.push(Fx::Report(match t.origin {
                        TurnOrigin::Agent => Item::AgentTurn(AgentTurnRequest {
                            turn_id: t.id,
                            origin: magpie_api::turns::TurnOrigin::Agent,
                            at: wire(now()),
                        }),
                        _ => Item::state(t.id, running_report(&sid)),
                    }));
                }
                let lane = turn.as_ref().map(|t| t.id);
                st.turn = turn;
                st.queued = queued;
                st.pending_inputs = prior.run.pending_inputs;
                st.images = prior.run.images;
                st.images_session = prior.generation.as_ref().and_then(|g| g.session_id.clone());
                st.open_tools = prior.run.open_tools.iter().cloned().collect();
                st.tracker = Tracker::from_saved(&prior.tasks);
                st.asks = prior
                    .asks
                    .iter()
                    .map(|a| Ask {
                        ask_id: a.ask_id.clone(),
                        cli_request_id: a.cli_request_id.clone(),
                        tool_name: a.tool_name.clone(),
                        tool_use_id: a.tool_use_id.clone(),
                        since: OffsetDateTime::parse(&a.since, &time::format_description::well_known::Rfc3339)
                            .unwrap_or_else(|_| now()),
                        // the 30 min timeout goes on from where it was
                        since_mono: ago(a.elapsed_ms.unwrap_or(0)),
                        lane: a.lane.as_deref().and_then(|l| Uuid::parse_str(l).ok()).or(lane),
                    })
                    .collect();
                st.run = RunKind::parse(&prior.run.state);
                if st.run == RunKind::Starting {
                    st.run = RunKind::Idle;
                }
                st.idle_since = (st.run == RunKind::Idle).then(now);
            } else {
                // the generation died while no supervisor watched: its turn failed (7.9), and nothing it wrote is
                // waited for any more
                if let Some(g) = prior.generation.as_ref() {
                    self.reporter.generation_ended(g.n);
                }
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

/// What step 5 of `start_turn` found.
enum Prepared {
    /// The turn is installed: write the message.
    Write,
    /// Interrupted, killed or released while it was prepared.
    Stopped,
    /// The generation exited.
    Gone,
    /// A replayed turn that already ended.
    Answered,
}

fn running_report(sid: &str) -> TurnStateReport {
    TurnStateReport {
        provider_session_id: Some(sid.to_owned()),
        transcript_path: Some(format!("projects/-workspace/{sid}.jsonl")),
        ..end_report(ReportedState::Running, None)
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
            // 4: the managed files (at every start: idempotent, and the descriptor may have changed); what they held
            // before is known from run.json, so a write that fails now leaves the old etag standing
            if let Some(p) = &ctx.prior {
                lock(&self.st).etag.clone_from(&p.managed_etag);
            }
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
                st.live.as_mut().map(|g| {
                    let (tx, rx) = mpsc::unbounded_channel();
                    g.writer = tx;
                    (
                        g.n,
                        g.pid,
                        g.since,
                        g.stdout.clone(),
                        g.stderr.clone(),
                        g.offset,
                        g.initialized,
                        rx,
                    )
                })
            };
            let mut stale = None;
            if let Some((n, pid, since, out, err, offset, initialized, rx)) = resumed {
                tracing::info!(gen = n, pid, offset, initialized, "claude re-attached");
                let turn = lock(&self.st).turn.as_ref().map(|t| t.id);
                self.transcript.set_turn(turn);
                self.start_gen_tasks(n, pid, since, out, err, offset, rx);
                // one still initialising when the previous supervisor went (its answer may be read already): ended
                // and started again, never left beside a new one
                stale = (!initialized).then_some(n);
            }
            let me = self.arc();
            me.go(me.reporter.clone().run());
            me.go(me.reporter.clone().run_tasks());
            me.go(me.reporter.clone().run_live());
            me.go(me.clone().tick_loop());
            me.go(me.clone().heartbeat_loop());
            self.start_poll();
            // pre-spawn (4.3 step 8) so the first turn does not pay for start-up and resume
            if alive.is_none() || stale.is_some() {
                let session = match &ctx.lease.provider_session_id {
                    Some(s) => Session::Resume(s.clone()),
                    None => Session::New(conversation.to_string()),
                };
                let me = self.arc();
                self.go(async move {
                    if let Some(n) = stale {
                        tracing::warn!(gen = n, "a generation found initialising after a restart: ended");
                        let _gen = me.gen_lane.lock().await;
                        me.kill_gen(n, Why::Stale).await;
                    }
                    if let Err(GenError::Failed(e)) = me.ensure_gen(&session, None).await {
                        tracing::warn!(error = %e, first, "pre-spawn failed");
                    }
                });
            } else if lock(&self.st).run == RunKind::Idle {
                // idle at the restart: an idle point (a configuration changed meanwhile is applied now, 6.4)
                let me = self.arc();
                self.go(async move { me.idle_point().await });
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

    fn restored(&self) -> bool {
        // set in `attach` right before `restore` runs, with no await between them
        self.ctx.get().is_some()
    }

    fn end_turn(&self, reason: String, deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let open = self.with(Change::State, |st, fx| {
                st.release = Some(reason.clone());
                // a turn still being prepared is ended by step 5 (failed sandbox.lost) and spawns nothing more
                if let Some(s) = st.starting {
                    st.cancel_starting = Some(s);
                }
                let mut send = false;
                for t in [st.turn.as_mut(), st.queued.as_mut()].into_iter().flatten() {
                    if t.mark_interrupt() && t.handed {
                        send = true;
                    }
                }
                if send {
                    fx.push(Fx::Stdin(control::interrupt(&format!("intr-{}", Uuid::new_v4()))));
                }
                let open = st.turn.is_some() || st.starting.is_some();
                if st.turn.is_some() {
                    st.set_run(RunKind::Stopping);
                }
                for id in st.tracker.event_ids() {
                    fx.push(Fx::Stdin(control::stop_task(&format!("stop-{}", Uuid::new_v4()), &id)));
                }
                open
            });
            if open {
                let ended = self
                    .wait_until(Some(deadline), |s| {
                        s.turn.is_none() && s.queued.is_none() && s.starting.is_none()
                    })
                    .await;
                if !ended {
                    let n = lock(&self.st).live.as_ref().map(|g| g.n);
                    if let Some(n) = n {
                        tracing::warn!("the interrupt did not end the turn in time: killed");
                        self.kill_gen(n, Why::Release).await;
                    }
                    // no generation (or it would not die), or a start still preparing: reported lost all the same
                    self.with(Change::State, |st, fx| {
                        while st.turn.is_some() {
                            let o = Outcome::Failed(error(DataCode::SandboxLost, format!("released: {reason}")));
                            self.end_turn_now(st, fx, o, false, None);
                        }
                        if let Some(s) = st.starting {
                            fx.push(Fx::Report(Item::state(
                                s,
                                end_report(
                                    ReportedState::Failed,
                                    Some(error(DataCode::SandboxLost, format!("released: {reason}"))),
                                ),
                            )));
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
            // a start_turn or spawn in progress finishes (it refuses to start anything now) before the generation
            // is ended, so nothing it spawned outlives the release
            let _turn = tokio::time::timeout_at(deadline, self.turn_lane.lock()).await;
            let _gen = tokio::time::timeout_at(deadline, self.gen_lane.lock()).await;
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

    fn park(&self, reason: &str) -> bool {
        let me = self.me.clone();
        self.with(Change::State, |st, fx| {
            if !st.asking() {
                return false;
            }
            let Some(turn) = st.turn.take() else {
                return false;
            };
            let worked = st.worked(&turn);
            let asks: Vec<String> = st.asks.drain(..).map(|a| a.ask_id).collect();
            // the wait goes on in magpie, which keeps the questions; nothing of it is this run's any more
            st.asks_since = None;
            st.open_tools.clear();
            st.pending_inputs = 0;
            st.release = Some(reason.to_owned());
            if let Some(g) = st.live.as_mut() {
                g.why = Some(Why::Park);
            }
            st.parking = Some(turn.id);
            tracing::info!(turn = %turn.id, asks = ?asks, ms = worked.as_millis(), reason, "turn parked: it waits only on its questions, the sandbox goes");
            let report = TurnStateReport {
                state: ReportedState::Parked,
                at: wire(now()),
                provider_session_id: None,
                transcript_path: None,
                exit_code: None,
                error: None,
                held: None,
                tasks: None,
                last_uuid: None,
                usage: turn.usage,
                duration_ms: Some(u64::try_from(worked.as_millis()).unwrap_or(u64::MAX)),
            };
            let id = turn.id;
            fx.push(Fx::Report(Item::State {
                turn: id,
                report,
                wait: Wait::Nothing,
                done: Some(Box::new(move |_| {
                    if let Some(d) = me.upgrade() {
                        d.with(Change::State, |st, _| {
                            if st.parking == Some(id) {
                                st.parking = None;
                            }
                        });
                    }
                })),
            }));
            fx.push(Fx::SetTurn(None));
            st.set_run(RunKind::Idle);
            true
        })
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
        Box::pin(async move { crate::sent_files::send(&self.reporter, req, fds).await })
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

/// What the model is told when nobody answered its question within [`ASK_TIMEOUT`] (Deyao, 2026-11-05): a safety
/// limit only, since a question that waits never holds a sandbox (its turn is parked, DECISIONS 17); the model may
/// ask again.
fn ask_timeout_answer() -> String {
    format!(
        "User has not answered in {}, the session has timed out, ask again if needed.",
        span(ASK_TIMEOUT)
    )
}

/// A duration in its largest whole unit: "7 days", "30 minutes", "1 hour".
fn span(d: Duration) -> String {
    let s = d.as_secs();
    let (n, unit) = if s >= 86_400 && s.is_multiple_of(86_400) {
        (s / 86_400, "day")
    } else if s >= 3_600 && s.is_multiple_of(3_600) {
        (s / 3_600, "hour")
    } else if s >= 60 && s.is_multiple_of(60) {
        (s / 60, "minute")
    } else {
        (s, "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}
