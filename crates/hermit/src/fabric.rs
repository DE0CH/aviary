//! What the core needs from the place sandboxes run and from the workspace store (hermit 8.1): one provider each in
//! the YYC implementation (the gVisor fabric of hermit 13, WP-19; `JuiceFS` workspaces of hermit 15, WP-20). The
//! traits exist only so tests inject fakes: no configuration selects an implementation.
//!
//! Every method is a network call (sandboxd, EC2, `JuiceFS`) and is never made inside a transaction.
//!
//! The fabric itself is [`gvisor::Gvisor`] with its parts in the submodules.

pub mod ec2;
pub mod gvisor;
pub mod hosts;
pub mod images;
pub mod migrate;
pub mod network;
pub mod packing;
pub mod placement;
pub mod sandboxd;
pub mod scaler;

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use hermit_api::error::ApiError;
use hermit_api::workspaces::{ByteRange, Entries, FileContent, Workspace};
use uuid::Uuid;

/// A boxed future, so the traits are object-safe (`Arc<dyn Fabric>`).
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where a sandbox was placed (start step 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub host: String,
    pub ip: IpAddr,
}

/// The answer of a placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Placed(Placement),
    /// No host has a free slot: the start asks for a host (phase `host`).
    NoSlot,
}

/// The answer of a host launch request (hermit 13.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Launched,
    /// A booting host already covers it.
    Planned,
    /// At the cap (`maxHosts`).
    Full,
    /// Launches are off (`maxHosts` 0 or sandboxes disabled).
    Off,
}

/// A sandbox to create (start step 5).
#[derive(Clone)]
pub struct CreateSpec {
    pub name: String,
    pub sandbox_id: Uuid,
    pub host: String,
    pub ip: IpAddr,
    /// `sha256:<64 hex>`.
    pub image: String,
    /// The pinned reference (`<repo>@sha256:…`).
    pub image_ref: String,
    /// The boot environment of hermit 5.1 (it carries the sandbox token).
    pub env: Vec<(String, String)>,
    pub cpu: f32,
    pub memory_bytes: u64,
    pub pids: u32,
    pub open_files: u32,
    /// The conversation's workspace.
    pub workspace: Uuid,
}

impl std::fmt::Debug for CreateSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSpec")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("ip", &self.ip)
            .field("image", &self.image)
            .field("env", &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Why a create failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CreateError {
    /// sandboxd's 503 `busy`: retried every 3 s within the deadline.
    #[error("host busy")]
    Busy,
    /// 409: the address is held by a stale sandbox (destroyed, then retried once).
    #[error("address in use by {0}")]
    IpInUse(String),
    /// sandboxd could not be reached or did not answer in time (a transport error or a timeout): retried, up to 3
    /// tries in all, before `start_failed` (note `unreachable`).
    #[error("sandboxd unreachable: {0}")]
    Unreachable(String),
    /// A definitive sandboxd answer (any other 4xx/5xx): the start fails at once with `start_failed` and `class` as
    /// its note.
    #[error("{class}: {message}")]
    Failed { class: String, message: String },
}

/// A fabric failure (placement, destroy).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FabricError(pub String);

impl From<aviary_pg::Error> for FabricError {
    fn from(e: aviary_pg::Error) -> Self {
        Self(format!("database: {e}"))
    }
}

impl From<aviary_pg::reexports::tokio_postgres::Error> for FabricError {
    fn from(e: aviary_pg::reexports::tokio_postgres::Error) -> Self {
        Self(format!("database: {e}"))
    }
}

/// Where sandboxes run.
pub trait Fabric: Send + Sync + 'static {
    /// Chooses a host and an address for sandbox `id` (step 3). The core records them on the row; an address the
    /// unique index refuses (a race) is placed again.
    fn place(&self, id: Uuid, memory_bytes: u64) -> Fut<'_, Result<Place, FabricError>>;
    /// Asks for a host because a start found no slot (13.5).
    fn request_host(&self) -> Fut<'_, Launch>;
    /// Creates the sandbox on its host (sandboxd `POST /v1/sandboxes`, plus the gateway's peers). Creating a
    /// running sandbox of the same name and address again is a success.
    fn create<'a>(&'a self, spec: &'a CreateSpec) -> Fut<'a, Result<(), CreateError>>;
    /// The base URL of the sandbox's control port (`http://<ip>:7070` on the real fabric).
    fn control_url(&self, host: &str, ip: IpAddr) -> String;
    /// The last bytes of the sandbox's console, best effort (5 s).
    fn console_tail<'a>(&'a self, host: &'a str, name: &'a str) -> Fut<'a, Option<Vec<u8>>>;
    /// Destroys the sandbox `name` on `host` (or on every reachable host when `None`), and on `also` (a migration
    /// target). A sandbox that is already gone is fine.
    fn destroy<'a>(
        &'a self,
        host: Option<&'a str>,
        also: Option<&'a str>,
        name: &'a str,
    ) -> Fut<'a, Result<(), FabricError>>;
    /// Whether the gateway has answered every heartbeat for the last `gatewaySettleSeconds` (hermit 12 step 3,
    /// 13.12 step 4): while it is down or settling, silence proves nothing and nothing is judged lost.
    fn gateway_settled(&self) -> bool;
    /// Called once the replica's core exists (the fabric reads the database and the configuration through it).
    fn attach(&self, _core: &Arc<crate::Core>) {}
    /// One scaler tick (every [`crate::Timings::scaler`]): the pass of hermit 13.4 when `leader` (this replica holds
    /// lock 7300/4), otherwise only what every replica reads (the gateway's state).
    fn tick<'a>(&'a self, _core: &'a Arc<crate::Core>, _leader: bool) -> Fut<'a, ()> {
        Box::pin(async {})
    }
    /// The `migrate` task of a claim this replica holds (13.10). A fabric without live migration clears the claim,
    /// so the row is judged again by its state.
    fn migrate(&self, core: Arc<crate::Core>, claim: crate::tasks::Claim) -> Fut<'static, ()> {
        Box::pin(async move {
            tracing::warn!(sandbox = %claim.id, "migrate task without live migration: claim cleared");
            let _ = crate::tasks::clear(&core, &claim).await;
        })
    }
    /// What the operator page shows of the fabric (hermit 18.3, 18.6): the scaler's plan, the egress targets, the
    /// slots per host. Nothing known by default. An error is the snapshot's `errors.fabricOps`.
    fn ops(&self) -> Fut<'_, Result<FabricOps, String>> {
        Box::pin(async { Ok(FabricOps::default()) })
    }
}

/// The scaler's plan as the page shows it (hermit 13.4, 18.3 `scaler.plan`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScalerPlan {
    /// Slots needed now.
    pub need: u32,
    /// People typing (warm hints counted).
    pub demand: u32,
    /// Starts waiting for a host.
    pub waiting: u32,
    /// Hosts the plan launches.
    pub planned: u32,
    /// Free slots on ready hosts.
    pub available: u32,
    /// Slots on booting hosts.
    pub pending: u32,
    /// Slots per host.
    pub per_host: u32,
}

/// The fabric's part of the operator page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FabricOps {
    pub plan: Option<ScalerPlan>,
    /// When the scaler leader last finished a pass (a problem after 30 s, 18.6).
    pub last_pass_at: Option<hermit_api::Timestamp>,
    /// How many egress targets the gateway has now (0 is a problem, 18.6).
    pub egress_targets: Option<usize>,
    /// Sandbox slots per live host name.
    pub slots: std::collections::BTreeMap<String, u32>,
}

/// Why a workspace call failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The store is not usable (503 `workspace_unavailable`): a CLI or metadata failure, a refused delete.
    #[error("workspace store unavailable: {0}")]
    Unavailable(String),
    /// This replica's mount is broken or missing (15.6): 503 `workspace_unavailable`, and a start at its workspace
    /// step hands over to a replica whose mount works.
    #[error("workspace store not mounted: {0}")]
    NotMounted(String),
    /// A contract answer (`no_workspace`, `no_path`, `not_a_dir`, `exists`, 416, …).
    #[error("{0:?}")]
    Api(ApiError),
}

impl From<WorkspaceError> for ApiError {
    fn from(e: WorkspaceError) -> Self {
        match e {
            WorkspaceError::Unavailable(m) | WorkspaceError::NotMounted(m) => {
                hermit_api::error::api(503, hermit_api::error::codes::WORKSPACE_UNAVAILABLE, m).retry_after(2)
            }
            WorkspaceError::Api(a) => a,
        }
    }
}

/// What the ops verdict needs to know of a replica's workspace store (hermit 15.4, 15.6, 18.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreHealth {
    /// Why the mount is unusable, while it is.
    pub unmounted: Option<String>,
    /// The last delete refused for an entry hermit may not remove (when, which workspace path).
    pub delete_refused: Option<(hermit_api::Timestamp, String)>,
}

/// `/workspace` storage (hermit 3.8, 15). Paths reaching it are already checked against 3.8's rules; the
/// orchestration around clone and delete (the flush, the release first) is the core's.
pub trait Workspaces: Send + Sync + 'static {
    /// Makes the workspace if absent and (re)sets its quota; `true` when it was made.
    fn ensure(&self, wid: Uuid) -> Fut<'_, Result<(Workspace, bool), WorkspaceError>>;
    fn get(&self, wid: Uuid) -> Fut<'_, Result<Option<Workspace>, WorkspaceError>>;
    fn entries<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        cursor: Option<&'a str>,
    ) -> Fut<'a, Result<Entries, WorkspaceError>>;
    fn file<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        range: Option<ByteRange>,
    ) -> Fut<'a, Result<FileContent, WorkspaceError>>;
    /// A copy-on-write copy of `from` with its own quota; `exists` when `wid` exists, `no_workspace` without `from`.
    fn clone_workspace(&self, wid: Uuid, from: Uuid) -> Fut<'_, Result<Workspace, WorkspaceError>>;
    /// Removes the tree and its quota (absent is fine).
    fn delete(&self, wid: Uuid) -> Fut<'_, Result<(), WorkspaceError>>;
    /// The store's state as this replica sees it (fine unless a store says otherwise).
    fn health(&self) -> StoreHealth {
        StoreHealth::default()
    }
    /// Checks the store now (the mount) and answers [`Workspaces::health`].
    fn probe(&self) -> Fut<'_, StoreHealth> {
        Box::pin(async { self.health() })
    }
}

/// The fabric and the workspace store a replica uses.
#[derive(Clone)]
pub struct Providers {
    pub fabric: Arc<dyn Fabric>,
    pub workspaces: Arc<dyn Workspaces>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Providers(..)")
    }
}

/// The providers of a build that has neither the fabric nor the workspace store yet (until WP-19 and WP-20 land):
/// every start fails with `start_failed` (`fabric_unavailable`) and every workspace call answers 503.
#[derive(Debug, Default)]
pub struct Unbuilt;

impl Fabric for Unbuilt {
    fn place(&self, _id: Uuid, _memory: u64) -> Fut<'_, Result<Place, FabricError>> {
        Box::pin(async { Err(FabricError("the fabric is not built yet".into())) })
    }
    fn request_host(&self) -> Fut<'_, Launch> {
        Box::pin(async { Launch::Off })
    }
    fn create<'a>(&'a self, _spec: &'a CreateSpec) -> Fut<'a, Result<(), CreateError>> {
        Box::pin(async {
            Err(CreateError::Failed {
                class: "fabric_unavailable".into(),
                message: "the fabric is not built yet".into(),
            })
        })
    }
    fn control_url(&self, _host: &str, ip: IpAddr) -> String {
        format!("http://{}", std::net::SocketAddr::new(ip, 7070))
    }
    fn console_tail<'a>(&'a self, _host: &'a str, _name: &'a str) -> Fut<'a, Option<Vec<u8>>> {
        Box::pin(async { None })
    }
    fn destroy<'a>(
        &'a self,
        _host: Option<&'a str>,
        _also: Option<&'a str>,
        _name: &'a str,
    ) -> Fut<'a, Result<(), FabricError>> {
        Box::pin(async { Ok(()) })
    }
    fn gateway_settled(&self) -> bool {
        false
    }
}

impl Workspaces for Unbuilt {
    fn ensure(&self, _wid: Uuid) -> Fut<'_, Result<(Workspace, bool), WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
    fn get(&self, _wid: Uuid) -> Fut<'_, Result<Option<Workspace>, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
    fn entries<'a>(
        &'a self,
        _wid: Uuid,
        _path: &'a str,
        _cursor: Option<&'a str>,
    ) -> Fut<'a, Result<Entries, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
    fn file<'a>(
        &'a self,
        _wid: Uuid,
        _path: &'a str,
        _range: Option<ByteRange>,
    ) -> Fut<'a, Result<FileContent, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
    fn clone_workspace(&self, _wid: Uuid, _from: Uuid) -> Fut<'_, Result<Workspace, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
    fn delete(&self, _wid: Uuid) -> Fut<'_, Result<(), WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "the workspace store is not built yet".into(),
            ))
        })
    }
}
