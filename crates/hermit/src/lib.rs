//! hermit: the YYC implementation of the hermit contract (hermit spec 8-23), as a library so the binary and the
//! tests run the same replica. [`app::App::start`] starts one replica on a database; its router serves the
//! contract through `hermit_api::guarded_router`.
//!
//! Modules: [`config`] (hermit.json), [`catalogue`] (staff list), [`tokens`] (sandbox and control tokens), [`db`]
//! (schema, rows, `bump`), [`live`] (the in-memory liveness map), [`fabric`] (the provider traits), [`sandboxes`]
//! (get-or-start, release, activity), [`starts`] (the resumable start task), [`feed`] (the change feed and the
//! long-poll), [`janitor`], [`sandbox_routes`] (the supervisor's routes), [`supervisor_client`], [`people`],
//! [`deployment`], [`warm`], [`server`] (the `Server` trait), [`app`] (start, loops, drain, kill).

#![allow(clippy::similar_names)] // `core` (the replica) beside `code` (a release code)

pub mod admin;
pub mod app;
pub mod catalogue;
pub mod config;
pub mod console;
pub mod db;
pub mod deployment;
pub mod fabric;
pub mod feed;
pub mod health_verdict;
pub mod janitor;
pub mod live;
pub mod ops;
pub mod people;
pub mod sandbox_routes;
pub mod sandboxes;
pub mod server;
pub mod snapshot;
pub mod starts;
pub mod supervisor_client;
pub mod tasks;
pub mod tokens;
pub mod warm;
pub mod workspaces;

use std::sync::Arc;
use std::time::Duration;

use aviary_pg::{Listener, Pool};
use tokio::sync::watch;

use crate::catalogue::Catalogue;
use crate::config::Config;
use crate::fabric::Providers;
use crate::live::LiveMap;
use crate::tokens::Keys;

/// The fixed timings of hermit 10.3, 12, 13.12 and 23. Code, not configuration: tests shorten them to run in
/// seconds ([`Timings::default`] is the production set).
#[derive(Clone, Debug)]
pub struct Timings {
    /// A row claim's lease (15 s), renewed every `renew` (5 s).
    pub claim: Duration,
    pub renew: Duration,
    /// The takeover sweep (2 s).
    pub sweep: Duration,
    /// The janitor pass (30 s) and the leader retry (5 s).
    pub janitor: Duration,
    pub leader_retry: Duration,
    /// The start's health poll (300 ms).
    pub health_poll: Duration,
    /// The live map's full reload (30 s) and the catalogue's re-read (30 s).
    pub live_reload: Duration,
    pub catalogue_reload: Duration,
    /// How often a held request re-reads without a notification (1 s).
    pub recheck: Duration,
    /// Capacity retry (2 s), host poll (3 s), busy create retry (3 s), workspace retry (2 s).
    pub capacity_retry: Duration,
    pub host_poll: Duration,
    pub busy_retry: Duration,
    pub workspace_retry: Duration,
    /// A release call's retry (5 s) and the gap between the two health checks of a silence (5 s).
    pub release_retry: Duration,
    pub health_gap: Duration,
    /// Silence of activity reports before the health checks (90 s), and the `PUT`'s early check (45 s).
    pub silence: Duration,
    pub put_silence: Duration,
    /// A pool victim must have had no busy report for this long (90 s).
    pub victim_quiet: Duration,
    /// A pool victim that refused is skipped this long (60 s).
    pub victim_skip: Duration,
    /// The supervisor's release deadline (30 s).
    pub release_deadline: Duration,
    /// A releasing row stuck this long is ended `stuck` (10 min).
    pub stuck_release: Duration,
    /// A gone sandbox stays visible to `GET …/sandbox` this long (10 min).
    pub gone_visible: Duration,
    /// How long a `PUT` waits for a releasing sandbox to end (5 s), and a workspace delete for its sandbox (60 s).
    pub put_release_wait: Duration,
    pub delete_wait: Duration,
    /// How often a replica writes its own state (`replicas`: the workspace mount, a refused delete) (10 s).
    pub replica_report: Duration,
    /// A pause before a new start's first step: zero in production; the conformance test uses it to see a
    /// long-poll time out on a row that has not changed since its `PUT`.
    pub start_pause: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        let s = Duration::from_secs;
        Self {
            claim: s(15),
            renew: s(5),
            sweep: s(2),
            janitor: s(30),
            leader_retry: s(5),
            health_poll: Duration::from_millis(300),
            live_reload: s(30),
            catalogue_reload: s(30),
            recheck: s(1),
            capacity_retry: s(2),
            host_poll: s(3),
            busy_retry: s(3),
            workspace_retry: s(2),
            release_retry: s(5),
            health_gap: s(5),
            silence: s(90),
            put_silence: s(45),
            victim_quiet: s(90),
            victim_skip: s(60),
            release_deadline: s(30),
            stuck_release: s(600),
            gone_visible: s(600),
            put_release_wait: s(5),
            delete_wait: s(60),
            replica_report: s(10),
            start_pause: Duration::ZERO,
        }
    }
}

/// Everything one replica shares between its routes, tasks and loops.
pub struct Core {
    pub cfg: Arc<Config>,
    pub catalogue: Arc<Catalogue>,
    pub keys: Arc<Keys>,
    pub pool: Pool,
    pub listener: Arc<Listener>,
    pub live: Arc<LiveMap>,
    pub providers: Providers,
    /// The operator page's other sources and its snapshot cache (hermit 18).
    pub ops: ops::OpsState,
    /// Calls to supervisors (no proxy, HTTP/1.1).
    pub http: reqwest::Client,
    /// This replica's name (`task_owner`).
    pub pod: String,
    pub timings: Timings,
    pub draining: watch::Receiver<bool>,
    pub tasks: tasks::TaskSet,
    /// Held by a sweep pass and by the drain's hand-back, so a pass in flight when the drain starts cannot claim a
    /// row after the claims were handed back.
    pub sweep_lock: tokio::sync::Mutex<()>,
    /// This replica's live claims and the gate of their side effects ([`tasks::Claims`]).
    pub claims: Arc<tasks::Claims>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").field("pod", &self.pod).finish_non_exhaustive()
    }
}

impl Core {
    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self.draining.borrow()
    }

    /// A claim lease as a Postgres interval in milliseconds.
    #[must_use]
    pub fn claim_ms(&self) -> i64 {
        i64::try_from(self.timings.claim.as_millis()).unwrap_or(15_000)
    }
}

/// Milliseconds of a duration as `i64` for SQL (`$n * interval '1 millisecond'`).
#[must_use]
pub fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Resolves once `rx` says the replica is draining (never, if the sender is gone without saying so).
pub async fn until_draining(mut rx: watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
