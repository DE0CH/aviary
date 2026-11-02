//! The harness of hermit's integration tests: a fresh database per test on the CI Postgres
//! (`AVIARY_TEST_DATABASE_URL`), a fake fabric whose sandboxes are fake supervisors on local ports, in-memory
//! workspaces, and replicas started in-process (any number, on one database) with short timings.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_fakes::FakeSupervisor;
use aviary_fakes::supervisor::Running;
use hermit::app::{App, StartOptions};
use hermit::catalogue::{Catalogue, Loaded};
use hermit::config::{Config, HermitJson};
use hermit::fabric::{
    CreateError, CreateSpec, Fabric, FabricError, Fut, Launch, Place, Placement, Providers, WorkspaceError, Workspaces,
};
use hermit::tokens::Keys;
use hermit::{Core, Timings};
use hermit_api::Timestamp;
use hermit_api::error::{self, codes};
use hermit_api::workspaces::{
    Amount, ByteRange, ENTRIES_PAGE, Entries, Entry, EntryType, FileContent, FileMeta, Workspace,
};
use tokio::sync::Notify;
use uuid::Uuid;

pub const KEYS: &str = "1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
pub const STAFF: &str = "staff@example.net";
pub const ADMIN: &str = "boss@example.com";

pub fn admin_url() -> String {
    aviary_core::env::test_only("AVIARY_TEST_DATABASE_URL")
        .unwrap_or_else(|| panic!("AVIARY_TEST_DATABASE_URL is not set (ci.yml's test job sets it)"))
}

fn with_db(url: &str, db: &str) -> String {
    let (before, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let slash = before.rfind('/').expect("url has a database path");
    let mut out = format!("{}/{db}", &before[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

fn admin() -> aviary_pg::Pool {
    aviary_pg::connect(&admin_url(), "hermit-test", 1, Duration::from_secs(15)).unwrap()
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
    let name = format!("hermit_{test}_{}", Uuid::new_v4().simple());
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

/// The test timings: production's shape, seconds instead of minutes.
pub fn timings() -> Timings {
    let ms = Duration::from_millis;
    Timings {
        claim: ms(1500),
        renew: ms(300),
        sweep: ms(150),
        janitor: Duration::from_secs(3600),
        leader_retry: ms(300),
        health_poll: ms(50),
        live_reload: ms(500),
        catalogue_reload: ms(200),
        recheck: ms(200),
        capacity_retry: ms(100),
        host_poll: ms(100),
        busy_retry: ms(100),
        workspace_retry: ms(50),
        release_retry: ms(100),
        health_gap: ms(100),
        release_deadline: ms(1500),
        put_release_wait: ms(1500),
        delete_wait: ms(5000),
        ..Timings::default()
    }
}

pub fn config(edit: impl FnOnce(&mut serde_json::Value)) -> Arc<Config> {
    let mut v: serde_json::Value = serde_json::from_str(include_str!("../fixtures/hermit.json")).unwrap();
    edit(&mut v);
    let file: HermitJson = serde_json::from_value(v).unwrap();
    Arc::new(
        Config::from_parts(
            file,
            "You are the deployment's assistant.\n",
            "Workspace {workspaceGiB} GiB, {workspaceFiles} files; scratch {tmpGiB} GiB.",
            "Write a short title.",
        )
        .unwrap(),
    )
}

pub fn catalogue() -> Arc<Catalogue> {
    let bytes = serde_json::json!({"users": ["@example.org", STAFF, ADMIN], "admins": [ADMIN]}).to_string();
    Arc::new(Catalogue::fixed(Loaded::parse(bytes.as_bytes()).unwrap()))
}

/// A gate a fake call waits at while it is shut (a start held in one of its phases).
#[derive(Default)]
pub struct Gate {
    shut: AtomicBool,
    /// Calls that reached the gate.
    pub reached: AtomicU32,
}

impl Gate {
    pub fn shut(&self) {
        self.shut.store(true, Ordering::SeqCst);
    }

    pub fn open(&self) {
        self.shut.store(false, Ordering::SeqCst);
    }

    async fn pass(&self) {
        self.reached.fetch_add(1, Ordering::SeqCst);
        while self.shut.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// One fake sandbox: a fake supervisor on a local port.
pub struct FakeSandbox {
    pub supervisor: Arc<FakeSupervisor>,
    pub running: Running,
    pub host: String,
    pub spec_env: Vec<(String, String)>,
}

/// The fake fabric: one host, addresses from 198.18.0.10, sandboxes as fake supervisors.
pub struct FakeFabric {
    keys: Arc<Keys>,
    next_ip: AtomicU32,
    pub by_ip: Mutex<HashMap<IpAddr, Arc<FakeSandbox>>>,
    pub by_name: Mutex<HashMap<String, IpAddr>>,
    pub created: Mutex<Vec<String>>,
    pub destroyed: Mutex<Vec<String>>,
    pub creates: AtomicU32,
    /// When set, `create` waits for `release_create` (a start held in its `creating` phase).
    pub hold_create: AtomicBool,
    pub create_gate: Notify,
    /// A delay before a created supervisor answers ready (the start stays in `supervisor`).
    pub ready_delay: Mutex<Duration>,
    pub no_slot: AtomicBool,
    pub launch: Mutex<Launch>,
    pub settled: AtomicBool,
    /// A create that fails with this class.
    pub fail_create: Mutex<Option<String>>,
    /// This many creates fail as unreachable (a transport error) before the next ones work.
    pub unreachable_creates: AtomicU32,
    /// Placement waits here (a start held in `placing`, or in `host` while it polls).
    pub place_gate: Gate,
    /// Placement fails with a fabric error while set.
    pub place_error: AtomicBool,
    /// Every supervisor ever made, by sandbox id (their call logs outlive the destroy).
    pub all: Mutex<Vec<(Uuid, Arc<FakeSupervisor>)>>,
}

impl FakeFabric {
    pub fn new(keys: Arc<Keys>) -> Arc<Self> {
        Arc::new(Self {
            keys,
            next_ip: AtomicU32::new(10),
            by_ip: Mutex::new(HashMap::new()),
            by_name: Mutex::new(HashMap::new()),
            created: Mutex::new(Vec::new()),
            destroyed: Mutex::new(Vec::new()),
            creates: AtomicU32::new(0),
            hold_create: AtomicBool::new(false),
            create_gate: Notify::new(),
            ready_delay: Mutex::new(Duration::ZERO),
            no_slot: AtomicBool::new(false),
            launch: Mutex::new(Launch::Full),
            settled: AtomicBool::new(true),
            fail_create: Mutex::new(None),
            unreachable_creates: AtomicU32::new(0),
            place_gate: Gate::default(),
            place_error: AtomicBool::new(false),
            all: Mutex::new(Vec::new()),
        })
    }

    pub fn release_create(&self) {
        self.hold_create.store(false, Ordering::SeqCst);
        self.create_gate.notify_waiters();
    }

    /// The fake supervisor of a sandbox id.
    pub fn supervisor(&self, row_ip: IpAddr) -> Option<Arc<FakeSupervisor>> {
        self.by_ip.lock().unwrap().get(&row_ip).map(|s| s.supervisor.clone())
    }

    pub fn sandbox_by_name(&self, name: &str) -> Option<Arc<FakeSandbox>> {
        let ip = *self.by_name.lock().unwrap().get(name)?;
        self.by_ip.lock().unwrap().get(&ip).cloned()
    }

    /// The bodies of the release calls sandbox `id`'s supervisor received.
    pub fn release_bodies(&self, id: Uuid) -> Vec<serde_json::Value> {
        self.all
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, _)| *i == id)
            .flat_map(|(_, s)| s.script().calls_to("release"))
            .filter_map(|c| c.body)
            .collect()
    }

    /// The sandbox's supervisor stops answering (its address leads nowhere).
    pub fn silence(&self, ip: IpAddr) {
        self.by_ip.lock().unwrap().remove(&ip);
    }

    pub fn live_count(&self) -> usize {
        self.by_ip.lock().unwrap().len()
    }
}

impl Fabric for FakeFabric {
    fn place(&self, _id: Uuid, _memory: u64) -> Fut<'_, Result<Place, FabricError>> {
        Box::pin(async move {
            self.place_gate.pass().await;
            if self.place_error.load(Ordering::SeqCst) {
                return Err(FabricError("the fake fabric was told to fail placement".into()));
            }
            if self.no_slot.load(Ordering::SeqCst) {
                return Ok(Place::NoSlot);
            }
            let n = self.next_ip.fetch_add(1, Ordering::SeqCst);
            let ip = Ipv4Addr::new(
                198,
                18,
                u8::try_from(n / 250).unwrap(),
                u8::try_from(10 + n % 240).unwrap(),
            );
            Ok(Place::Placed(Placement {
                host: "fake-1".into(),
                ip: IpAddr::V4(ip),
            }))
        })
    }

    fn request_host(&self) -> Fut<'_, Launch> {
        Box::pin(async move { *self.launch.lock().unwrap() })
    }

    fn create<'a>(&'a self, spec: &'a CreateSpec) -> Fut<'a, Result<(), CreateError>> {
        Box::pin(async move {
            self.creates.fetch_add(1, Ordering::SeqCst);
            while self.hold_create.load(Ordering::SeqCst) {
                let n = self.create_gate.notified();
                if !self.hold_create.load(Ordering::SeqCst) {
                    break;
                }
                let _ = tokio::time::timeout(Duration::from_millis(100), n).await;
            }
            if self
                .unreachable_creates
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(CreateError::Unreachable("connection refused (fake)".into()));
            }
            if let Some(class) = self.fail_create.lock().unwrap().clone() {
                return Err(CreateError::Failed {
                    class,
                    message: "the fake fabric was told to fail".into(),
                });
            }
            if self.by_ip.lock().unwrap().contains_key(&spec.ip) {
                // the same sandbox again: sandboxd is idempotent
                return Ok(());
            }
            let token = self.keys.control_token(self.keys.newest(), spec.sandbox_id).unwrap();
            let sup = FakeSupervisor::new(&token);
            let delay = *self.ready_delay.lock().unwrap();
            if !delay.is_zero() {
                sup.set_ready(false);
                let s = sup.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    s.set_ready(true);
                });
            }
            let running = sup.serve().await.unwrap();
            self.all.lock().unwrap().push((spec.sandbox_id, sup.clone()));
            let sbx = Arc::new(FakeSandbox {
                supervisor: sup,
                running,
                host: spec.host.clone(),
                spec_env: spec.env.clone(),
            });
            self.by_ip.lock().unwrap().insert(spec.ip, sbx);
            self.by_name.lock().unwrap().insert(spec.name.clone(), spec.ip);
            self.created.lock().unwrap().push(spec.name.clone());
            Ok(())
        })
    }

    fn control_url(&self, _host: &str, ip: IpAddr) -> String {
        self.by_ip
            .lock()
            .unwrap()
            .get(&ip)
            .map_or_else(|| "http://127.0.0.1:9".to_owned(), |s| s.running.url())
    }

    fn console_tail<'a>(&'a self, _host: &'a str, name: &'a str) -> Fut<'a, Option<Vec<u8>>> {
        Box::pin(async move { Some(format!("console of {name}\n").into_bytes()) })
    }

    fn destroy<'a>(
        &'a self,
        _host: Option<&'a str>,
        _also: Option<&'a str>,
        name: &'a str,
    ) -> Fut<'a, Result<(), FabricError>> {
        Box::pin(async move {
            if let Some(ip) = self.by_name.lock().unwrap().remove(name) {
                self.by_ip.lock().unwrap().remove(&ip);
            }
            self.destroyed.lock().unwrap().push(name.to_owned());
            Ok(())
        })
    }

    fn gateway_settled(&self) -> bool {
        self.settled.load(Ordering::SeqCst)
    }
}

struct Ws {
    created_at: Timestamp,
    files: BTreeMap<String, Vec<u8>>,
}

/// In-memory workspaces.
#[derive(Default)]
pub struct FakeWorkspaces {
    ws: Mutex<HashMap<Uuid, Ws>>,
    pub ensured: Mutex<Vec<Uuid>>,
    pub broken: AtomicBool,
    /// `ensure` waits here (a start held in `workspace`).
    pub ensure_gate: Gate,
}

const QUOTA: Amount = Amount {
    bytes: 1 << 30,
    inodes: 1000,
};

impl FakeWorkspaces {
    fn of(id: Uuid, w: &Ws) -> Workspace {
        Workspace {
            id,
            created_at: w.created_at,
            quota: QUOTA,
            usage: Amount {
                bytes: w.files.values().map(|f| f.len() as u64).sum(),
                inodes: w.files.len() as u64,
            },
        }
    }

    pub fn write(&self, wid: Uuid, path: &str, data: &[u8]) {
        let mut ws = self.ws.lock().unwrap();
        let w = ws.entry(wid).or_insert_with(|| Ws {
            created_at: Timestamp::now(),
            files: BTreeMap::new(),
        });
        w.files.insert(path.to_owned(), data.to_vec());
    }

    pub fn exists(&self, wid: Uuid) -> bool {
        self.ws.lock().unwrap().contains_key(&wid)
    }

    fn check(&self) -> Result<(), WorkspaceError> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(WorkspaceError::Unavailable("the fake store is broken".into()));
        }
        Ok(())
    }
}

fn no_workspace() -> WorkspaceError {
    WorkspaceError::Api(error::not_found(codes::NO_WORKSPACE, "no such workspace"))
}

impl Workspaces for FakeWorkspaces {
    fn ensure(&self, wid: Uuid) -> Fut<'_, Result<(Workspace, bool), WorkspaceError>> {
        Box::pin(async move {
            self.ensure_gate.pass().await;
            self.check()?;
            self.ensured.lock().unwrap().push(wid);
            let mut ws = self.ws.lock().unwrap();
            let made = !ws.contains_key(&wid);
            let w = ws.entry(wid).or_insert_with(|| Ws {
                created_at: Timestamp::now(),
                files: BTreeMap::new(),
            });
            Ok((Self::of(wid, w), made))
        })
    }

    fn get(&self, wid: Uuid) -> Fut<'_, Result<Option<Workspace>, WorkspaceError>> {
        Box::pin(async move {
            self.check()?;
            Ok(self.ws.lock().unwrap().get(&wid).map(|w| Self::of(wid, w)))
        })
    }

    fn entries<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        _cursor: Option<&'a str>,
    ) -> Fut<'a, Result<Entries, WorkspaceError>> {
        Box::pin(async move {
            self.check()?;
            let ws = self.ws.lock().unwrap();
            let w = ws.get(&wid).ok_or_else(no_workspace)?;
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            let mut names: BTreeMap<String, Entry> = BTreeMap::new();
            for (p, data) in &w.files {
                let Some(rest) = p.strip_prefix(&prefix) else { continue };
                let (name, kind, size) = match rest.split_once('/') {
                    Some((d, _)) => (d.to_owned(), EntryType::Dir, 0),
                    None => (rest.to_owned(), EntryType::File, data.len() as u64),
                };
                names.entry(name.clone()).or_insert(Entry {
                    name,
                    kind,
                    size,
                    mtime: w.created_at,
                });
            }
            if !path.is_empty() && names.is_empty() {
                return Err(WorkspaceError::Api(error::not_found(codes::NO_PATH, "no such path")));
            }
            Ok(Entries {
                path: path.to_owned(),
                entries: names.into_values().take(ENTRIES_PAGE).collect(),
                next_cursor: None,
            })
        })
    }

    fn file<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        range: Option<ByteRange>,
    ) -> Fut<'a, Result<FileContent, WorkspaceError>> {
        Box::pin(async move {
            self.check()?;
            let ws = self.ws.lock().unwrap();
            let w = ws.get(&wid).ok_or_else(no_workspace)?;
            let data = w
                .files
                .get(path)
                .ok_or_else(|| WorkspaceError::Api(error::not_found(codes::NO_PATH, "no such path")))?;
            let size = data.len() as u64;
            let served = match range {
                Some(r) => Some(
                    r.resolve(size)
                        .ok_or_else(|| WorkspaceError::Api(error::range_not_satisfiable(size)))?,
                ),
                None => None,
            };
            let bytes = served.map_or_else(
                || data.clone(),
                |(a, b)| data[usize::try_from(a).unwrap()..=usize::try_from(b).unwrap()].to_vec(),
            );
            let meta = FileMeta {
                size,
                mtime: w.created_at,
                etag: FileMeta::etag_for(size, 0),
                served,
            };
            Ok(FileContent::from_bytes(meta, bytes))
        })
    }

    fn clone_workspace(&self, wid: Uuid, from: Uuid) -> Fut<'_, Result<Workspace, WorkspaceError>> {
        Box::pin(async move {
            self.check()?;
            let mut ws = self.ws.lock().unwrap();
            let files = ws.get(&from).ok_or_else(no_workspace)?.files.clone();
            if ws.contains_key(&wid) {
                return Err(WorkspaceError::Api(error::api(
                    409,
                    codes::EXISTS,
                    "the workspace exists",
                )));
            }
            let w = Ws {
                created_at: Timestamp::now(),
                files,
            };
            let out = Self::of(wid, &w);
            ws.insert(wid, w);
            Ok(out)
        })
    }

    fn delete(&self, wid: Uuid) -> Fut<'_, Result<(), WorkspaceError>> {
        Box::pin(async move {
            self.check()?;
            self.ws.lock().unwrap().remove(&wid);
            Ok(())
        })
    }
}

/// A database, the fakes and the callers' tokens; replicas start on it.
pub struct Harness {
    pub db: TestDb,
    pub keys: Arc<Keys>,
    pub config: Arc<Config>,
    pub catalogue: Arc<Catalogue>,
    pub fabric: Arc<FakeFabric>,
    pub workspaces: Arc<FakeWorkspaces>,
    pub tokens: HashMap<Caller, String>,
    pub timings: Timings,
}

pub const CALLERS: [Caller; 6] = [
    Caller::Pigeon,
    Caller::Magpie,
    Caller::Gateway,
    Caller::Tools,
    Caller::Monitor,
    Caller::Admin,
];

impl Harness {
    pub async fn new(test: &str) -> Self {
        Self::with(test, config(|_| {}), timings()).await
    }

    pub async fn with(test: &str, config: Arc<Config>, timings: Timings) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let keys = Arc::new(Keys::parse(KEYS).unwrap());
        let tokens = CALLERS
            .iter()
            .map(|c| (*c, format!("test-{}-{}", c.as_str(), Uuid::new_v4().simple())))
            .collect();
        Self {
            db: fresh_db(test).await,
            fabric: FakeFabric::new(keys.clone()),
            keys,
            config,
            catalogue: catalogue(),
            workspaces: Arc::new(FakeWorkspaces::default()),
            tokens,
            timings,
        }
    }

    pub fn callers(&self) -> Callers {
        Callers::new(self.tokens.iter().map(|(c, t)| (*c, vec![t.clone()]))).unwrap()
    }

    pub async fn replica(&self, pod: &str) -> Replica {
        let app = App::start(StartOptions {
            config: self.config.clone(),
            catalogue: self.catalogue.clone(),
            reload_catalogue: false,
            keys: self.keys.clone(),
            database_url: self.db.url.clone(),
            pod: pod.to_owned(),
            pool_size: 4,
            providers: Providers {
                fabric: self.fabric.clone(),
                workspaces: self.workspaces.clone(),
            },
            timings: self.timings.clone(),
            callers: self.callers(),
        })
        .await
        .unwrap();
        let app = Arc::new(app);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = app.router();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Replica { app, url, server }
    }

    pub fn client(&self, caller: Caller) -> Client {
        Client {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            token: self.tokens[&caller].clone(),
        }
    }

    pub fn pool(&self) -> aviary_pg::Pool {
        aviary_pg::connect(&self.db.url, "hermit-test-check", 2, Duration::from_secs(15)).unwrap()
    }

    /// Runs SQL on the test database.
    pub async fn sql(&self, sql: &str) -> u64 {
        self.pool().get().await.unwrap().execute(sql, &[]).await.unwrap()
    }
}

/// A replica serving on a local port.
pub struct Replica {
    pub app: Arc<App>,
    pub url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Replica {
    pub fn core(&self) -> &Arc<Core> {
        &self.app.core
    }

    /// Dies the way SIGKILL would.
    pub fn kill(&self) {
        self.app.kill();
        self.server.abort();
    }
}

impl Drop for Replica {
    fn drop(&mut self) {
        self.server.abort();
        self.app.kill();
    }
}

/// A caller's HTTP client.
pub struct Client {
    pub http: reqwest::Client,
    pub token: String,
}

pub struct Answer {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: serde_json::Value,
}

impl Client {
    pub async fn call(
        &self,
        base: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        extra: &[(&str, String)],
    ) -> Answer {
        let surface = if path.starts_with("/v1/sandbox/") {
            hermit_api::SANDBOX
        } else {
            hermit_api::CORE
        };
        let mut rb = self
            .http
            .request(method.parse().unwrap(), format!("{base}{path}"))
            .bearer_auth(&self.token)
            .header("aviary-protocol", format!("{}={}", surface.0, surface.1))
            .timeout(Duration::from_secs(40));
        for (k, v) in extra {
            rb = rb.header(*k, v);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let res = rb.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let bytes = res.bytes().await.unwrap();
        Answer {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Waits until `f` holds, up to `limit`.
pub async fn eventually<F, Fut2>(limit: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut2,
    Fut2: std::future::Future<Output = bool>,
{
    let end = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f().await
}
