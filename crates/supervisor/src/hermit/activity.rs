//! `POST http://hermit/v1/sandbox/activity` (supervisor 15.3): every `min(activityEverySeconds, 15)` s and at once on
//! every change of the run state. Events (memory-guard kills, generation exits) ride on the next accepted report; a
//! failed report is retried with its events kept, at most 32. A reply naming a pending release is acted on exactly
//! as `POST /v1/lifecycle/release` with the same fields.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hermit_api::Timestamp;
use hermit_api::sandbox_surface::{ACTIVITY_MAX_EVENTS, ActivityEvent, ActivityReport, ActivityState, MemoryUse};
use supervisor_api::HERMIT_SANDBOX;
use supervisor_api::control::{Memory, ReleaseRequest, SupervisorFacts};

use super::Hermit;
use crate::lifecycle::{BoxFuture, Lifecycle, Phase};
use crate::limits::{ACTIVITY_EVERY_MAX, ACTIVITY_RETRY, HERMIT_TIMEOUT};
use crate::memguard::{self, Proc};

/// The supervisor's own facts every report carries.
#[derive(Debug, Clone)]
pub struct Facts {
    pub boot_id: String,
    /// The keeper's count of supervisor restarts.
    pub restarts: u32,
    pub proc: Proc,
}

impl Facts {
    /// `L` and `U` now.
    #[must_use]
    pub fn memory(&self, hermit: &Hermit) -> Memory {
        let mem = self.proc.meminfo();
        let descriptor = hermit.descriptor().map_or(u64::MAX, |d| d.resources.memory_bytes);
        let limit = memguard::limit(descriptor, mem);
        Memory {
            limit_bytes: if limit == u64::MAX { 0 } else { limit },
            used_bytes: mem.map_or(0, |m| m.used()),
        }
    }

    #[must_use]
    pub fn supervisor(&self) -> SupervisorFacts {
        SupervisorFacts {
            version: super::version(),
            boot_id: self.boot_id.clone(),
            restarts: self.restarts,
            rss_bytes: super::own_rss(),
        }
    }
}

/// The activity loop.
#[derive(Debug)]
pub struct Activity {
    hermit: Hermit,
    lifecycle: Arc<Lifecycle>,
    facts: Facts,
    events: Mutex<VecDeque<ActivityEvent>>,
}

impl Activity {
    #[must_use]
    pub fn new(hermit: Hermit, lifecycle: Arc<Lifecycle>, facts: Facts) -> Arc<Self> {
        Arc::new(Self {
            hermit,
            lifecycle,
            facts,
            events: Mutex::new(VecDeque::new()),
        })
    }

    /// Queues an event for the next report (the oldest are dropped beyond 32) and sends at once.
    pub fn event(&self, e: ActivityEvent) {
        let mut q = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        q.push_back(e);
        while q.len() > ACTIVITY_MAX_EVENTS {
            q.pop_front();
        }
        drop(q);
        self.lifecycle.activity_now.notify_one();
    }

    /// The report as of now, and how many queued events it carries.
    #[must_use]
    pub fn report(&self) -> (ActivityReport, usize) {
        let s = self.lifecycle.reported();
        let events: Vec<ActivityEvent> = self
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let n = events.len();
        let mem = self.facts.memory(&self.hermit);
        (
            ActivityReport {
                state: s.state,
                turn_id: s.turn_id,
                tasks: s.tasks,
                // a run that waits only on a person's answer is idle for hermit (DECISIONS 17)
                idle_since: if matches!(s.state, ActivityState::Idle | ActivityState::Asking) {
                    s.idle_since
                } else {
                    None
                },
                last_output_at: s.last_output_at,
                boot_id: self.facts.boot_id.clone(),
                protocol: HERMIT_SANDBOX.1,
                version: super::version(),
                restarts: self.facts.restarts,
                rss_bytes: super::own_rss(),
                memory: MemoryUse {
                    limit: mem.limit_bytes,
                    used: mem.used_bytes,
                },
                events,
            },
            n,
        )
    }

    /// Sends one report, given at most `limit` (never more than [`HERMIT_TIMEOUT`]); on success drops the events it
    /// carried and acts on a pending release. Whether it was accepted, and the state it carried.
    async fn send(self: &Arc<Self>, limit: Duration) -> (bool, ActivityState) {
        let (report, n) = self.report();
        let state = report.state;
        let limit = limit.min(HERMIT_TIMEOUT);
        let answer = match tokio::time::timeout(limit, self.hermit.client().sandbox_activity(&report)).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err(format!("no answer within {limit:?}")),
        };
        match answer {
            Ok(reply) => {
                {
                    let mut q = self.events.lock().unwrap_or_else(PoisonError::into_inner);
                    for _ in 0..n.min(q.len()) {
                        q.pop_front();
                    }
                }
                if let Some(p) = reply.release
                    && report.state != ActivityState::Released
                {
                    let req = ReleaseRequest::from_pending(&p, Timestamp::now());
                    let l = self.lifecycle.clone();
                    tokio::spawn(async move {
                        let _ = l.release(req).await;
                    });
                }
                (true, state)
            }
            Err(e) => {
                tracing::info!(error = %e, "activity report failed");
                (false, state)
            }
        }
    }

    /// The final `released` report of a release: retried until `deadline`, and every attempt ends by it (a hermit
    /// that holds the request never keeps the release past its deadline).
    pub fn final_report(self: &Arc<Self>, deadline: tokio::time::Instant) -> BoxFuture<'static, ()> {
        let me = self.clone();
        Box::pin(async move {
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                if left.is_zero() {
                    tracing::warn!("the final released report was not accepted by the release's deadline");
                    return;
                }
                if me.send(left).await.0 {
                    return;
                }
                tokio::time::sleep_until((tokio::time::Instant::now() + ACTIVITY_RETRY).min(deadline)).await;
            }
        })
    }

    fn cadence(&self) -> Duration {
        self.hermit
            .descriptor()
            .map_or(ACTIVITY_EVERY_MAX, |d| {
                Duration::from_secs(u64::from(d.lifecycle.activity_every_seconds.max(1)))
            })
            .min(ACTIVITY_EVERY_MAX)
    }

    /// The loop: ends after the `released` report was accepted.
    pub async fn run(self: Arc<Self>) {
        let mut status = self.lifecycle.agent().status();
        let mut phases = self.lifecycle.phases();
        loop {
            // the state this report carried: a change while it was in flight is a change to send at once
            let (ok, sent_state) = self.send(HERMIT_TIMEOUT).await;
            if ok && sent_state == ActivityState::Released {
                return;
            }
            let wait = if ok { self.cadence() } else { ACTIVITY_RETRY };
            let until = tokio::time::Instant::now() + wait;
            // wait for the cadence, or a change of state (not every output line)
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(until) => break,
                    () = self.lifecycle.activity_now.notified() => break,
                    r = status.changed() => {
                        if r.is_err() || status.borrow_and_update().state != sent_state {
                            break;
                        }
                    }
                    r = phases.changed() => {
                        if r.is_err() || *phases.borrow_and_update() == Phase::Released {
                            break;
                        }
                    }
                }
            }
        }
    }
}
