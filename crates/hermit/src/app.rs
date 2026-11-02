//! One hermit replica: start (migrations, the start-up checks, the listener, the live map), its loops (live map,
//! catalogue, takeover sweep, janitor on the leader), the router, the drain of hermit 20.2, and a kill for tests
//! (a replica that dies without handing anything back).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aviary_core::auth::Callers;
use aviary_core::drain;
use aviary_pg::{Leader, Listener};
use axum::Router;
use hermit_api::server::{RouteGuard, guarded_router};
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::catalogue::Catalogue;
use crate::config::Config;
use crate::fabric::Providers;
use crate::live::LiveMap;
use crate::server::Hermit;
use crate::tasks::{self, TaskSet};
use crate::tokens::Keys;
use crate::{Core, Timings, db, janitor};

/// What a replica is started with.
pub struct StartOptions {
    pub config: Arc<Config>,
    pub catalogue: Arc<Catalogue>,
    /// Re-read the catalogue file every 30 s (off for a catalogue fixed in a test).
    pub reload_catalogue: bool,
    pub keys: Arc<Keys>,
    pub database_url: String,
    /// This replica's name (`HERMIT_POD`).
    pub pod: String,
    /// Pooled connections for requests and tasks (10 in production).
    pub pool_size: usize,
    pub providers: Providers,
    pub timings: Timings,
    /// The callers' pair tokens (hermit 3.2).
    pub callers: Callers,
}

impl std::fmt::Debug for StartOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartOptions")
            .field("pod", &self.pod)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("database: {0}")]
    Db(#[from] aviary_pg::Error),
    /// A live row's token key is missing from `HERMIT_TOKEN_KEYS` (6.1): the roll must not become ready.
    #[error("live sandboxes use token key(s) {0:?}, which HERMIT_TOKEN_KEYS does not hold")]
    MissingKid(Vec<i16>),
}

/// A running replica.
pub struct App {
    pub core: Arc<Core>,
    callers: Callers,
    draining: watch::Sender<bool>,
    loops: Mutex<Vec<AbortHandle>>,
    leader: Mutex<Option<Arc<Leader>>>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("pod", &self.core.pod)
            .finish_non_exhaustive()
    }
}

impl App {
    /// Starts a replica: migrations (lock 7300/1), the key check, the listener, the first live map load, then the
    /// loops. The caller serves [`App::router`].
    ///
    /// # Errors
    /// A database error, or a live row's key missing.
    pub async fn start(o: StartOptions) -> Result<Self, StartError> {
        let pool = db::connect(&o.database_url, &o.pod, o.pool_size)?;
        db::migrate(&pool).await?;
        let kids: Vec<i16> = pool
            .get()
            .await?
            .query(
                "select distinct token_kid from sandboxes where status <> 'gone' and token_kid is not null",
                &[],
            )
            .await?
            .iter()
            .filter_map(|r| r.try_get::<_, i16>(0).ok())
            .filter(|k| !u32::try_from(*k).is_ok_and(|k| o.keys.has(k)))
            .collect();
        if !kids.is_empty() {
            return Err(StartError::MissingKid(kids));
        }
        let listener = Arc::new(Listener::start(&pool, &[db::CHANNEL])?);
        let live = Arc::new(LiveMap::default());
        live.reload(&pool).await?;
        if o.reload_catalogue {
            o.catalogue.reload();
        }
        let (draining_tx, draining) = watch::channel(false);
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .no_proxy()
            .http1_only()
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let core = Arc::new(Core {
            cfg: o.config,
            catalogue: o.catalogue,
            keys: o.keys,
            pool: pool.clone(),
            listener,
            live,
            providers: o.providers,
            http,
            pod: o.pod,
            timings: o.timings,
            draining,
            tasks: TaskSet::default(),
            sweep_lock: tokio::sync::Mutex::new(()),
        });
        let leader = Arc::new(Leader::start_many(
            &pool,
            &[db::LOCK_JANITOR],
            core.timings.leader_retry,
        ));
        let app = Self {
            core: core.clone(),
            callers: o.callers,
            draining: draining_tx,
            loops: Mutex::new(Vec::new()),
            leader: Mutex::new(Some(leader.clone())),
        };
        app.spawn(live_loop(core.clone()));
        if o.reload_catalogue {
            app.spawn(catalogue_loop(core.clone()));
        }
        app.spawn(sweep_loop(core.clone()));
        app.spawn(janitor::run(core, leader));
        Ok(app)
    }

    fn spawn(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let h = tokio::spawn(fut).abort_handle();
        self.loops.lock().unwrap_or_else(PoisonError::into_inner).push(h);
    }

    /// The contract router behind the caller table (3.2).
    pub fn router(&self) -> Router {
        guarded_router(
            Arc::new(Hermit(self.core.clone())),
            RouteGuard::new(self.callers.clone()),
        )
    }

    /// Readiness beyond the start (20.1): a catalogue loaded once and the live map loaded once; never Postgres,
    /// `JuiceFS` or the k8s API afterwards.
    #[must_use]
    pub fn ready(&self) -> bool {
        !self.core.is_draining() && self.core.catalogue.get().is_some() && self.core.live.loaded()
    }

    /// Whether this replica leads the janitor now.
    #[must_use]
    pub fn leads(&self) -> bool {
        self.leader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|l| l.watch(&db::LOCK_JANITOR).is_some_and(|w| *w.borrow()))
    }

    /// Drain (20.2) steps 2-4 and 6: not ready, warm off, held long-polls answered; claims handed back; leadership
    /// released; the loops stopped and the pool closed. The caller waits for in-flight requests (step 5) between
    /// [`App::begin_drain`] and [`App::finish_drain`]; `drain` does both with no wait.
    pub async fn drain(&self) {
        let _ = self.begin_drain().await;
        self.finish_drain().await;
    }

    /// Steps 2-4.
    /// Returns how many claims were handed back (`None` when the database refused: they lapse in 15 s).
    pub async fn begin_drain(&self) -> Option<u64> {
        let _ = self.draining.send(true);
        let handed = match tasks::hand_back(&self.core).await {
            Ok(n) => {
                tracing::info!(claims = n, "claims handed back");
                Some(n)
            }
            Err(e) => {
                tracing::warn!(error = %e, "claims not handed back (they lapse in 15 s)");
                None
            }
        };
        let janitor = self.loops.lock().unwrap_or_else(PoisonError::into_inner).pop();
        if let Some(h) = janitor {
            h.abort();
        }
        let leader = self.leader.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(l) = leader {
            // the janitor loop held the other reference: wait for its abort to drop it
            for _ in 0..50 {
                if Arc::strong_count(&l) == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            match Arc::try_unwrap(l) {
                Ok(l) => {
                    l.release().await;
                    tracing::info!("leader released");
                }
                Err(l) => drop(l),
            }
        }
        handed
    }

    /// Step 5's second half: waits up to `max` for the supervisor and sandboxd calls this replica's tasks already
    /// sent (their claims were handed back, so their results are not written), then stops what is left. Returns
    /// how many tasks were stopped.
    pub async fn settle_tasks(&self, max: Duration) -> usize {
        let stopped = self.core.tasks.settle(max).await;
        if stopped > 0 {
            tracing::info!(tasks = stopped, "tasks stopped at the drain's deadline");
        }
        stopped
    }

    /// Step 6: the loops and any task left stopped, the pool closed.
    pub async fn finish_drain(&self) {
        for h in self.loops.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            h.abort();
        }
        self.core.tasks.abort_all();
        // a claim taken by a request that began before the drain is handed back too
        if let Err(e) = tasks::hand_back(&self.core).await {
            tracing::warn!(error = %e, "late claims not handed back (they lapse in 15 s)");
        }
        self.core.pool.close();
    }

    /// Ends the replica the way SIGKILL would: nothing handed back (claims lapse, the leader's connection closes).
    pub fn kill(&self) {
        for h in self.loops.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            h.abort();
        }
        self.core.tasks.abort_all();
        drop(self.leader.lock().unwrap_or_else(PoisonError::into_inner).take());
        self.core.pool.close();
    }
}

/// The drain of hermit 20.2 as the binary runs it on SIGTERM: [`drain::Drain`] marks `/readyz` 503 first, then
/// these hooks in order (preStop 10 s + 10 + 35 + 5 s stays inside `terminationGracePeriodSeconds: 90`):
/// 1. steps 2-4: draining (held long-polls answered, warm off), claims handed back, leadership released;
/// 2. step 5: `stop_requests` (the API server's graceful shutdown: in-flight requests finish) and, at the same
///    time, the supervisor and sandboxd calls already sent, both within 35 s;
/// 3. step 6: the loops stopped and the pool closed.
pub fn drain_hooks<F, Fut>(d: &mut drain::Drain, app: Arc<App>, stop_requests: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    const IN_FLIGHT: Duration = Duration::from_secs(35);
    {
        let app = app.clone();
        d.hook(
            "hand back claims and leadership",
            Duration::from_secs(10),
            move || async move {
                let _ = app.begin_drain().await;
            },
        );
    }
    {
        let app = app.clone();
        d.hook("in-flight requests and calls", IN_FLIGHT, move || async move {
            // the hook's own deadline is the same 35 s; the tasks' wait ends a little earlier so they are stopped
            // here rather than abandoned
            let tasks = app.settle_tasks(IN_FLIGHT.saturating_sub(Duration::from_millis(500)));
            tokio::join!(stop_requests(), tasks);
        });
    }
    d.hook("close", Duration::from_secs(5), move || async move {
        app.finish_drain().await;
    });
}

async fn live_loop(core: Arc<Core>) {
    let mut w = core.listener.channel_waiter(db::CHANNEL);
    loop {
        let _ = tokio::time::timeout(core.timings.live_reload, w.changed()).await;
        // a burst of changes is one reload
        tokio::time::sleep(Duration::from_millis(20)).await;
        if let Err(e) = core.live.reload(&core.pool).await {
            tracing::warn!(error = %e, "live map reload failed (serving the last one)");
        }
    }
}

async fn catalogue_loop(core: Arc<Core>) {
    loop {
        tokio::time::sleep(core.timings.catalogue_reload).await;
        let c = core.catalogue.clone();
        let _ = tokio::task::spawn_blocking(move || c.reload()).await;
    }
}

async fn sweep_loop(core: Arc<Core>) {
    loop {
        tokio::time::sleep(core.timings.sweep).await;
        if let Err(e) = tasks::sweep_once(&core).await {
            tracing::warn!(error = %e, "takeover sweep failed");
        }
    }
}
