//! The service: its shared state ([`Magpie`]), its start (pool, migrations, LISTEN, jobs) and drain, and the API
//! router: magpie-api's router over [`Magpie`]'s [`Server`] implementation, behind the caller check of
//! [`crate::auth`] and aviary-core's standard layers.
//!
//! Each route's handler is in `crate::http`; a route no package has built yet answers magpie-api's default 501
//! `not_supported`.

use std::sync::Arc;
use std::time::Duration;

use aviary_core::auth::Callers;
use aviary_core::drain::Draining;
use aviary_core::ops::Readiness;
use aviary_pg::Pool;
use axum::Router;
use magpie_api::Admitted;
use magpie_api::changes::{ChangesQuery, ChangesResponse};
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use magpie_api::ops::{OpsStats, PeopleQuery, PeopleResponse, ViewAsLog, ViewAsLogQuery};
use magpie_api::sandbox::{
    CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr, CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery,
    LeaseResponse,
};
use magpie_api::server::{Ctx, Server};
use uuid::Uuid;

use crate::changes::Read;
use crate::fanout::Fanout;
use crate::http::{self, Answer};
use crate::jobs::{self, Jobs, outbox};
use crate::{auth, db, limits};

/// What a replica needs to start: bindings and credentials only (magpie 1: no behaviour switches).
#[derive(Debug, Clone)]
pub struct Config {
    /// `MAGPIE_DATABASE_URL`.
    pub database_url: String,
    /// The callers' pair tokens (`MAGPIE_ACCEPT_*`).
    pub callers: Callers,
    /// hermit's base URL: the fixed endpoint name `http://hermit` (magpie 1), another address only in tests.
    pub hermit_url: String,
    /// `MAGPIE_TOKEN_HERMIT`.
    pub hermit_token: String,
    /// This replica's name (the downward API's `POD_NAME`): driver claims and logs.
    pub pod: String,
}

/// The shared state of one replica.
#[derive(Debug)]
pub struct Magpie {
    /// The request pool.
    pub pool: Pool,
    /// The LISTEN hub.
    pub fanout: Fanout,
    /// Draining (held polls answer at once).
    pub draining: Draining,
    /// This replica's name.
    pub pod: String,
    /// hermit, for calls outside the outbox.
    pub hermit: hermit_api::client::Client,
    /// This replica's state itself, for work that outlives a request (a streamed cc file read).
    this: std::sync::Weak<Magpie>,
}

impl Magpie {
    /// The shared state as an `Arc` (it always lives in one: [`Service::start`] makes it so).
    ///
    /// # Panics
    /// Never while a request is served (the service holds the `Arc`).
    #[must_use]
    pub fn me(&self) -> Arc<Magpie> {
        #[allow(clippy::expect_used)] // the Service owns the Arc for as long as requests are served
        self.this.upgrade().expect("magpie's state outlives its requests")
    }

    /// What the change feed needs.
    #[must_use]
    pub fn read(&self) -> Read<'_> {
        Read {
            pool: &self.pool,
            fanout: &self.fanout,
            draining: &self.draining,
        }
    }
}

/// A started replica: its state, the router's callers and its jobs.
#[derive(Debug)]
pub struct Service {
    /// The shared state.
    pub app: Arc<Magpie>,
    callers: Callers,
    jobs: Option<Jobs>,
}

/// A start that cannot go on (a recorded migration edited, a malformed migration list).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartError(#[from] aviary_pg::Error);

/// How long a replica waits between attempts to reach Postgres at start.
const START_RETRY: Duration = Duration::from_secs(2);

impl Service {
    /// Starts a replica: the pool, the migrations (by whichever replica wins the lock; retried while Postgres
    /// cannot be reached), the LISTEN connection, the jobs. `readiness` gets the `migrations` and `listen` gates,
    /// opened as each is done; the caller marks the start finished.
    ///
    /// # Errors
    /// [`StartError`] for a database URL that does not parse, an edited or malformed migration.
    pub async fn start(cfg: Config, readiness: &Readiness, draining: Draining) -> Result<Self, StartError> {
        Self::start_with(cfg, readiness, draining, jobs::Intervals::default()).await
    }

    /// [`Service::start`] with other job intervals (tests).
    ///
    /// # Errors
    /// As [`Service::start`].
    pub async fn start_with(
        cfg: Config,
        readiness: &Readiness,
        draining: Draining,
        intervals: jobs::Intervals,
    ) -> Result<Self, StartError> {
        let migrations_gate = readiness.gate("migrations");
        let listen_gate = readiness.gate("listen");
        let pool = aviary_pg::connect(
            &cfg.database_url,
            "magpie",
            limits::POOL_SIZE,
            limits::STATEMENT_TIMEOUT,
        )?;
        let list = db::migrations()?;
        loop {
            match aviary_pg::migrate(&pool, &db::migrate_key(), &list).await {
                Ok(applied) => {
                    tracing::info!(applied = ?applied, "migrations verified");
                    break;
                }
                Err(e @ (aviary_pg::Error::MigrationEdited { .. } | aviary_pg::Error::BadMigration(_))) => {
                    return Err(e.into());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "migrations not applied yet, retrying");
                    tokio::time::sleep(START_RETRY).await;
                }
            }
        }
        migrations_gate.open();
        let fanout = Fanout::start(&pool, intervals.requery).map_err(|e| match e {
            crate::error::Error::Db(d) => StartError(d),
            other => StartError(aviary_pg::Error::Config(other.to_string())),
        })?;
        fanout.ready().await;
        listen_gate.open();
        let http = aviary_core::http::client_builder(&aviary_core::version::Build::new("magpie"), None)
            .build()
            .map_err(|e| StartError(aviary_pg::Error::Config(format!("http client: {e}"))))?;
        let hermit = hermit_api::client::Client::new(http, cfg.hermit_url.clone()).bearer(cfg.hermit_token.clone());
        let jobs = Jobs::start_with(&pool, outbox::Hermit(hermit.clone()), intervals);
        Ok(Self {
            app: Arc::new_cyclic(|this| Magpie {
                pool,
                fanout,
                draining,
                pod: cfg.pod,
                hermit,
                this: this.clone(),
            }),
            callers: cfg.callers,
            jobs: Some(jobs),
        })
    }

    /// The API router (port 8080).
    pub fn router(&self) -> Router {
        router(self.app.clone(), self.callers.clone())
    }

    /// Whether this replica holds the job leader lock now.
    #[must_use]
    pub fn is_leader(&self) -> bool {
        self.jobs.as_ref().is_some_and(Jobs::is_leader)
    }

    /// The drain's last steps (magpie 18.3), after the listener stopped and in-flight requests finished: driver
    /// claims handed back (another replica takes the turns at once), the leader lock released, the LISTEN connection
    /// closed, the pool closed.
    pub async fn shutdown(mut self) {
        let released = async {
            self.app
                .pool
                .get()
                .await?
                .execute(
                    "update turns set driver_until = now() where driver = $1 and driver_until > now()",
                    &[&self.app.pod],
                )
                .await
        }
        .await;
        if let Err(e) = released {
            tracing::warn!(error = %e, "driver claims not handed back (they lapse in 30 s)");
        }
        if let Some(jobs) = self.jobs.take() {
            jobs.stop().await;
        }
        // the LISTEN connection before the pool, so its task does not try to reconnect to a closed pool
        self.app.fanout.stop().await;
        self.app.pool.close();
    }
}

/// magpie-api's router over `app`, every route behind the caller check, with aviary-core's standard layers.
pub fn router(app: Arc<Magpie>, callers: Callers) -> Router {
    let api = magpie_api::server::router(app).route_layer(axum::middleware::from_fn_with_state(callers, auth::check));
    aviary_core::http::standard(api, limits::BODY_MAX)
}

impl Server for Magpie {
    async fn list_conversations(&self, ctx: Ctx, query: ListQuery) -> Answer<ListResponse> {
        http::conversations::list(self, ctx, query).await
    }

    async fn get_conversation(&self, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
        http::conversations::get(self, ctx, id).await
    }

    async fn create_conversation(&self, ctx: Ctx, body: CreateRequest) -> Answer<Admitted<CreateResponse>> {
        http::conversations::create(self, ctx, body).await
    }

    async fn get_creation(&self, ctx: Ctx, idempotency_key: String) -> Answer<CreateResponse> {
        http::creations::get(self, ctx, idempotency_key).await
    }

    async fn rename_conversation(&self, ctx: Ctx, id: Uuid, body: RenameRequest) -> Answer<Conversation> {
        http::conversations::rename(self, ctx, id, body).await
    }

    async fn delete_conversation(&self, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
        http::conversations::delete(self, ctx, id).await
    }

    async fn restore_conversation(&self, ctx: Ctx, id: Uuid) -> Answer<Conversation> {
        http::conversations::restore(self, ctx, id).await
    }

    async fn purge_conversation(&self, ctx: Ctx, id: Uuid) -> Answer<()> {
        http::conversations::purge(self, ctx, id).await
    }

    async fn changes(&self, ctx: Ctx, query: ChangesQuery) -> Answer<ChangesResponse> {
        http::changes::changes(self, ctx, query).await
    }

    async fn ops_people(&self, ctx: Ctx, query: PeopleQuery) -> Answer<PeopleResponse> {
        http::ops::people(self, ctx, query).await
    }

    async fn ops_view_as(&self, ctx: Ctx, query: ViewAsLogQuery) -> Answer<ViewAsLog> {
        http::ops::view_as(self, ctx, query).await
    }

    async fn ops_stats(&self, ctx: Ctx) -> Answer<OpsStats> {
        http::ops::stats(self, ctx).await
    }

    async fn lease(&self, ctx: Ctx) -> Answer<LeaseResponse> {
        http::sandbox_cc::lease(self, ctx).await
    }

    async fn cc_tree(&self, ctx: Ctx) -> Answer<CcTree> {
        http::sandbox_cc::tree(self, ctx).await
    }

    async fn cc_file(&self, ctx: Ctx, query: CcFileQuery) -> Answer<axum::response::Response> {
        http::sandbox_cc::file(self.me(), ctx, query).await
    }

    async fn cc_write(&self, ctx: Ctx, query: CcWriteQuery, body: axum::body::Body) -> Answer<CcSizeVersion> {
        http::sandbox_cc::write(self, ctx, query, body).await
    }

    async fn cc_truncate(&self, ctx: Ctx, body: CcTruncate) -> Answer<CcSizeVersion> {
        http::sandbox_cc::truncate(self, ctx, body).await
    }

    async fn cc_rename(&self, ctx: Ctx, body: CcRename) -> Answer<CcVersion> {
        http::sandbox_cc::rename(self, ctx, body).await
    }

    async fn cc_unlink(&self, ctx: Ctx, body: CcPathOp) -> Answer<()> {
        http::sandbox_cc::unlink(self, ctx, body).await
    }

    async fn cc_mkdir(&self, ctx: Ctx, body: CcMkdir) -> Answer<()> {
        http::sandbox_cc::mkdir(self, ctx, body).await
    }

    async fn cc_rmdir(&self, ctx: Ctx, body: CcPathOp) -> Answer<()> {
        http::sandbox_cc::rmdir(self, ctx, body).await
    }

    async fn cc_setattr(&self, ctx: Ctx, body: CcSetattr) -> Answer<CcVersion> {
        http::sandbox_cc::setattr(self, ctx, body).await
    }
}
