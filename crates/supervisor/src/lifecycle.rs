//! The lifecycle state machine (supervisor 15.5) and the seams the other parts plug into.
//!
//! ```text
//!  booting ──(ready)──> serving ──(prepare pause|snapshot, idle or force)──> quiesced ──(resumed | clock jump)──> serving
//!                           │ (prepare migrate) ──> serving (no change)            └─(release)─┐
//!                           └──────────(release: not busy, or force)─────────────────────────────> released ──> exit 0
//!                           (release, busy, not forced) ──> 409 sandbox_busy, stays serving
//! ```
//!
//! The Claude Code driver ([`Agent`], WP-23) and the `~/.claude` filesystem ([`Filesystem`], WP-22) are traits, so
//! this module's rules (an unforced release or a pause refused while busy, a forced release ending the turn
//! cleanly, a second release waiting for the first's outcome, the clock-jump resume) hold whatever implements them.
//! [`Lifecycle::admit`] is the one door a `start_turn` goes through: the busy check of a release and the admission of
//! a turn take the same lock, so a release can never slip between a received `start_turn` and its `starting` state.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hermit_api::sandbox_surface::ActivityState;
use supervisor_api::control::{
    Generation, Outcome, PrepareKind, PrepareRequest, ReleaseRequest, ResumedKind, RunSummary, TurnOrigin,
};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use uuid::Uuid;

use crate::limits::{CLOCK_JUMP, CLOCK_TICK, INTERRUPT_GRACE};

/// A boxed future (the traits are used as objects).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The agent's run state as the lifecycle and the activity reports see it (the driver's full state is its own).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStatus {
    pub state: ActivityState,
    pub turn_id: Option<Uuid>,
    pub origin: Option<TurnOrigin>,
    /// Open background tasks.
    pub tasks: u32,
    /// Open asks (a pending ask keeps the sandbox from an unforced release or pause).
    pub asks: u32,
    pub last_output_at: Option<hermit_api::Timestamp>,
    /// Since when the run has been idle.
    pub idle_since: Option<hermit_api::Timestamp>,
}

impl RunStatus {
    #[must_use]
    pub fn idle() -> Self {
        Self {
            state: ActivityState::Idle,
            turn_id: None,
            origin: None,
            tasks: 0,
            asks: 0,
            last_output_at: None,
            idle_since: Some(hermit_api::Timestamp::now()),
        }
    }

    /// Busy for the lifecycle: a busy state (15.3) or an open ask.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.state.is_busy() || self.asks > 0
    }

    #[must_use]
    pub fn summary(&self) -> RunSummary {
        RunSummary {
            state: self.state,
            turn_id: self.turn_id,
            tasks: self.tasks,
        }
    }
}

/// The Claude Code driver (WP-23), as the lifecycle needs it.
pub trait Agent: Send + Sync + 'static {
    /// The run state, updated on every change.
    fn status(&self) -> watch::Receiver<RunStatus>;
    /// The live generation, for `GET /v1/state`.
    fn generation(&self) -> Option<Generation>;
    /// Claude Code's main pid (the memory guard's last resort).
    fn claude_pid(&self) -> Option<i32>;
    /// A forced release during a turn: interrupt it (at most 15 s, by `deadline`) so the transcript ends cleanly,
    /// report it `failed` with `{"code": "sandbox.lost", "message": "released: <reason>"}`, stop tasks.
    fn end_turn(&self, reason: String, deadline: Instant) -> BoxFuture<'_, ()>;
    /// The end of the generation: close Claude Code's stdin, 5 s, SIGTERM its group, 3 s, SIGKILL; then every
    /// report acknowledged (by `deadline`) and the final `idle` heartbeat.
    fn stop(&self, deadline: Instant) -> BoxFuture<'_, ()>;
    /// A pause: stop the command long-poll.
    fn quiesce(&self) -> BoxFuture<'_, ()>;
    /// After a restore: re-open what was closed and restart the loops.
    fn resume(&self) -> BoxFuture<'_, ()>;
    /// The memory guard killed Claude Code's main process: end the running turn with `run.oom`.
    fn oom_killed(&self);
}

/// What the `~/.claude` filesystem (WP-22) reports for `GET /v1/state`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsStats {
    pub inflight: u32,
    pub blocked: u32,
    pub oldest_blocked_ms: Option<u64>,
    pub lease_epoch: Option<u64>,
    pub magpie_reachable: bool,
}

/// The `~/.claude` filesystem (WP-22), as the lifecycle needs it.
pub trait Filesystem: Send + Sync + 'static {
    fn stats(&self) -> FsStats;
    /// Waits until every FUSE operation in flight is answered and every write stored, by `deadline`; `false` when
    /// one is still blocked on magpie then.
    fn settle(&self, deadline: Instant) -> BoxFuture<'_, bool>;
    /// Before a pause: close every pooled HTTP connection so none is half-open after a long pause.
    fn close_connections(&self);
    /// After a restore: re-take the lease if magpie answers `lease_lost`, re-load the tree, revalidate the cache.
    fn resumed(&self) -> BoxFuture<'_, ()>;
    /// The end of a release.
    fn unmount(&self) -> BoxFuture<'_, ()>;
}

/// The lifecycle's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Booting,
    Serving,
    /// Prepared for a pause or snapshot: commands stopped, connections closed.
    Quiesced,
    /// The release sequence runs: no turn is admitted.
    Releasing,
    /// The release sequence has finished: the process exits.
    Released,
}

/// What health's `ready` waits for (15.4): the descriptor, the lease, the mount, `/workspace`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // four independent conditions, as 15.4 lists them
pub struct Readiness {
    pub descriptor: bool,
    pub lease: bool,
    pub mounted: bool,
    pub workspace: bool,
}

impl Readiness {
    #[must_use]
    pub fn all(&self) -> bool {
        self.descriptor && self.lease && self.mounted && self.workspace
    }
}

/// The lifecycle: phase, readiness, admission and the lifecycle calls.
pub struct Lifecycle {
    agent: Arc<dyn Agent>,
    fs: Arc<dyn Filesystem>,
    phase: watch::Sender<Phase>,
    ready: watch::Sender<Readiness>,
    /// Held while a turn is admitted and while a release or pause checks busy; `true` while turns are admitted.
    admission: Mutex<bool>,
    /// One release at a time; a second call waits and gets the same outcome.
    release_lock: tokio::sync::Mutex<()>,
    /// Woken by `resumed` and the clock-jump detector.
    resumed: Notify,
    /// What the activity loop must send at once (a state change it cannot see in the agent's status).
    pub(crate) activity_now: Notify,
    /// The descriptor re-read hook (set by the start sequence).
    refresh: Mutex<Option<Refresh>>,
    /// The final `released` activity report (set by the start sequence), sent before the unmount.
    final_report: Mutex<Option<FinalReport>>,
}

/// The hook that re-reads hermit's descriptor.
pub type Refresh = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The hook that sends the final `released` activity report, retried until the deadline it is given.
pub type FinalReport = Arc<dyn Fn(Instant) -> BoxFuture<'static, ()> + Send + Sync>;

impl std::fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lifecycle")
            .field("phase", &*self.phase.borrow())
            .field("ready", &*self.ready.borrow())
            .finish_non_exhaustive()
    }
}

impl Lifecycle {
    #[must_use]
    pub fn new(agent: Arc<dyn Agent>, fs: Arc<dyn Filesystem>) -> Arc<Self> {
        Arc::new(Self {
            agent,
            fs,
            phase: watch::channel(Phase::Booting).0,
            ready: watch::channel(Readiness::default()).0,
            admission: Mutex::new(true),
            release_lock: tokio::sync::Mutex::new(()),
            resumed: Notify::new(),
            activity_now: Notify::new(),
            refresh: Mutex::new(None),
            final_report: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn agent(&self) -> &Arc<dyn Agent> {
        &self.agent
    }

    #[must_use]
    pub fn fs(&self) -> &Arc<dyn Filesystem> {
        &self.fs
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        *self.phase.borrow()
    }

    #[must_use]
    pub fn phases(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    #[must_use]
    pub fn readiness(&self) -> Readiness {
        *self.ready.borrow()
    }

    #[must_use]
    pub fn readiness_changes(&self) -> watch::Receiver<Readiness> {
        self.ready.subscribe()
    }

    /// Updates readiness; the first time everything is ready the phase becomes `serving`.
    pub fn set_ready(&self, f: impl FnOnce(&mut Readiness)) {
        self.ready.send_modify(f);
        if self.readiness().all() {
            self.phase.send_if_modified(|p| {
                let change = *p == Phase::Booting;
                if change {
                    *p = Phase::Serving;
                }
                change
            });
        }
    }

    /// `ready` of `/v1/health`.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.readiness().all() && matches!(self.phase(), Phase::Serving | Phase::Quiesced)
    }

    /// The state the activity reports and health carry: the agent's, or `released` once released.
    #[must_use]
    pub fn reported(&self) -> RunStatus {
        let mut s = self.agent.status().borrow().clone();
        if self.phase() == Phase::Released {
            s.state = ActivityState::Released;
        }
        s
    }

    /// Sets the hook that re-reads hermit's descriptor (after a restore).
    pub fn on_refresh(&self, f: Refresh) {
        *self.refresh.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
    }

    /// Sets the hook that sends the final `released` activity report.
    pub fn on_final_report(&self, f: FinalReport) {
        *self.final_report.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
    }

    /// The door for a `start_turn` (the driver calls it before anything else): `mark` runs under the admission lock
    /// and must make the agent busy (`starting`); `false` (and `mark` not run) once a release or pause has begun.
    pub fn admit(&self, mark: impl FnOnce()) -> bool {
        let open = self.admission.lock().unwrap_or_else(PoisonError::into_inner);
        if *open {
            mark();
            true
        } else {
            false
        }
    }

    /// Closes admission when `allowed(busy status)` says so; the status it saw.
    fn close_admission_if(&self, force: bool) -> Result<(), RunStatus> {
        let mut open = self.admission.lock().unwrap_or_else(PoisonError::into_inner);
        let status = self.agent.status().borrow().clone();
        if status.busy() && !force {
            return Err(status);
        }
        *open = false;
        Ok(())
    }

    fn reopen_admission(&self) {
        *self.admission.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    /// `POST /v1/lifecycle/release`, the activity reply's repeat, and SIGTERM (forced). Unforced and busy: `Busy`,
    /// nothing changed. Otherwise the sequence of 15.5, then `Done` (the caller exits 0 after answering).
    pub async fn release(&self, req: ReleaseRequest) -> Outcome {
        let _one = self.release_lock.lock().await;
        if self.phase() == Phase::Released {
            return Outcome::Done;
        }
        let status = match self.close_admission_if(req.force) {
            Ok(()) => self.agent.status().borrow().clone(),
            Err(busy) => {
                tracing::info!(reason = %req.reason, state = %busy.state.as_str(), "lifecycle release refused: busy");
                return Outcome::Busy(busy.summary());
            }
        };
        tracing::info!(reason = %req.reason, force = req.force, "lifecycle release");
        self.phase.send_replace(Phase::Releasing);
        let deadline = Instant::now() + Duration::from_millis(u64::from(req.deadline_ms));
        if req.force && status.state.is_busy() {
            let interrupt_by = deadline.min(Instant::now() + INTERRUPT_GRACE);
            self.agent.end_turn(req.reason.as_str().to_owned(), interrupt_by).await;
        }
        self.agent.stop(deadline).await;
        if !self.fs.settle(deadline).await {
            tracing::warn!("release: FUSE operations still blocked at the deadline");
        }
        self.phase.send_replace(Phase::Released);
        self.activity_now.notify_waiters();
        let report = self.final_report.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(f) = report {
            f(deadline).await;
        }
        self.fs.unmount().await;
        Outcome::Done
    }

    /// `POST /v1/lifecycle/prepare`.
    pub async fn prepare(&self, req: PrepareRequest) -> Outcome {
        match req.kind {
            PrepareKind::Migrate => Outcome::Done,
            PrepareKind::Snapshot => {
                // every /workspace write the sandbox made reaches its filesystem
                let _ = tokio::task::spawn_blocking(|| {
                    // SAFETY: sync(2) has no arguments and no memory effects.
                    #[allow(unsafe_code)]
                    unsafe {
                        libc::sync();
                    }
                })
                .await;
                Outcome::Done
            }
            PrepareKind::Pause => {
                let _one = self.release_lock.lock().await;
                if matches!(self.phase(), Phase::Releasing | Phase::Released) {
                    return Outcome::Done;
                }
                if let Err(busy) = self.close_admission_if(req.force) {
                    return Outcome::Busy(busy.summary());
                }
                self.agent.quiesce().await;
                let deadline = Instant::now() + Duration::from_millis(u64::from(req.deadline_ms));
                if !self.fs.settle(deadline).await && !req.force {
                    // a write still blocked on magpie: not now
                    self.agent.resume().await;
                    self.reopen_admission();
                    return Outcome::Busy(self.agent.status().borrow().summary());
                }
                self.fs.close_connections();
                self.phase.send_replace(Phase::Quiesced);
                Outcome::Done
            }
        }
    }

    /// `POST /v1/lifecycle/resumed`, or a clock jump: re-open, re-read the descriptor, re-take the lease if lost,
    /// re-load the tree, revalidate the cache, restart loops.
    pub async fn resumed(&self, kind: ResumedKind) {
        let _one = self.release_lock.lock().await;
        if matches!(self.phase(), Phase::Releasing | Phase::Released) {
            return;
        }
        tracing::info!(kind = ?kind, "lifecycle resumed");
        let refresh = self.refresh.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(r) = refresh {
            r().await;
        }
        self.fs.resumed().await;
        if self.phase() == Phase::Quiesced || kind == ResumedKind::Pause {
            self.agent.resume().await;
        }
        self.reopen_admission();
        self.phase.send_if_modified(|p| {
            let change = *p == Phase::Quiesced;
            if change {
                *p = Phase::Serving;
            }
            change
        });
        self.resumed.notify_waiters();
        self.activity_now.notify_waiters();
    }

    /// The clock-jump detector's task: a restore that hermit did not announce is handled as `resumed`.
    pub async fn watch_clock(self: Arc<Self>) {
        let mut jump = ClockJump::new(wall_ms(), mono_ms());
        loop {
            tokio::time::sleep(CLOCK_TICK).await;
            if jump.tick(wall_ms(), mono_ms()) {
                tracing::info!("clock jump: treating as resumed");
                self.resumed(ResumedKind::Pause).await;
            }
        }
    }
}

fn clock_ms(clock: libc::clockid_t) -> i64 {
    // SAFETY: an all-zero timespec is valid and clock_gettime writes into it.
    #[allow(unsafe_code)]
    let ts = unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(clock, &raw mut ts);
        ts
    };
    ts.tv_sec.saturating_mul(1000) + ts.tv_nsec / 1_000_000
}

/// `CLOCK_REALTIME` in milliseconds.
#[must_use]
pub fn wall_ms() -> i64 {
    clock_ms(libc::CLOCK_REALTIME)
}

/// `CLOCK_MONOTONIC` in milliseconds (it does not advance while a checkpointed sandbox is not running).
#[must_use]
pub fn mono_ms() -> i64 {
    clock_ms(libc::CLOCK_MONOTONIC)
}

/// Clock-jump detection (15.5, 20): `CLOCK_REALTIME` advanced more than 30 s beyond `CLOCK_MONOTONIC` across one
/// tick.
#[derive(Clone, Copy, Debug)]
pub struct ClockJump {
    wall: i64,
    mono: i64,
}

impl ClockJump {
    #[must_use]
    pub fn new(wall: i64, mono: i64) -> Self {
        Self { wall, mono }
    }

    /// `true` when this tick's wall-clock advance exceeds its monotonic advance by more than [`CLOCK_JUMP`].
    pub fn tick(&mut self, wall: i64, mono: i64) -> bool {
        let skew = (wall - self.wall) - (mono - self.mono);
        self.wall = wall;
        self.mono = mono;
        skew > i64::try_from(CLOCK_JUMP.as_millis()).unwrap_or(i64::MAX)
    }
}

/// An agent with no Claude Code driver yet: always idle, nothing to stop (WP-23 replaces it).
#[derive(Debug)]
pub struct IdleAgent {
    status: watch::Sender<RunStatus>,
}

impl Default for IdleAgent {
    fn default() -> Self {
        Self {
            status: watch::channel(RunStatus::idle()).0,
        }
    }
}

impl Agent for IdleAgent {
    fn status(&self) -> watch::Receiver<RunStatus> {
        self.status.subscribe()
    }
    fn generation(&self) -> Option<Generation> {
        None
    }
    fn claude_pid(&self) -> Option<i32> {
        None
    }
    fn end_turn(&self, _reason: String, _deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn stop(&self, _deadline: Instant) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn quiesce(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn resume(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn oom_killed(&self) {}
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use hermit_api::sandbox::{ReleaseCallCode, ReleaseReason};

    use super::*;

    /// A scripted agent: a status the test sets, and a log of what the lifecycle asked.
    pub(crate) struct FakeAgent {
        pub status: watch::Sender<RunStatus>,
        pub log: Mutex<Vec<String>>,
    }

    impl FakeAgent {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                status: watch::channel(RunStatus::idle()).0,
                log: Mutex::new(Vec::new()),
            })
        }
        pub fn set(&self, state: ActivityState) {
            self.status.send_modify(|s| {
                s.state = state;
                s.turn_id = state.is_busy().then(Uuid::now_v7);
            });
        }
        pub fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn note(&self, s: impl Into<String>) {
            self.log.lock().unwrap().push(s.into());
        }
    }

    impl Agent for FakeAgent {
        fn status(&self) -> watch::Receiver<RunStatus> {
            self.status.subscribe()
        }
        fn generation(&self) -> Option<Generation> {
            None
        }
        fn claude_pid(&self) -> Option<i32> {
            None
        }
        fn end_turn(&self, reason: String, _deadline: Instant) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.note(format!("end_turn {reason}"));
                // the interrupt ends the turn: failed, sandbox.lost
                self.set(ActivityState::Idle);
            })
        }
        fn stop(&self, _deadline: Instant) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.note("stop") })
        }
        fn quiesce(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.note("quiesce") })
        }
        fn resume(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.note("resume") })
        }
        fn oom_killed(&self) {
            self.note("oom");
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeFs {
        pub stuck: AtomicBool,
        pub unmounted: AtomicBool,
        pub resumed: AtomicU32,
        pub closed: AtomicU32,
    }

    impl Filesystem for FakeFs {
        fn stats(&self) -> FsStats {
            FsStats {
                lease_epoch: Some(7),
                magpie_reachable: true,
                ..FsStats::default()
            }
        }
        fn settle(&self, deadline: Instant) -> BoxFuture<'_, bool> {
            Box::pin(async move {
                if self.stuck.load(Ordering::SeqCst) {
                    tokio::time::sleep_until(deadline).await;
                    false
                } else {
                    true
                }
            })
        }
        fn close_connections(&self) {
            self.closed.fetch_add(1, Ordering::SeqCst);
        }
        fn resumed(&self) -> BoxFuture<'_, ()> {
            self.resumed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn unmount(&self) -> BoxFuture<'_, ()> {
            self.unmounted.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn release(force: bool) -> ReleaseRequest {
        ReleaseRequest {
            reason: if force {
                ReleaseReason::Deleted
            } else {
                ReleaseReason::Idle
            },
            code: ReleaseCallCode::SandboxReleased,
            force,
            deadline_ms: 30_000,
        }
    }

    fn pause(force: bool) -> PrepareRequest {
        PrepareRequest {
            kind: PrepareKind::Pause,
            force,
            deadline_ms: 200,
        }
    }

    fn ready(l: &Lifecycle) {
        l.set_ready(|r| {
            *r = Readiness {
                descriptor: true,
                lease: true,
                mounted: true,
                workspace: true,
            }
        });
    }

    #[tokio::test]
    async fn not_ready_until_descriptor_lease_mount_and_workspace() {
        let l = Lifecycle::new(FakeAgent::new(), Arc::new(FakeFs::default()));
        assert!(!l.is_ready());
        l.set_ready(|r| r.descriptor = true);
        l.set_ready(|r| r.lease = true);
        l.set_ready(|r| r.mounted = true);
        assert!(!l.is_ready());
        assert_eq!(l.phase(), Phase::Booting);
        l.set_ready(|r| r.workspace = true);
        assert!(l.is_ready());
        assert_eq!(l.phase(), Phase::Serving);
    }

    #[tokio::test]
    async fn an_unforced_release_while_busy_is_refused_and_changes_nothing() {
        for state in [
            ActivityState::Starting,
            ActivityState::Running,
            ActivityState::Stopping,
            ActivityState::Held,
        ] {
            let agent = FakeAgent::new();
            let fs = Arc::new(FakeFs::default());
            let l = Lifecycle::new(agent.clone(), fs.clone());
            ready(&l);
            agent.set(state);
            match l.release(release(false)).await {
                Outcome::Busy(run) => assert_eq!(run.state, state),
                Outcome::Done => panic!("released while {state:?}"),
            }
            assert_eq!(l.phase(), Phase::Serving);
            assert_eq!(agent.log(), Vec::<String>::new());
            assert!(!fs.unmounted.load(Ordering::SeqCst));
            // the turn goes on: a new start_turn is still admitted
            assert!(l.admit(|| {}));
        }
    }

    #[tokio::test]
    async fn an_open_ask_counts_as_busy() {
        let agent = FakeAgent::new();
        let l = Lifecycle::new(agent.clone(), Arc::new(FakeFs::default()));
        ready(&l);
        agent.status.send_modify(|s| s.asks = 1);
        assert!(matches!(l.release(release(false)).await, Outcome::Busy(_)));
        assert!(matches!(l.prepare(pause(false)).await, Outcome::Busy(_)));
    }

    #[tokio::test]
    async fn an_idle_release_runs_the_sequence_once() {
        let agent = FakeAgent::new();
        let fs = Arc::new(FakeFs::default());
        let l = Lifecycle::new(agent.clone(), fs.clone());
        ready(&l);
        let (a, b) = tokio::join!(l.release(release(false)), l.release(release(false)));
        assert_eq!((a, b), (Outcome::Done, Outcome::Done));
        assert_eq!(
            agent.log(),
            vec!["stop"],
            "the second call waited for the first's outcome"
        );
        assert_eq!(l.phase(), Phase::Released);
        assert_eq!(l.reported().state, ActivityState::Released);
        assert!(fs.unmounted.load(Ordering::SeqCst));
        assert!(!l.admit(|| panic!("admitted after release")));
    }

    #[tokio::test]
    async fn a_forced_release_ends_the_turn_cleanly_first() {
        let agent = FakeAgent::new();
        let l = Lifecycle::new(agent.clone(), Arc::new(FakeFs::default()));
        ready(&l);
        agent.set(ActivityState::Running);
        assert_eq!(l.release(release(true)).await, Outcome::Done);
        assert_eq!(agent.log(), vec!["end_turn deleted", "stop"]);
        assert_eq!(l.phase(), Phase::Released);
    }

    #[tokio::test]
    async fn a_start_turn_racing_a_release_is_either_refused_or_makes_it_busy() {
        for _ in 0..200 {
            let agent = FakeAgent::new();
            let l = Lifecycle::new(agent.clone(), Arc::new(FakeFs::default()));
            ready(&l);
            let l2 = l.clone();
            let a2 = agent.clone();
            let admit = tokio::spawn(async move { l2.admit(|| a2.set(ActivityState::Starting)) });
            let out = l.release(release(false)).await;
            let admitted = admit.await.unwrap();
            match out {
                // released: the turn was not admitted
                Outcome::Done => assert!(!admitted),
                // refused: the turn was admitted first
                Outcome::Busy(run) => assert!(admitted && run.state == ActivityState::Starting),
            }
        }
    }

    #[tokio::test]
    async fn pause_refuses_while_busy_and_quiesces_when_idle() {
        let agent = FakeAgent::new();
        let fs = Arc::new(FakeFs::default());
        let l = Lifecycle::new(agent.clone(), fs.clone());
        ready(&l);
        agent.set(ActivityState::Held);
        assert!(matches!(l.prepare(pause(false)).await, Outcome::Busy(_)));
        assert_eq!(agent.log(), Vec::<String>::new());
        agent.set(ActivityState::Idle);
        assert_eq!(l.prepare(pause(false)).await, Outcome::Done);
        assert_eq!(l.phase(), Phase::Quiesced);
        assert_eq!(fs.closed.load(Ordering::SeqCst), 1);
        assert!(!l.admit(|| {}), "no turn while quiesced");
        l.resumed(ResumedKind::Pause).await;
        assert_eq!(l.phase(), Phase::Serving);
        assert_eq!(agent.log(), vec!["quiesce", "resume"]);
        assert_eq!(fs.resumed.load(Ordering::SeqCst), 1);
        assert!(l.admit(|| {}));
    }

    #[tokio::test]
    async fn a_write_blocked_on_magpie_refuses_an_unforced_pause() {
        let agent = FakeAgent::new();
        let fs = Arc::new(FakeFs::default());
        fs.stuck.store(true, Ordering::SeqCst);
        let l = Lifecycle::new(agent.clone(), fs.clone());
        ready(&l);
        assert!(matches!(l.prepare(pause(false)).await, Outcome::Busy(_)));
        assert_eq!(l.phase(), Phase::Serving);
        assert_eq!(agent.log(), vec!["quiesce", "resume"]);
        assert!(l.admit(|| {}));
        // forced: left blocked, resumes after the restore
        assert_eq!(l.prepare(pause(true)).await, Outcome::Done);
        assert_eq!(l.phase(), Phase::Quiesced);
    }

    #[tokio::test]
    async fn migrate_and_snapshot_never_refuse() {
        let agent = FakeAgent::new();
        let l = Lifecycle::new(agent.clone(), Arc::new(FakeFs::default()));
        ready(&l);
        agent.set(ActivityState::Running);
        for kind in [PrepareKind::Migrate, PrepareKind::Snapshot] {
            let r = l
                .prepare(PrepareRequest {
                    kind,
                    force: false,
                    deadline_ms: 1000,
                })
                .await;
            assert_eq!(r, Outcome::Done);
        }
        assert_eq!(l.phase(), Phase::Serving);
    }

    #[test]
    fn clock_jumps_are_detected_and_ordinary_ticks_are_not() {
        let mut c = ClockJump::new(1_000_000, 50_000);
        assert!(!c.tick(1_001_000, 51_000)); // a normal second
        assert!(!c.tick(1_031_000, 52_000)); // 29 s of NTP step: not a restore
        assert!(c.tick(1_100_000, 53_000)); // 68 s more wall than monotonic: restored
        assert!(!c.tick(1_101_000, 54_000));
        assert!(!c.tick(1_090_000, 55_000)); // the wall clock stepping back is no restore
    }

    #[tokio::test]
    async fn a_clock_jump_runs_the_resume_path() {
        let agent = FakeAgent::new();
        let fs = Arc::new(FakeFs::default());
        let l = Lifecycle::new(agent.clone(), fs.clone());
        ready(&l);
        let refreshed = Arc::new(AtomicU32::new(0));
        let r = refreshed.clone();
        l.on_refresh(Arc::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }));
        assert_eq!(l.prepare(pause(false)).await, Outcome::Done);
        // what watch_clock does on a jump
        let mut c = ClockJump::new(0, 0);
        assert!(c.tick(3_600_000, 1000));
        l.resumed(ResumedKind::Pause).await;
        assert_eq!(l.phase(), Phase::Serving);
        assert_eq!(refreshed.load(Ordering::SeqCst), 1);
        assert_eq!(fs.resumed.load(Ordering::SeqCst), 1);
    }
}
