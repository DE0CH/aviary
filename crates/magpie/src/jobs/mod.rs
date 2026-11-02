//! Background jobs (magpie 15). One replica at a time holds the job leader lock
//! (`pg_try_advisory_lock(hashtext('magpie.jobs'))` on a dedicated connection, retried every 10 s by the others);
//! losing the connection releases it, and a draining replica releases it first. Every job is idempotent, so a
//! takeover mid-pass is safe. Before each pass a leader job confirms the lock ([`aviary_pg::Leader::confirm`]); the
//! outbox, whose pass can run for minutes with hermit unreachable, confirms it again before each item.
//!
//! WP-11's jobs: purge, outbox, retention; WP-12's: the limbo sweeper; WP-13's live cleanup runs on every replica.
//! The others of magpie 15 (hermit feed, blob GC, blob uploader, availability tick, turn sweeper) are added here by
//! their packages.

pub mod outbox;
pub mod retention;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use aviary_pg::{Leader, Pool};
use tokio::task::JoinHandle;

use crate::blobs::Blobs;
use crate::blobs::gc::Gc;
use crate::blobs::uploader::Uploader;
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

/// A job every replica runs (no leader lock): `pass` every `every`.
fn every_replica<F, Fut>(name: &'static str, every: Duration, pool: Pool, pass: F) -> JoinHandle<()>
where
    F: Fn(Pool) -> Fut + Send + 'static,
    Fut: Future<Output = crate::error::Result<String>> + Send,
{
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match pass(pool.clone()).await {
                Ok(what) if what.is_empty() => {}
                Ok(what) => tracing::info!(job = name, what = %what, "job pass"),
                Err(e) => tracing::warn!(job = name, error = %e, "job pass failed"),
            }
        }
    })
}

impl Jobs {
    /// Starts bidding for the leader lock and the leader loops; `deliver` makes the outbox's hermit calls.
    pub fn start<D: outbox::Deliver>(pool: &Pool, deliver: D, blobs: Arc<Blobs>) -> Self {
        Self::start_with(pool, deliver, blobs, Intervals::default())
    }

    /// [`Jobs::start`] with other intervals (tests).
    pub fn start_with<D: outbox::Deliver>(pool: &Pool, deliver: D, blobs: Arc<Blobs>, iv: Intervals) -> Self {
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
        let l = leader.clone();
        tasks.push(leader_loop(leader.clone(), "outbox", iv.outbox, move || {
            let p = p.clone();
            let d = deliver.clone();
            let l = l.clone();
            async move {
                let n = outbox::pass_while(&p, d.as_ref(), || l.confirm()).await?;
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
        let p = pool.clone();
        tasks.push(leader_loop(leader.clone(), "limbo", iv.limbo, move || {
            let p = p.clone();
            async move {
                let n = crate::ccfs::limbo::sweep(&p).await?;
                Ok(if n == 0 {
                    String::new()
                } else {
                    format!("{n} lines retracted")
                })
            }
        }));
        tasks.push(every_replica(
            "live cleanup",
            crate::live::CLEANUP_EVERY,
            pool.clone(),
            |p| async move {
                let n = crate::live::cleanup(&p).await?;
                Ok(if n == 0 {
                    String::new()
                } else {
                    format!("{n} live frames deleted")
                })
            },
        ));
        let p = pool.clone();
        let b = blobs.clone();
        let gc = Arc::new(Gc::default());
        tasks.push(leader_loop(leader.clone(), "blob gc", iv.blob_gc, move || {
            let p = p.clone();
            let b = b.clone();
            let gc = gc.clone();
            async move {
                let n = gc.pass(&p, &b).await?;
                Ok(if n == 0 { String::new() } else { format!("{n} freed") })
            }
        }));
        let up = Arc::new(Uploader::default());
        tasks.push(every_replica("blob uploader", iv.upload, pool.clone(), move |p| {
            let b = blobs.clone();
            let up = up.clone();
            async move {
                let n = up.pass(&p, &b).await?;
                Ok(if n == 0 { String::new() } else { format!("{n} stored") })
            }
        }));
        Self { leader, tasks }
    }

    /// The leader lock, for leader work that runs outside the interval loops (the hermit feed follower).
    #[must_use]
    pub fn leader(&self) -> Arc<Leader> {
        self.leader.clone()
    }

    /// Adds a task the jobs own: stopped with them (the turn sweeper, the hermit feed follower).
    pub fn push(&mut self, task: JoinHandle<()>) {
        self.tasks.push(task);
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
    /// The limbo sweeper (magpie 6.7).
    pub limbo: Duration,
    /// The blob uploader (every replica).
    pub upload: Duration,
    /// The blob GC.
    pub blob_gc: Duration,
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
            limbo: limits::LIMBO_SWEEP_EVERY,
            upload: limits::UPLOAD_EVERY,
            blob_gc: limits::BLOB_GC_EVERY,
            requery: limits::REQUERY,
        }
    }
}
