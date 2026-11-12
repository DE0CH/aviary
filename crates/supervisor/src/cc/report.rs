//! Reports to magpie (supervisor 11): turn states, agent-started turns, asks and command acks, task events in
//! 250 ms batches. Reports are ordered per lane: one lane per turn (its agent-turn open, its states, the asks opened
//! during it and the acks of the commands about it, so `running` precedes the end and an interrupt's ack follows
//! the outcome) and one lane for everything else; lanes do not wait for each other, so a turn whose end waits for
//! its transcript lines (8.2) holds back nothing but itself. Every report is retried on 5xx, connection errors,
//! 429, `409 transcript_pending` and `409 conversation_migrating` (0.5 s doubling to 10 s, or magpie's
//! `retryAfterMs`) for as long as the lease is valid; a refusal that no retry can cure is logged as an error and
//! counted.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use magpie_api::client::ClientError;
use magpie_api::errors::ErrorCode;
use magpie_api::live::{FrameOp, LiveFrame, LiveFramesRequest};
use magpie_api::sandbox::{
    AckRequest, AckResult, AgentTurnRequest, AskReport, AskResolvedReport, ReportError, ReportedState, TaskReportEvent,
    TasksReport, TurnStateReport,
};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use uuid::Uuid;

use crate::cc::{Stored, Transcript};
use crate::limits::{REPORT_RETRY_FIRST, REPORT_RETRY_MAX};

/// How often task events are sent.
pub const TASKS_BATCH: Duration = Duration::from_millis(250);
/// How often live frames are sent (8.3): often enough to read as typing, seldom enough to be one request per breath.
pub const LIVE_BATCH: Duration = Duration::from_millis(120);
/// Frames waiting for a batch beyond this are dropped (magpie is slow or away): live text is never a backlog.
const LIVE_PENDING_MAX: usize = 4_000;
/// The longest `retryAfterMs` honoured for one wait.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(30);
/// The longest an end report waits for its uuids while the generation lives (a uuid stdout showed that Claude Code
/// never writes to the main transcript would otherwise hold the turn's lane for ever). Well above the stuck rule's
/// 10 min, so a slow magpie is never cut short.
pub const UUID_WAIT_CAP: Duration = Duration::from_mins(30);
/// After the generation exited and the writes in flight are answered: how long to look whether the uuids are
/// stored after all.
const UUID_LAST_LOOK: Duration = Duration::from_secs(2);

/// Called once magpie answered an item (`true`) or it was given up (`false`: the lease was lost).
pub type Done = Box<dyn FnOnce(bool) + Send>;

/// What an end report waits for before it is sent (8.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Wait {
    /// Nothing (a turn that never ran, a `running` report).
    #[default]
    Nothing,
    /// magpie has acknowledged main-transcript writes holding these uuids, written by `generation`. When that
    /// generation exits first (a kill, a release, a crash after the result), the writes in flight are answered and
    /// the report goes without `lastUuid` unless the lines are stored by then: lines a dead generation never wrote
    /// cannot be waited for. [`UUID_WAIT_CAP`] bounds the wait as a last resort (logged as an error).
    Uuids { uuids: Vec<String>, generation: u32 },
    /// The generation is gone: the filesystem writes already in flight are answered.
    Settle,
}

/// One report.
// a state report is the big one (it may carry a generation's slash commands); a lane holds a handful of items
#[allow(clippy::large_enum_variant)]
pub enum Item {
    /// `POST /v1/sandbox/turns/{turnId}/state`, after `wait`.
    State {
        turn: Uuid,
        report: TurnStateReport,
        wait: Wait,
        done: Option<Done>,
    },
    AgentTurn(AgentTurnRequest),
    AskOpen(AskReport),
    AskResolved(String, AskResolvedReport),
    Ack {
        seq: u64,
        result: AckResult,
        done: Option<Done>,
    },
}

impl Item {
    /// A state report that waits for nothing.
    #[must_use]
    pub fn state(turn: Uuid, report: TurnStateReport) -> Self {
        Item::State {
            turn,
            report,
            wait: Wait::Nothing,
            done: None,
        }
    }

    /// The lane an item goes in when none is named: its turn's, or the shared one.
    fn lane(&self) -> Option<Uuid> {
        match self {
            Item::State { turn, .. } => Some(*turn),
            Item::AgentTurn(r) => Some(r.turn_id),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::State { turn, report, .. } => write!(f, "state {turn} {:?}", report.state),
            Item::AgentTurn(r) => write!(f, "agent turn {}", r.turn_id),
            Item::AskOpen(r) => write!(f, "ask {}", r.ask_id),
            Item::AskResolved(id, _) => write!(f, "ask resolved {id}"),
            Item::Ack { seq, .. } => write!(f, "ack {seq}"),
        }
    }
}

/// What an attempt came to.
enum Attempt {
    Done,
    /// Retried, after at least the given wait when magpie named one.
    Retry(String, Option<Duration>),
    /// The lease is gone (or the call can never succeed): stop with this item.
    GiveUp(String),
}

/// The wait magpie asked for (`retryAfterMs`, or `retry-after`), at most 30 s.
pub(crate) fn retry_after(e: &ClientError) -> Option<Duration> {
    let r = e.api()?;
    r.retry_after_ms
        .map(Duration::from_millis)
        .or(r.retry_after.map(Duration::from_secs))
        .map(|d| d.min(RETRY_AFTER_MAX))
}

/// Whether a failed report is worth another attempt (the reporter's own rule; a lost lease is not).
pub(crate) fn retryable(e: &ClientError) -> bool {
    matches!(classify(e), Attempt::Retry(..))
}

fn classify(e: &ClientError) -> Attempt {
    match e.code() {
        Some(ErrorCode::LeaseLost) => Attempt::GiveUp("lease lost".into()),
        Some(ErrorCode::TranscriptPending | ErrorCode::ConversationMigrating | ErrorCode::RateLimited) => {
            Attempt::Retry(e.to_string(), retry_after(e))
        }
        _ if e.is_retryable() => Attempt::Retry(e.to_string(), retry_after(e)),
        _ => match e.api() {
            Some(r) if r.status >= 500 || r.status == 429 => Attempt::Retry(e.to_string(), retry_after(e)),
            _ => Attempt::GiveUp(e.to_string()),
        },
    }
}

/// One lane's queue.
#[derive(Default)]
struct Lane {
    items: VecDeque<Item>,
    /// A task is draining it.
    active: bool,
}

/// The reporter.
/// A frame with nothing set (`n` and the rest are filled in by who makes it).
#[must_use]
pub fn blank_frame() -> LiveFrame {
    LiveFrame {
        n: 0,
        message_id: None,
        index: None,
        op: FrameOp::Status,
        block: None,
        name: None,
        offset: None,
        text: None,
        parent_tool_use_id: None,
        status: None,
        attempt: None,
        retry_in_ms: None,
        at: None,
    }
}

/// The live frames read since the last batch.
#[derive(Default)]
struct LiveQueue {
    frames: Vec<(Uuid, LiveFrame)>,
    /// Frames were dropped before they were sent.
    gap: bool,
}

pub struct Reporter {
    magpie: magpie_api::client::Client,
    lease: Mutex<Option<Uuid>>,
    transcript: Arc<dyn Transcript>,
    lanes: Mutex<HashMap<Option<Uuid>, Lane>>,
    wake: Notify,
    /// Reports given up for a reason other than the lease (each one logged as an error).
    given_up: AtomicU64,
    /// Items queued and not yet answered (the queue plus the one in flight).
    pending: watch::Sender<usize>,
    tasks: Mutex<Vec<TaskReportEvent>>,
    tasks_pending: watch::Sender<usize>,
    live: Mutex<LiveQueue>,
    /// The highest generation that has exited.
    gone: watch::Sender<u32>,
    /// Agent-started turns magpie refused (409 `bad_transition`: a person's turn was active, 7.5's race).
    refused: Mutex<HashSet<Uuid>>,
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reporter")
            .field("pending", &*self.pending.borrow())
            .finish_non_exhaustive()
    }
}

impl Reporter {
    #[must_use]
    pub fn new(magpie: magpie_api::client::Client, transcript: Arc<dyn Transcript>) -> Arc<Self> {
        Arc::new(Self {
            magpie,
            lease: Mutex::new(None),
            transcript,
            lanes: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            given_up: AtomicU64::new(0),
            pending: watch::channel(0).0,
            tasks: Mutex::new(Vec::new()),
            tasks_pending: watch::channel(0).0,
            live: Mutex::new(LiveQueue::default()),
            gone: watch::channel(0).0,
            refused: Mutex::new(HashSet::new()),
        })
    }

    /// Generation `n` exited (every earlier one is gone too): end reports waiting for its lines stop waiting.
    pub fn generation_ended(&self, n: u32) {
        self.gone.send_if_modified(|g| {
            let later = n > *g;
            if later {
                *g = n;
            }
            later
        });
    }

    /// Waits for `uuids` (8.2); `(stuck, keep lastUuid)`.
    async fn wait_uuids(&self, turn: Uuid, uuids: Vec<String>, generation: u32) -> (bool, bool) {
        if uuids.is_empty() {
            return (false, false);
        }
        let mut gone = self.gone.subscribe();
        let waited = tokio::select! {
            biased;
            s = self.transcript.wait_stored(uuids.clone()) => Some(s),
            _ = gone.wait_for(|g| *g >= generation) => None,
            () = tokio::time::sleep(UUID_WAIT_CAP) => {
                tracing::error!(turn = %turn, generation, uuids = uuids.len(), "a turn's lines were not stored in 30 min: its end is sent without lastUuid");
                return (false, false);
            }
        };
        match waited {
            Some(Stored::All) => (false, true),
            Some(Stored::Stuck) => (true, false),
            None => {
                // the generation is gone: what it wrote is answered first, then one last look
                self.transcript.settle().await;
                let last = tokio::time::timeout(UUID_LAST_LOOK, self.transcript.wait_stored(uuids)).await;
                let stored = matches!(last, Ok(Stored::All));
                tracing::info!(turn = %turn, generation, stored, "the generation exited while the turn's end waited for its lines");
                (false, stored)
            }
        }
    }

    /// The lease the reports carry.
    pub fn set_lease(&self, lease: Uuid) {
        *self.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(lease);
    }

    /// The lease now: the filesystem's when it re-took one, else the start's.
    #[must_use]
    pub fn lease(&self) -> Option<Uuid> {
        self.transcript
            .lease()
            .or(*self.lease.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The calls with the current lease.
    #[must_use]
    pub fn calls(&self) -> magpie_api::client::SandboxCalls<'_> {
        self.magpie.sandbox(self.lease())
    }

    /// Queues a report in its own lane (its turn's, or the shared one).
    pub fn push(&self, item: Item) {
        let lane = item.lane();
        self.push_in(lane, item);
    }

    /// Queues a report in `lane` (sent after every earlier report of that lane).
    pub fn push_in(&self, lane: Option<Uuid>, item: Item) {
        self.pending.send_modify(|n| *n += 1);
        let idle = {
            let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
            let l = lanes.entry(lane).or_default();
            l.items.push_back(item);
            !l.active
        };
        if idle {
            self.wake.notify_one();
        }
    }

    /// How many reports were given up for a reason other than a lost lease.
    #[must_use]
    pub fn given_up(&self) -> u64 {
        self.given_up.load(Ordering::SeqCst)
    }

    /// Queues a live frame of `turn` for the next batch (its `n` is given when it is sent).
    pub fn live_frame(&self, turn: Uuid, frame: LiveFrame) {
        let mut q = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if q.frames.len() >= LIVE_PENDING_MAX {
            q.frames.clear();
            q.gap = true;
        }
        q.frames.push((turn, frame));
    }

    /// The live frames' task (8.3): every [`LIVE_BATCH`], the frames read since go to magpie in one request per turn,
    /// numbered per turn from 1. Live text is best effort: a batch magpie does not take (the turn ended, magpie is
    /// away) is dropped, never retried, and the next batch of that turn starts with a `gap` frame, so magpie sends
    /// the open blocks whole again.
    pub async fn run_live(self: Arc<Self>) {
        let mut turn: Option<Uuid> = None;
        let mut n = 0u64;
        let mut gap = false;
        loop {
            tokio::time::sleep(LIVE_BATCH).await;
            let (frames, dropped) = {
                let mut q = self.live.lock().unwrap_or_else(PoisonError::into_inner);
                (std::mem::take(&mut q.frames), std::mem::take(&mut q.gap))
            };
            gap |= dropped;
            let mut rest = frames.into_iter().peekable();
            while let Some((t, first)) = rest.next() {
                if turn != Some(t) {
                    (turn, n, gap) = (Some(t), 0, false);
                }
                let mut batch = Vec::new();
                if gap {
                    n += 1;
                    batch.push(LiveFrame {
                        n,
                        op: FrameOp::Gap,
                        ..blank_frame()
                    });
                }
                let mut next = Some(first);
                while let Some(mut f) = next {
                    n += 1;
                    f.n = n;
                    batch.push(f);
                    next = rest.next_if(|(u, _)| *u == t).map(|(_, f)| f);
                }
                gap = match self.calls().live_frames(t, &LiveFramesRequest { frames: batch }).await {
                    Ok(()) => false,
                    Err(e) => {
                        tracing::debug!(turn = %t, error = %e, "live frames not taken: dropped");
                        true
                    }
                };
            }
        }
    }

    /// Queues a task event for the next batch.
    pub fn task_event(&self, ev: TaskReportEvent) {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
        self.tasks_pending.send_modify(|n| *n += 1);
    }

    /// Waits until every report queued so far is answered (or given up), by `deadline`; `false` at the deadline.
    pub async fn drain(&self, deadline: Instant) -> bool {
        let mut a = self.pending.subscribe();
        let mut b = self.tasks_pending.subscribe();
        tokio::time::timeout_at(deadline, async {
            let _ = a.wait_for(|n| *n == 0).await;
            let _ = b.wait_for(|n| *n == 0).await;
        })
        .await
        .is_ok()
    }

    async fn retry<F, Fut>(&self, what: &str, mut f: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Attempt>,
    {
        let mut delay = REPORT_RETRY_FIRST;
        let mut logged = false;
        loop {
            match f().await {
                Attempt::Done => return true,
                Attempt::GiveUp(why) => {
                    if why == "lease lost" {
                        tracing::warn!(report = what, why = %why, "report given up");
                    } else {
                        let n = self.given_up.fetch_add(1, Ordering::SeqCst) + 1;
                        tracing::error!(event = "report_given_up", report = what, why = %why, given_up = n, "a report magpie refused for good was dropped");
                    }
                    return false;
                }
                Attempt::Retry(why, after) => {
                    if !logged {
                        tracing::info!(report = what, why = %why, "magpie unavailable: report retried");
                        logged = true;
                    }
                    tokio::time::sleep(after.map_or(delay, |a| a.max(delay))).await;
                    delay = (delay * 2).min(REPORT_RETRY_MAX);
                }
            }
        }
    }

    /// Whether magpie refused `turn` as an agent turn.
    fn refused(&self, turn: Uuid) -> bool {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&turn)
    }

    fn refuse(&self, turn: Uuid) {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner).insert(turn);
    }

    async fn send(&self, item: Item) {
        let what = format!("{item:?}");
        match item {
            Item::State {
                turn,
                mut report,
                wait,
                done,
            } => {
                let stuck = match wait {
                    Wait::Nothing => false,
                    Wait::Uuids { uuids, generation } => {
                        let (stuck, keep) = self.wait_uuids(turn, uuids, generation).await;
                        if !keep {
                            report.last_uuid = None;
                        }
                        stuck
                    }
                    Wait::Settle => {
                        self.transcript.settle().await;
                        false
                    }
                };
                if stuck {
                    report.state = ReportedState::Failed;
                    report.error = Some(ReportError {
                        code: magpie_api::errors::DataCode::CcWriteStuck,
                        message: "a transcript write kept failing for 10 min while magpie answered".into(),
                    });
                    report.last_uuid = None;
                }
                let refused = self.refused(turn);
                let ok = self
                    .retry(&what, || async {
                        match self.calls().turn_state(turn, &report).await {
                            Ok(()) => Attempt::Done,
                            Err(e) if e.code() == Some(&ErrorCode::BadTransition) => {
                                // a replay of what magpie has, or magpie ended the turn first: final either way
                                tracing::info!(turn = %turn, error = %e, "turn state already settled");
                                Attempt::Done
                            }
                            Err(e) if refused && e.code() == Some(&ErrorCode::NotFound) => {
                                // an agent turn magpie refused and never opened: its lines went to magpie's active
                                // turn (magpie 6.3), and there is nothing to end
                                tracing::info!(turn = %turn, "a refused agent turn's report: nothing to settle");
                                Attempt::Done
                            }
                            Err(e) => classify(&e),
                        }
                    })
                    .await;
                if let Some(d) = done {
                    d(ok);
                }
            }
            Item::AgentTurn(r) => {
                self.retry(&what, || async {
                    match self.calls().agent_turn(&r).await {
                        Ok(_) => Attempt::Done,
                        Err(e) if e.code() == Some(&ErrorCode::BadTransition) => {
                            // a person's turn is active in magpie (7.5's race): magpie credits this turn's lines to
                            // that one; the refusal is final, and this turn's own reports have nothing to settle
                            tracing::info!(turn = %r.turn_id, error = %e, "agent turn refused: another turn is active");
                            self.refuse(r.turn_id);
                            Attempt::Done
                        }
                        Err(e) => classify(&e),
                    }
                })
                .await;
            }
            Item::AskOpen(r) => {
                self.retry(&what, || async {
                    match self.calls().ask_open(&r).await {
                        Ok(()) => Attempt::Done,
                        Err(e) => classify(&e),
                    }
                })
                .await;
            }
            Item::AskResolved(id, r) => {
                self.retry(&what, || async {
                    match self.calls().ask_resolved(&id, &r).await {
                        Ok(()) => Attempt::Done,
                        Err(e) => classify(&e),
                    }
                })
                .await;
            }
            Item::Ack { seq, result, done } => {
                let req = AckRequest { result };
                let ok = self
                    .retry(&what, || async {
                        match self.calls().ack_command(seq, &req).await {
                            Ok(()) => Attempt::Done,
                            Err(e) => classify(&e),
                        }
                    })
                    .await;
                if let Some(d) = done {
                    d(ok);
                }
            }
        }
    }

    /// The reporter's task: one task per lane with reports, each sending its lane's reports in order (ending the
    /// reporter ends them all).
    pub async fn run(self: Arc<Self>) {
        let mut set = tokio::task::JoinSet::new();
        loop {
            let start: Vec<Option<Uuid>> = {
                let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
                lanes
                    .iter_mut()
                    .filter(|(_, l)| !l.active && !l.items.is_empty())
                    .map(|(k, l)| {
                        l.active = true;
                        *k
                    })
                    .collect()
            };
            for k in start {
                set.spawn(self.clone().lane(k));
            }
            tokio::select! {
                () = self.wake.notified() => {}
                Some(_) = set.join_next(), if !set.is_empty() => {}
            }
        }
    }

    async fn lane(self: Arc<Self>, key: Option<Uuid>) {
        loop {
            let next = {
                let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(i) = lanes.get_mut(&key).and_then(|l| l.items.pop_front()) else {
                    lanes.remove(&key);
                    return;
                };
                i
            };
            self.send(next).await;
            self.pending.send_modify(|n| *n = n.saturating_sub(1));
        }
    }

    /// The task-event batches' task.
    pub async fn run_tasks(self: Arc<Self>) {
        loop {
            tokio::time::sleep(TASKS_BATCH).await;
            let batch = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
            if batch.is_empty() {
                continue;
            }
            let n = batch.len();
            let req = TasksReport { events: batch };
            self.retry("tasks", || async {
                match self.calls().tasks(&req).await {
                    Ok(()) => Attempt::Done,
                    Err(e) => classify(&e),
                }
            })
            .await;
            self.tasks_pending.send_modify(|p| *p = p.saturating_sub(n));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn refusal(status: u16, code: ErrorCode) -> ClientError {
        ClientError::Api(magpie_api::errors::Refusal {
            status,
            retry_after: None,
            code,
            message: String::new(),
            retry_after_ms: None,
            extra: serde_json::Map::new(),
        })
    }

    #[test]
    fn what_is_retried() {
        assert!(matches!(
            classify(&refusal(503, ErrorCode::Unavailable)),
            Attempt::Retry(..)
        ));
        assert!(matches!(
            classify(&refusal(409, ErrorCode::TranscriptPending)),
            Attempt::Retry(..)
        ));
        assert!(matches!(
            classify(&refusal(500, ErrorCode::Internal)),
            Attempt::Retry(..)
        ));
        assert!(matches!(
            classify(&refusal(429, ErrorCode::RateLimited)),
            Attempt::Retry(..)
        ));
        let mut m = refusal(409, ErrorCode::ConversationMigrating);
        if let ClientError::Api(r) = &mut m {
            r.retry_after_ms = Some(1500);
        }
        assert!(matches!(classify(&m), Attempt::Retry(_, Some(d)) if d == Duration::from_millis(1500)));
        assert!(matches!(
            classify(&refusal(409, ErrorCode::LeaseLost)),
            Attempt::GiveUp(_)
        ));
        assert!(matches!(
            classify(&refusal(400, ErrorCode::BadRequest)),
            Attempt::GiveUp(_)
        ));
    }

    /// Real reports (an agent turn, its states, an ask, an ack) through fake magpie answering 503 to everything for
    /// 20 min (paused time): none is dropped, each arrives exactly once, in its lane's order.
    #[tokio::test(start_paused = true)]
    #[allow(clippy::too_many_lines)] // the outage, then every report checked
    async fn real_reports_survive_a_20_minute_fake_magpie_outage() {
        use magpie_api::sandbox::{AgentTurnRequest, CommandBody};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let magpie = aviary_fakes::magpie::FakeMagpie::unguarded();
        let served = magpie.serve().await.unwrap();
        let cid = Uuid::now_v7();
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in [
            ("aviary-sandbox-id", "sbx-outage".to_owned()),
            ("aviary-sandbox-conversation", cid.to_string()),
            ("aviary-sandbox-person", "staff@example.org".to_owned()),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let http = reqwest::Client::builder().default_headers(h).build().unwrap();
        let client = magpie_api::client::Client::new(http, served.url(), "t");
        let lease = client.sandbox(None).lease().await.unwrap();
        let r = Reporter::new(client, Arc::new(crate::cc::Stored0));
        r.set_lease(lease.lease_id);
        let seq = magpie.push_command(cid, None, CommandBody::StopTasks(magpie_api::Empty {}));
        let fault = magpie
            .faults()
            .everywhere(aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 });
        let runner = tokio::spawn(r.clone().run());
        let turn = Uuid::now_v7();
        let at = magpie_api::Timestamp::now();
        let state = |state| TurnStateReport {
            state,
            at,
            provider_session_id: None,
            transcript_path: None,
            exit_code: None,
            error: None,
            held: None,
            tasks: None,
            last_uuid: None,
            usage: None,
            duration_ms: None,
            commands: None,
        };
        r.push(Item::AgentTurn(AgentTurnRequest {
            turn_id: turn,
            origin: magpie_api::turns::TurnOrigin::Agent,
            at,
        }));
        r.push(Item::state(turn, state(ReportedState::Running)));
        r.push_in(
            Some(turn),
            Item::AskOpen(AskReport {
                ask_id: "ask-outage".into(),
                tool_name: "AskUserQuestion".into(),
                tool_use_id: Some("toolu_1".into()),
                input: serde_json::Map::new(),
            }),
        );
        r.push(Item::state(turn, state(ReportedState::Completed)));
        r.push(Item::Ack {
            seq,
            result: AckResult::default(),
            done: None,
        });
        let start = Instant::now();
        tokio::time::sleep(Duration::from_mins(20)).await;
        assert_eq!(
            magpie.script().calls().len(),
            1,
            "nothing but the lease got through the outage"
        );
        assert!(
            magpie.faults().hits() > 100,
            "retried throughout: {}",
            magpie.faults().hits()
        );
        magpie.faults().remove(fault);
        assert!(r.drain(Instant::now() + Duration::from_secs(60)).await);
        assert!(start.elapsed() < Duration::from_mins(21));
        let routes: Vec<&str> = magpie
            .script()
            .calls()
            .iter()
            .map(|c| c.route)
            .filter(|r| *r != "lease")
            .collect();
        let count = |n: &str| routes.iter().filter(|x| **x == n).count();
        assert_eq!(count("agent_turn"), 1, "{routes:?}");
        assert_eq!(count("turn_state"), 2, "{routes:?}");
        assert_eq!(count("ask_open"), 1, "{routes:?}");
        assert_eq!(count("ack_command"), 1, "{routes:?}");
        let states: Vec<String> = magpie
            .script()
            .calls_to("turn_state")
            .iter()
            .map(|c| c.body.as_ref().unwrap()["state"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(states, ["running", "completed"]);
        let lane: Vec<&str> = routes.iter().copied().filter(|x| *x != "ack_command").collect();
        assert_eq!(
            lane,
            ["agent_turn", "turn_state", "ask_open", "turn_state"],
            "the turn's lane in order"
        );
        assert!(magpie.acks(cid).contains_key(&seq));
        assert_eq!(r.given_up(), 0);
        runner.abort();
    }

    /// 7.5's race: magpie refuses an agent turn while a person's turn is active (409 `bad_transition`). The
    /// refusal and that turn's end report (404, magpie never opened it) are settled, not given up; another turn's
    /// 404 still is.
    #[tokio::test]
    async fn a_refused_agent_turn_is_settled_not_given_up() {
        use aviary_fakes::script::Reply;
        use magpie_api::sandbox::TurnStateReport;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let fake = aviary_fakes::magpie::FakeMagpie::unguarded();
        let served = fake.serve().await.unwrap();
        fake.script().always(
            "agent_turn",
            Reply::error(409, "bad_transition", "a person turn is active"),
        );
        fake.script()
            .always("turn_state", Reply::error(404, "not_found", "no such turn"));
        let client = magpie_api::client::Client::new(reqwest::Client::new(), served.url(), "t");
        let r = Reporter::new(client, Arc::new(crate::cc::Stored0));
        r.set_lease(Uuid::new_v4());
        tokio::spawn(r.clone().run());
        let end = || TurnStateReport {
            state: ReportedState::Completed,
            at: magpie_api::Timestamp::now(),
            provider_session_id: None,
            transcript_path: None,
            exit_code: None,
            error: None,
            held: None,
            tasks: None,
            last_uuid: None,
            usage: None,
            duration_ms: None,
            commands: None,
        };
        let agent = Uuid::now_v7();
        r.push(Item::AgentTurn(AgentTurnRequest {
            turn_id: agent,
            origin: magpie_api::turns::TurnOrigin::Agent,
            at: magpie_api::Timestamp::now(),
        }));
        r.push(Item::state(agent, end()));
        assert!(r.drain(Instant::now() + Duration::from_secs(10)).await);
        assert_eq!(r.given_up(), 0, "the refusal and its turn's 404 are settled");
        assert_eq!(fake.script().calls_to("agent_turn").len(), 1, "never retried");
        assert_eq!(fake.script().calls_to("turn_state").len(), 1);
        // a turn magpie did not refuse: its 404 is a report given up
        r.push(Item::state(Uuid::now_v7(), end()));
        assert!(r.drain(Instant::now() + Duration::from_secs(10)).await);
        assert_eq!(r.given_up(), 1);
    }

    /// No report is dropped across a 20 min outage: the retry keeps going at the 10 s cap (paused time).
    #[tokio::test(start_paused = true)]
    async fn a_report_survives_a_20_minute_outage() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = magpie_api::client::Client::new(reqwest::Client::new(), "http://127.0.0.1:9", "t");
        let r = Reporter::new(client, Arc::new(crate::cc::Stored0));
        let start = Instant::now();
        let attempts = AtomicU32::new(0);
        let ok = r
            .retry("test", || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                if start.elapsed() < Duration::from_mins(20) {
                    Attempt::Retry("magpie down".into(), None)
                } else {
                    Attempt::Done
                }
            })
            .await;
        assert!(ok);
        let n = attempts.load(Ordering::SeqCst);
        // 0.5, 1, 2, 4, 8, then every 10 s: about 120 attempts in 20 min
        assert!((110..=130).contains(&n), "{n}");
        assert!(start.elapsed() < Duration::from_mins(20) + REPORT_RETRY_MAX + Duration::from_secs(1));
    }
}
