//! The supervisor's start, on every start, first or after a restart (supervisor 4.3), and its shutdown (4.7).
//!
//! 1. `oom_score_adj -1000`; `boot_id`; `run.json` if present (a restart).
//! 2. The sandbox token is the boot proxy's credential (DESIGN 12 D1); the descriptor `GET http://hermit/v1/sandbox/self`
//!    is read until it answers.
//! 3. `:7070` (answering 503 until the descriptor is read, then `ready: false` until step 9), `agent.sock` and the
//!    docker socket.
//! 4. First start only: `/etc/hosts` gets `localhost` and `sandbox` where missing (DESIGN 12 D2), apt's proxy
//!    configuration from the descriptor's `env`; the managed Claude Code files are the driver's ([`Agent::attach`]).
//! 5. The lease from magpie (30 s on `not_current_sandbox`, backoff on 503, never an exit).
//! 6. `~/.claude` mounted ([`Filesystem::mount`]; a fresh connection or the keeper's fd).
//! 7. Claude Code: re-attached to the live generation named in `run.json` (checked against the keeper's `STATUS`
//!    and `EXITS`), or pre-spawned, by the driver.
//! 8. The activity loop, the memory guard, the clock-jump detector; health `ready: true` once `/workspace` is there.
//!
//! The process exits 0 once a release has finished (hermit's call, the activity reply's repeat, or SIGTERM, which
//! is always a forced release), 70 on an internal error (the keeper restarts it).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aviary_core::version::Build;
use hermit_api::sandbox::{ReleaseCallCode, ReleaseReason};
use hermit_api::sandbox_surface::{ActivityEvent, ActivityEventKind, SandboxSelf};
use supervisor_api::control::{RELEASE_DEADLINE_MS, ReleaseRequest};
use supervisor_api::server::{control_router, local_router};

use crate::boot::{BootEnv, ROOT_HOME};
use crate::docker::{Docker, DockerPaths};
use crate::hermit::Hermit;
use crate::hermit::activity::{Activity, Facts};
use crate::hermit::control::Control;
use crate::hermit::exec::Exec;
use crate::keeper::watch::Exits;
use crate::keeper::{self, ExitRecord};
use crate::lifecycle::{Agent, AttachContext, Filesystem, Lifecycle, Phase};
use crate::local::{Local, serve_unix};
use crate::magpie::{self, LeaseWait};
use crate::memguard::{self, Guard, GuardHost, Kill, Proc};
use crate::state::{Change, Persisted, RunState, StateFile, VERSION};

/// The supervisor's exit code on an internal error (the keeper restarts it).
pub const EXIT_INTERNAL: i32 = 70;
/// How long the process lingers after the release so its `200` reaches hermit.
const ANSWER_GRACE: Duration = Duration::from_millis(300);
/// How often a missing `/workspace` is looked for again, and a refused mount retried.
const RECHECK: Duration = Duration::from_secs(5);
const MOUNT_RETRY: Duration = Duration::from_secs(30);

/// Where the supervisor's files are (the sandbox's paths; tests use a directory of their own).
#[derive(Clone, Debug)]
pub struct Paths {
    pub run_dir: PathBuf,
    pub state_dir: PathBuf,
    pub exec_dir: PathBuf,
    pub agent_socket: PathBuf,
    pub docker_socket: PathBuf,
    pub etc_hosts: PathBuf,
    pub apt_proxy: PathBuf,
    pub sys_cgroup: PathBuf,
    pub proc: PathBuf,
    pub oom_score_adj: PathBuf,
    pub docker: DockerPaths,
    /// `:7070` on every address.
    pub control: SocketAddr,
    /// The workspace and the exec runs' ids.
    pub workspace: Option<String>,
    pub exec_ids: (u32, u32),
}

impl Paths {
    /// The sandbox's paths (the same in every deployment).
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            run_dir: keeper::RUN_DIR.into(),
            state_dir: crate::state::STATE_DIR.into(),
            exec_dir: crate::hermit::exec::EXEC_DIR.into(),
            agent_socket: supervisor_api::local::AGENT_SOCKET.into(),
            docker_socket: supervisor_api::local::DOCKER_SOCKET.into(),
            etc_hosts: "/etc/hosts".into(),
            apt_proxy: "/etc/apt/apt.conf.d/80sandbox-proxy".into(),
            sys_cgroup: "/sys/fs/cgroup".into(),
            proc: "/proc".into(),
            oom_score_adj: "/proc/self/oom_score_adj".into(),
            docker: DockerPaths::sandbox(),
            control: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, supervisor_api::CONTROL_PORT)),
            workspace: None,
            exec_ids: (crate::limits::EXEC_UID, crate::limits::EXEC_GID),
        }
    }
}

/// What the generation named in `run.json` turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reattach {
    /// A first start, or no generation was live.
    Nothing,
    /// Still running under the keeper: re-attach.
    Alive { n: u32, pid: i32 },
    /// It exited while no supervisor watched.
    Exited { n: u32, exit: Option<ExitRecord> },
}

/// Looks up the generation `prior` names in the keeper (4.3 step 8).
#[must_use]
pub fn reattach(keeper: &keeper::Client, prior: Option<&RunState>) -> Reattach {
    let Some(g) = prior.and_then(|s| s.generation.as_ref()) else {
        return Reattach::Nothing;
    };
    let alive = keeper
        .status()
        .is_ok_and(|s| s.children.iter().any(|(pid, name)| *pid == g.pid && name == "claude"));
    if alive {
        return Reattach::Alive { n: g.n, pid: g.pid };
    }
    let exit = keeper
        .exits(0)
        .ok()
        .and_then(|list| list.into_iter().rev().find(|e| e.pid == g.pid));
    Reattach::Exited { n: g.n, exit }
}

/// Adds `localhost` and `sandbox` to `/etc/hosts` where missing, keeping every other line (DESIGN 12 D2).
///
/// # Errors
/// The read or write error.
pub fn write_hosts(path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let has = |addr: &str, name: &str| {
        text.lines().any(|l| {
            let mut f = l.split('#').next().unwrap_or("").split_whitespace();
            f.next() == Some(addr) && f.any(|n| n == name)
        })
    };
    let mut add = String::new();
    for (addr, name) in [
        ("127.0.0.1", "localhost"),
        ("::1", "localhost"),
        ("127.0.1.1", "sandbox"),
    ] {
        if !has(addr, name) {
            add.push_str(addr);
            add.push(' ');
            add.push_str(name);
            add.push('\n');
        }
    }
    if add.is_empty() {
        return Ok(());
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&add);
    std::fs::write(path, out)
}

/// apt's proxy configuration from the descriptor's `http_proxy`/`https_proxy` (quotes and newlines stripped, 0644);
/// nothing when neither is set.
///
/// # Errors
/// The write error.
pub fn write_apt_proxy(path: &Path, d: &SandboxSelf) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let clean = |v: &String| {
        v.chars()
            .filter(|c| !matches!(c, '"' | '\n' | '\r'))
            .collect::<String>()
    };
    let mut text = String::new();
    if let Some(v) = d.env.get("http_proxy") {
        text.push_str(&apt_line("http", &clean(v)));
    }
    if let Some(v) = d.env.get("https_proxy") {
        text.push_str(&apt_line("https", &clean(v)));
    }
    if text.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
}

fn apt_line(scheme: &str, proxy: &str) -> String {
    format!("Acquire::{scheme}::Proxy \"{proxy}\";\n")
}

struct GuardLink {
    agent: Arc<dyn Agent>,
    activity: Arc<Activity>,
}

impl GuardHost for GuardLink {
    fn claude_pid(&self) -> Option<i32> {
        self.agent.claude_pid()
    }
    fn kill(&self, pid: i32) {
        // SAFETY: kill(2) has no memory effects.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    fn killed(&self, kill: &Kill) {
        self.activity.event(ActivityEvent {
            kind: ActivityEventKind::MemoryKill,
            at: hermit_api::Timestamp::now(),
            pid: u32::try_from(kill.victim.pid).ok(),
            comm: Some(kill.victim.comm.clone()),
            rss: Some(kill.victim.rss),
            exit_code: None,
            signal: Some(libc::SIGKILL),
        });
        if kill.claude {
            self.agent.oom_killed();
        }
    }
}

/// Everything the start sequence is given.
pub struct Start {
    pub paths: Paths,
    pub boot: BootEnv,
    pub build: Build,
    pub hermit: Hermit,
    pub magpie: magpie_api::client::Client,
    pub keeper: keeper::Client,
    pub agent: Arc<dyn Agent>,
    pub fs: Arc<dyn Filesystem>,
    /// The bound `:7070` listener (bound by the caller, so a test can use port 0).
    pub control: tokio::net::TcpListener,
}

impl std::fmt::Debug for Start {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Start")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// What a running supervisor exposes to its caller (the binary waits on it; tests drive it).
#[derive(Clone, Debug)]
pub struct Running {
    pub lifecycle: Arc<Lifecycle>,
    pub hermit: Hermit,
}

impl Start {
    /// Runs the start sequence in the background; the lifecycle is returned at once.
    #[must_use]
    pub fn spawn(self) -> (Running, tokio::task::JoinHandle<i32>) {
        let lifecycle = Lifecycle::new(self.agent.clone(), self.fs.clone());
        let running = Running {
            lifecycle: lifecycle.clone(),
            hermit: self.hermit.clone(),
        };
        let h = tokio::spawn(self.run(lifecycle));
        (running, h)
    }

    #[allow(clippy::too_many_lines)] // the start sequence, step by step as 4.3 lists it
    async fn run(self, lifecycle: Arc<Lifecycle>) -> i32 {
        let Self {
            paths,
            boot,
            build: _,
            hermit,
            magpie,
            keeper,
            agent,
            fs,
            control,
        } = self;
        // 1
        if let Err(e) = std::fs::write(&paths.oom_score_adj, "-1000") {
            tracing::info!(error = %e, "oom_score_adj not set");
        }
        let boot_id = std::fs::read_to_string(paths.run_dir.join(keeper::BOOT_ID_NAME))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let file = StateFile::new(&paths.state_dir);
        let prior = match file.load() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "run state unreadable");
                return EXIT_INTERNAL;
            }
        };
        let restart = prior.is_some();
        let persisted = Persisted::new(prior.clone().unwrap_or_else(|| RunState {
            version: VERSION,
            boot_id: boot_id.clone(),
            ..RunState::default()
        }));
        tokio::spawn(persisted.clone().writer(file));
        let k = keeper.clone();
        let status = tokio::task::spawn_blocking(move || k.status())
            .await
            .ok()
            .and_then(Result::ok);
        let restarts = status.as_ref().map_or(0, |s| s.supervisor_restarts);
        let k = keeper.clone();
        let after = tokio::task::spawn_blocking(move || k.exits(0))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|l| l.last().map(|e| e.n))
            .unwrap_or(0);
        let exits = Exits::new(keeper.clone(), after);
        tokio::spawn(exits.clone().run());
        tracing::info!(version = %crate::hermit::version(), boot_id = %boot_id, restart, "supervisor started");

        // 3 (before 2, so :7070 answers 503 while the descriptor is read)
        let proc = Proc::new(&paths.proc);
        let facts = Facts {
            boot_id: boot_id.clone(),
            restarts,
            proc: proc.clone(),
        };
        let exec = Exec::with(
            keeper.clone(),
            exits.clone(),
            boot.clone(),
            paths.exec_dir.clone(),
            paths
                .workspace
                .clone()
                .unwrap_or_else(|| crate::hermit::exec::WORKSPACE.into()),
            paths.exec_ids,
        );
        let control_server = Arc::new(Control {
            hermit: hermit.clone(),
            lifecycle: lifecycle.clone(),
            facts: facts.clone(),
            exec,
        });
        let router = control_router(control_server);
        tokio::spawn(async move {
            let _ = axum::serve(control, router).await;
        });

        // 2
        let descriptor = hermit.fetch_initial().await;
        lifecycle.set_ready(|r| r.descriptor = true);
        tokio::spawn(hermit.clone().refresh_loop());
        {
            let h = hermit.clone();
            lifecycle.on_refresh(Arc::new(move || {
                let h = h.clone();
                Box::pin(async move {
                    let _ = h.refresh().await;
                })
            }));
        }
        let local = Arc::new(Local { hermit: hermit.clone() });
        if let Err(e) = serve_unix(&paths.agent_socket, 0o666, local_router(local)) {
            tracing::error!(error = %e, "agent socket");
        }
        let docker = Docker::new(
            keeper.clone(),
            paths.docker.clone(),
            boot.child_env(ROOT_HOME, &descriptor.env, &[]),
        );
        if let Err(e) = serve_unix(&paths.docker_socket, 0o666, docker.router()) {
            tracing::error!(error = %e, "docker socket");
        }

        // 4
        if !restart {
            if let Err(e) = write_hosts(&paths.etc_hosts) {
                tracing::warn!(error = %e, "/etc/hosts not written");
            }
            if let Err(e) = write_apt_proxy(&paths.apt_proxy, &descriptor) {
                tracing::warn!(error = %e, "apt proxy not written");
            }
        }
        {
            let ws = PathBuf::from(
                paths
                    .workspace
                    .clone()
                    .unwrap_or_else(|| descriptor.resources.workspace.path.clone()),
            );
            let l = lifecycle.clone();
            tokio::spawn(async move {
                let mut logged = false;
                while !ws.is_dir() {
                    if !logged {
                        tracing::warn!(path = %ws.display(), "workspace missing");
                        logged = true;
                    }
                    tokio::time::sleep(RECHECK).await;
                }
                l.set_ready(|r| r.workspace = true);
            });
        }

        // the activity loop runs from here (hermit sees `booting` sandboxes as alive)
        let activity = Activity::new(hermit.clone(), lifecycle.clone(), facts);
        tokio::spawn(activity.clone().run());
        {
            let a = activity.clone();
            lifecycle.on_final_report(Arc::new(move |deadline| a.final_report(deadline)));
        }
        tokio::spawn(sigterm(lifecycle.clone()));

        // 5
        let lease = magpie::take_lease(&magpie, descriptor.sandbox.conversation_id, |w| {
            if w != LeaseWait::Unavailable {
                tracing::info!(wait = ?w, "lease not granted yet");
            }
        })
        .await;
        persisted.update(Change::State, |s| s.lease_epoch = Some(lease.epoch));
        lifecycle.set_ready(|r| r.lease = true);

        // 6
        loop {
            match fs.mount(&lease, &keeper, restart).await {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "mount failed");
                    tokio::time::sleep(MOUNT_RETRY).await;
                }
            }
        }
        tracing::info!("fuse mounted");
        lifecycle.set_ready(|r| r.mounted = true);

        // 7
        let k = keeper.clone();
        let p = prior.clone();
        let generation = tokio::task::spawn_blocking(move || reattach(&k, p.as_ref()))
            .await
            .unwrap_or(Reattach::Nothing);
        let limit = memguard::limit(descriptor.resources.memory_bytes, proc.meminfo());
        let cgroup = memguard::agent_cgroup(&paths.sys_cgroup, limit);
        agent
            .attach(AttachContext {
                prior,
                generation,
                lease,
                descriptor: descriptor.clone(),
                persisted,
                keeper,
                exits,
                boot,
                cgroup,
            })
            .await;

        // 8
        let guard = Guard::new(
            proc,
            descriptor.resources.memory_bytes,
            std::process::id().cast_signed(),
        );
        tokio::spawn(guard.run(Arc::new(GuardLink {
            agent: agent.clone(),
            activity,
        })));
        tokio::spawn(lifecycle.clone().watch_clock());

        let mut phases = lifecycle.phases();
        let _ = phases.wait_for(|p| *p == Phase::Released).await;
        tokio::time::sleep(ANSWER_GRACE).await;
        tracing::info!("supervisor released");
        0
    }
}

/// SIGTERM (the keeper forwarding a runtime stop) is always a forced release (4.7).
async fn sigterm(lifecycle: Arc<Lifecycle>) {
    let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
        return;
    };
    if s.recv().await.is_some() {
        tracing::info!("SIGTERM: forced release");
        let _ = lifecycle
            .release(ReleaseRequest {
                reason: ReleaseReason::Requested,
                code: ReleaseCallCode::SandboxLost,
                force: true,
                deadline_ms: RELEASE_DEADLINE_MS,
            })
            .await;
    }
}

#[cfg(test)]
mod tests;
