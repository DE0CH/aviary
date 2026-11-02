//! The supervisor's start, on every start, first or after a restart (supervisor 4.3), and its shutdown (4.7).
//!
//! 0. SIGTERM is handled from the first moment: a forced release at any step.
//! 1. `oom_score_adj -1000`; `boot_id`; `run.json` if present; the keeper's `STATUS` and held `fuse` fd. A restart
//!    is a readable `run.json` of this boot, or a keeper that has restarted a supervisor, holds the `fuse` fd or runs
//!    a `claude` ([`Started::classify`]): an unreadable `run.json` never makes a restart look like a first start.
//!    The released marker ([`keeper::RELEASED_NAME`]) means the predecessor finished a release: this supervisor
//!    only reports `released` and exits 0.
//! 2. The sandbox token is the boot proxy's credential (DESIGN 12 D1); the descriptor `GET http://hermit/v1/sandbox/self`
//!    is read until it answers.
//! 3. `:7070` (answering 503 until the descriptor is read, then `ready: false` until step 9), `agent.sock` and the
//!    docker socket.
//! 4. Unless a `run.json` of this boot exists (both writes are idempotent): `/etc/hosts` gets `localhost` and `sandbox` where missing (DESIGN 12 D2), apt's proxy
//!    configuration from the descriptor's `env`; the managed Claude Code files are the driver's ([`Agent::attach`]).
//! 5. The lease from magpie (30 s on `not_current_sandbox`, backoff on 503, never an exit).
//! 6. `~/.claude` mounted ([`Filesystem::mount`]; a fresh connection or the keeper's fd).
//! 7. Claude Code: re-attached to the live generation named in `run.json` (checked against the keeper's `STATUS`
//!    and `EXITS`), or pre-spawned, by the driver.
//!    A live `claude` that `run.json` does not name (no `run.json`, or a crash between spawn and record) is
//!    killed first, so a second generation never runs beside it.
//! 8. The clock-jump detector; health `ready: true` once that is done and `/workspace` is there (the activity loop
//!    and the memory guard run from step 2 on).
//!
//! Every waiting step (the descriptor, the lease, the mount retry, the attach) gives way to a release: once one has
//! begun the sequence stops, `attach` is never called, and the process exits 0 when the release has finished
//! (hermit's call, the activity reply's repeat, or SIGTERM, which is always a forced release). 70 on an internal
//! error (the keeper restarts it).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aviary_core::version::Build;
use hermit_api::sandbox_surface::{ActivityEvent, ActivityEventKind, SandboxSelf};
use supervisor_api::server::{control_router, local_router};

use crate::boot::{BootEnv, ROOT_HOME};
use crate::docker::{Docker, DockerPaths};
use crate::hermit::Hermit;
use crate::hermit::activity::{Activity, Facts};
use crate::hermit::control::Control;
use crate::hermit::exec::Exec;
use crate::keeper::watch::Exits;
use crate::keeper::{self, ExitRecord, KeeperStatus};
use crate::lifecycle::{Agent, AttachContext, Filesystem, Lifecycle, Phase, sigterm_release};
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

/// What step 1 found: whether this start is a restart, and what to make of `run.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// Anything a previous supervisor of this boot left: `run.json`, a restart count, the `fuse` fd, a `claude`.
    pub restart: bool,
    /// `run.json` was written in this boot (its `boot_id` is the keeper's): its generation pid means something.
    pub same_boot: bool,
    /// The keeper holds the `fuse` fd: the mount takes it back (`Filesystem::mount(restart = true)`).
    pub fuse_held: bool,
    /// Live `claude` children `run.json` does not name: killed before the agent attaches.
    pub strays: Vec<i32>,
}

impl Started {
    /// Classifies a start from `run.json`, the keeper's `boot_id`, its `STATUS` and whether it holds `fuse`.
    #[must_use]
    pub fn classify(prior: Option<&RunState>, boot_id: &str, status: Option<&KeeperStatus>, fuse_held: bool) -> Self {
        let same_boot = prior.is_some_and(|p| p.boot_id.is_empty() || boot_id.is_empty() || p.boot_id == boot_id);
        let named = prior
            .filter(|_| same_boot)
            .and_then(|p| p.generation.as_ref())
            .map(|g| g.pid);
        let claude: Vec<i32> = status
            .map(|s| {
                s.children
                    .iter()
                    .filter(|(_, n)| n == "claude")
                    .map(|(p, _)| *p)
                    .collect()
            })
            .unwrap_or_default();
        let keeper_state = status.is_some_and(|s| s.supervisor_restarts > 0) || fuse_held || !claude.is_empty();
        Self {
            restart: same_boot || keeper_state,
            same_boot,
            fuse_held,
            strays: claude.into_iter().filter(|p| Some(*p) != named).collect(),
        }
    }
}

/// Looks up the generation `prior` names in the keeper (4.3 step 8). A `run.json` of another boot names no live
/// process: its generation is reported exited.
#[must_use]
pub fn reattach(keeper: &keeper::Client, prior: Option<&RunState>, same_boot: bool) -> Reattach {
    let Some(g) = prior.and_then(|s| s.generation.as_ref()) else {
        return Reattach::Nothing;
    };
    if !same_boot {
        return Reattach::Exited { n: g.n, exit: None };
    }
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
        // 0
        tokio::spawn(sigterm(lifecycle.clone()));
        // 1
        if let Err(e) = std::fs::write(&paths.oom_score_adj, "-1000") {
            tracing::info!(error = %e, "oom_score_adj not set");
        }
        let boot_id = std::fs::read_to_string(paths.run_dir.join(keeper::BOOT_ID_NAME))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let marker = paths.run_dir.join(keeper::RELEASED_NAME);
        let released_before = marker.exists();
        lifecycle.set_released_marker(marker);
        let file = StateFile::new(&paths.state_dir);
        let prior = match file.load() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "run state unreadable");
                return EXIT_INTERNAL;
            }
        };
        let k = keeper.clone();
        let (status, fuse_held) = tokio::task::spawn_blocking(move || {
            let held = k.get_fd(keeper::FUSE_FD).ok().flatten().is_some();
            (k.status().ok(), held)
        })
        .await
        .unwrap_or((None, false));
        let started = Started::classify(prior.as_ref(), &boot_id, status.as_ref(), fuse_held);
        if prior.is_some() && !started.same_boot {
            tracing::warn!("run state is from another boot: its generation is not re-attached");
        }
        if prior.is_none() && started.restart {
            tracing::warn!(fuse_held, strays = ?started.strays, "no run state, but the keeper has a previous supervisor's state");
        }
        let restarts = status.as_ref().map_or(0, |s| s.supervisor_restarts);
        let persisted = Persisted::new(RunState {
            version: VERSION,
            boot_id: boot_id.clone(),
            ..prior.clone().unwrap_or_default()
        });
        if !released_before {
            tokio::spawn(persisted.clone().writer(file));
        }
        let k = keeper.clone();
        let after = tokio::task::spawn_blocking(move || k.exits(0))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|l| l.last().map(|e| e.n))
            .unwrap_or(0);
        let exits = Exits::new(keeper.clone(), after);
        tokio::spawn(exits.clone().run());
        tracing::info!(version = %crate::hermit::version(), boot_id = %boot_id, restart = started.restart, "supervisor started");

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
        let activity = Activity::new(hermit.clone(), lifecycle.clone(), facts);

        if released_before {
            // the predecessor finished a release (and crashed before its exit, or the keeper restarted it): report
            // `released` until hermit has it, answer every release `released`, admit nothing, then exit 0
            tracing::info!("released before this start: reporting released only");
            lifecycle.mark_released();
            let _ = hermit.fetch_initial().await;
            activity.run().await;
            tokio::time::sleep(ANSWER_GRACE).await;
            return 0;
        }

        // 2
        let Some(descriptor) = unless_released(&lifecycle, hermit.fetch_initial()).await else {
            return finish(&lifecycle).await;
        };
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
        // the activity loop (hermit sees `booting` sandboxes as alive) and the memory guard run from here: exec and
        // dockerd can run before the mount
        tokio::spawn(activity.clone().run());
        {
            let a = activity.clone();
            lifecycle.on_final_report(Arc::new(move |deadline| a.final_report(deadline)));
        }
        // the guard kills processes: only inside a sandbox, where the keeper is PID 1 (4.1), never on a host that
        // runs a supervisor for a test
        // SAFETY: getppid has no memory effects.
        #[allow(unsafe_code)]
        let under_keeper = unsafe { libc::getppid() } == 1;
        if under_keeper {
            let guard = Guard::new(
                proc.clone(),
                descriptor.resources.memory_bytes,
                std::process::id().cast_signed(),
            );
            tokio::spawn(guard.run(Arc::new(GuardLink {
                agent: agent.clone(),
                activity,
            })));
        } else {
            tracing::warn!("memory guard off: the parent is not the keeper as PID 1");
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

        // 4 (idempotent: done unless a run.json of this boot says a predecessor did it)
        if !(prior.is_some() && started.same_boot) {
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

        // 5
        let take = magpie::take_lease(&magpie, descriptor.sandbox.conversation_id, |w| {
            if w != LeaseWait::Unavailable {
                tracing::info!(wait = ?w, "lease not granted yet");
            }
        });
        let Some(lease) = unless_released(&lifecycle, take).await else {
            return finish(&lifecycle).await;
        };
        persisted.update(Change::State, |s| s.lease_epoch = Some(lease.epoch));
        lifecycle.set_lease_epoch(lease.epoch);
        lifecycle.set_ready(|r| r.lease = true);

        // 6
        let mount = async {
            loop {
                match fs.mount(&lease, &keeper, started.fuse_held).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "mount failed");
                        tokio::time::sleep(MOUNT_RETRY).await;
                    }
                }
            }
        };
        if unless_released(&lifecycle, mount).await.is_none() {
            return finish(&lifecycle).await;
        }
        tracing::info!("fuse mounted");
        lifecycle.set_ready(|r| r.mounted = true);

        // 7
        let attach = async {
            kill_strays(&keeper, &exits, &started.strays).await;
            let k = keeper.clone();
            let p = prior.clone();
            let same = started.same_boot;
            let generation = tokio::task::spawn_blocking(move || reattach(&k, p.as_ref(), same))
                .await
                .unwrap_or(Reattach::Nothing);
            let limit = memguard::limit(descriptor.resources.memory_bytes, proc.meminfo());
            let cgroup = memguard::agent_cgroup(&paths.sys_cgroup, limit);
            agent
                .attach(AttachContext {
                    prior: prior.clone(),
                    generation,
                    lease: lease.clone(),
                    descriptor: descriptor.clone(),
                    persisted: persisted.clone(),
                    keeper: keeper.clone(),
                    exits: exits.clone(),
                    boot: boot.clone(),
                    cgroup,
                })
                .await;
        };
        if !lifecycle.attach_unless_released(attach).await {
            return finish(&lifecycle).await;
        }

        // 8
        tokio::spawn(lifecycle.clone().watch_clock());
        lifecycle.set_ready(|r| r.attached = true);
        finish(&lifecycle).await
    }
}

/// Runs `f` unless a release begins first (then `None`).
async fn unless_released<T>(lifecycle: &Lifecycle, f: impl std::future::Future<Output = T>) -> Option<T> {
    let mut phases = lifecycle.phases();
    tokio::select! {
        biased;
        _ = phases.wait_for(|p| matches!(p, Phase::Releasing | Phase::Released)) => None,
        v = f => Some(v),
    }
}

/// Waits for the release to finish, lets its answer reach hermit, and gives the exit code (0).
async fn finish(lifecycle: &Lifecycle) -> i32 {
    lifecycle.wait_released().await;
    tokio::time::sleep(ANSWER_GRACE).await;
    tracing::info!("supervisor released");
    0
}

/// Kills the `claude` processes no `run.json` names (a crash between spawn and record), and waits for their exits.
async fn kill_strays(keeper: &keeper::Client, exits: &Exits, strays: &[i32]) {
    for &pid in strays {
        tracing::warn!(pid, "a claude no run state names: killed");
        let k = keeper.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = k.signal(-pid, libc::SIGKILL);
            k.signal(pid, libc::SIGKILL)
        })
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(5), exits.wait(pid)).await;
    }
}

/// SIGTERM (the keeper forwarding a runtime stop) is always a forced release (4.7), within the keeper's grace.
async fn sigterm(lifecycle: Arc<Lifecycle>) {
    let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
        return;
    };
    if s.recv().await.is_some() {
        tracing::info!("SIGTERM: forced release");
        let _ = lifecycle.release(sigterm_release()).await;
    }
}

#[cfg(test)]
mod tests;
