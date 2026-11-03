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
    running_keeper: crate::keeper::testing::Running,
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
        running_keeper: k,
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
            // never the host's /proc: a test supervisor must not see (or kill) the host's processes
            proc: d.join("proc"),
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
        fs: Arc<dyn Filesystem>,
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
    // the filesystem reports none: the lease the start sequence took
    assert_eq!(st.lease_epoch, Some(w.magpie.lease_epoch(w.cid)));
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
    l.activity_now.notify_one();
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
async fn not_ready_while_the_filesystem_refuses_to_mount() {
    let w = world().await;
    let fs: Arc<dyn Filesystem> = Arc::new(crate::lifecycle::UnbuiltFilesystem);
    let (running, _done, cc, _) = w.start(FakeAgent::new(), fs).await;
    let l = running.lifecycle.clone();
    until("the lease", || l.readiness().lease).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let h = cc.health().await.unwrap();
    assert!(h.ok && !h.ready, "{h:?}");
    assert!(!l.readiness().mounted);
    assert_eq!(l.phase(), Phase::Booting);
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
    assert_eq!(reattach(&again, Some(&state(pid)), true), Reattach::Alive { n: 3, pid });
    assert_eq!(reattach(&again, None, true), Reattach::Nothing);
    // a run.json of another boot names no live process, whatever the pid
    assert_eq!(
        reattach(&again, Some(&state(pid)), false),
        Reattach::Exited { n: 3, exit: None }
    );
    // it dies while no supervisor runs
    again.signal(pid, libc::SIGKILL).unwrap();
    k.wait_exit(pid);
    match reattach(&again, Some(&state(pid)), true) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_while_waiting_for_the_lease_exits_without_attaching() {
    let w = world().await;
    let agent = FakeAgent::new();
    let _f = w
        .magpie
        .faults()
        .on("lease", aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 });
    let fs = Arc::new(FakeFs::default());
    let (running, done, cc, _) = w.start(agent.clone(), fs.clone()).await;
    let l = running.lifecycle.clone();
    until("the descriptor", || l.readiness().descriptor).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rel = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    assert_eq!(cc.release(&rel).await.unwrap(), Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(agent.log(), vec!["stop"], "attach never ran");
    assert!(fs.mounts.lock().unwrap().is_empty());
    assert!(w.keeper.status().unwrap().released, "the keeper was told RELEASED");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_while_the_mount_is_retried_exits() {
    let w = world().await;
    let fs: Arc<dyn Filesystem> = Arc::new(crate::lifecycle::UnbuiltFilesystem);
    let agent = FakeAgent::new();
    let (running, done, cc, _) = w.start(agent.clone(), fs).await;
    let l = running.lifecycle.clone();
    until("the lease", || l.readiness().lease).await;
    let rel = ReleaseRequest::new(ReleaseReason::Deleted, true, 30_000).unwrap();
    assert_eq!(cc.release(&rel).await.unwrap(), Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(agent.log(), vec!["stop"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_state_change_during_an_in_flight_report_is_sent_at_once() {
    let w = world().await;
    let agent = FakeAgent::new();
    let (running, _done, _cc, _) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;
    let states = |w: &World| -> Vec<String> {
        w.hermit
            .script()
            .calls_to("sandbox_activity")
            .into_iter()
            .filter_map(|c| c.body?.get("state")?.as_str().map(str::to_owned))
            .collect()
    };
    until("an idle report", || states(&w).iter().any(|s| s == "idle")).await;
    // hold the next report in flight, change the state while it is held
    let hold = aviary_fakes::Hold::new();
    let f = w
        .hermit
        .faults()
        .on("sandbox_activity", aviary_fakes::FaultKind::Hold(hold.clone()));
    l.activity_now.notify_one();
    until("a report in flight", || hold.waiting() == 1).await;
    agent.set(ActivityState::Running);
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.hermit.faults().remove(f);
    hold.release();
    let t = Instant::now();
    until("the running report", || states(&w).iter().any(|s| s == "running")).await;
    // at once, not at the next 15 s tick
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn released_before_this_start_reports_released_and_takes_no_lease() {
    let w = world().await;
    w.keeper.released().unwrap();
    let agent = FakeAgent::new();
    let fs = Arc::new(FakeFs::default());
    let (running, done, cc, _) = w.start(agent.clone(), fs.clone()).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(running.lifecycle.phase(), Phase::Released);
    let reported = w.hermit.script().calls_to("sandbox_activity");
    assert!(
        reported
            .iter()
            .all(|c| c.body.as_ref().and_then(|b| b.get("state")?.as_str()) == Some("released")),
        "{reported:?}"
    );
    assert!(!reported.is_empty());
    assert_eq!(w.magpie.lease_epoch(w.cid), 0, "no lease taken");
    assert!(agent.log().is_empty() && fs.mounts.lock().unwrap().is_empty());
    drop(cc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_run_json_a_held_fuse_fd_and_a_live_claude_still_mean_a_restart() {
    let w = world().await;
    // a predecessor mounted and spawned Claude Code, then died before run.json reached the disk
    let (r, wr) = std::os::unix::net::UnixStream::pair().unwrap();
    w.keeper
        .put_fd(keeper::FUSE_FD, std::os::fd::AsRawFd::as_raw_fd(&r))
        .unwrap();
    let mut spec = w.running_keeper.spec("claude", &["/bin/sleep", "100"]);
    spec.setsid = true;
    let stray = w.keeper.spawn(&spec).unwrap();
    let agent = FakeAgent::new();
    let fs = Arc::new(FakeFs::default());
    let (running, _done, _cc, _) = w.start(agent.clone(), fs.clone()).await;
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;
    assert_eq!(*fs.mounts.lock().unwrap(), vec![true], "the keeper's fd is taken back");
    // the claude no run.json names was killed before the agent attached (so no second generation beside it)
    let e = w.running_keeper.wait_exit(stray);
    assert_eq!(e.signal, Some(libc::SIGKILL));
    assert_eq!(agent.log(), vec!["attach Nothing"]);
    // hosts was still written (idempotent) since no run.json of this boot said so
    assert!(
        std::fs::read_to_string(w.dir.join("hosts"))
            .unwrap()
            .contains("sandbox")
    );
    drop(wr);
}

#[test]
fn a_start_is_classified_from_run_json_and_the_keeper() {
    let st = |restarts, children: &[(i32, &str)]| KeeperStatus {
        boot_id: "b1".into(),
        supervisor_restarts: restarts,
        started_at_ms: 0,
        children: children.iter().map(|(p, n)| (*p, (*n).to_owned())).collect(),
        orphans: 0,
        released: false,
    };
    let prior = |boot: &str, pid| RunState {
        boot_id: boot.into(),
        generation: Some(GenState {
            n: 1,
            pid,
            ..GenState::default()
        }),
        ..RunState::default()
    };
    // a first start
    let s = Started::classify(None, "b1", Some(&st(0, &[])), false);
    assert!(!s.restart && !s.same_boot && s.strays.is_empty());
    // a restart with run.json: its generation is not a stray, another claude is
    let s = Started::classify(
        Some(&prior("b1", 10)),
        "b1",
        Some(&st(1, &[(10, "claude"), (11, "claude"), (12, "dockerd")])),
        true,
    );
    assert!(s.restart && s.same_boot && s.fuse_held);
    assert_eq!(s.strays, vec![11]);
    // run.json unreadable (moved aside), the keeper restarted a supervisor: still a restart
    let s = Started::classify(None, "b1", Some(&st(2, &[])), false);
    assert!(s.restart);
    // run.json of another boot: its pid means nothing; the start is a first one for this keeper
    let s = Started::classify(Some(&prior("b0", 10)), "b1", Some(&st(0, &[(10, "claude")])), false);
    assert!(!s.same_boot && s.restart);
    assert_eq!(s.strays, vec![10]);
    let s = Started::classify(Some(&prior("b0", 10)), "b1", Some(&st(0, &[])), false);
    assert!(!s.same_boot && !s.restart);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sigterm_release_ends_in_time_while_hermit_holds_every_report() {
    let w = world().await;
    let agent = FakeAgent::new();
    let fs = Arc::new(FakeFs::default());
    let (running, done, _cc, _) = w.start(agent.clone(), fs.clone()).await;
    let l = running.lifecycle.clone();
    until("ready", || l.is_ready()).await;
    // hermit accepts the connection and never answers (an activity route that hangs)
    let hold = aviary_fakes::Hold::new();
    let _f = w
        .hermit
        .faults()
        .on("sandbox_activity", aviary_fakes::FaultKind::Hold(hold.clone()));
    let t = Instant::now();
    assert_eq!(l.release(sigterm_release()).await, Outcome::Done);
    let took = t.elapsed();
    // the final report gave up at the release's deadline, so the unmount ran before the keeper's SIGKILL
    assert!(took < crate::limits::SIGTERM_RELEASE_DEADLINE, "{took:?}");
    assert!(fs.unmounted.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(t.elapsed() < crate::limits::KEEPER_STOP_GRACE, "{:?}", t.elapsed());
    hold.release();
}

/// A previous supervisor's state: `claude` running under the keeper mid-turn, named in a `run.json` of this boot.
fn crashed_mid_turn(w: &World) -> (i32, Uuid) {
    let mut spec = w.running_keeper.spec("claude", &["/bin/sleep", "100"]);
    spec.stdin_pipe = true;
    let pid = w.keeper.spawn(&spec).unwrap();
    let boot_id = std::fs::read_to_string(w.dir.join("run").join(keeper::BOOT_ID_NAME))
        .unwrap()
        .trim()
        .to_owned();
    let turn = Uuid::now_v7();
    StateFile::new(w.dir.join("state"))
        .save(&RunState {
            version: VERSION,
            boot_id,
            generation: Some(GenState {
                n: 1,
                pid,
                ..GenState::default()
            }),
            run: crate::state::RunPart {
                state: "running".into(),
                turn_id: Some(turn.to_string()),
                ..crate::state::RunPart::default()
            },
            ..RunState::default()
        })
        .unwrap();
    (pid, turn)
}

fn turn_states(w: &World, turn: Uuid) -> Vec<serde_json::Value> {
    w.magpie
        .script()
        .calls_to("turn_state")
        .into_iter()
        .filter(|c| c.param("turnId") == Some(turn.to_string().as_str()))
        .filter_map(|c| c.body)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_before_attach_ends_the_previous_generation_and_reports_its_turn() {
    let w = world().await;
    let (pid, turn) = crashed_mid_turn(&w);
    // the mount is retried (the lease is held), so the agent never attaches
    let fs: Arc<dyn Filesystem> = Arc::new(crate::lifecycle::UnbuiltFilesystem);
    let agent = FakeAgent::new();
    let (running, done, cc, _) = w.start(agent.clone(), fs).await;
    let l = running.lifecycle.clone();
    until("the lease", || l.readiness().lease).await;
    // the previous supervisor's turn is running: an unforced release is refused and changes nothing
    let idle = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    assert!(matches!(cc.release(&idle).await.unwrap(), Outcome::Busy(r) if r.turn_id == Some(turn)));
    assert_eq!(l.phase(), Phase::Booting);
    // a forced one (the conversation was deleted) ends it
    let rel = ReleaseRequest::new(ReleaseReason::Deleted, true, 30_000).unwrap();
    assert_eq!(cc.release(&rel).await.unwrap(), Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(agent.log(), vec!["stop"], "attach never ran");
    // the generation was ended the graceful way (stdin closed, 5 s, SIGTERM): sleep ignores stdin, so SIGTERM
    let e = w.running_keeper.wait_exit(pid);
    assert_eq!(e.signal, Some(libc::SIGTERM));
    let states = turn_states(&w, turn);
    assert_eq!(states.len(), 1, "{states:?}");
    assert_eq!(states[0]["state"], "failed");
    assert_eq!(states[0]["error"]["code"], "sandbox.lost");
    assert_eq!(states[0]["error"]["message"], "released: deleted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sigterm_during_the_lease_wait_ends_the_previous_generation() {
    let w = world().await;
    let (pid, turn) = crashed_mid_turn(&w);
    let _f = w
        .magpie
        .faults()
        .on("lease", aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 });
    let agent = FakeAgent::new();
    let (running, done, _cc, _) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    until("the descriptor", || l.readiness().descriptor).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(l.release(sigterm_release()).await, Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(w.running_keeper.wait_exit(pid).signal.is_some());
    // no lease: magpie ends the turn from hermit's gone (magpie 8.7), nothing is sent without one
    assert_eq!(turn_states(&w, turn), Vec::<serde_json::Value>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_before_the_descriptor_still_reports_released() {
    let w = world().await;
    let _f = w.hermit.faults().on(
        "sandbox_self",
        aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let agent = FakeAgent::new();
    let (running, done, _cc, _) = w.start(agent.clone(), Arc::new(FakeFs::default())).await;
    let l = running.lifecycle.clone();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!l.readiness().descriptor);
    assert_eq!(l.release(sigterm_release()).await, Outcome::Done);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let released = w
        .hermit
        .script()
        .calls_to("sandbox_activity")
        .into_iter()
        .filter_map(|c| c.body)
        .any(|b| b["state"] == "released");
    assert!(released, "hermit got the final released report");
}
