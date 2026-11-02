//! Background jobs (magpie 15). One replica at a time holds the job leader lock
//! (`pg_try_advisory_lock(hashtext('magpie.jobs'))` on a dedicated connection, retried every 10 s by the others);
//! losing the connection releases it, and a draining replica releases it first. Every job is idempotent, so a
//! takeover mid-pass is safe. Before each pass a leader job confirms the lock ([`aviary_pg::Leader::confirm`]).
//!
//! WP-11's jobs: purge, outbox, retention. The others of magpie 15 (hermit feed, blob GC, blob uploader,
//! availability tick, limbo flush, live cleanup, turn sweeper) are added here by their packages.

pub mod outbox;
pub mod retention;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use aviary_pg::{Leader, Pool};
use tokio::task::JoinHandle;

use crate::conversations::delete;
use crate::limits;

/// The running jobs of one replica.
#[derive(Debug)]
pub struct Jobs {
    leader: Arc<Leader>,
    tasks: Vec<JoinHandle<()>>,
}

/// One leader job: runs `pass` every `every` while this replica leads.
fn leader_loop<F, Fut>(leader: Arc<Leader>, name: &'static str, every: Duration, mut pass: F) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = crate::error::Result<String>> + Send,
{
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !leader.confirm().await {
                continue;
            }
            match pass().await {
                Ok(what) if what.is_empty() => {}
                Ok(what) => tracing::info!(job = name, what = %what, "job pass"),
                Err(e) => tracing::warn!(job = name, error = %e, "job pass failed"),
            }
        }
    })
}

impl Jobs {
    /// Starts bidding for the leader lock and the leader loops; `deliver` makes the outbox's hermit calls.
    pub fn start<D: outbox::Deliver>(pool: &Pool, deliver: D) -> Self {
        Self::start_with(pool, deliver, Intervals::default())
    }

    /// [`Jobs::start`] with other intervals (tests).
    pub fn start_with<D: outbox::Deliver>(pool: &Pool, deliver: D, iv: Intervals) -> Self {
        let leader = Arc::new(Leader::start_with(pool, crate::db::jobs_key(), iv.leader_retry));
        let deliver = Arc::new(deliver);
        let mut tasks = Vec::new();
        let p = pool.clone();
        tasks.push(leader_loop(leader.clone(), "purge", iv.purge, move || {
            let p = p.clone();
            async move {
                let n = delete::purge_due(&p, limits::PURGE_BATCH).await?;
                Ok(if n == 0 { String::new() } else { format!("{n} purged") })
            }
        }));
        let p = pool.clone();
        tasks.push(leader_loop(leader.clone(), "outbox", iv.outbox, move || {
            let p = p.clone();
            let d = deliver.clone();
            async move {
                let n = outbox::pass(&p, d.as_ref()).await?;
                Ok(if n == 0 { String::new() } else { format!("{n} done") })
            }
        }));
        let p = pool.clone();
        tasks.push(leader_loop(leader.clone(), "retention", iv.retention, move || {
            let p = p.clone();
            async move {
                let n = retention::pass(&p).await?;
                Ok(if n == 0 {
                    String::new()
                } else {
                    format!("{n} rows deleted")
                })
            }
        }));
        Self { leader, tasks }
    }

    /// Whether this replica leads now.
    #[must_use]
    pub fn is_leader(&self) -> bool {
        *self.leader.is_leader().borrow()
    }

    /// Stops the loops and gives the leader lock up (drain: another replica takes the jobs within one retry).
    pub async fn stop(self) {
        for t in &self.tasks {
            t.abort();
        }
        for t in self.tasks {
            let _ = t.await;
        }
        if let Ok(leader) = Arc::try_unwrap(self.leader) {
            leader.release().await;
        } else {
            tracing::warn!("job leader still shared at drain; its connection closes with the process");
        }
    }
}

/// How often each leader job runs, and how often a held poll re-queries.
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    /// The leader lock's retry.
    pub leader_retry: Duration,
    /// The purge.
    pub purge: Duration,
    /// The outbox.
    pub outbox: Duration,
    /// Retention.
    pub retention: Duration,
    /// A held poll's re-query without a notification ([`limits::REQUERY`]; tests lengthen it to show a wake-up came
    /// from a notification or a reconnect).
    pub requery: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            leader_retry: limits::LEADER_RETRY,
            purge: limits::PURGE_EVERY,
            outbox: limits::OUTBOX_EVERY,
            retention: limits::RETENTION_EVERY,
            requery: limits::REQUERY,
        }
    }
}
