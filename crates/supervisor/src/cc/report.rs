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
/// The longest `retryAfterMs` honoured for one wait.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(30);

/// Called once magpie answered an item (`true`) or it was given up (`false`: the lease was lost).
pub type Done = Box<dyn FnOnce(bool) + Send>;

/// What an end report waits for before it is sent (8.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Wait {
    /// Nothing (a turn that never ran, a `running` report).
    #[default]
    Nothing,
    /// magpie has acknowledged main-transcript writes holding these uuids.
    Uuids(Vec<String>),
    /// The generation is gone: the filesystem writes already in flight are answered.
    Settle,
}

/// One report.
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

fn retry_after(e: &ClientError) -> Option<Duration> {
    let r = e.api()?;
    r.retry_after_ms
        .map(Duration::from_millis)
        .or(r.retry_after.map(Duration::from_secs))
        .map(|d| d.min(RETRY_AFTER_MAX))
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
            refused: Mutex::new(HashSet::new()),
        })
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
                    Wait::Uuids(u) => !u.is_empty() && self.transcript.wait_stored(u).await == Stored::Stuck,
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
            route: None,
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
