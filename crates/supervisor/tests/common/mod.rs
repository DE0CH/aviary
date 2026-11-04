//! What the integration tests share: fake hermit and fake magpie behind the reference gateway (the sandbox token
//! in the proxy URL, DESIGN 12 D1), the sandbox's paths under a test directory, and small process helpers.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use aviary_core::auth::{Caller, Callers};
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::magpie::FakeMagpie;
use hermit_api::sandbox::PutSandboxRequest;
use supervisor::boot::BootEnv;
use supervisor::lifecycle::{BoxFuture, Filesystem, FsStats};
use supervisor::start::Paths;
use uuid::Uuid;

/// The fakes a supervisor talks to.
pub struct Fakes {
    pub hermit: std::sync::Arc<FakeHermit>,
    pub magpie: std::sync::Arc<FakeMagpie>,
    pub gateway: test_gateway::Running,
    pub cid: Uuid,
    pub sandbox: Uuid,
    _served: (aviary_fakes::hermit::Running, aviary_fakes::magpie::Running),
}

impl Fakes {
    pub async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let hermit = FakeHermit::with_config(HermitConfig::default());
        let magpie = FakeMagpie::new();
        let hs = hermit.serve().await.unwrap();
        let ms = magpie.serve().await.unwrap();
        let cid = Uuid::now_v7();
        let sandbox = hs
            .client(reqwest::Client::new(), Caller::Magpie)
            .put_sandbox(
                cid,
                &PutSandboxRequest {
                    person: "staff@example.org".into(),
                },
            )
            .await
            .unwrap()
            .sandbox()
            .id;
        let token = |n: &str| format!("{n}-{}", Uuid::new_v4().simple());
        let mut registry = prometheus_client::registry::Registry::default();
        let gateway = test_gateway::start(
            test_gateway::Config {
                proxy_listen: "127.0.0.1:0".parse().unwrap(),
                api_listen: "127.0.0.1:0".parse().unwrap(),
                upstreams: test_gateway::Upstreams {
                    magpie: ms.url(),
                    hermit: hs.url(),
                    tools: "http://127.0.0.1:9".into(),
                    model: "http://127.0.0.1:9".into(),
                },
                credentials: test_gateway::Credentials {
                    magpie: magpie.token(Caller::Gateway).unwrap(),
                    hermit: hermit.token(Caller::Gateway).unwrap(),
                    tools: token("t"),
                    model: token("m"),
                },
                callers: Callers::new([(Caller::Pigeon, vec![token("p")]), (Caller::Hermit, vec![token("h")])])
                    .unwrap(),
                self_hosts: vec![],
                ops_summary: test_gateway::default_ops_summary(),
                drain: test_gateway::DrainBounds::default(),
                upload_cap: test_gateway::UPLOAD_CAP,
                upstream_idle: gateway_api::limits::IDLE_TIMEOUT,
                faults: test_gateway::Faults::default(),
            },
            &mut registry,
        )
        .await
        .unwrap();
        gateway.keys_loaded().await;
        Self {
            hermit,
            magpie,
            gateway,
            cid,
            sandbox,
            _served: (hs, ms),
        }
    }

    /// The proxy URL with a fresh sandbox token (the boot environment's `HTTP_PROXY`).
    pub fn proxy(&self) -> String {
        let st = self.hermit.mint_token(self.cid, Ipv4Addr::LOCALHOST).unwrap();
        format!("http://sbx:{st}@{}", self.gateway.proxy_addr())
    }

    /// A `:7070` client with the sandbox's control token.
    pub fn control(&self, port: &str) -> supervisor_api::client::ControlClient {
        supervisor_api::client::ControlClient::new(
            reqwest::Client::new(),
            format!("http://127.0.0.1:{}", port.trim()),
            self.hermit.control_token(self.sandbox).unwrap(),
        )
    }
}

/// The boot environment of a test supervisor.
pub fn boot(proxy: String) -> BootEnv {
    BootEnv {
        proxy,
        path: "/usr/bin:/bin".into(),
        lang: None,
        lc_all: None,
    }
}

/// The sandbox's paths under `d` (the keeper's run directory is `d/run`).
pub fn paths(d: &Path) -> Paths {
    std::fs::create_dir_all(d.join("workspace")).unwrap();
    // SAFETY: getuid/getgid have no memory effects.
    #[allow(unsafe_code)]
    let ids = unsafe { (libc::getuid(), libc::getgid()) };
    let mut paths = Paths::sandbox();
    paths.run_dir = d.join("run");
    paths.state_dir = d.join("state");
    paths.exec_dir = d.join("exec");
    paths.agent_socket = d.join("agent.sock");
    paths.docker_socket = d.join("docker.sock");
    paths.etc_hosts = d.join("hosts");
    paths.apt_proxy = d.join("apt");
    paths.sys_cgroup = d.join("no-cgroup");
    paths.oom_score_adj = d.join("oom");
    // never the host's /proc: a test supervisor must not see (or kill) the host's processes
    paths.proc = d.join("proc");
    paths.workspace = Some(d.join("workspace").to_string_lossy().into_owned());
    paths.exec_ids = ids;
    paths
}

/// A filesystem that mounts at once and has nothing in flight (WP-22's layer is not in these tests).
pub struct NullFs;

impl Filesystem for NullFs {
    fn mount(
        &self,
        _lease: &magpie_api::sandbox::LeaseResponse,
        _keeper: &supervisor::keeper::Client,
        _restart: bool,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn stats(&self) -> FsStats {
        FsStats {
            magpie_reachable: true,
            ..FsStats::default()
        }
    }
    fn settle(&self, _deadline: tokio::time::Instant) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    fn close_connections(&self) {}
    fn resumed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn unmount(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

pub fn pipe() -> (i32, i32) {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two fds into the local array.
    #[allow(unsafe_code)]
    let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(r, 0);
    (fds[0], fds[1])
}

/// `VmRSS` of `pid` in bytes (0 when it is gone).
pub fn rss(pid: u32) -> u64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

/// A fresh test directory.
pub fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

// A real supervisor process under a keeper: the keeper runs on a thread of the test process, its supervisor argv
// is this test binary again, running only the ignored child test, which runs the start sequence with a recording
// agent and filesystem and exits with its exit code.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use supervisor::keeper::server::{Config, Keeper, Signals};
use supervisor::keeper::{self, Client, SpawnSpec};
use supervisor::lifecycle::{Agent, AttachContext, RunStatus};
use supervisor::start::{Reattach, Start};
use supervisor::state::{Change, GenState};
use supervisor_api::control::Generation;
use tokio::sync::watch;
use tokio::time::Instant;

/// What the parent tells the child supervisor.
#[derive(Serialize, Deserialize)]
pub struct ChildConfig {
    pub dir: PathBuf,
    pub proxy: String,
}

/// The child supervisor's entry: does nothing unless started by [`KeeperThread`] (a `cfg=` argument).
pub fn child_main() {
    let Some(cfg_path) = std::env::args().find_map(|a| a.strip_prefix("cfg=").map(PathBuf::from)) else {
        return;
    };
    let cfg: ChildConfig = serde_json::from_str(&std::fs::read_to_string(cfg_path).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let code = rt.block_on(async move {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let d = cfg.dir.clone();
        let boot = boot(cfg.proxy);
        let build = aviary_core::version::Build::new("supervisor");
        aviary_core::log::init(&build, None, aviary_core::log::Output::Stderr);
        let http = boot.fixed_names_client(&build).unwrap();
        let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = control.local_addr().unwrap().port().to_string();
        std::fs::write(d.join("port.tmp"), port).unwrap();
        std::fs::rename(d.join("port.tmp"), d.join("port")).unwrap();
        let start = Start {
            paths: paths(&d),
            boot,
            build,
            hermit: supervisor::hermit::Hermit::new(http.clone()),
            magpie: supervisor::magpie::client(http),
            keeper: Client::new(&d.join("run")),
            agent: RecordingAgent::new(&d),
            fs: Arc::new(RecordingFs { dir: d.clone() }),
            control,
        };
        let (_running, done) = start.spawn();
        done.await.unwrap_or(70)
    });
    std::process::exit(code);
}

fn append(path: &Path, line: &str) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{line}").unwrap();
}

/// The lines of `name` in `dir` (empty when absent).
pub fn lines(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// An agent that does what the driver's attach does with a generation (WP-23): pre-spawns a stand-in Claude Code
/// through the keeper and records it in `run.json`, or re-attaches to the live one; it logs to `agent.log`.
pub struct RecordingAgent {
    dir: PathBuf,
    status: watch::Sender<RunStatus>,
    live: Mutex<Option<(Client, i32)>>,
}

impl RecordingAgent {
    pub fn new(dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            dir: dir.to_owned(),
            status: watch::channel(RunStatus::idle()).0,
            live: Mutex::new(None),
        })
    }
    fn log(&self, line: &str) {
        append(&self.dir.join("agent.log"), line);
    }
}

impl Agent for RecordingAgent {
    fn attach(&self, ctx: AttachContext) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            match ctx.generation {
                Reattach::Alive { n, pid } => {
                    self.log(&format!("attach alive {n} {pid}"));
                    *self.live.lock().unwrap() = Some((ctx.keeper.clone(), pid));
                }
                other => {
                    let n = ctx
                        .prior
                        .as_ref()
                        .and_then(|p| p.generation.as_ref())
                        .map_or(0, |g| g.n)
                        + 1;
                    let spec = SpawnSpec {
                        name: "claude".into(),
                        // outlives every soak (soak_long restarts every 10 min); stop kills its group
                        argv: vec!["/bin/sleep".into(), "infinity".into()],
                        env: vec![],
                        cwd: None,
                        // SAFETY: getuid/getgid have no memory effects.
                        #[allow(unsafe_code)]
                        uid: unsafe { libc::getuid() },
                        // SAFETY: as above.
                        #[allow(unsafe_code)]
                        gid: unsafe { libc::getgid() },
                        setsid: true,
                        oom_score_adj: 100,
                        cgroup: None,
                        stdout: None,
                        stderr: None,
                        stdin_pipe: true,
                    };
                    let k = ctx.keeper.clone();
                    let pid = tokio::task::spawn_blocking(move || k.spawn(&spec))
                        .await
                        .unwrap()
                        .unwrap();
                    ctx.persisted.update(Change::State, |s| {
                        s.generation = Some(GenState {
                            n,
                            pid,
                            ..GenState::default()
                        });
                    });
                    ctx.persisted.flush().await;
                    self.log(&format!("attach {} spawned {n} {pid}", reattach_name(&other)));
                    *self.live.lock().unwrap() = Some((ctx.keeper.clone(), pid));
                }
            }
        })
    }
    fn status(&self) -> watch::Receiver<RunStatus> {
        self.status.subscribe()
    }
    fn generation(&self) -> Option<Generation> {
        None
    }
    fn claude_pid(&self) -> Option<i32> {
        self.live.lock().unwrap().as_ref().map(|(_, p)| *p)
    }
    fn end_turn(&self, _reason: String, _deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn stop(&self, _deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let live = self.live.lock().unwrap().take();
            if let Some((k, pid)) = live {
                let _ = tokio::task::spawn_blocking(move || k.signal(-pid, libc::SIGKILL)).await;
            }
            self.log("stop");
        })
    }
    fn quiesce(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn resume(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn oom_killed(&self) {}
}

fn reattach_name(r: &Reattach) -> &'static str {
    match r {
        Reattach::Nothing => "nothing",
        Reattach::Alive { .. } => "alive",
        Reattach::Exited { .. } => "exited",
    }
}

/// A filesystem that logs each mount's `restart` to `fs.log` and, on a first mount, hands the keeper a `fuse` fd.
pub struct RecordingFs {
    dir: PathBuf,
}

impl Filesystem for RecordingFs {
    fn mount(
        &self,
        _lease: &magpie_api::sandbox::LeaseResponse,
        keeper: &Client,
        restart: bool,
    ) -> BoxFuture<'_, Result<(), String>> {
        append(&self.dir.join("fs.log"), &format!("mount {restart}"));
        let k = keeper.clone();
        Box::pin(async move {
            if !restart {
                let f = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
                tokio::task::spawn_blocking(move || k.put_fd(keeper::FUSE_FD, std::os::fd::AsRawFd::as_raw_fd(&f)))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }
    fn stats(&self) -> FsStats {
        FsStats {
            magpie_reachable: true,
            ..FsStats::default()
        }
    }
    fn settle(&self, _deadline: Instant) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
    fn close_connections(&self) {}
    fn resumed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn unmount(&self) -> BoxFuture<'_, ()> {
        append(&self.dir.join("fs.log"), "unmount");
        Box::pin(async {})
    }
}

/// A keeper on a thread whose supervisor is this test binary's `child` test (see [`child_main`]).
pub struct KeeperThread {
    pub dir: PathBuf,
    pub client: Client,
    stop_w: i32,
    thread: Option<std::thread::JoinHandle<i32>>,
}

impl KeeperThread {
    /// `child` is the name of the ignored test that calls [`child_main`].
    pub fn start(dir: &Path, proxy: String, child: &str) -> Self {
        let cfg = dir.join("child.json");
        std::fs::write(
            &cfg,
            serde_json::to_string(&ChildConfig {
                dir: dir.to_owned(),
                proxy,
            })
            .unwrap(),
        )
        .unwrap();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let log = dir.join("child.log");
        // the child's output goes to child.log, not into the test's
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("exec \"$0\" \"$@\" >>'{}' 2>&1", log.display()),
            exe,
            child.into(),
            "--exact".into(),
            "--ignored".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
            format!("cfg={}", cfg.display()),
        ];
        let (stop_r, stop_w) = pipe();
        let mut k = Keeper::new(Config {
            run_dir: dir.join("run"),
            mount_run_tmpfs: false,
            spool_dir: dir.join("spool"),
            supervisor: Some(argv),
            protect_self: false,
            signals: Signals::Pipe(stop_r),
            stop_grace: supervisor::limits::KEEPER_STOP_GRACE,
        })
        .unwrap();
        let thread = std::thread::spawn(move || k.run());
        Self {
            dir: dir.to_owned(),
            client: Client::new(&dir.join("run")),
            stop_w,
            thread: Some(thread),
        }
    }

    /// The supervisor's pid, if one runs.
    pub fn supervisor(&self) -> Option<i32> {
        self.client
            .status()
            .ok()?
            .children
            .into_iter()
            .find(|(_, n)| n == "supervisor")
            .map(|(p, _)| p)
    }

    /// Waits for a supervisor that answers health `ready: true`; its `:7070` client and pid.
    pub async fn ready(&self, fakes: &Fakes) -> (supervisor_api::client::ControlClient, i32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "no ready supervisor; child.log:\n{}",
                lines(&self.dir, "child.log").join("\n")
            );
            if let (Ok(port), Some(pid)) = (std::fs::read_to_string(self.dir.join("port")), self.supervisor()) {
                let cc = fakes.control(&port);
                if cc.health().await.is_ok_and(|h| h.ready) {
                    return (cc, pid);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// SIGKILLs the supervisor (the keeper restarts it) and forgets its port.
    pub fn kill_supervisor(&self) -> i32 {
        let pid = self.supervisor().expect("a supervisor runs");
        let _ = std::fs::remove_file(self.dir.join("port"));
        // SAFETY: kill(2) has no memory effects.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        pid
    }

    pub fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            // SAFETY: writing one byte from a local to our own pipe.
            #[allow(unsafe_code)]
            unsafe {
                libc::write(self.stop_w, b"T".as_ptr().cast(), 1);
            }
            let _ = t.join();
        }
    }
}

impl Drop for KeeperThread {
    fn drop(&mut self) {
        self.stop();
    }
}
