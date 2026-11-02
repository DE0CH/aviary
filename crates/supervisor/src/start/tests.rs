//! The start sequence end to end against fake hermit and fake magpie behind the reference gateway (the proxy with
//! the sandbox token in its URL, DESIGN 12 D1), with a scripted agent and filesystem; and the restart's re-attach
//! against a real keeper.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aviary_core::auth::{Caller, Callers};
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::magpie::FakeMagpie;
use hermit_api::sandbox::{PutSandboxRequest, ReleaseReason};
use hermit_api::sandbox_surface::ActivityState;
use supervisor_api::client::ControlClient;
use supervisor_api::control::{Outcome, PrepareKind, PrepareRequest, ReleaseRequest};
use uuid::Uuid;

use super::*;
use crate::lifecycle::tests::{FakeAgent, FakeFs};
use crate::state::GenState;

struct World {
    dir: PathBuf,
    hermit: Arc<FakeHermit>,
    magpie: Arc<FakeMagpie>,
    _served: (aviary_fakes::hermit::Running, aviary_fakes::magpie::Running),
    gateway: test_gateway::Running,
    cid: Uuid,
    sandbox: Uuid,
    _keeper: crate::keeper::testing::Running,
    keeper: keeper::Client,
}

async fn world() -> World {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let hermit = FakeHermit::with_config(HermitConfig::default());
    let magpie = FakeMagpie::new();
    let hs = hermit.serve().await.unwrap();
    let ms = magpie.serve().await.unwrap();
    let cid = Uuid::now_v7();
    let http = reqwest::Client::new();
    let put = hs
        .client(http.clone(), Caller::Magpie)
        .put_sandbox(
            cid,
            &PutSandboxRequest {
                person: "staff@example.org".into(),
            },
        )
        .await
        .unwrap();
    let sandbox = put.sandbox().id;
    let token = |n: &str| format!("{n}-{}", Uuid::new_v4().simple());
    let cfg = test_gateway::Config {
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
            tools: token("tools"),
            model: token("model"),
        },
        callers: Callers::new([(Caller::Pigeon, vec![token("p")]), (Caller::Hermit, vec![token("h")])]).unwrap(),
        self_hosts: vec![],
        ops_summary: test_gateway::default_ops_summary(),
        drain: test_gateway::DrainBounds::default(),
        upload_cap: test_gateway::UPLOAD_CAP,
        upstream_idle: gateway_api::limits::IDLE_TIMEOUT,
        faults: test_gateway::Faults::default(),
    };
    let mut registry = prometheus_client::registry::Registry::default();
    let gateway = test_gateway::start(cfg, &mut registry).await.unwrap();
    gateway.keys_loaded().await;
    let k = crate::keeper::testing::Running::start(None);
    World {
        dir: k.dir.clone(),
        keeper: k.client.clone(),
        _keeper: k,
        hermit,
        magpie,
        _served: (hs, ms),
        gateway,
        cid,
        sandbox,
    }
}

impl World {
    fn boot(&self) -> BootEnv {
        let token = self.hermit.mint_token(self.cid, Ipv4Addr::LOCALHOST).unwrap();
        BootEnv {
            proxy: format!("http://sbx:{token}@{}", self.gateway.proxy_addr()),
            path: "/usr/bin:/bin".into(),
            lang: None,
            lc_all: None,
        }
    }

    fn paths(&self) -> Paths {
        let d = &self.dir;
        std::fs::create_dir_all(d.join("workspace")).unwrap();
        std::fs::write(d.join("hosts"), "127.0.0.1 localhost\n10.0.0.9 other\n").unwrap();
        // SAFETY: getuid/getgid have no memory effects.
        #[allow(unsafe_code)]
        let ids = unsafe { (libc::getuid(), libc::getgid()) };
        Paths {
            run_dir: d.join("run"),
            state_dir: d.join("state"),
            exec_dir: d.join("exec"),
            agent_socket: d.join("agent.sock"),
            docker_socket: d.join("docker.sock"),
            etc_hosts: d.join("hosts"),
            apt_proxy: d.join("apt/80sandbox-proxy"),
            sys_cgroup: d.join("no-cgroup"),
            proc: "/proc".into(),
            oom_score_adj: d.join("oom_score_adj"),
            docker: DockerPaths::sandbox(),
            control: "127.0.0.1:0".parse().unwrap(),
            workspace: Some(d.join("workspace").to_string_lossy().into_owned()),
            exec_ids: ids,
        }
    }

    async fn start(
        &self,
        agent: Arc<FakeAgent>,
        fs: Arc<FakeFs>,
    ) -> (Running, tokio::task::JoinHandle<i32>, ControlClient, SocketAddr) {
        let boot = self.boot();
        let build = Build::new("supervisor");
        let http = boot.fixed_names_client(&build).unwrap();
        let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = control.local_addr().unwrap();
        let s = Start {
            paths: self.paths(),
            boot,
            build,
            hermit: Hermit::new(http.clone()),
            magpie: magpie::client(http),
            keeper: self.keeper.clone(),
            agent,
            fs,
            control,
        };
        let (running, h) = s.spawn();
        let token = self.hermit.control_token(self.sandbox).unwrap();
        let cc = ControlClient::new(reqwest::Client::new(), format!("http://{addr}"), token);
        (running, h, cc, addr)
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boots_through_the_gateway_reports_and_releases() {
    let w = world().await;
    let agent = FakeAgent::new();
    let (running, done, cc, addr) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;

    // the descriptor came through the proxy with the token from HTTP_PROXY, the lease from magpie
    let d = running.hermit.descriptor().unwrap();
    assert_eq!(d.sandbox.conversation_id, w.cid);
    assert!(w.magpie.lease_epoch(w.cid) >= 1);
    assert_eq!(agent.log(), vec!["attach Nothing"]);
    let health = cc.health().await.unwrap();
    assert!(health.ok && health.ready && !health.busy);
    assert_eq!(health.state, ActivityState::Idle);
    assert_eq!(health.version, crate::hermit::version());
    // first start: hosts gained localhost (v6) and sandbox, and kept every other line; no apt proxy in this env
    let hosts = std::fs::read_to_string(w.dir.join("hosts")).unwrap();
    assert_eq!(
        hosts,
        "127.0.0.1 localhost\n10.0.0.9 other\n::1 localhost\n127.0.1.1 sandbox\n"
    );
    assert!(!w.dir.join("apt/80sandbox-proxy").exists());
    assert_eq!(std::fs::read_to_string(w.dir.join("oom_score_adj")).unwrap(), "-1000");

    // the activity report reached hermit
    until("an activity report", || {
        w.hermit
            .live(w.cid)
            .and_then(|s| s.supervisor)
            .is_some_and(|i| i.version == crate::hermit::version())
    })
    .await;

    // a wrong control token: 401
    let bad = ControlClient::new(reqwest::Client::new(), format!("http://{addr}"), "not-the-token");
    assert_eq!(bad.health().await.unwrap_err().status(), Some(401));

    // state
    let st = cc.state().await.unwrap();
    assert_eq!(st.lease_epoch, Some(7));
    assert!(st.magpie_reachable);
    assert!(st.supervisor.rss_bytes > 0);

    // busy: unforced release and pause are refused; nothing changes
    agent.set(ActivityState::Running);
    let rel = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    match cc.release(&rel).await.unwrap() {
        Outcome::Busy(run) => assert_eq!(run.state, ActivityState::Running),
        Outcome::Done => panic!("released while running"),
    }
    let pause = PrepareRequest {
        kind: PrepareKind::Pause,
        force: false,
        deadline_ms: 1000,
    };
    assert!(matches!(cc.prepare(&pause).await.unwrap(), Outcome::Busy(_)));
    assert_eq!(l.phase(), Phase::Serving);

    // forced: the turn is ended cleanly, then the sequence runs and the process exits 0
    let forced = ReleaseRequest::new(ReleaseReason::Deleted, true, 30_000).unwrap();
    assert_eq!(cc.release(&forced).await.unwrap(), Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(agent.log(), vec!["attach Nothing", "end_turn deleted", "stop"]);
    // hermit was told `released`
    let s = w
        .hermit
        .sandboxes()
        .into_iter()
        .find(|s| s.conversation_id == w.cid)
        .unwrap();
    assert!(s.supervisor.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_release_in_the_activity_reply_is_acted_on() {
    let w = world().await;
    let agent = FakeAgent::new();
    let (running, done, _cc, _) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;
    // the direct call was lost; the next reply repeats it (unforced, idle: released)
    w.hermit.request_release(w.cid, ReleaseReason::Lifetime, false);
    l.activity_now.notify_waiters();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(l.phase(), Phase::Released);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_ready_until_the_lease_is_granted() {
    let w = world().await;
    let agent = FakeAgent::new();
    // magpie refuses the lease for now (as when hermit names another sandbox the live one)
    let f = w
        .magpie
        .faults()
        .on("lease", aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 });
    let (running, _done, cc, _) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    until("the descriptor", || l.readiness().descriptor).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let h = cc.health().await.unwrap();
    assert!(h.ok && !h.ready, "{h:?}");
    assert!(!l.readiness().lease);
    w.magpie.faults().remove(f);
    until("ready", || l.is_ready()).await;
    assert!(cc.health().await.unwrap().ready);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_control_api_answers_503_before_the_descriptor() {
    let w = world().await;
    let f = w.hermit.faults().on(
        "sandbox_self",
        aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (running, _done, cc, _) = w.start(FakeAgent::new(), Arc::new(FakeFs::default())).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let e = cc.health().await.unwrap_err();
    assert_eq!(e.status(), Some(503));
    w.hermit.faults().remove(f);
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;
}

#[test]
fn a_restart_reattaches_to_a_live_generation_and_sees_one_that_died() {
    let k = crate::keeper::testing::Running::start(None);
    let pid = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    let state = |pid| RunState {
        generation: Some(GenState {
            n: 3,
            pid,
            ..GenState::default()
        }),
        ..RunState::default()
    };
    // a new supervisor (a new client) reads run.json and finds it alive
    let again = keeper::Client::new(&k.dir.join("run"));
    assert_eq!(reattach(&again, Some(&state(pid))), Reattach::Alive { n: 3, pid });
    assert_eq!(reattach(&again, None), Reattach::Nothing);
    // it dies while no supervisor runs
    again.signal(pid, libc::SIGKILL).unwrap();
    k.wait_exit(pid);
    match reattach(&again, Some(&state(pid))) {
        Reattach::Exited { n: 3, exit: Some(e) } => assert_eq!(e.signal, Some(libc::SIGKILL)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn hosts_and_apt_files() {
    let d = std::env::temp_dir().join(format!("hosts-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    let hosts = d.join("hosts");
    // missing file: all three lines
    write_hosts(&hosts).unwrap();
    assert_eq!(
        std::fs::read_to_string(&hosts).unwrap(),
        "127.0.0.1 localhost\n::1 localhost\n127.0.1.1 sandbox\n"
    );
    // idempotent
    write_hosts(&hosts).unwrap();
    assert_eq!(std::fs::read_to_string(&hosts).unwrap().lines().count(), 3);
    let mut desc: SandboxSelf =
        serde_json::from_str(include_str!("../../../hermit-api/fixtures/sandbox-self.json")).unwrap();
    desc.env
        .insert("http_proxy".into(), "http://sbx:t@192.0.2.1:3128\"\n".into());
    desc.env
        .insert("https_proxy".into(), "http://sbx:t@192.0.2.1:3128".into());
    let apt = d.join("apt/80sandbox-proxy");
    write_apt_proxy(&apt, &desc).unwrap();
    assert_eq!(
        std::fs::read_to_string(&apt).unwrap(),
        "Acquire::http::Proxy \"http://sbx:t@192.0.2.1:3128\";\nAcquire::https::Proxy \"http://sbx:t@192.0.2.1:3128\";\n"
    );
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&apt).unwrap().permissions()) & 0o777,
        0o644
    );
    let _ = std::fs::remove_dir_all(d);
}
