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
use axum::body::Body;
use axum::response::Response;
use magpie_api::changes::{ChangesQuery, ChangesResponse};
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use magpie_api::events::{EventsPage, EventsQuery};
use magpie_api::files::{FilesPanel, RawFileQuery};
use magpie_api::legacy::{
    LegacyPathRequest, TicketClaim, TicketClaimed, TicketDone, TicketRequest, TicketResponse, WindowState,
};
use magpie_api::live::{LiveFramesRequest, LiveState};
use magpie_api::ops::{
    AdminConversation, AdminConversationList, AdminListQuery, LegacyImportStatus, OpsStats, PeopleQuery,
    PeopleResponse, PersonPurgeRequest, PersonPurgeResponse, ViewAsLog, ViewAsLogQuery, ViewAsRequest,
    WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use magpie_api::rows::{
    PromptsResponse, SearchQuery, SearchResponse, TimelinePage, TimelineQuery, TranscriptPage, TranscriptQuery,
};
use magpie_api::sandbox::{
    AckRequest, AgentTurnRequest, AskReport, AskResolvedReport, CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr,
    CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery, CommandsQuery, CommandsResponse, Heartbeat,
    LeaseResponse, TasksReport, TurnStateReport,
};
use magpie_api::sandbox::{SentFilesCommitted, SentFilesRequest, SentFilesResponse};
use magpie_api::server::{Ctx, Server};
use magpie_api::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use magpie_api::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use magpie_api::uploads::{BlobQuery, SandboxBlobQuery, UploadMeta, UploadResponse};
use magpie_api::{Admitted, Empty};
use uuid::Uuid;

use crate::blobs::Blobs;
use crate::blobs::store::BucketConfig;
use crate::changes::Read;
use crate::fanout::Fanout;
use crate::http::{self, Answer};
use crate::jobs::{self, Jobs, outbox};
use crate::turns::{self, driver::Driver};
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
    /// The bucket (`MAGPIE_S3_*`).
    pub bucket: BucketConfig,
    /// The import Deployment's base URL for the switch window's synchronous ingest: the fixed endpoint name
    /// `http://magpie-import` ([`crate::legacy::ingest::URL`]), another address only in tests.
    pub import_url: String,
    /// `MAGPIE_TOKEN_IMPORT`.
    pub import_token: String,
    /// The switch window's metrics (registered on the ops registry by the binary).
    pub legacy_metrics: crate::legacy::Metrics,
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
    /// The blob store (bucket, read cache, decode permits).
    pub blobs: Arc<Blobs>,
    /// The switch window's metrics.
    pub legacy: crate::legacy::Metrics,
    /// The import Deployment, for the synchronous ingest of an old asset.
    pub ingest: crate::legacy::ingest::Ingest,
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

    /// What M9 needs.
    #[must_use]
    pub fn reader(&self) -> crate::rows::read::Reader<'_> {
        crate::rows::read::Reader {
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
        Self::start_inner(cfg, readiness, draining, intervals, true).await
    }

    /// magpie's library without its jobs (no leader lock, purge, outbox, driver or hermit feed): the importer's
    /// process (`magpie import …`, cutover 7.1), which writes through the same functions as a replica and serves no
    /// API. Waits for the migrations a replica applies (it applies them itself when it is first).
    ///
    /// # Errors
    /// As [`Service::start`].
    pub async fn start_library(cfg: Config, readiness: &Readiness, draining: Draining) -> Result<Self, StartError> {
        Self::start_inner(cfg, readiness, draining, jobs::Intervals::default(), false).await
    }

    async fn start_inner(
        cfg: Config,
        readiness: &Readiness,
        draining: Draining,
        intervals: jobs::Intervals,
        run_jobs: bool,
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
        let ingest = crate::legacy::ingest::Ingest::new(http.clone(), cfg.import_url.clone(), cfg.import_token.clone());
        let hermit = hermit_api::client::Client::new(http, cfg.hermit_url.clone()).bearer(cfg.hermit_token.clone());
        let blobs = Arc::new(Blobs::new(&cfg.bucket, &cfg.database_url).map_err(|e| match e {
            crate::error::Error::Db(d) => StartError(d),
            other => StartError(aviary_pg::Error::Config(other.to_string())),
        })?);
        let jobs = run_jobs.then(|| {
            let mut jobs = Jobs::start_with(&pool, outbox::Hermit(hermit.clone()), blobs.clone(), intervals);
            let driver = Driver {
                pool: pool.clone(),
                hermit: hermit.clone(),
                pod: cfg.pod.clone(),
                draining: draining.clone(),
            };
            jobs.push(turns::sweeper::start(driver.clone()));
            jobs.push(crate::hermit::feed::spawn(jobs.leader(), driver));
            jobs
        });
        Ok(Self {
            app: Arc::new_cyclic(|this| Magpie {
                pool,
                fanout,
                draining,
                pod: cfg.pod,
                hermit,
                this: this.clone(),
                blobs,
                legacy: cfg.legacy_metrics,
                ingest,
            }),
            callers: cfg.callers,
            jobs,
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

    async fn timeline(&self, ctx: Ctx, id: Uuid, query: TimelineQuery) -> Answer<TimelinePage> {
        http::timeline::timeline(self, ctx, id, query).await
    }

    async fn live(&self, ctx: Ctx, id: Uuid) -> Answer<Option<LiveState>> {
        http::timeline::live(self, ctx, id).await
    }

    async fn prompts(&self, ctx: Ctx, id: Uuid) -> Answer<PromptsResponse> {
        http::timeline::prompts(self, ctx, id).await
    }

    async fn search(&self, ctx: Ctx, id: Uuid, query: SearchQuery) -> Answer<SearchResponse> {
        http::timeline::search(self, ctx, id, query).await
    }

    async fn transcript(&self, ctx: Ctx, id: Uuid, query: TranscriptQuery) -> Answer<TranscriptPage> {
        http::timeline::transcript(self, ctx, id, query).await
    }

    async fn live_frames(&self, ctx: Ctx, turn_id: Uuid, body: LiveFramesRequest) -> Answer<()> {
        http::sandbox_live::frames(self, ctx, turn_id, body).await
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

    async fn blob(&self, ctx: Ctx, blob_id: String, query: BlobQuery) -> Answer<Response> {
        http::blobs::blob(self, ctx, blob_id, query).await
    }

    async fn sandbox_blob_get(&self, ctx: Ctx, sha: String, query: SandboxBlobQuery) -> Answer<Response> {
        http::blobs::sandbox_get(self, ctx, sha, query).await
    }

    async fn upload(&self, ctx: Ctx, meta: UploadMeta, body: Body) -> Answer<UploadResponse> {
        http::uploads::upload(self, ctx, meta, body).await
    }

    async fn view_as(&self, ctx: Ctx, body: ViewAsRequest) -> Answer<()> {
        http::viewas::start(self, ctx, body).await
    }

    async fn sandbox_blob_put(&self, ctx: Ctx, sha: String, body: Body) -> Answer<Admitted<Empty>> {
        http::blobs::sandbox_put(self, ctx, sha, body).await
    }

    async fn send_turn(&self, ctx: Ctx, id: Uuid, body: SendRequest) -> Answer<Admitted<SendResponse>> {
        http::turns::send(self, ctx, id, body).await
    }

    async fn cancel_turn(&self, ctx: Ctx, id: Uuid) -> Answer<CancelResponse> {
        http::turns::cancel(self, ctx, id).await
    }

    async fn answer_ask(&self, ctx: Ctx, id: Uuid, ask_id: String, body: AgentPermissionResponse) -> Answer<()> {
        http::turns::answer_ask(self, ctx, id, ask_id, body).await
    }

    async fn commands(&self, ctx: Ctx, query: CommandsQuery) -> Answer<CommandsResponse> {
        http::sandbox_turns::commands(self, ctx, query).await
    }

    async fn ack_command(&self, ctx: Ctx, cmd_seq: u64, body: AckRequest) -> Answer<()> {
        http::sandbox_turns::ack(self, ctx, cmd_seq, body).await
    }

    async fn agent_turn(&self, ctx: Ctx, body: AgentTurnRequest) -> Answer<Admitted<Empty>> {
        http::sandbox_turns::agent_turn(self, ctx, body).await
    }

    async fn turn_state(&self, ctx: Ctx, turn_id: Uuid, body: TurnStateReport) -> Answer<()> {
        http::sandbox_turns::turn_state(self, ctx, turn_id, body).await
    }

    async fn tasks(&self, ctx: Ctx, body: TasksReport) -> Answer<()> {
        http::sandbox_turns::tasks(self, ctx, body).await
    }

    async fn ask_open(&self, ctx: Ctx, body: AskReport) -> Answer<()> {
        http::sandbox_turns::ask_open(self, ctx, body).await
    }

    async fn ask_resolved(&self, ctx: Ctx, ask_id: String, body: AskResolvedReport) -> Answer<()> {
        http::sandbox_turns::ask_resolved(self, ctx, ask_id, body).await
    }

    async fn heartbeat(&self, ctx: Ctx, body: Heartbeat) -> Answer<()> {
        http::sandbox_turns::heartbeat(self, ctx, body).await
    }

    async fn files_panel(&self, ctx: Ctx, id: Uuid) -> Answer<FilesPanel> {
        http::files::panel(self, ctx, id).await
    }

    async fn files_raw(&self, ctx: Ctx, id: Uuid, query: RawFileQuery) -> Answer<Response> {
        http::files::raw(self, ctx, id, query).await
    }

    async fn sent_files(&self, ctx: Ctx, body: SentFilesRequest) -> Answer<SentFilesResponse> {
        http::sandbox_files::sent_files(self, ctx, body).await
    }

    async fn sent_files_commit(&self, ctx: Ctx, tool_use_id: String) -> Answer<SentFilesCommitted> {
        http::sandbox_files::commit(self, ctx, tool_use_id).await
    }

    async fn list_shares(&self, ctx: Ctx, id: Uuid) -> Answer<SharesList> {
        http::shares::list(self, ctx, id).await
    }

    async fn create_share(&self, ctx: Ctx, id: Uuid) -> Answer<ShareRef> {
        http::shares::create(self, ctx, id).await
    }

    async fn revoke_share(&self, ctx: Ctx, share_id: String) -> Answer<()> {
        http::shares::revoke(self, ctx, share_id).await
    }

    async fn get_share(&self, ctx: Ctx, share_id: String) -> Answer<ShareView> {
        http::shares::view(self, ctx, share_id).await
    }

    async fn share_files_raw(&self, ctx: Ctx, share_id: String, query: RawFileQuery) -> Answer<Response> {
        http::shares::files_raw(self, ctx, share_id, query).await
    }

    async fn continue_share(&self, ctx: Ctx, share_id: String) -> Answer<ContinueResponse> {
        http::shares::continue_share(self, ctx, share_id).await
    }

    async fn person_purge(&self, ctx: Ctx, person: String, body: PersonPurgeRequest) -> Answer<PersonPurgeResponse> {
        http::hermit::person_purge(self, ctx, person, body).await
    }

    async fn workspaces_exists(&self, ctx: Ctx, body: WorkspacesExistsRequest) -> Answer<WorkspacesExistsResponse> {
        http::hermit::workspaces_exists(self, ctx, body).await
    }

    async fn legacy_import(&self, ctx: Ctx) -> Answer<LegacyImportStatus> {
        http::hermit::legacy_import(self, ctx).await
    }

    async fn admin_conversations(&self, ctx: Ctx, query: AdminListQuery) -> Answer<AdminConversationList> {
        http::admin::conversations(self, ctx, query).await
    }

    async fn admin_conversation(&self, ctx: Ctx, id: Uuid) -> Answer<AdminConversation> {
        http::admin::conversation(self, ctx, id).await
    }

    async fn admin_events(&self, ctx: Ctx, id: Uuid, query: EventsQuery) -> Answer<EventsPage> {
        http::admin::events(self, ctx, id, query).await
    }

    async fn admin_timeline(&self, ctx: Ctx, id: Uuid, query: TimelineQuery) -> Answer<TimelinePage> {
        http::admin::timeline(self, ctx, id, query).await
    }

    async fn admin_transcript(&self, ctx: Ctx, id: Uuid, query: TranscriptQuery) -> Answer<TranscriptPage> {
        http::admin::transcript(self, ctx, id, query).await
    }

    async fn legacy_ticket(&self, ctx: Ctx, id: Uuid, body: TicketRequest) -> Answer<TicketResponse> {
        http::legacy::ticket(self, ctx, id, body).await
    }

    async fn legacy_ticket_done(&self, ctx: Ctx, ticket: Uuid, body: TicketDone) -> Answer<()> {
        http::legacy::ticket_done(self, ctx, ticket, body).await
    }

    async fn legacy_ticket_claim(&self, ctx: Ctx, ticket: Uuid, body: TicketClaim) -> Answer<TicketClaimed> {
        http::legacy::ticket_claim(self, ctx, ticket, body).await
    }

    async fn upload_legacy_path(&self, ctx: Ctx, upload_id: Uuid, body: LegacyPathRequest) -> Answer<()> {
        http::legacy::upload_legacy_path(self, ctx, upload_id, body).await
    }

    async fn legacy_window(&self, ctx: Ctx) -> Answer<WindowState> {
        http::legacy::window(self, ctx).await
    }

    async fn upload_raw(&self, ctx: Ctx, upload_id: Uuid) -> Answer<Response> {
        http::legacy::upload_raw(self, ctx, upload_id).await
    }
}
