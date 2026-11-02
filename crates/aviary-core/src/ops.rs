//! The ops port, `:9090` (platform 3.1, 3.2, 15.7): `/healthz`, `/readyz`, `/version`, `/metrics`. Never in a
//! Service.
//!
//! Readiness (decision 11): `/readyz` is 503 until the process has finished its own start, and while draining.
//! Readiness starts closed: the implicit [`START`] gate is open only once the service calls [`Readiness::started`]
//! at the end of its start, so `/readyz` is 503 straight after [`Ops::spawn`] and between named gates made one after
//! another. Named gates ([`Readiness::gate`]: `migrations`, `listen`…) say which part is still pending. A gate is
//! opened once; nothing later closes it, so a dependency failing after start never empties a Service.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;
use serde::{Deserialize, Serialize};

use crate::version::Build;

/// The ops port.
pub const PORT: u16 = 9090;

/// Where the ops server listens: every interface, [`PORT`].
pub const ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), PORT);

/// The content type of `/metrics` (Prometheus text exposition 0.0.4).
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The reason `/readyz` gives while draining.
pub const DRAINING: &str = "draining";

/// The implicit gate every process starts with, opened by [`Readiness::started`].
pub const START: &str = "start";

/// The service's start gates and the draining flag.
#[derive(Clone)]
pub struct Readiness {
    inner: Arc<ReadinessInner>,
}

struct ReadinessInner {
    started: AtomicBool,
    gates: Mutex<Vec<(String, bool)>>,
    draining: AtomicBool,
    draining_gauge: Gauge,
}

impl fmt::Debug for Readiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Readiness").field("why", &self.why()).finish()
    }
}

impl Readiness {
    fn new(draining_gauge: Gauge) -> Self {
        Self {
            inner: Arc::new(ReadinessInner {
                started: AtomicBool::new(false),
                gates: Mutex::new(Vec::new()),
                draining: AtomicBool::new(false),
                draining_gauge,
            }),
        }
    }

    /// A readiness handle that is not attached to an ops server (tests of components that take one). Not ready
    /// until [`Readiness::started`], like the one of [`Ops`].
    #[must_use]
    pub fn detached() -> Self {
        Self::new(Gauge::default())
    }

    /// A closed start gate named `name` (`migrations`, `listener`, `listen`, `config`…); `/readyz` is 503 with this
    /// name in `why` until [`Gate::open`]. Made during start, before [`Readiness::started`].
    pub fn gate(&self, name: &str) -> Gate {
        let mut gates = self.inner.gates.lock().unwrap_or_else(PoisonError::into_inner);
        gates.push((name.to_owned(), false));
        Gate {
            readiness: self.clone(),
            index: gates.len() - 1,
        }
    }

    /// The process has finished its own start: opens the [`START`] gate. `/readyz` is 200 from now on once every
    /// named gate is open, until draining. Called once, last in start.
    pub fn started(&self) {
        self.inner.started.store(true, Ordering::SeqCst);
    }

    /// Marks the process as draining: `/readyz` 503 from now on, `<svc>_draining` 1.
    pub fn set_draining(&self) {
        self.inner.draining.store(true, Ordering::SeqCst);
        self.inner.draining_gauge.set(1);
    }

    /// Whether draining has started.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::SeqCst)
    }

    /// Why the process is not ready: `draining` first, then each closed named gate in the order they were made, then
    /// [`START`] until [`Readiness::started`]. Empty when ready.
    #[must_use]
    pub fn why(&self) -> Vec<String> {
        let mut why = Vec::new();
        if self.is_draining() {
            why.push(DRAINING.to_owned());
        }
        let gates = self.inner.gates.lock().unwrap_or_else(PoisonError::into_inner);
        why.extend(gates.iter().filter(|(_, open)| !open).map(|(n, _)| n.clone()));
        if !self.inner.started.load(Ordering::SeqCst) {
            why.push(START.to_owned());
        }
        why
    }

    /// Whether `/readyz` answers 200.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.why().is_empty()
    }
}

/// One start gate; see [`Readiness::gate`].
#[derive(Debug)]
#[must_use = "a gate that is never opened keeps /readyz at 503"]
pub struct Gate {
    readiness: Readiness,
    index: usize,
}

impl Gate {
    /// That part of the start is done. Never closed again.
    pub fn open(self) {
        let mut gates = self
            .readiness
            .inner
            .gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(g) = gates.get_mut(self.index) {
            g.1 = true;
        }
    }
}

/// The body of `/healthz` and `/readyz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Whether the check passed.
    pub ok: bool,
    /// Why not (`/readyz` 503 only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub why: Vec<String>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct BuildLabels {
    version: String,
    revision: String,
}

/// A metrics registry whose names start with the service's prefix (`magpie_…`). The service registers its own
/// metrics on it, then hands it to [`Ops::new`].
#[must_use]
pub fn registry(build: &Build) -> Registry {
    Registry::with_prefix(build.metric_prefix())
}

/// The ops server's state: build facts, the metrics registry, readiness.
#[derive(Clone)]
pub struct Ops {
    inner: Arc<OpsInner>,
}

struct OpsInner {
    build: Build,
    registry: Registry,
    readiness: Readiness,
}

impl fmt::Debug for Ops {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ops")
            .field("build", &self.inner.build)
            .field("readiness", &self.inner.readiness)
            .finish_non_exhaustive()
    }
}

impl Ops {
    /// The ops state for `build`, adding `<svc>_build_info{version,revision}` (always 1) and `<svc>_draining` to
    /// `registry` (made by [`registry`]).
    #[must_use]
    pub fn new(build: Build, mut registry: Registry) -> Self {
        let info = Family::<BuildLabels, Gauge>::default();
        info.get_or_create(&BuildLabels {
            version: crate::version::sha12().to_owned(),
            revision: crate::version::REVISION.to_owned(),
        })
        .set(1);
        registry.register("build_info", "The build: version (sha12) and revision; always 1", info);
        let draining = Gauge::default();
        registry.register("draining", "1 once the process has started draining", draining.clone());
        Self {
            inner: Arc::new(OpsInner {
                build,
                registry,
                readiness: Readiness::new(draining),
            }),
        }
    }

    /// The readiness gates and draining flag.
    #[must_use]
    pub fn readiness(&self) -> &Readiness {
        &self.inner.readiness
    }

    /// The build facts.
    #[must_use]
    pub fn build(&self) -> &Build {
        &self.inner.build
    }

    /// The Prometheus text of every registered metric.
    #[must_use]
    pub fn metrics_text(&self) -> String {
        let mut out = String::new();
        // writing into a String does not fail
        let _ = prometheus_client::encoding::text::encode_registry(&mut out, &self.inner.registry);
        out
    }

    /// The ops routes, with the response defaults of [`crate::http::defaults`].
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .route("/version", get(version))
            .route("/metrics", get(metrics))
            .with_state(self.clone())
            .layer(axum::middleware::from_fn(crate::http::defaults))
    }

    /// Binds `addr` (normally [`ADDR`]) and serves [`Ops::router`] on a background task for the life of the
    /// process: the ops port keeps answering while draining.
    ///
    /// # Errors
    /// When the address cannot be bound.
    pub async fn spawn(&self, addr: SocketAddr) -> std::io::Result<SocketAddr> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        let router = self.router();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!(error = %e, "ops server ended");
            }
        });
        Ok(local)
    }
}

async fn healthz() -> Response {
    crate::http::json(
        StatusCode::OK,
        &Health {
            ok: true,
            why: Vec::new(),
        },
    )
}

async fn readyz(State(ops): State<Ops>) -> Response {
    let why = ops.readiness().why();
    if why.is_empty() {
        crate::http::json(StatusCode::OK, &Health { ok: true, why })
    } else {
        crate::http::json(StatusCode::SERVICE_UNAVAILABLE, &Health { ok: false, why })
    }
}

async fn version(State(ops): State<Ops>) -> Response {
    crate::http::json(StatusCode::OK, &ops.build().info())
}

async fn metrics(State(ops): State<Ops>) -> Response {
    let mut r = ops.metrics_text().into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(METRICS_CONTENT_TYPE));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Build;
    use prometheus_client::metrics::counter::Counter;

    async fn start() -> (Ops, String, reqwest::Client) {
        let build = Build::new("magpie").surface("magpie-sandbox", 2);
        let mut reg = registry(&build);
        let turns = Counter::<u64>::default();
        reg.register("turns", "Turns started", turns.clone());
        turns.inc();
        let ops = Ops::new(build.clone(), reg);
        let addr = ops.spawn("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let client = crate::http::client_builder(&build, None).build().unwrap();
        (ops, format!("http://{addr}"), client)
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let (_ops, base, c) = start().await;
        let r = c.get(format!("{base}/healthz")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.headers()["content-type"], "application/json; charset=utf-8");
        assert_eq!(
            r.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"ok": true})
        );
    }

    #[tokio::test]
    async fn readyz_follows_gates_and_draining() {
        let (ops, base, c) = start().await;
        let get = || async {
            let r = c.get(format!("{base}/readyz")).send().await.unwrap();
            (r.status().as_u16(), r.json::<serde_json::Value>().await.unwrap())
        };
        // straight after spawn: not ready, before any gate exists
        assert_eq!(get().await, (503, serde_json::json!({"ok": false, "why": ["start"]})));

        let migrations = ops.readiness().gate("migrations");
        assert_eq!(
            get().await,
            (503, serde_json::json!({"ok": false, "why": ["migrations", "start"]}))
        );
        migrations.open();
        // between one gate opening and the next being made: still not ready
        assert_eq!(get().await, (503, serde_json::json!({"ok": false, "why": ["start"]})));
        let listen = ops.readiness().gate("listen");
        listen.open();
        assert_eq!(get().await, (503, serde_json::json!({"ok": false, "why": ["start"]})));
        ops.readiness().started();
        assert_eq!(get().await, (200, serde_json::json!({"ok": true})));

        ops.readiness().set_draining();
        assert_eq!(
            get().await,
            (503, serde_json::json!({"ok": false, "why": ["draining"]}))
        );
        // the ops port keeps serving while draining
        let h = c.get(format!("{base}/healthz")).send().await.unwrap();
        assert_eq!(h.status(), 200);
    }

    #[tokio::test]
    async fn version_reports_build_facts() {
        let (_ops, base, c) = start().await;
        let v: serde_json::Value = c
            .get(format!("{base}/version"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["name"], "magpie");
        assert_eq!(v["version"], crate::version::sha12());
        assert_eq!(v["revision"], crate::version::REVISION);
        assert!(v["builtAt"].is_string());
        assert_eq!(v["provides"], serde_json::json!({"magpie-sandbox": 2}));
        assert_eq!(v["accepts"], serde_json::json!({"magpie-sandbox": [1, 2]}));
    }

    #[tokio::test]
    async fn metrics_has_build_info_and_draining() {
        let (ops, base, c) = start().await;
        let r = c.get(format!("{base}/metrics")).send().await.unwrap();
        assert_eq!(r.headers()["content-type"], METRICS_CONTENT_TYPE);
        let text = r.text().await.unwrap();
        let info = format!(
            "magpie_build_info{{version=\"{}\",revision=\"{}\"}} 1",
            crate::version::sha12(),
            crate::version::REVISION
        );
        assert!(text.contains(&info), "{text}");
        assert!(text.contains("# TYPE magpie_draining gauge"), "{text}");
        assert!(text.contains("\nmagpie_draining 0\n"), "{text}");
        assert!(text.contains("magpie_turns_total 1"), "{text}");
        assert!(!text.contains("# EOF"), "0.0.4 text has no EOF marker: {text}");

        ops.readiness().set_draining();
        let text = c
            .get(format!("{base}/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(text.contains("\nmagpie_draining 1\n"), "{text}");
    }

    #[test]
    fn detached_readiness_works() {
        let r = Readiness::detached();
        assert!(!r.is_ready());
        assert_eq!(r.why(), [START]);
        let g = r.gate("config");
        r.started();
        assert_eq!(r.why(), ["config"]);
        g.open();
        assert!(r.is_ready());
    }
}
