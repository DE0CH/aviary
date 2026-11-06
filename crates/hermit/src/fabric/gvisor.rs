//! The YYC fabric (hermit 13): gVisor sandboxes on AWS hosts behind the `WireGuard` gateway. [`Gvisor`] is the
//! [`Fabric`] the binary runs; its parts are the sandboxd clients ([`super::sandboxd`]), placement
//! ([`super::placement`]), the scaler ([`super::scaler`]), EC2 ([`super::ec2`]), network pushes ([`super::network`]),
//! live migration ([`super::migrate`]) and image warm-ups ([`super::images`]).

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use sandboxd_api::{Client, Env, Spec};
use uuid::Uuid;

use crate::Core;
use crate::config::{Config, parse_quantity};
use crate::fabric::ec2::Ec2;
use crate::fabric::hosts::{self, HostRow, HostStatus, Sizing};
use crate::fabric::network::{self, EgressSource};
use crate::fabric::sandboxd::{self, Clients, Connect};
use crate::fabric::{CreateError, CreateSpec, Fabric, FabricError, Fut, Launch, Place, migrate, placement, scaler};
use crate::tasks::Claim;

/// The secrets a new AWS host gets in its user data (hermit 13.5): the shared host server certificate and the CA
/// that signs hermit's client certificate, and the `JuiceFS` metadata URL and password.
#[derive(Clone)]
pub struct HostSecrets {
    pub ca: String,
    pub cert: String,
    pub key: String,
    pub juicefs_meta_url: String,
    pub juicefs_meta_password: String,
}

impl std::fmt::Debug for HostSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostSecrets(..)")
    }
}

/// The supervisor's control address of a sandbox (`http://<ip>:7070` on the real fabric; tests map addresses to
/// local fake supervisors).
pub trait ControlUrl: Send + Sync + 'static {
    fn url(&self, ip: IpAddr) -> String;
}

/// `http://<ip>:7070`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Port7070;

impl ControlUrl for Port7070 {
    fn url(&self, ip: IpAddr) -> String {
        format!("http://{}", std::net::SocketAddr::new(ip, supervisor_api::CONTROL_PORT))
    }
}

/// What the fabric is built from: everything that talks to the outside world, so tests replace it.
pub struct Deps {
    pub connect: Arc<dyn Connect>,
    /// `None`: no AWS credentials, launches answer `off`.
    pub ec2: Option<Arc<dyn Ec2>>,
    pub egress: Arc<dyn EgressSource>,
    /// `None`: launches answer `off` (a host could not join).
    pub host_secrets: Option<HostSecrets>,
    pub control: Arc<dyn ControlUrl>,
}

impl std::fmt::Debug for Deps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deps").finish_non_exhaustive()
    }
}

/// The YYC fabric.
pub struct Gvisor {
    core: OnceLock<Weak<Core>>,
    pub(crate) clients: Clients,
    pub(crate) ec2: Option<Arc<dyn Ec2>>,
    pub(crate) egress: Arc<dyn EgressSource>,
    pub(crate) host_secrets: Option<HostSecrets>,
    control: Arc<dyn ControlUrl>,
    /// Since when the gateway has answered every heartbeat (unix ms), 0 while it does not: the leader's own view,
    /// and every other replica's copy of it from the gateway's row.
    pub(crate) gateway_up_since: AtomicI64,
    /// The gateway's loss is reported once per outage.
    pub(crate) gateway_lost_reported: AtomicBool,
    /// When the last EC2 reconcile and orphan sweep ran (at most every [`crate::Timings::reconcile`]).
    pub(crate) last_reconcile: Mutex<Option<tokio::time::Instant>>,
    /// Migrations the scaler just started on this replica (the task runs from step 2; a takeover asks the target).
    pub(crate) fresh_migrations: Mutex<HashSet<Uuid>>,
    /// The sandbox image digest the gateway holds or is pulling ([`super::images::warm_gateway`]); `None` until then
    /// and again after a failed pull, so the next pass asks again.
    pub(crate) gateway_image: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Gvisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gvisor").finish_non_exhaustive()
    }
}

impl Gvisor {
    #[must_use]
    pub fn new(d: Deps) -> Self {
        Self {
            core: OnceLock::new(),
            clients: Clients::new(d.connect),
            ec2: d.ec2,
            egress: d.egress,
            host_secrets: d.host_secrets,
            control: d.control,
            gateway_up_since: AtomicI64::new(0),
            gateway_lost_reported: AtomicBool::new(false),
            last_reconcile: Mutex::new(None),
            fresh_migrations: Mutex::default(),
            gateway_image: Arc::default(),
        }
    }

    /// The replica's core (set by [`Fabric::attach`] when the replica starts).
    pub(crate) fn core(&self) -> Result<Arc<Core>, FabricError> {
        self.core
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| FabricError("the fabric is not attached to a replica".into()))
    }

    /// The client of a host row.
    ///
    /// # Errors
    /// When the client cannot be built.
    pub fn client(&self, h: &HostRow) -> Result<Client, sandboxd_api::Error> {
        self.clients.of(h)
    }

    /// The slot sizes from the configuration.
    #[must_use]
    pub fn sizing(cfg: &Config) -> Sizing {
        let f = &cfg.file.fabric;
        Sizing {
            memory: cfg.memory_bytes,
            cpu: cfg.file.sandbox.cpu,
            overcommit: f.cpu_overcommit,
            min_available: parse_quantity(&f.min_available_memory).unwrap_or(256 << 20),
        }
    }

    /// Pushes the gateway's peers from the rows (13.8).
    ///
    /// # Errors
    /// A database error or the gateway's answer.
    pub async fn push_gateway_peers(&self, core: &Core) -> Result<(), FabricError> {
        push_gateway_peers(&self.clients, core).await
    }

    /// Whether the gateway has answered for `d` without a gap.
    #[must_use]
    pub fn gateway_up_for(&self, d: Duration) -> bool {
        let since = self.gateway_up_since.load(Ordering::Acquire);
        since > 0 && now_ms() - since >= i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
    }

    /// Sets the gateway's "answering since" (tests use it to skip the settle time).
    pub fn set_gateway_up_since(&self, unix_ms: i64) {
        self.gateway_up_since.store(unix_ms, Ordering::Release);
    }
}

/// Pushes the gateway's peers from the rows (13.8): after every placement and committed migration, and in each
/// pass. Never inside a transaction.
///
/// # Errors
/// A database error or the gateway's answer.
pub async fn push_gateway_peers(clients: &Clients, core: &Core) -> Result<(), FabricError> {
    let hosts = hosts::live(&core.pool).await?;
    let Some(gw) = hosts.iter().find(|h| h.is_gateway()) else {
        return Ok(());
    };
    let placed: Vec<(String, IpAddr)> = core
        .pool
        .get()
        .await?
        .query(
            "select host, ip from sandboxes where status <> 'gone' and host is not null and ip is not null",
            &[],
        )
        .await?
        .iter()
        .filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?)))
        .collect();
    let peers = network::gateway_peers(&hosts, &placed);
    clients
        .of(gw)
        .map_err(|e| FabricError(e.to_string()))?
        .set_peers(peers)
        .await
        .map_err(|e| FabricError(format!("gateway peers: {e}")))?;
    Ok(())
}

/// Now as unix milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

/// `sha256:<hex>` -> `<hex>` (sandboxd takes the bare digest).
#[must_use]
pub fn bare_digest(d: &str) -> &str {
    d.strip_prefix("sha256:").unwrap_or(d)
}

impl Fabric for Gvisor {
    fn attach(&self, core: &Arc<Core>) {
        let _ = self.core.set(Arc::downgrade(core));
    }

    fn place(&self, id: Uuid, memory_bytes: u64) -> Fut<'_, Result<Place, FabricError>> {
        Box::pin(async move {
            let core = self.core()?;
            let f = &core.cfg.file.fabric;
            placement::place(
                &core.pool,
                Self::sizing(&core.cfg),
                Config::secs(f.host_dead_seconds),
                &f.sandbox_cidr,
                id,
                memory_bytes,
            )
            .await
        })
    }

    fn request_host(&self) -> Fut<'_, Launch> {
        Box::pin(async move {
            let Ok(core) = self.core() else {
                return Launch::Off;
            };
            match scaler::launch(self, &core, "a sandbox waits for a host", false).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "launch failed");
                    Launch::Planned
                }
            }
        })
    }

    fn create<'a>(&'a self, spec: &'a CreateSpec) -> Fut<'a, Result<(), CreateError>> {
        Box::pin(async move {
            let core = self.core().map_err(|e| CreateError::Unreachable(e.0))?;
            let host = hosts::get(&core.pool, &spec.host)
                .await
                .map_err(|e| CreateError::Unreachable(e.to_string()))?
                .filter(|h| h.status != HostStatus::Gone)
                .ok_or_else(|| CreateError::Failed {
                    class: "host_gone".into(),
                    message: format!("host {} is gone", spec.host),
                })?;
            let client = self.client(&host).map_err(|e| sandboxd::create_error(&e))?;
            let s = Spec {
                name: spec.name.clone(),
                ip: spec.ip.to_string(),
                image: bare_digest(&spec.image).to_owned(),
                image_ref: Some(spec.image_ref.clone()),
                env: Env(spec.env.clone()),
                cpu: f64::from(spec.cpu),
                memory: spec.memory_bytes,
                cpu_features: core.cfg.file.fabric.cpu_features.clone(),
                workspace: Some(spec.workspace.to_string()),
                pids: Some(spec.pids),
            };
            client.create(&s).await.map_err(|e| sandboxd::create_error(&e))?;
            // the gateway routes the new /32 to its host
            if let Err(e) = self.push_gateway_peers(&core).await {
                tracing::warn!(sandbox = %spec.sandbox_id, error = %e, "gateway peers after a create (the next pass pushes them)");
            }
            Ok(())
        })
    }

    fn control_url(&self, _host: &str, ip: IpAddr) -> String {
        self.control.url(ip)
    }

    fn console_tail<'a>(&'a self, host: &'a str, name: &'a str) -> Fut<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let core = self.core().ok()?;
            let h = hosts::get(&core.pool, host).await.ok()??;
            if h.status == HostStatus::Gone {
                return None;
            }
            self.client(&h).ok()?.log(name, sandboxd_api::LOG_TAIL_MAX).await.ok()
        })
    }

    fn destroy<'a>(
        &'a self,
        host: Option<&'a str>,
        also: Option<&'a str>,
        name: &'a str,
    ) -> Fut<'a, Result<(), FabricError>> {
        Box::pin(async move {
            let core = self.core()?;
            let live: Vec<HostRow> = hosts::live(&core.pool)
                .await?
                .into_iter()
                .filter(|h| !h.is_gateway())
                .collect();
            let on = |n: &str| live.iter().find(|h| h.name == n).cloned();
            // its own host while it lives; every reachable host when it is unknown or gone
            let mut targets: Vec<(HostRow, bool)> = match host.and_then(on) {
                Some(h) => vec![(h, true)],
                None => live.iter().cloned().map(|h| (h, false)).collect(),
            };
            if let Some(t) = also.and_then(on)
                && !targets.iter().any(|(h, _)| h.name == t.name)
            {
                targets.push((t, true));
            }
            let mut failed = Vec::new();
            for (h, must) in targets {
                let r = match self.client(&h) {
                    Ok(c) => c.delete(name).await.map(|_| ()),
                    Err(e) => Err(e),
                };
                match r {
                    // a host that does not answer while it was only searched contributes nothing
                    Err(e) if must && sandboxd::status(&e) != Some(404) => failed.push(format!("{}: {e}", h.name)),
                    Ok(()) | Err(_) => {}
                }
            }
            if failed.is_empty() {
                Ok(())
            } else {
                Err(FabricError(failed.join("; ")))
            }
        })
    }

    fn gateway_settled(&self) -> bool {
        let settle = self.core().map_or(Duration::from_secs(120), |c| {
            Config::secs(c.cfg.file.fabric.gateway_settle_seconds)
        });
        self.gateway_up_for(settle)
    }

    fn tick<'a>(&'a self, core: &'a Arc<Core>, leader: bool) -> Fut<'a, ()> {
        Box::pin(async move {
            if leader {
                match scaler::pass(self, core).await {
                    Ok(r) if !r.is_quiet() => tracing::info!(?r, "scaler pass"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "scaler pass failed"),
                }
            } else if let Err(e) = scaler::follow(self, core).await {
                tracing::warn!(error = %e, "gateway state not read");
            }
        })
    }

    fn ops(&self) -> Fut<'_, Result<crate::fabric::FabricOps, String>> {
        Box::pin(async move {
            let core = self.core().map_err(|e| e.to_string())?;
            scaler::ops(&core).await.map_err(|e| e.to_string())
        })
    }

    fn migrate(&self, core: Arc<Core>, claim: Claim) -> Fut<'static, ()> {
        let fresh = self
            .fresh_migrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&claim.id);
        Box::pin(migrate::run(core, claim, fresh, self.clients.clone()))
    }
}
