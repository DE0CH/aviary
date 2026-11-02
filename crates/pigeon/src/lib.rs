//! pigeon, the stateless gateway the web and iOS apps talk to (pigeon spec 1-6, 10-14).
//!
//! - [`identity`]: Cloudflare Access tokens (any team hermit names), who may use the deployment (hermit), first
//!   names, and the request identity of spec 4.6.
//! - [`http`]: the routing table of 5.1 on port 8080, path normalisation, the apps' error shape, same-origin rules,
//!   the model list and the warm hint.
//! - [`static_files`] and [`shell`]: the app bundle at `/srv/app` and the rewritten `index.html`.
//! - [`signin`], [`viewas`], [`ops`], [`uploads`], [`blobs`], [`shares`]: the REST routes beside the socket.
//! - [`server`]: the 8080 listener (timeouts, connection tracking, the drain of 11.2).
//! - [`paseo`]: the `/paseo-ws` protocol (spec 7): transport, handshake, dispatch, pushes, the row and object
//!   projections, the strings; [`feeds`]: live streaming from magpie (spec 8).
//!
//! pigeon holds no durable state: everything in memory is a cache of an upstream answer. It reaches its upstreams by
//! fixed names ([`Upstreams::cluster`]); its only configuration is the secrets of [`Secrets::from_env`].

pub mod blobs;
pub mod feeds;
pub mod http;
pub mod identity;
pub mod metrics;
pub mod ops;
pub mod paseo;
pub mod server;
pub mod shares;
pub mod shell;
pub mod signin;
pub mod static_files;
pub mod uploads;
pub mod viewas;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aviary_core::version::{Build, Protocol};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The app bundle's mount point (spec 17.1).
pub const APP_DIR: &str = "/srv/app";
/// The public port (spec 3.2).
pub const PORT: u16 = 8080;

/// The upload client's read timeout, which reqwest runs from the request's start to the answer's headers: far past
/// any upload, so only the upload's own idle and first-byte bounds end one (uploads.rs).
const UPLOAD_READ_BACKSTOP: Duration = Duration::from_hours(24);

/// Where pigeon finds its upstreams. In a deployment these are the fixed names of [`Upstreams::cluster`]; tests point
/// the same fields at local fakes.
#[derive(Clone)]
pub struct Upstreams {
    /// magpie's base URL.
    pub magpie: String,
    /// hermit's base URL.
    pub hermit: String,
    /// The model list's base URL (egress's in-cluster API).
    pub model: String,
    /// The origin of a Cloudflare Access team's endpoints (`/cdn-cgi/access/*`), from its team domain.
    pub access: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl std::fmt::Debug for Upstreams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstreams")
            .field("magpie", &self.magpie)
            .field("hermit", &self.hermit)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Upstreams {
    /// The fixed names of every deployment (spec 1.1): `http://magpie`, `http://hermit`, `http://model`, and
    /// `https://<team domain>` for Access.
    #[must_use]
    pub fn cluster() -> Self {
        Self {
            magpie: "http://magpie".into(),
            hermit: "http://hermit".into(),
            model: "http://model".into(),
            access: Arc::new(|team| format!("https://{team}")),
        }
    }
}

/// An HMAC key list: the first signs, every one verifies (spec 3.3).
#[derive(Clone)]
pub struct KeyList(pub Vec<[u8; 32]>);

impl std::fmt::Debug for KeyList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyList({} keys)", self.0.len())
    }
}

impl KeyList {
    /// Parses a comma-separated list of base64url 32-byte keys.
    ///
    /// # Errors
    /// When the list is empty or an entry is not 32 bytes of base64url.
    pub fn parse(entries: &[String]) -> Result<Self, &'static str> {
        let mut keys = Vec::new();
        for e in entries {
            let e = e.trim();
            if e.is_empty() {
                continue;
            }
            let bytes = URL_SAFE_NO_PAD
                .decode(e.trim_end_matches('='))
                .map_err(|_| "an entry is not base64url")?;
            let key: [u8; 32] = bytes.try_into().map_err(|_| "an entry is not 32 bytes")?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Err("no key");
        }
        Ok(Self(keys))
    }
}

/// pigeon's environment: secrets only (spec 3.3).
#[derive(Clone)]
pub struct Secrets {
    /// `PIGEON_TOKEN_MAGPIE`.
    pub token_magpie: String,
    /// `PIGEON_TOKEN_HERMIT`.
    pub token_hermit: String,
    /// `PIGEON_TOKEN_MODEL`.
    pub token_model: String,
    /// `PIGEON_VIEW_AS_KEYS`.
    pub view_as_keys: KeyList,
    /// `PIGEON_BLOB_KEYS`.
    pub blob_keys: KeyList,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets").finish_non_exhaustive()
    }
}

impl Secrets {
    /// Reads every variable; a missing or empty one, or a key list that does not parse, ends start (exit 78) with
    /// a log line naming the variable, never its value.
    #[must_use]
    pub fn from_env() -> Self {
        let keys = |name: &str| match KeyList::parse(&aviary_core::env::required_list(name)) {
            Ok(k) => k,
            Err(why) => {
                tracing::error!(name, why, "bad env");
                std::process::exit(aviary_core::env::EXIT_CONFIG)
            }
        };
        Self {
            token_magpie: aviary_core::env::required("PIGEON_TOKEN_MAGPIE"),
            token_hermit: aviary_core::env::required("PIGEON_TOKEN_HERMIT"),
            token_model: aviary_core::env::required("PIGEON_TOKEN_MODEL"),
            view_as_keys: keys("PIGEON_VIEW_AS_KEYS"),
            blob_keys: keys("PIGEON_BLOB_KEYS"),
        }
    }
}

/// The timings of spec 13 that tests shorten. [`Tuning::default`] is the specification; a deployment never
/// changes it.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Deployment facts re-read period (60 s).
    pub facts_every: Duration,
    /// Retry period before the first successful facts or JWKS read (1 s).
    pub start_retry: Duration,
    /// A team's JWKS is re-read after this (1 h).
    pub jwks_ttl: Duration,
    /// An unknown `kid` refetches at most this often per team (30 s).
    pub jwks_cooldown: Duration,
    /// A verified token is trusted without the RSA check for this long, at most until its `exp` (60 s).
    pub token_cache: Duration,
    /// A hermit people answer is fresh this long (15 s).
    pub people_fresh: Duration,
    /// …and used after that while hermit fails (300 s).
    pub people_stale: Duration,
    /// A first name is kept this long (6 h)…
    pub first_name_ok: Duration,
    /// …or this long after a failed get-identity (5 min).
    pub first_name_err: Duration,
    /// The model list is re-read after this (60 s).
    pub model_ttl: Duration,
    /// One warm hint per (person, conversation) per this (30 s).
    pub warm_every: Duration,
    /// The longest an upload or download may run after SIGTERM (300 s).
    pub transfer_bound: Duration,
    /// A transfer's longest wait for its next bytes (60 s).
    pub transfer_idle: Duration,
    /// The server's WebSocket ping period (30 s, 7.1.3).
    pub socket_ping: Duration,
    /// The open-socket re-check period (60 s, 7.1.4).
    pub socket_recheck: Duration,
    /// A long-poll's `waitMs` (25 s, 8.1).
    pub poll_wait: Duration,
    /// The pollers' backoff after an error: first wait (0.5 s)…
    pub poll_backoff_min: Duration,
    /// …doubling up to this (5 s, 8.2).
    pub poll_backoff_max: Duration,
    /// A poller outlives its last socket by this (30 s, 8.2).
    pub poller_linger: Duration,
    /// The longest a turn event waits for its rows (2 s, 8.4).
    pub turn_hold: Duration,
    /// Sockets close within this after SIGTERM (15 s, 11.2 step 3)…
    pub drain_jitter: Duration,
    /// …each after its in-flight requests, at most this (10 s).
    pub drain_rpc_wait: Duration,
    /// A socket is request-free after this long with nothing but heartbeats (5 s, DESIGN 9.3).
    pub socket_quiet: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            facts_every: Duration::from_secs(60),
            start_retry: Duration::from_secs(1),
            jwks_ttl: Duration::from_hours(1),
            jwks_cooldown: Duration::from_secs(30),
            token_cache: Duration::from_secs(60),
            people_fresh: Duration::from_secs(15),
            people_stale: Duration::from_secs(300),
            first_name_ok: Duration::from_hours(6),
            first_name_err: Duration::from_mins(5),
            model_ttl: Duration::from_secs(60),
            warm_every: Duration::from_secs(30),
            transfer_bound: Duration::from_secs(300),
            transfer_idle: Duration::from_secs(60),
            socket_ping: Duration::from_secs(30),
            socket_recheck: Duration::from_secs(60),
            poll_wait: Duration::from_secs(25),
            poll_backoff_min: Duration::from_millis(500),
            poll_backoff_max: Duration::from_secs(5),
            poller_linger: Duration::from_secs(30),
            turn_hold: Duration::from_secs(2),
            drain_jitter: Duration::from_secs(15),
            drain_rpc_wait: Duration::from_secs(10),
            socket_quiet: Duration::from_secs(5),
        }
    }
}

/// Everything [`App::new`] needs.
#[derive(Debug, Clone)]
pub struct Config {
    /// The build (`server: pigeon/<sha12>`, `User-Agent`).
    pub build: Build,
    /// The upstreams.
    pub upstreams: Upstreams,
    /// The secrets.
    pub secrets: Secrets,
    /// The app bundle's directory.
    pub app_dir: PathBuf,
    /// The timings.
    pub tuning: Tuning,
}

impl Config {
    /// A deployment's configuration: the fixed names, the secrets from the environment, `/srv/app`, spec timings.
    #[must_use]
    pub fn from_env(build: Build) -> Self {
        Self {
            build,
            upstreams: Upstreams::cluster(),
            secrets: Secrets::from_env(),
            app_dir: PathBuf::from(APP_DIR),
            tuning: Tuning::default(),
        }
    }
}

/// The application state every handler shares. Cheap to clone.
#[derive(Clone)]
pub struct App(Arc<Inner>);

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct Inner {
    pub(crate) build: Build,
    pub(crate) tuning: Tuning,
    pub(crate) secrets: Secrets,
    pub(crate) upstreams: Upstreams,
    /// magpie for small calls (15 s per call).
    pub(crate) magpie: magpie_api::client::Client,
    /// magpie for sends and creates (20 s, spec 13).
    pub(crate) magpie_turns: magpie_api::client::Client,
    /// magpie's long-polls (`waitMs` plus 10 s: 35 s).
    pub(crate) magpie_poll: magpie_api::client::Client,
    /// magpie for downloads (no total timeout; reqwest's read timeout, which resets on every body read).
    pub(crate) magpie_transfer: magpie_api::client::Client,
    /// magpie for uploads (M13): the upload bounds its own waits ([`UPLOAD_READ_BACKSTOP`]).
    pub(crate) magpie_upload: magpie_api::client::Client,
    pub(crate) hermit: hermit_api::Client,
    /// Plain client for hermit's `/ops/*` pages.
    pub(crate) hermit_http: reqwest::Client,
    pub(crate) model_http: reqwest::Client,
    pub(crate) identity: identity::Identities,
    pub(crate) bundle: static_files::Bundle,
    pub(crate) shell: shell::Shell,
    pub(crate) warm: http::Warm,
    pub(crate) models: http::Models,
    pub(crate) metrics: metrics::Metrics,
    pub(crate) socket: Arc<dyn paseo::SocketHandler>,
    pub(crate) projector: Arc<dyn paseo::RowProjector>,
    pub(crate) transfers: server::Transfers,
}

impl App {
    /// Builds the state: loads the app bundle (a bundle that cannot be read ends start) and the clients. Nothing is
    /// fetched yet: [`App::start`] runs the background loads that gate readiness.
    ///
    /// # Errors
    /// When the app bundle cannot be read.
    pub fn new(config: Config, registry: &mut prometheus_client::registry::Registry) -> std::io::Result<Self> {
        let Config {
            build,
            upstreams,
            secrets,
            app_dir,
            tuning,
        } = config;
        let rpc = |protocol: Option<Protocol>, timeout: Duration| {
            aviary_core::http::client_builder(&build, protocol)
                .timeout(timeout)
                .build()
                .unwrap_or_default()
        };
        let magpie_protocol = Some(Protocol::new(magpie_api::SURFACE_PERSON, magpie_api::PROTOCOL_PERSON));
        let magpie = magpie_api::client::Client::new(
            rpc(magpie_protocol, Duration::from_secs(15)),
            upstreams.magpie.clone(),
            secrets.token_magpie.clone(),
        );
        let magpie_turns = magpie_api::client::Client::new(
            rpc(magpie_protocol, Duration::from_secs(20)),
            upstreams.magpie.clone(),
            secrets.token_magpie.clone(),
        );
        let magpie_poll = magpie_api::client::Client::new(
            rpc(magpie_protocol, tuning.poll_wait + Duration::from_secs(10)),
            upstreams.magpie.clone(),
            secrets.token_magpie.clone(),
        );
        let transfer_http = aviary_core::http::client_builder(&build, magpie_protocol)
            .read_timeout(tuning.transfer_idle)
            .build()
            .unwrap_or_default();
        let magpie_transfer =
            magpie_api::client::Client::new(transfer_http, upstreams.magpie.clone(), secrets.token_magpie.clone());
        // uploads: reqwest's read timeout would run from the request's start to magpie's answer, which comes only
        // after the whole body, and so fail a slow upload that keeps moving. The upload enforces its own idle bound
        // per chunk and its first-byte bound after the body (uploads.rs); this one only stops a connection that
        // hangs past any upload's lifetime.
        let upload_http = aviary_core::http::client_builder(&build, magpie_protocol)
            .read_timeout(UPLOAD_READ_BACKSTOP)
            .build()
            .unwrap_or_default();
        let magpie_upload =
            magpie_api::client::Client::new(upload_http, upstreams.magpie.clone(), secrets.token_magpie.clone());
        let hermit_protocol = Some(Protocol::new(hermit_api::CORE.0, hermit_api::CORE.1));
        let hermit = hermit_api::Client::new(rpc(hermit_protocol, Duration::from_secs(10)), upstreams.hermit.clone())
            .bearer(secrets.token_hermit.clone());
        let hermit_http = rpc(hermit_protocol, Duration::from_secs(30));
        let model_http = rpc(None, Duration::from_secs(5));
        let external = rpc(None, Duration::from_secs(5));
        let bundle = static_files::Bundle::load(&app_dir)?;
        let metrics = metrics::Metrics::register(registry);
        let identity = identity::Identities::new(
            hermit.clone(),
            external,
            upstreams.access.clone(),
            tuning.clone(),
            secrets.view_as_keys.clone(),
            metrics.clone(),
        );
        Ok(Self(Arc::new(Inner {
            build,
            secrets,
            upstreams,
            magpie,
            magpie_turns,
            magpie_poll,
            magpie_transfer,
            magpie_upload,
            hermit,
            hermit_http,
            model_http,
            identity,
            shell: shell::Shell::new(bundle.index().to_vec()),
            bundle,
            warm: http::Warm::new(tuning.warm_every),
            models: http::Models::default(),
            metrics,
            socket: Arc::new(paseo::transport::PaseoSocket::default()),
            projector: Arc::new(paseo::mapping::PaseoProjector),
            transfers: server::Transfers::default(),
            tuning,
        })))
    }

    /// The same state with the `/paseo-ws` handler and the row projection of the socket package.
    #[must_use]
    pub fn with_socket(self, socket: Arc<dyn paseo::SocketHandler>, projector: Arc<dyn paseo::RowProjector>) -> Self {
        let mut inner = (*self.0).clone();
        inner.socket = socket;
        inner.projector = projector;
        Self(Arc::new(inner))
    }

    /// The same state with the Paseo socket on a hub whose conversations `owners` routes (WP-50's switch window:
    /// its [`feeds::Owners`] hands conversations to a [`feeds::Delegate`]; outside it, [`feeds::NativeOnly`], the
    /// default of [`App::new`]).
    #[must_use]
    pub fn with_owners(self, owners: Arc<dyn feeds::Owners>) -> Self {
        let projector = self.0.projector.clone();
        let socket = paseo::transport::PaseoSocket::new(feeds::Hub::new(owners));
        self.with_socket(Arc::new(socket), projector)
    }

    /// The replica's sockets and pollers (the Paseo socket's hub).
    #[must_use]
    pub fn hub(&self) -> Option<feeds::Hub> {
        self.0.socket.hub()
    }

    /// Starts the background work that gates readiness: the deployment facts (every 60 s), then every team's JWKS.
    /// `deployment` and `jwks` open once each has loaded.
    pub fn start(&self, readiness: &aviary_core::ops::Readiness) {
        let facts_gate = readiness.gate("deployment");
        let jwks_gate = readiness.gate("jwks");
        self.0.identity.start(facts_gate, jwks_gate);
    }

    /// The identity service (facts, tokens, people).
    #[must_use]
    pub fn identities(&self) -> &identity::Identities {
        &self.0.identity
    }

    /// The `/paseo-ws` handler (its drain, for tests and the process's drain).
    #[must_use]
    pub fn socket(&self) -> Arc<dyn paseo::SocketHandler> {
        self.0.socket.clone()
    }

    /// The image URL signer (spec 5.9) for the socket's row projection.
    #[must_use]
    pub fn blob_signer(&self) -> blobs::Signer {
        blobs::Signer::new(&self.0.secrets.blob_keys)
    }

    /// The router of port 8080.
    pub fn router(&self) -> axum::Router {
        http::router(self.clone())
    }

    /// The build this process runs.
    #[must_use]
    pub fn build(&self) -> &Build {
        &self.0.build
    }

    /// The process's drain (spec 11.2), as `main` runs it: on SIGTERM `readiness` turns draining (`/readyz` 503),
    /// then one hook closes `server`'s listener, lets in-flight requests finish and waits for transfers, all within
    /// the transfer bound counted from the signal (300 s; the pod is killed at 320 s). With nothing moving it ends
    /// at once.
    #[must_use]
    pub fn drain(
        &self,
        readiness: &aviary_core::ops::Readiness,
        server: server::Listener,
    ) -> aviary_core::drain::Drain {
        let bound = self.0.tuning.transfer_bound;
        let mut d = aviary_core::drain::Drain::new(readiness.clone());
        let app = self.clone();
        // the hook's own deadline is a backstop a second past the bound the waits inside keep
        d.hook(
            "listener, requests and transfers",
            bound + Duration::from_secs(1),
            move || async move {
                let started = std::time::Instant::now();
                // step 3 (sockets, request-free first) runs beside steps 2 and 4 (the listener and transfers)
                let tuning = &app.0.tuning;
                let sockets = app.0.socket.drain(tuning.drain_jitter, tuning.drain_rpc_wait);
                let ((), ()) = tokio::join!(sockets, server.drain(bound));
                let left = bound.saturating_sub(started.elapsed());
                if tokio::time::timeout(left, app.transfers().idle()).await.is_err() {
                    tracing::warn!(
                        transfers = app.transfers().active(),
                        "drain: transfers still running at the bound"
                    );
                }
                app.observe_drain(started.elapsed());
            },
        );
        d
    }

    /// Records how long the drain waited for requests and transfers (`pigeon_drain_transfer_seconds`).
    pub fn observe_drain(&self, waited: Duration) {
        self.0.metrics.drain_transfer(waited.as_secs_f64());
    }

    /// The in-flight transfers (the drain waits for them).
    #[must_use]
    pub fn transfers(&self) -> &server::Transfers {
        &self.0.transfers
    }
}
