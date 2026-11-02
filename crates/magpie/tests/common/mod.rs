//! magpie replicas in-process against the CI Postgres (`AVIARY_TEST_DATABASE_URL`, platform 6.1): a fresh database
//! per test, any number of replicas on it, each with its own HTTP port, readiness and drain.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::drain::{Drain, Signals};
use aviary_core::ops::Readiness;
use magpie::app::{Config, Service};
use magpie::jobs::Intervals;
use magpie_api::client::Client;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub fn admin_url() -> String {
    aviary_core::env::test_only("AVIARY_TEST_DATABASE_URL")
        .unwrap_or_else(|| panic!("AVIARY_TEST_DATABASE_URL is not set (ci.yml's test job sets it)"))
}

/// `url` with its database name replaced.
pub fn with_db(url: &str, db: &str) -> String {
    let (before, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let slash = before.rfind('/').expect("url has a database path");
    let mut out = format!("{}/{db}", &before[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

/// `url` pointing at another host:port.
pub fn with_addr(url: &str, addr: SocketAddr) -> String {
    let (head, tail) = url.split_once('@').expect("url has credentials");
    let rest = tail.split_once('/').expect("url has a path").1;
    format!("{head}@{addr}/{rest}")
}

fn admin() -> aviary_pg::Pool {
    aviary_pg::connect(&admin_url(), "magpie-test-admin", 1, Duration::from_secs(15)).unwrap()
}

/// A test's own database, dropped with the guard.
pub struct TestDb {
    pub url: String,
    name: String,
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                admin()
                    .get()
                    .await?
                    .batch_execute(&format!("drop database if exists {name} with (force)"))
                    .await
            })
        })
        .join();
    }
}

pub async fn fresh_db(test: &str) -> TestDb {
    let name = format!("magpie_{test}_{}", Uuid::new_v4().simple());
    admin()
        .get()
        .await
        .unwrap()
        .batch_execute(&format!("create database {name}"))
        .await
        .unwrap();
    TestDb {
        url: with_db(&admin_url(), &name),
        name,
    }
}

/// One pair token per caller, shared by every replica of a test.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub pigeon: String,
    pub gateway: String,
    pub hermit: String,
    pub admin: String,
}

impl Tokens {
    pub fn new() -> Self {
        let t = |c: &str| format!("test-{c}-{}", Uuid::new_v4().simple());
        Self {
            pigeon: t("pigeon"),
            gateway: t("gateway"),
            hermit: t("hermit"),
            admin: t("admin"),
        }
    }

    pub fn callers(&self) -> Callers {
        Callers::new([
            (Caller::Pigeon, vec![self.pigeon.clone()]),
            (Caller::Gateway, vec![self.gateway.clone()]),
            (Caller::Hermit, vec![self.hermit.clone()]),
            (Caller::Admin, vec![self.admin.clone()]),
        ])
        .unwrap()
    }
}

pub fn http() -> reqwest::Client {
    let build = aviary_core::version::Build::new("magpie-test");
    aviary_core::http::client_builder(&build, None).build().unwrap()
}

/// Fast job intervals for tests.
pub fn fast() -> Intervals {
    Intervals {
        leader_retry: Duration::from_millis(200),
        purge: Duration::from_millis(200),
        outbox: Duration::from_millis(200),
        retention: Duration::from_millis(200),
    }
}

/// Job intervals that never run within a test (the test calls the passes itself).
pub fn idle() -> Intervals {
    Intervals {
        leader_retry: Duration::from_millis(200),
        purge: Duration::from_secs(3600),
        outbox: Duration::from_secs(3600),
        retention: Duration::from_secs(3600),
    }
}

/// A running replica.
pub struct Replica {
    pub service: Option<Service>,
    pub url: String,
    pub tokens: Tokens,
    pub readiness: Readiness,
    drain: Mutex<Option<Drain>>,
    server: JoinHandle<()>,
}

impl Drop for Replica {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Log lines of the replicas go to the test's output (shown for a failing test).
fn init_logs() {
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::INFO)
            .finish(),
    );
}

/// Starts a replica on `db_url` (hermit at `hermit_url`).
pub async fn replica_with(db_url: &str, tokens: &Tokens, hermit_url: &str, iv: Intervals) -> Replica {
    init_logs();
    let readiness = Readiness::detached();
    let drain = Drain::new(readiness.clone());
    let cfg = Config {
        database_url: db_url.to_owned(),
        callers: tokens.callers(),
        hermit_url: hermit_url.to_owned(),
        hermit_token: "test-magpie-hermit".to_owned(),
        pod: format!("magpie-test-{}", Uuid::new_v4().simple()),
    };
    let service = Service::start_with(cfg, &readiness, drain.draining(), iv)
        .await
        .unwrap();
    readiness.started();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = service.router();
    let mut draining = drain.draining();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { draining.wait().await })
            .await;
    });
    Replica {
        service: Some(service),
        url: format!("http://{addr}"),
        tokens: tokens.clone(),
        readiness,
        drain: Mutex::new(Some(drain)),
        server,
    }
}

/// A replica with hermit at an address nothing answers on.
pub async fn replica(db_url: &str, tokens: &Tokens) -> Replica {
    replica_with(db_url, tokens, "http://127.0.0.1:1", idle()).await
}

impl Replica {
    pub fn client(&self, token: &str) -> Client {
        Client::new(http(), self.url.clone(), token.to_owned())
    }

    pub fn pigeon(&self) -> Client {
        self.client(&self.tokens.pigeon)
    }

    pub fn hermit(&self) -> Client {
        self.client(&self.tokens.hermit)
    }

    pub fn admin(&self) -> Client {
        self.client(&self.tokens.admin)
    }

    pub fn app(&self) -> &magpie::app::Magpie {
        &self.service.as_ref().unwrap().app
    }

    pub fn pool(&self) -> &aviary_pg::Pool {
        &self.app().pool
    }

    /// Starts draining (as SIGTERM would): `/readyz` 503 and held polls answer at once.
    pub fn start_drain(&self) {
        let drain = self.drain.lock().unwrap().take().unwrap();
        tokio::spawn(async move {
            let (_tx, mut signals) = Signals::channel();
            drain.drain(&mut signals).await;
        });
    }
}

/// A TCP proxy in front of Postgres that a test can cut (every connection closed, new ones refused) and restore:
/// "Postgres became unreachable" for the replicas behind it only.
pub struct CutProxy {
    pub addr: SocketAddr,
    state: Arc<Mutex<ProxyState>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct ProxyState {
    cut: bool,
    conns: Vec<JoinHandle<()>>,
}

impl Drop for CutProxy {
    fn drop(&mut self) {
        self.task.abort();
        for c in self.state.lock().unwrap().conns.drain(..) {
            c.abort();
        }
    }
}

impl CutProxy {
    pub async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ProxyState::default()));
        let st = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if st.lock().unwrap().cut {
                    let _ = inbound.shutdown().await;
                    continue;
                }
                let conn = tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                st.lock().unwrap().conns.push(conn);
            }
        });
        Self { addr, state, task }
    }

    pub fn cut(&self) {
        let mut s = self.state.lock().unwrap();
        s.cut = true;
        for c in s.conns.drain(..) {
            c.abort();
        }
    }

    pub fn restore(&self) {
        self.state.lock().unwrap().cut = false;
    }
}

/// The Postgres address of a URL.
pub fn pg_addr(url: &str) -> SocketAddr {
    let tail = url.split_once('@').unwrap().1;
    let hostport = tail.split_once('/').unwrap().0;
    hostport.parse().unwrap()
}

/// An admin pool on a test database (statements the API does not offer: set `migrating`, age rows).
pub fn sql(url: &str) -> aviary_pg::Pool {
    aviary_pg::connect(url, "magpie-test-sql", 2, Duration::from_secs(15)).unwrap()
}

pub async fn eventually(limit: Duration, mut f: impl AsyncFnMut() -> bool) -> bool {
    let end = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f().await
}
