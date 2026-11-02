//! Reports to magpie (supervisor 11): turn states, agent-started turns, asks and command acks in one ordered queue,
//! task events in 250 ms batches. Every report is retried on 5xx, connection errors and `409 transcript_pending`
//! (0.5 s doubling to 10 s) for as long as the lease is valid: none is ever dropped. A turn's end report first waits
//! for the turn's transcript lines (8.2).

use std::collections::VecDeque;
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

/// Called once magpie answered an item (`true`) or it was given up (`false`: the lease was lost).
pub type Done = Box<dyn FnOnce(bool) + Send>;

/// One report.
pub enum Item {
    /// `POST /v1/sandbox/turns/{turnId}/state`; an end report with `wait` first waits for those uuids.
    State {
        turn: Uuid,
        report: TurnStateReport,
        wait: Vec<String>,
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
    Retry(String),
    /// The lease is gone (or the call can never succeed): stop with this item.
    GiveUp(String),
}

fn classify(e: &ClientError) -> Attempt {
    match e.code() {
        Some(ErrorCode::LeaseLost) => Attempt::GiveUp("lease lost".into()),
        Some(ErrorCode::TranscriptPending) => Attempt::Retry(e.to_string()),
        _ if e.is_retryable() => Attempt::Retry(e.to_string()),
        _ => match e.api() {
            Some(r) if r.status >= 500 => Attempt::Retry(e.to_string()),
            _ => Attempt::GiveUp(e.to_string()),
        },
    }
}

/// The reporter.
pub struct Reporter {
    magpie: magpie_api::client::Client,
    lease: Mutex<Option<Uuid>>,
    transcript: Arc<dyn Transcript>,
    queue: Mutex<VecDeque<Item>>,
    wake: Notify,
    /// Items queued and not yet answered (the queue plus the one in flight).
    pending: watch::Sender<usize>,
    tasks: Mutex<Vec<TaskReportEvent>>,
    tasks_pending: watch::Sender<usize>,
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
            queue: Mutex::new(VecDeque::new()),
            wake: Notify::new(),
            pending: watch::channel(0).0,
            tasks: Mutex::new(Vec::new()),
            tasks_pending: watch::channel(0).0,
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

    /// Queues a report (sent in order after every earlier one).
    pub fn push(&self, item: Item) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(item);
        self.pending.send_modify(|n| *n += 1);
        self.wake.notify_one();
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
                    tracing::warn!(report = what, why = %why, "report given up");
                    return false;
                }
                Attempt::Retry(why) => {
                    if !logged {
                        tracing::info!(report = what, why = %why, "magpie unavailable: report retried");
                        logged = true;
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(REPORT_RETRY_MAX);
                }
            }
        }
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
                if !wait.is_empty() && self.transcript.wait_stored(wait).await == Stored::Stuck {
                    report.state = ReportedState::Failed;
                    report.error = Some(ReportError {
                        code: magpie_api::errors::DataCode::CcWriteStuck,
                        message: "a transcript write kept failing for 10 min while magpie answered".into(),
                    });
                    report.last_uuid = None;
                }
                let ok = self
                    .retry(&what, || async {
                        match self.calls().turn_state(turn, &report).await {
                            Ok(()) => Attempt::Done,
                            Err(e) if e.code() == Some(&ErrorCode::BadTransition) => {
                                // a replay of what magpie has, or magpie ended the turn first: final either way
                                tracing::info!(turn = %turn, error = %e, "turn state already settled");
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

    /// The ordered queue's task.
    pub async fn run(self: Arc<Self>) {
        loop {
            let next = self.queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
            match next {
                Some(item) => {
                    self.send(item).await;
                    self.pending.send_modify(|n| *n = n.saturating_sub(1));
                }
                None => self.wake.notified().await,
            }
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
            Attempt::Retry(_)
        ));
        assert!(matches!(
            classify(&refusal(409, ErrorCode::TranscriptPending)),
            Attempt::Retry(_)
        ));
        assert!(matches!(
            classify(&refusal(500, ErrorCode::Internal)),
            Attempt::Retry(_)
        ));
        assert!(matches!(
            classify(&refusal(409, ErrorCode::LeaseLost)),
            Attempt::GiveUp(_)
        ));
        assert!(matches!(
            classify(&refusal(400, ErrorCode::BadRequest)),
            Attempt::GiveUp(_)
        ));
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
                    Attempt::Retry("magpie down".into())
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
