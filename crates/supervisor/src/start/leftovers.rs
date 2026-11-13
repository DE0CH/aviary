//! What a release ends besides the agent's generation (15.5: "everything that matters is already in magpie", and
//! nothing keeps writing `/workspace` after the answer): every process the keeper runs for this sandbox other than
//! the supervisor and the agent's own `claude` (exec runs, dockerd), and, when the release came before the agent
//! attached on a restart, what the previous supervisor left: its `claude` (ended as a generation is: stdin closed,
//! 5 s, SIGTERM to its group, 3 s, SIGKILL) and its running turn, reported `failed` with `sandbox.lost` while the
//! lease is held (without a lease, magpie ends it from hermit's `gone`, magpie 8.7).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use magpie_api::errors::{DataCode, ErrorCode};
use magpie_api::sandbox::{LeaseResponse, ReportError, ReportedState, TurnStateReport};
use tokio::time::Instant;
use uuid::Uuid;

use crate::keeper;
use crate::lifecycle::{Agent, BoxFuture, LeftoverCall};
use crate::limits::{KILL_AFTER_TERM, STDIN_CLOSE_GRACE};
use crate::state::RunState;

/// How often the keeper's `STATUS` is read while waiting for processes to end.
const GONE_POLL: Duration = Duration::from_millis(50);
/// After SIGKILL, how long to wait for the keeper to reap.
const KILL_REAP: Duration = Duration::from_secs(2);
/// Between attempts of a report magpie did not answer.
const REPORT_RETRY: Duration = Duration::from_millis(500);

/// Everything the hook needs, captured by the start sequence.
pub struct Leftovers {
    pub keeper: keeper::Client,
    pub agent: Arc<dyn Agent>,
    /// `run.json` of this boot (a restart), if any.
    pub prior: Option<RunState>,
    pub magpie: magpie_api::client::Client,
    /// The lease, once the start sequence holds it.
    pub lease: Arc<OnceLock<LeaseResponse>>,
}

impl std::fmt::Debug for Leftovers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Leftovers")
            .field("prior", &self.prior.is_some())
            .field("lease", &self.lease.get().map(|l| l.epoch))
            .finish_non_exhaustive()
    }
}

impl Leftovers {
    /// The hook for [`crate::lifecycle::Lifecycle::on_leftovers`].
    #[must_use]
    pub fn hook(self: Arc<Self>) -> crate::lifecycle::Leftovers {
        Arc::new(move |call: LeftoverCall| -> BoxFuture<'static, ()> {
            let me = self.clone();
            Box::pin(async move { me.run(call).await })
        })
    }

    async fn run(&self, call: LeftoverCall) {
        let restored = self.agent.restored();
        self.end_processes(call.attached, call.deadline).await;
        if !call.attached && !restored {
            self.report_turns(&call.reason, call.deadline).await;
        }
    }

    async fn status(&self) -> Option<keeper::KeeperStatus> {
        let k = self.keeper.clone();
        tokio::task::spawn_blocking(move || k.status()).await.ok()?.ok()
    }

    /// The keeper's children this release must end: not the supervisor, not the agent's `claude`, and no `claude`
    /// at all once the agent has attached (they are its own).
    async fn targets(&self, attached: bool) -> Vec<(i32, String)> {
        let own = self.agent.claude_pid();
        self.status().await.map_or_else(Vec::new, |s| {
            s.children
                .into_iter()
                .filter(|(pid, name)| {
                    name != "supervisor"
                        && !(crate::harness::of_process(name).is_some() && (attached || Some(*pid) == own))
                })
                .collect()
        })
    }

    /// Waits until the keeper runs none of `pids`, or `until`; the ones still running.
    async fn wait_gone(&self, pids: &[i32], until: Instant) -> Vec<i32> {
        loop {
            let live: Vec<i32> = match self.status().await {
                Some(s) => pids
                    .iter()
                    .copied()
                    .filter(|p| s.children.iter().any(|(c, _)| c == p))
                    .collect(),
                None => pids.to_vec(),
            };
            if live.is_empty() || Instant::now() >= until {
                return live;
            }
            tokio::time::sleep(GONE_POLL.min(until.saturating_duration_since(Instant::now()))).await;
        }
    }

    async fn signal(&self, pids: &[i32], sig: i32) {
        let k = self.keeper.clone();
        let pids = pids.to_vec();
        let _ = tokio::task::spawn_blocking(move || {
            for p in pids {
                // its group (every one is spawned with setsid) and the process itself
                let _ = k.signal(-p, sig);
                let _ = k.signal(p, sig);
            }
        })
        .await;
    }

    async fn end_processes(&self, attached: bool, deadline: Instant) {
        let targets = self.targets(attached).await;
        if targets.is_empty() {
            return;
        }
        tracing::info!(processes = ?targets, "release: ending what the agent does not own");
        let claude: Vec<i32> = targets
            .iter()
            .filter(|(_, n)| crate::harness::of_process(n).is_some())
            .map(|(p, _)| *p)
            .collect();
        let all: Vec<i32> = targets.iter().map(|(p, _)| *p).collect();
        if !claude.is_empty() {
            // a generation's end (4.5): stdin closed first, so Claude Code can finish its transcript
            let k = self.keeper.clone();
            let c = claude.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for p in c {
                    let _ = k.close_stdin(p);
                }
            })
            .await;
            let first =
                (Instant::now() + STDIN_CLOSE_GRACE).min(deadline.checked_sub(KILL_AFTER_TERM).unwrap_or(deadline));
            self.wait_gone(&claude, first).await;
        }
        let live = self.wait_gone(&all, Instant::now()).await;
        if live.is_empty() {
            return;
        }
        self.signal(&live, libc::SIGTERM).await;
        let live = self
            .wait_gone(&live, (Instant::now() + KILL_AFTER_TERM).min(deadline))
            .await;
        if live.is_empty() {
            return;
        }
        tracing::warn!(pids = ?live, "release: still running after SIGTERM: killed");
        self.signal(&live, libc::SIGKILL).await;
        let live = self.wait_gone(&live, Instant::now() + KILL_REAP).await;
        if !live.is_empty() {
            tracing::error!(pids = ?live, "release: still running after SIGKILL");
        }
    }

    /// The previous supervisor's turns: the running and the queued one `failed` with `sandbox.lost`, and the ends
    /// magpie had not answered sent again (both idempotent in magpie).
    async fn report_turns(&self, reason: &str, deadline: Instant) {
        let Some(prior) = &self.prior else { return };
        let mut reports: Vec<(Uuid, TurnStateReport)> = prior
            .ending
            .iter()
            .filter_map(|e| {
                Some((
                    Uuid::parse_str(&e.turn_id).ok()?,
                    serde_json::from_value(e.report.clone()).ok()?,
                ))
            })
            .collect();
        for id in [prior.run.turn_id.as_deref(), prior.run.queued_turn_id.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(|t| Uuid::parse_str(t).ok())
        {
            if !reports.iter().any(|(t, _)| *t == id) {
                reports.push((id, lost(reason)));
            }
        }
        if reports.is_empty() {
            return;
        }
        let Some(lease) = self.lease.get() else {
            tracing::warn!(
                turns = reports.len(),
                "release before the lease: the previous supervisor's turns end from hermit's gone"
            );
            return;
        };
        let calls = self.magpie.sandbox(Some(lease.lease_id));
        for (turn, report) in reports {
            loop {
                let attempt = tokio::time::timeout_at(deadline, calls.turn_state(turn, &report)).await;
                match attempt {
                    Ok(Ok(())) => {
                        tracing::info!(turn = %turn, state = ?report.state, "release: a previous supervisor's turn reported");
                        break;
                    }
                    Ok(Err(e)) if e.code() == Some(&ErrorCode::BadTransition) => break,
                    Ok(Err(e)) if crate::cc::report::retryable(&e) && Instant::now() < deadline => {
                        tokio::time::sleep_until((Instant::now() + REPORT_RETRY).min(deadline)).await;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(turn = %turn, error = %e, "release: a previous supervisor's turn not reported");
                        break;
                    }
                    Err(_) => {
                        tracing::warn!(turn = %turn, "release: a previous supervisor's turn not reported by the deadline");
                        return;
                    }
                }
            }
        }
    }
}

/// `failed` with `{"code": "sandbox.lost", "message": "released: <reason>"}` (15.5, 11.6).
fn lost(reason: &str) -> TurnStateReport {
    TurnStateReport {
        state: ReportedState::Failed,
        at: magpie_api::Timestamp::now(),
        harness: magpie_api::Harness::default(),
        provider_session_id: None,
        transcript_path: None,
        exit_code: None,
        error: Some(ReportError {
            code: DataCode::SandboxLost,
            message: format!("released: {reason}"),
        }),
        held: None,
        tasks: None,
        last_uuid: None,
        usage: None,
        duration_ms: None,
        commands: None,
    }
}
