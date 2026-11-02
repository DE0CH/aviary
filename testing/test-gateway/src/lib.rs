//! `test-gateway`: the reference implementation of the sandbox gateway contract (platform 3.11, 7.3) and the oracle
//! `aviary-conformance gateway` is checked against. Nothing in it is deployment-specific:
//!
//! - **`:3128`, the proxy** (3.11.1-3.11.6): the credential `Proxy-Authorization: Basic sbx:<token>` (407 when
//!   missing), the `hst1.` token verified locally with hermit's published keys (refetched every 5 min and at most
//!   every 10 s on an unknown kid), liveness from hermit cached 30 s and served stale without limit while hermit
//!   fails, then the fixed names `magpie`, `hermit`, `model`, `tools` with the contract's paths, headers, body
//!   limits and streaming. A self host (`--self-host`) has no local paths here (404); `CONNECT` and every other host
//!   are refused with 403 `not_allowed` (the harness has no internet).
//! - **`:8082`, the in-cluster API** (3.11.7): `GET /v1/aviary/models` for pigeon's bearer (forwarded to the model
//!   upstream), `GET /v1/ops/summary` for hermit's (a fixture in the shape of 15.3).
//! - **Drain** (3.11.6): [`Running::drain`] stops accepting connections, closes idle kept-alive ones, answers the
//!   rest with `Connection: close`, and lets in-flight work finish within its bound (model streams 600 s, `magpie`
//!   and `hermit` 60 s, `tools` 180 s); what is still open then is cut and counted in
//!   `test_gateway_drain_cut_total{kind}`.
//!
//! [`Faults`] seed the four mistakes the conformance suite must catch; the binary never sets them.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use axum::response::IntoResponse;
use gateway_api::credential::{CredentialError, PROXY_AUTHENTICATE, parse_proxy_authorization};
use gateway_api::liveness::{Lookup, View};
use gateway_api::models::MODELS;
use gateway_api::ops::{OPS_SUMMARY, OpsSummary};
use gateway_api::{Claims, FixedName, KeySet, TokenKeys, TokenLiveness, VerifyError, codes, headers, limits};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Empty, Full, LengthLimitError, Limited, StreamBody};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

/// Errors carried by streamed bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// Every body the gateway sends, both ways.
pub type Body = BoxBody<Bytes, BoxError>;

/// The upstream base URLs (`http://host[:port]`, no path).
#[derive(Clone, Debug)]
pub struct Upstreams {
    pub magpie: String,
    pub hermit: String,
    pub tools: String,
    /// The `model` route's upstream (the harness: mock-model).
    pub model: String,
}

impl Default for Upstreams {
    fn default() -> Self {
        Self {
            magpie: "http://magpie".into(),
            hermit: "http://hermit".into(),
            tools: "http://tools".into(),
            model: "http://mock-model".into(),
        }
    }
}

/// The credentials the gateway presents upstream. `Debug` writes none of them.
#[derive(Clone)]
pub struct Credentials {
    /// gateway → magpie pair token.
    pub magpie: String,
    /// gateway → hermit pair token (also for the key set and liveness lookups).
    pub hermit: String,
    /// gateway → tools pair token.
    pub tools: String,
    /// The model upstream's key, sent as `x-api-key`.
    pub model: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

/// How long in-flight work may run on after a drain starts (3.11.6 step 3).
#[derive(Clone, Copy, Debug)]
pub struct DrainBounds {
    /// `model` streams and CONNECT tunnels.
    pub model: Duration,
    /// `magpie` and `hermit` requests.
    pub fixed: Duration,
    /// `tools` calls.
    pub tools: Duration,
}

impl Default for DrainBounds {
    fn default() -> Self {
        Self {
            model: limits::DRAIN_MODEL,
            fixed: limits::DRAIN_FIXED,
            tools: limits::DRAIN_TOOLS,
        }
    }
}

/// Seeded mistakes, each one a way a gateway can break the contract. For the conformance suite's own tests only.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // independent knobs, any combination
pub struct Faults {
    /// Forward the client's `Aviary-*` headers instead of stripping them.
    pub keep_client_aviary_headers: bool,
    /// Treat a failed liveness lookup (hermit refused, slow, 5xx) as "not live".
    pub hermit_failure_refuses: bool,
    /// Count fixed-name request bodies against the per-connection upload cap.
    pub upload_cap_counts_fixed_names: bool,
    /// Cut every open request at once on drain.
    pub drain_cuts_streams: bool,
}

/// The gateway's configuration.
#[derive(Debug)]
pub struct Config {
    pub proxy_listen: SocketAddr,
    pub api_listen: SocketAddr,
    pub upstreams: Upstreams,
    pub credentials: Credentials,
    /// The in-cluster API's callers: pigeon (models) and hermit (ops summary).
    pub callers: Callers,
    /// Hosts that are the proxy itself (its VIP, its Service names): served locally, never forwarded.
    pub self_hosts: Vec<String>,
    /// `GET /v1/ops/summary`'s answer.
    pub ops_summary: OpsSummary,
    pub drain: DrainBounds,
    /// The per-connection cap on internet uploads (YYC's `maxUploadBytes`); never applied to fixed names.
    pub upload_cap: u64,
    pub faults: Faults,
}

/// The default ops summary: gateway-api's fixture.
///
/// # Panics
/// Never: the fixture is part of the crate and its tests decode it.
#[must_use]
pub fn default_ops_summary() -> OpsSummary {
    serde_json::from_str(include_str!("../../../crates/gateway-api/fixtures/ops-summary.json"))
        .unwrap_or_else(|e| panic!("ops-summary fixture: {e}"))
}

/// The upload cap of YYC's egress (A6.F8), the default here.
pub const UPLOAD_CAP: u64 = 8 * 1024 * 1024;

const HERMIT_CORE: &str = "hermit-core=1";
const JSON: &str = "application/json; charset=utf-8";
const KEYS_TIMEOUT: Duration = Duration::from_secs(2);
/// A liveness entry unused this long is forgotten.
const LIVENESS_FORGET: Duration = Duration::from_secs(600);
/// After a failed liveness lookup, the next try waits this long.
const LIVENESS_RETRY: Duration = Duration::from_secs(2);
/// The `tools` route's request timeout (at least 180 s, 3.11.4).
const TOOLS_TIMEOUT: Duration = Duration::from_secs(190);
/// How long a body over the limit is read and discarded before the 413 (beyond that the connection is closed).
const DISCARD_FACTOR: u64 = 2;

/// What one in-flight request is, for the drain bounds and the cut counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EncodeLabelValue)]
pub enum Kind {
    Model,
    Fixed,
    Tools,
}

impl Kind {
    const ALL: [Self; 3] = [Self::Model, Self::Fixed, Self::Tools];

    fn index(self) -> usize {
        match self {
            Self::Model => 0,
            Self::Fixed => 1,
            Self::Tools => 2,
        }
    }

    fn of(name: FixedName) -> Self {
        match name {
            FixedName::Model => Self::Model,
            FixedName::Tools => Self::Tools,
            FixedName::Magpie | FixedName::Hermit => Self::Fixed,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct KindLabel {
    kind: Kind,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct SandboxRoute {
    sandbox: String,
    route: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct CodeLabel {
    code: String,
}

#[derive(Debug, Default, Clone)]
struct Metrics {
    drain_cut: Family<KindLabel, Counter>,
    fixed: Family<SandboxRoute, Counter>,
    refused: Family<CodeLabel, Counter>,
}

impl Metrics {
    fn register(&self, registry: &mut Registry) {
        registry.register(
            "drain_cut",
            "Requests still open at their drain bound, cut",
            self.drain_cut.clone(),
        );
        registry.register(
            "fixed_requests",
            "Requests to magpie and hermit per sandbox and route",
            self.fixed.clone(),
        );
        registry.register("refused", "The gateway's own refusals by code", self.refused.clone());
    }
}

/// In-flight requests per kind, and the signal that cuts them.
#[derive(Debug)]
struct Inflight {
    kinds: [KindState; 3],
}

#[derive(Debug)]
struct KindState {
    n: AtomicUsize,
    zero: Notify,
    cut: watch::Sender<bool>,
}

impl Inflight {
    fn new() -> Self {
        let k = || KindState {
            n: AtomicUsize::new(0),
            zero: Notify::new(),
            cut: watch::channel(false).0,
        };
        Self { kinds: [k(), k(), k()] }
    }

    fn enter(self: &Arc<Self>, kind: Kind) -> InflightGuard {
        self.kinds[kind.index()].n.fetch_add(1, Ordering::SeqCst);
        InflightGuard {
            inflight: self.clone(),
            kind,
        }
    }

    fn count(&self, kind: Kind) -> usize {
        self.kinds[kind.index()].n.load(Ordering::SeqCst)
    }

    async fn wait_zero(&self, kind: Kind) {
        let k = &self.kinds[kind.index()];
        loop {
            let notified = k.zero.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if k.n.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// Held for the life of one request, response body included.
#[derive(Debug)]
struct InflightGuard {
    inflight: Arc<Inflight>,
    kind: Kind,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let k = &self.inflight.kinds[self.kind.index()];
        if k.n.fetch_sub(1, Ordering::SeqCst) == 1 {
            k.zero.notify_waiters();
        }
    }
}

#[derive(Debug, Default)]
struct LiveEntry {
    view: View,
    refreshing: bool,
    retry_at: Option<Instant>,
    used: Option<Instant>,
}

#[derive(Debug)]
struct Keys {
    set: RwLock<KeySet>,
    /// When a fetch was last attempted (the 10 s bound on unknown-kid refetches).
    attempted: Mutex<Option<Instant>>,
    fetching: tokio::sync::Mutex<()>,
}

struct Inner {
    cfg: Config,
    client: Client<HttpConnector, Body>,
    keys: Keys,
    live: Mutex<HashMap<Uuid, LiveEntry>>,
    inflight: Arc<Inflight>,
    draining: watch::Sender<bool>,
    /// True once a key set has been fetched.
    keys_loaded: watch::Sender<bool>,
    metrics: Metrics,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|e| match e {}).boxed()
}

fn empty() -> Body {
    Empty::new().map_err(|e| match e {}).boxed()
}

/// An `ApiError` (the platform envelope) as a proxy response.
fn refusal(e: &ApiError) -> Response<Body> {
    let mut r = Response::new(full(serde_json::to_vec(e.body()).unwrap_or_default()));
    *r.status_mut() = e.status();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    r
}

fn not_allowed(message: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, codes::NOT_ALLOWED, message)
}

/// Hop-by-hop headers (RFC 9110 7.6.1), never forwarded either way.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// The headers named in `Connection`, which are hop-by-hop too.
fn connection_named(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|s| s.trim().to_ascii_lowercase()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A running gateway.
#[derive(Debug)]
pub struct Running {
    inner: Arc<Inner>,
    proxy: SocketAddr,
    api: SocketAddr,
    tasks: Mutex<Option<JoinSet<()>>>,
}

/// What a drain did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Requests cut at their bound, per kind.
    pub cut: Vec<(Kind, usize)>,
}

impl Running {
    /// The proxy's bound address.
    #[must_use]
    pub fn proxy_addr(&self) -> SocketAddr {
        self.proxy
    }

    /// The in-cluster API's bound address.
    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.api
    }

    /// Resolves once hermit's key set has been fetched (the binary's `token-keys` readiness gate).
    pub async fn keys_loaded(&self) {
        let mut rx = self.inner.keys_loaded.subscribe();
        let _ = rx.wait_for(|l| *l).await;
    }

    /// Whether a drain has started.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self.inner.draining.borrow()
    }

    /// Drains (3.11.6): stop accepting connections, close idle kept-alive ones, answer the rest with `Connection:
    /// close`, let in-flight work finish up to its bound, then cut and count what is left. Returns once every
    /// connection has ended (or 5 s after the cut).
    pub async fn drain(&self) -> DrainReport {
        let inner = &self.inner;
        inner.draining.send_replace(true);
        let started = tokio::time::Instant::now();
        let bounds = if inner.cfg.faults.drain_cuts_streams {
            DrainBounds {
                model: Duration::ZERO,
                fixed: Duration::ZERO,
                tools: Duration::ZERO,
            }
        } else {
            inner.cfg.drain
        };
        let waits = Kind::ALL.map(|kind| {
            let inner = inner.clone();
            let bound = match kind {
                Kind::Model => bounds.model,
                Kind::Fixed => bounds.fixed,
                Kind::Tools => bounds.tools,
            };
            async move {
                if tokio::time::timeout_at(started + bound, inner.inflight.wait_zero(kind))
                    .await
                    .is_ok()
                {
                    return (kind, 0);
                }
                let open = inner.inflight.count(kind);
                inner
                    .metrics
                    .drain_cut
                    .get_or_create(&KindLabel { kind })
                    .inc_by(open as u64);
                tracing::warn!(kind = ?kind, open, "drain bound reached, cutting");
                inner.inflight.kinds[kind.index()].cut.send_replace(true);
                let _ = tokio::time::timeout(Duration::from_secs(5), inner.inflight.wait_zero(kind)).await;
                (kind, open)
            }
        });
        let [a, b, c] = waits;
        let (a, b, c) = tokio::join!(a, b, c);
        let tasks = lock(&self.tasks).take();
        if let Some(mut tasks) = tasks {
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while tasks.join_next().await.is_some() {}
            })
            .await;
            tasks.abort_all();
        }
        DrainReport {
            cut: [a, b, c].into_iter().filter(|(_, n)| *n > 0).collect(),
        }
    }

    /// Ends the gateway at once (tests).
    pub fn abort(&self) {
        if let Some(mut t) = lock(&self.tasks).take() {
            t.abort_all();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Starts the gateway: binds both listeners, fetches hermit's key set in the background ([`Running::keys_loaded`])
/// and registers its metrics on `registry`.
///
/// # Errors
/// When a listener cannot be bound.
pub async fn start(cfg: Config, registry: &mut Registry) -> std::io::Result<Running> {
    let proxy_listener = TcpListener::bind(cfg.proxy_listen).await?;
    let api_listener = TcpListener::bind(cfg.api_listen).await?;
    let proxy = proxy_listener.local_addr()?;
    let api = api_listener.local_addr()?;
    let metrics = Metrics::default();
    metrics.register(registry);
    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    connector.set_connect_timeout(Some(Duration::from_secs(5)));
    let client = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(64)
        .http1_preserve_header_case(false)
        .build(connector);
    let inner = Arc::new(Inner {
        cfg,
        client,
        keys: Keys {
            set: RwLock::new(KeySet::default()),
            attempted: Mutex::new(None),
            fetching: tokio::sync::Mutex::new(()),
        },
        live: Mutex::new(HashMap::new()),
        inflight: Arc::new(Inflight::new()),
        draining: watch::channel(false).0,
        keys_loaded: watch::channel(false).0,
        metrics,
    });
    let mut tasks = JoinSet::new();
    tasks.spawn(accept(inner.clone(), proxy_listener));
    tasks.spawn(serve_api(inner.clone(), api_listener));
    tasks.spawn(keys_loop(inner.clone()));
    tasks.spawn(liveness_sweeper(inner.clone()));
    Ok(Running {
        inner,
        proxy,
        api,
        tasks: Mutex::new(Some(tasks)),
    })
}

// ---- the proxy ----

/// Resolves once the flag is true (or its sender is gone).
async fn wait_true(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

async fn accept(inner: Arc<Inner>, listener: TcpListener) {
    let mut draining = inner.draining.subscribe();
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            () = wait_true(&mut draining) => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    conns.spawn(connection(inner.clone(), stream, peer));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
    // no new connections from here: the listening socket is closed, so a connect is refused
    drop(listener);
    while conns.join_next().await.is_some() {}
}

async fn connection(inner: Arc<Inner>, stream: tokio::net::TcpStream, peer: SocketAddr) {
    let uploaded = Arc::new(AtomicU64::new(0));
    let svc_inner = inner.clone();
    let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
        let inner = svc_inner.clone();
        let uploaded = uploaded.clone();
        async move { Ok::<_, Infallible>(handle(inner, peer, uploaded, req).await) }
    });
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .keep_alive(true)
        .header_read_timeout(limits::IDLE_TIMEOUT)
        .serve_connection(TokioIo::new(stream), svc);
    tokio::pin!(conn);
    let mut draining = inner.draining.subscribe();
    tokio::select! {
        _ = conn.as_mut() => return,
        () = wait_true(&mut draining) => {}
    }
    // an idle connection closes now; a busy one after its answer, which carries `Connection: close`
    conn.as_mut().graceful_shutdown();
    let _ = conn.await;
}

/// Where a request is going.
enum Target {
    Fixed(FixedName),
    SelfHost,
    Internet(String),
}

fn target_of(inner: &Inner, req: &Request<Incoming>) -> Target {
    let (host, port) = if let Some(a) = req.uri().authority() {
        (a.host().to_owned(), a.port_u16())
    } else {
        let h = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        h.parse::<hyper::http::uri::Authority>()
            .map_or_else(|_| (String::new(), None), |a| (a.host().to_owned(), a.port_u16()))
    };
    let with_port = match port {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    if req.method() != Method::CONNECT
        && let Some(name) = FixedName::from_host(&with_port)
    {
        return Target::Fixed(name);
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if req.method() != Method::CONNECT && inner.cfg.self_hosts.iter().any(|s| s.eq_ignore_ascii_case(bare)) {
        return Target::SelfHost;
    }
    Target::Internet(with_port)
}

async fn handle(
    inner: Arc<Inner>,
    peer: SocketAddr,
    uploaded: Arc<AtomicU64>,
    req: Request<Incoming>,
) -> Response<Body> {
    let mut resp = route(&inner, peer, &uploaded, req).await;
    if *inner.draining.borrow() {
        resp.headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    resp
}

fn refuse(inner: &Inner, e: &ApiError) -> Response<Body> {
    inner
        .metrics
        .refused
        .get_or_create(&CodeLabel {
            code: e.code().to_owned(),
        })
        .inc();
    refusal(e)
}

async fn route(inner: &Arc<Inner>, peer: SocketAddr, uploaded: &AtomicU64, req: Request<Incoming>) -> Response<Body> {
    let target = target_of(inner, &req);
    // (1) the credential
    let presented = req.headers().get(header::PROXY_AUTHORIZATION).or_else(|| {
        matches!(target, Target::SelfHost)
            .then(|| req.headers().get(header::AUTHORIZATION))
            .flatten()
    });
    let token = match presented.and_then(|v| v.to_str().ok()).map(parse_proxy_authorization) {
        Some(Ok(t)) => t,
        Some(Err(CredentialError::Malformed)) => return refuse(inner, &not_allowed("not a sandbox credential")),
        None | Some(Err(CredentialError::Missing)) => {
            let mut r = refuse(
                inner,
                &ApiError::new(
                    StatusCode::PROXY_AUTHENTICATION_REQUIRED,
                    "proxy_auth_required",
                    "the proxy needs the sandbox credential",
                ),
            );
            r.headers_mut()
                .insert(header::PROXY_AUTHENTICATE, HeaderValue::from_static(PROXY_AUTHENTICATE));
            return r;
        }
    };
    let claims = match verify(inner, &token, peer.ip()).await {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(why = %e, "sandbox token refused");
            return refuse(inner, &not_allowed("sandbox token not valid"));
        }
    };
    if !admits(inner, claims.sandbox_id) {
        return refuse(inner, &not_allowed("sandbox not live"));
    }
    match target {
        Target::Fixed(name) => {
            let pq = req.uri().path_and_query().map_or("/", |p| p.as_str()).to_owned();
            if !name.allows(req.method().as_str(), &pq) {
                return refuse(inner, &not_allowed("path not routed on this name"));
            }
            forward(inner, name, &claims, uploaded, req, &pq).await
        }
        Target::SelfHost => refuse(inner, &ApiError::not_found("the proxy serves no local paths here")),
        Target::Internet(host) => {
            tracing::info!(
                sandbox = %claims.sandbox_id,
                kind = "internet",
                host = %host,
                decision = "deny:not_allowed",
                "internet request refused"
            );
            refuse(inner, &not_allowed("this gateway has no internet"))
        }
    }
}

async fn verify(inner: &Inner, token: &str, from: IpAddr) -> Result<Claims, VerifyError> {
    let first = inner
        .keys
        .set
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .verify(token, unix_now(), from);
    match first {
        Err(VerifyError::UnknownKid(_)) => {
            refetch_keys(inner, true).await;
            inner
                .keys
                .set
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .verify(token, unix_now(), from)
        }
        other => other,
    }
}

/// Whether the liveness view admits a verified token's sandbox, starting a background refresh when one is due.
fn admits(inner: &Arc<Inner>, sandbox: Uuid) -> bool {
    let now = Instant::now();
    let mut live = lock(&inner.live);
    let e = live.entry(sandbox).or_default();
    e.used = Some(now);
    let admit = e.view.admits();
    if e.view.refresh_due(now) && !e.refreshing && e.retry_at.is_none_or(|t| now >= t) {
        e.refreshing = true;
        tokio::spawn(refresh_liveness(inner.clone(), sandbox));
    }
    admit
}

async fn refresh_liveness(inner: Arc<Inner>, sandbox: Uuid) {
    let mut lookup = lookup_liveness(&inner, sandbox).await;
    if inner.cfg.faults.hermit_failure_refuses && lookup == Lookup::Failed {
        lookup = Lookup::Unknown;
    }
    let now = Instant::now();
    let mut live = lock(&inner.live);
    let e = live.entry(sandbox).or_default();
    e.view.update(&lookup, now);
    e.refreshing = false;
    e.retry_at = (lookup == Lookup::Failed).then(|| now + LIVENESS_RETRY);
}

async fn liveness_sweeper(inner: Arc<Inner>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let now = Instant::now();
        let mut due = Vec::new();
        {
            let mut live = lock(&inner.live);
            live.retain(|_, e| {
                e.used
                    .is_some_and(|u| now.saturating_duration_since(u) < LIVENESS_FORGET)
            });
            for (id, e) in live.iter_mut() {
                if e.view.refresh_due(now) && !e.refreshing && e.retry_at.is_none_or(|t| now >= t) {
                    e.refreshing = true;
                    due.push(*id);
                }
            }
        }
        for id in due {
            tokio::spawn(refresh_liveness(inner.clone(), id));
        }
    }
}

async fn hermit_get(inner: &Inner, path: &str, timeout: Duration) -> Option<(StatusCode, Bytes)> {
    let uri: Uri = format!("{}{path}", inner.cfg.upstreams.hermit.trim_end_matches('/'))
        .parse()
        .ok()?;
    let req = Request::get(uri)
        .header(header::HOST, "hermit")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", inner.cfg.credentials.hermit),
        )
        .header(hermit_api::headers::PROTOCOL, HERMIT_CORE)
        .header(hermit_api::headers::REQUEST, Uuid::now_v7().to_string())
        .body(empty())
        .ok()?;
    let fut = async {
        let res = inner.client.request(req).await.ok()?;
        let status = res.status();
        let body = Limited::new(res.into_body(), 1 << 20).collect().await.ok()?.to_bytes();
        Some((status, body))
    };
    tokio::time::timeout(timeout, fut).await.ok().flatten()
}

async fn lookup_liveness(inner: &Inner, sandbox: Uuid) -> Lookup {
    let path = hermit_api::routes::fill(gateway_api::SANDBOX_TOKEN, &[&sandbox.to_string()]);
    match hermit_get(inner, &path, limits::LIVENESS_TIMEOUT).await {
        Some((StatusCode::OK, body)) => match serde_json::from_slice::<TokenLiveness>(&body) {
            Ok(a) => Lookup::Answer(a),
            Err(_) => Lookup::Failed,
        },
        Some((StatusCode::NOT_FOUND, _)) => Lookup::Unknown,
        _ => Lookup::Failed,
    }
}

/// Fetches the key set; `unknown_kid` limits it to once per 10 s. A failed fetch keeps the last set.
async fn refetch_keys(inner: &Inner, unknown_kid: bool) -> bool {
    let _one = inner.keys.fetching.lock().await;
    {
        let mut at = lock(&inner.keys.attempted);
        if unknown_kid && at.is_some_and(|t| t.elapsed() < limits::KEYS_UNKNOWN_KID_MIN) {
            return false;
        }
        *at = Some(Instant::now());
    }
    let Some((StatusCode::OK, body)) = hermit_get(inner, gateway_api::SANDBOX_TOKEN_KEYS, KEYS_TIMEOUT).await else {
        tracing::warn!("token key set fetch failed; keeping the last set");
        return false;
    };
    match serde_json::from_slice::<TokenKeys>(&body) {
        Ok(keys) => {
            *inner.keys.set.write().unwrap_or_else(PoisonError::into_inner) = KeySet::from_keys(&keys);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "token key set unreadable; keeping the last set");
            false
        }
    }
}

async fn keys_loop(inner: Arc<Inner>) {
    loop {
        if refetch_keys(&inner, false).await {
            inner.keys_loaded.send_replace(true);
            tokio::time::sleep(limits::KEYS_REFRESH).await;
        } else {
            let wait = if *inner.keys_loaded.borrow() {
                limits::KEYS_UNKNOWN_KID_MIN
            } else {
                Duration::from_secs(1)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// The error a body over its limit raised, anywhere in a send error's chain.
fn over_limit(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(e);
    while let Some(err) = cur {
        if err.is::<LengthLimitError>() {
            return true;
        }
        cur = err.source();
    }
    false
}

/// Reads and drops a body over its limit (up to twice the limit) so the client can read the 413.
async fn discard(body: Incoming, limit: u64) {
    let mut body = body;
    let mut seen: u64 = 0;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(d) = frame.data_ref() {
            seen += d.len() as u64;
            if seen > limit.saturating_mul(DISCARD_FACTOR) {
                return;
            }
        }
    }
}

fn too_large(limit: u64) -> ApiError {
    ApiError::too_large(format!("request body over {limit} bytes"))
}

#[allow(clippy::too_many_lines)] // one request's path through the gateway, kept in order
async fn forward(
    inner: &Arc<Inner>,
    name: FixedName,
    claims: &Claims,
    uploaded: &AtomicU64,
    req: Request<Incoming>,
    pq: &str,
) -> Response<Body> {
    let kind = Kind::of(name);
    let guard = inner.inflight.enter(kind);
    let started = Instant::now();
    let limit = name.body_limit();
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if inner.cfg.faults.upload_cap_counts_fixed_names {
        let total = uploaded.fetch_add(declared.unwrap_or(0), Ordering::SeqCst) + declared.unwrap_or(0);
        if total > inner.cfg.upload_cap {
            return refuse(inner, &not_allowed("deny:upload"));
        }
    }
    let (parts, body) = req.into_parts();
    if declared.is_some_and(|n| n > limit) {
        discard(body, limit).await;
        return refuse(inner, &too_large(limit));
    }

    let (base, host, credential): (&str, String, (HeaderName, String)) = match name {
        FixedName::Magpie => (
            &inner.cfg.upstreams.magpie,
            "magpie".into(),
            (
                header::AUTHORIZATION,
                format!("Bearer {}", inner.cfg.credentials.magpie),
            ),
        ),
        FixedName::Hermit => (
            &inner.cfg.upstreams.hermit,
            "hermit".into(),
            (
                header::AUTHORIZATION,
                format!("Bearer {}", inner.cfg.credentials.hermit),
            ),
        ),
        FixedName::Tools => (
            &inner.cfg.upstreams.tools,
            "tools".into(),
            (header::AUTHORIZATION, format!("Bearer {}", inner.cfg.credentials.tools)),
        ),
        FixedName::Model => {
            let base = &inner.cfg.upstreams.model;
            let host = base
                .parse::<Uri>()
                .ok()
                .and_then(|u| u.authority().map(|a| a.as_str().to_owned()))
                .unwrap_or_default();
            (
                base,
                host,
                (
                    HeaderName::from_static("x-api-key"),
                    inner.cfg.credentials.model.clone(),
                ),
            )
        }
    };
    let Ok(uri) = format!("{}{pq}", base.trim_end_matches('/')).parse::<Uri>() else {
        return refuse(inner, &ApiError::bad_request("unusable path"));
    };

    let named = connection_named(&parts.headers);
    let mut out = HeaderMap::with_capacity(parts.headers.len() + 6);
    for (k, v) in &parts.headers {
        if hop_by_hop(k) || named.iter().any(|n| n == k.as_str()) || k == header::HOST {
            continue;
        }
        let aviary = k.as_str().starts_with(hermit_api::headers::PREFIX);
        let stripped = if inner.cfg.faults.keep_client_aviary_headers && aviary {
            k == hermit_api::headers::REQUEST
        } else {
            headers::is_stripped(k.as_str())
        };
        if !stripped {
            out.append(k.clone(), v.clone());
        }
    }
    for (k, v) in headers::identity(claims) {
        if let Ok(v) = HeaderValue::from_str(&v) {
            out.insert(k, v);
        }
    }
    let request_id = parts
        .headers
        .get(hermit_api::headers::REQUEST)
        .and_then(|v| v.to_str().ok())
        .filter(|v| headers::keep_request_id(v))
        .map_or_else(|| Uuid::now_v7().to_string(), str::to_ascii_lowercase);
    if let Ok(v) = HeaderValue::from_str(&request_id) {
        out.insert(hermit_api::headers::REQUEST, v);
    }
    if let Ok(v) = HeaderValue::from_str(&credential.1) {
        out.insert(credential.0, v);
    }
    if let Ok(v) = HeaderValue::from_str(&host) {
        out.insert(header::HOST, v);
    }

    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
    let body: Body = Limited::new(body, limit_usize).boxed();
    let mut upstream = Request::new(body);
    *upstream.method_mut() = parts.method.clone();
    *upstream.uri_mut() = uri;
    *upstream.headers_mut() = out;

    let sent = inner.client.request(upstream);
    let res = if kind == Kind::Tools {
        match tokio::time::timeout(TOOLS_TIMEOUT, sent).await {
            Ok(r) => r,
            Err(_) => {
                return refuse(
                    inner,
                    &ApiError::new(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", "tools did not answer"),
                );
            }
        }
    } else {
        sent.await
    };
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            if over_limit(&e) {
                return refuse(inner, &too_large(limit));
            }
            tracing::warn!(name = name.as_str(), error = %e, "upstream unreachable");
            return refuse(
                inner,
                &ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    codes::UPSTREAM_UNREACHABLE,
                    format!("{} could not be reached", name.as_str()),
                ),
            );
        }
    };

    match kind {
        Kind::Fixed => {
            inner
                .metrics
                .fixed
                .get_or_create(&SandboxRoute {
                    sandbox: claims.sandbox_id.to_string(),
                    route: name.as_str().to_owned(),
                })
                .inc();
        }
        Kind::Model | Kind::Tools => {
            tracing::info!(
                sandbox = %claims.sandbox_id,
                conversation = %claims.conversation_id,
                kind = name.as_str(),
                path = %parts.uri.path(),
                status = res.status().as_u16(),
                ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "fixed-name request"
            );
        }
    }

    let (rparts, rbody) = res.into_parts();
    let named = connection_named(&rparts.headers);
    let mut resp = Response::new(relay(rbody, guard, inner.inflight.kinds[kind.index()].cut.subscribe()));
    *resp.status_mut() = rparts.status;
    for (k, v) in &rparts.headers {
        if !hop_by_hop(k) && !named.iter().any(|n| n == k.as_str()) {
            resp.headers_mut().append(k.clone(), v.clone());
        }
    }
    resp
}

/// The upstream's body, streamed frame by frame as it comes; it holds the request's in-flight guard until it ends,
/// and ends with an error (the client sees the connection cut) when the drain cuts its kind.
fn relay(body: Incoming, guard: InflightGuard, cut: watch::Receiver<bool>) -> Body {
    let stream = futures_util::stream::unfold(Some((body, guard, cut)), |state| async move {
        let (mut body, guard, mut cut) = state?;
        tokio::select! {
            biased;
            cut_ok = async { cut.wait_for(|c| *c).await.is_ok() } => {
                if cut_ok {
                    let e: BoxError = "cut by the drain".into();
                    return Some((Err(e), None));
                }
                // the sender is gone only with the gateway itself
                let e: BoxError = "gateway stopped".into();
                Some((Err(e), None))
            }
            f = body.frame() => match f {
                None => None,
                Some(Ok(frame)) => Some((Ok(frame), Some((body, guard, cut)))),
                Some(Err(e)) => {
                    let e: BoxError = Box::new(e);
                    Some((Err(e), None))
                }
            },
        }
    });
    http_body_util::BodyExt::boxed(StreamBody::new(stream))
}

// ---- the in-cluster API ----

async fn serve_api(inner: Arc<Inner>, listener: TcpListener) {
    use axum::routing::get;
    let models = inner
        .cfg
        .callers
        .guard()
        .allow(Caller::Pigeon, &[])
        .router([(MODELS, get(models_route))]);
    let summary = inner
        .cfg
        .callers
        .guard()
        .allow(Caller::Hermit, &[])
        .router([(OPS_SUMMARY, get(summary_route))]);
    let app = models
        .merge(summary)
        .fallback(|| async { ApiError::not_found("no such route").into_response() })
        .with_state(inner.clone())
        .layer(axum::middleware::from_fn(aviary_core::http::defaults));
    let mut draining = inner.draining.subscribe();
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async move { wait_true(&mut draining).await })
        .await;
}

async fn models_route(axum::extract::State(inner): axum::extract::State<Arc<Inner>>) -> axum::response::Response {
    let base = inner.cfg.upstreams.model.trim_end_matches('/');
    let Ok(uri) = format!("{base}{MODELS}").parse::<Uri>() else {
        return ApiError::internal("bad model upstream").into_response();
    };
    let Ok(req) = Request::get(uri)
        .header("x-api-key", &inner.cfg.credentials.model)
        .body(empty())
    else {
        return ApiError::internal("bad model credential").into_response();
    };
    let fut = async {
        let res = inner.client.request(req).await.ok()?;
        let status = res.status();
        let body = Limited::new(res.into_body(), 1 << 20).collect().await.ok()?.to_bytes();
        Some((status, body))
    };
    match tokio::time::timeout(gateway_api::models::MODELS_TIMEOUT, fut).await {
        Ok(Some((status, body))) => {
            let mut r = (status, body).into_response();
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
            r
        }
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            codes::UPSTREAM_UNREACHABLE,
            "the model could not be reached",
        )
        .into_response(),
    }
}

async fn summary_route(axum::extract::State(inner): axum::extract::State<Arc<Inner>>) -> axum::response::Response {
    aviary_core::http::json(StatusCode::OK, &inner.cfg.ops_summary)
}
