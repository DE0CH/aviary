//! The supervisor's RSS under load (supervisor 5.4, 20: 64 MiB budget, 40 MiB target; WP-21 acceptance). The test
//! runs fake hermit and fake magpie behind the reference gateway, then starts this test binary again as a child
//! that runs only a keeper and the supervisor (the start sequence with an idle agent and a stand-in filesystem), so
//! the child's RSS is the supervisor's. It drives `:7070` (health, state, exec, snapshot prepares), the local socket
//! (secrets) and the activity loop for [`SOAK`], sampling the child's `VmRSS`, and checks the peak and that it stays
//! flat after the warm-up. The 24 h soak with synthetic turns needs the Claude Code driver (WP-23) and runs in
//! `nightly.yml` (WP-28) with `soak_long`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aviary_core::auth::{Caller, Callers};
use aviary_core::version::Build;
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::magpie::FakeMagpie;
use base64::Engine as _;
use hermit_api::sandbox::PutSandboxRequest;
use serde::{Deserialize, Serialize};
use supervisor::boot::BootEnv;
use supervisor::hermit::Hermit;
use supervisor::keeper::server::{Config, Keeper, Signals};
use supervisor::lifecycle::{BoxFuture, Filesystem, FsStats, IdleAgent};
use supervisor::limits::SUPERVISOR_RSS_BUDGET;
use supervisor::start::{Paths, Start};
use supervisor_api::ExecRequest;
use supervisor_api::client::ControlClient;
use supervisor_api::control::{PrepareKind, PrepareRequest};
use uuid::Uuid;

const SOAK: Duration = Duration::from_secs(20);
const MIB: u64 = 1024 * 1024;

/// What the parent tells the child.
#[derive(Serialize, Deserialize)]
struct ChildConfig {
    dir: PathBuf,
    proxy: String,
}

fn config_path(parent: u32) -> PathBuf {
    std::env::temp_dir().join(format!("supervisor-soak-{parent}.json"))
}

/// A filesystem that mounts at once and has nothing in flight (WP-22's layer is not in this soak).
struct NullFs;

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

/// The child: a keeper on a thread and the supervisor, until killed.
#[test]
#[ignore = "run by rss_stays_under_the_budget as its child"]
fn soak_child() {
    // SAFETY: getppid has no memory effects.
    #[allow(unsafe_code)]
    let parent = unsafe { libc::getppid() }.cast_unsigned();
    let Ok(text) = std::fs::read_to_string(config_path(parent)) else {
        return; // run by hand or by --ignored: nothing to do
    };
    let cfg: ChildConfig = serde_json::from_str(&text).unwrap();
    let (stop_r, _stop_w) = pipe();
    let mut k = Keeper::new(Config {
        run_dir: cfg.dir.join("run"),
        mount_run_tmpfs: false,
        spool_dir: cfg.dir.join("spool"),
        supervisor: None,
        protect_self: false,
        signals: Signals::Pipe(stop_r),
    })
    .unwrap();
    std::thread::spawn(move || k.run());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let boot = BootEnv {
            proxy: cfg.proxy,
            path: "/usr/bin:/bin".into(),
            lang: None,
            lc_all: None,
        };
        let build = Build::new("supervisor");
        let http = boot.fixed_names_client(&build).unwrap();
        let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        std::fs::write(cfg.dir.join("port"), control.local_addr().unwrap().port().to_string()).unwrap();
        let d = &cfg.dir;
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
        paths.workspace = Some(d.join("workspace").to_string_lossy().into_owned());
        paths.exec_ids = ids;
        let start = Start {
            paths,
            boot,
            build,
            hermit: Hermit::new(http.clone()),
            magpie: supervisor::magpie::client(http),
            keeper: supervisor::keeper::Client::new(&d.join("run")),
            agent: Arc::new(IdleAgent::default()),
            fs: Arc::new(NullFs),
            control,
        };
        let (_running, done) = start.spawn();
        let _ = done.await;
    });
}

fn pipe() -> (i32, i32) {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two fds into the local array.
    #[allow(unsafe_code)]
    let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(r, 0);
    (fds[0], fds[1])
}

fn rss(pid: u32) -> u64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

#[allow(clippy::too_many_lines)] // setup, drive, measure
async fn soak(duration: Duration) {
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
            callers: Callers::new([(Caller::Pigeon, vec![token("p")]), (Caller::Hermit, vec![token("h")])]).unwrap(),
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

    let dir = std::env::temp_dir().join(format!("supervisor-soak-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = hermit.mint_token(cid, Ipv4Addr::LOCALHOST).unwrap();
    let cfg = ChildConfig {
        dir: dir.clone(),
        proxy: format!("http://sbx:{st}@{}", gateway.proxy_addr()),
    };
    let cfg_path = config_path(std::process::id());
    std::fs::write(&cfg_path, serde_json::to_string(&cfg).unwrap()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["soak_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let port_file = dir.join("port");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !port_file.exists() {
        assert!(Instant::now() < deadline, "the child did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let port = std::fs::read_to_string(&port_file).unwrap();
    let cc = ControlClient::new(
        reqwest::Client::new(),
        format!("http://127.0.0.1:{}", port.trim()),
        hermit.control_token(sandbox).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cc.health().await.is_ok_and(|h| h.ready) {
        assert!(Instant::now() < deadline, "the supervisor did not become ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let pid = child.id();
    let started = Instant::now();
    let warm = duration / 4;
    let (mut peak, mut after_warmup, mut n) = (0u64, 0u64, 0u64);
    let payload = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; 256 * 1024]);
    while started.elapsed() < duration {
        let _ = cc.health().await.unwrap();
        let _ = cc.state().await.unwrap();
        let out = cc
            .exec(&ExecRequest {
                argv: vec!["/bin/cat".into()],
                cwd: None,
                stdin: Some(payload.clone()),
                timeout_ms: Some(10_000),
                env: std::collections::BTreeMap::default(),
            })
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        let _ = cc
            .prepare(&PrepareRequest {
                kind: PrepareKind::Snapshot,
                force: false,
                deadline_ms: 1000,
            })
            .await
            .unwrap();
        let _ = supervisor::local::ask_secret(&dir.join("agent.sock"), "NOT_GRANTED_KEY").await;
        n += 1;
        let r = rss(pid);
        peak = peak.max(r);
        if after_warmup == 0 && started.elapsed() >= warm {
            after_warmup = r;
        }
    }
    let end = rss(pid);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "soak: {n} rounds in {duration:?}, peak {} KiB, after warm-up {} KiB, end {} KiB",
        peak / 1024,
        after_warmup / 1024,
        end / 1024
    );
    assert!(n > 10, "only {n} rounds");
    assert!(peak > 0 && peak < SUPERVISOR_RSS_BUDGET, "peak RSS {} KiB", peak / 1024);
    // flat: no more than 8 MiB of growth after the warm-up
    assert!(end <= after_warmup + 8 * MIB, "RSS grew from {after_warmup} to {end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rss_stays_under_the_budget() {
    soak(SOAK).await;
}

/// The longer soak for `nightly.yml`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "an hour; nightly"]
async fn soak_long() {
    soak(Duration::from_secs(3600)).await;
}
