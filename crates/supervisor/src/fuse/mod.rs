//! `~/.claude` over `/dev/fuse` without libfuse (supervisor 9): the protocol ([`proto`]), the scope table
//! ([`scope`]), node ids ([`nodes`]), the tree and content caches ([`cache`]), local scope ([`local`]), magpie
//! ([`remote`]), the request journal ([`journal`]), the server ([`dispatch`]), the errno map ([`errno`]); and the
//! [`FuseFs`] the start sequence mounts (4.3 step 6).

pub mod cache;
pub mod dispatch;
pub mod errno;
pub mod journal;
pub mod local;
pub mod nodes;
pub mod proto;
pub mod remote;
pub mod scope;
pub mod seen;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::runtime::Handle;
use uuid::Uuid;

use crate::lifecycle::{BoxFuture, Filesystem, FsStats};
use dispatch::{Core, StartArgs};
use remote::{Remote, Timing};

/// Nanoseconds since the epoch (the supervisor's mtimes, 9.2).
#[must_use]
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Where the FUSE layer keeps its files, and its sizes. [`Config::sandbox`] is the only production value; tests use
/// [`Config::under`].
#[derive(Clone, Debug)]
pub struct Config {
    /// The mount point (`/home/sandbox/.claude`).
    pub mount: PathBuf,
    /// `nodes.log`, `tree.json`, `boot_id` (`/var/lib/supervisor/state`).
    pub state: PathBuf,
    /// The content cache (`/var/lib/supervisor/cache`).
    pub cache: PathBuf,
    /// Local scope's backing directory (`/var/lib/supervisor/local`).
    pub local: PathBuf,
    /// The journal (`/var/lib/supervisor/fuse-journal`).
    pub journal: PathBuf,
    pub slots: usize,
    pub slot_bytes: usize,
    pub max_write: u32,
    pub max_pages: u16,
    pub cache_budget: u64,
    pub attr_valid: Duration,
    pub timing: Timing,
}

impl Config {
    /// The sandbox's paths and the constants of 20.
    #[must_use]
    pub fn sandbox() -> Self {
        Self::under(Path::new("/var/lib/supervisor"), Path::new("/home/sandbox/.claude"))
    }

    /// Every path under `root` (tests), with the production sizes.
    #[must_use]
    pub fn under(root: &Path, mount: &Path) -> Self {
        use crate::limits as l;
        Self {
            mount: mount.to_owned(),
            state: root.join("state"),
            cache: root.join("cache"),
            local: root.join("local"),
            journal: root.join("fuse-journal"),
            slots: l::JOURNAL_SLOTS,
            slot_bytes: l::JOURNAL_SLOT_BYTES,
            max_write: l::FUSE_MAX_WRITE,
            max_pages: l::FUSE_MAX_PAGES,
            cache_budget: l::CACHE_BUDGET,
            attr_valid: l::FUSE_ATTR_VALID,
            timing: Timing::default(),
        }
    }
}

/// The `/dev/fuse` connection as the server sees it. A reply to a `unique` the sentry no longer waits for is an
/// `Err` with ENOENT.
pub trait Device: Send + Sync + 'static {
    /// Reads one request into `buf`.
    ///
    /// # Errors
    /// ENODEV once unmounted; EINTR/EAGAIN are retried by the caller.
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// Writes one reply.
    ///
    /// # Errors
    /// ENOENT when the request is no longer waited for.
    fn write(&self, buf: &[u8]) -> std::io::Result<()>;
    /// Unblocks readers (the test stand-in; the kernel's ends with the unmount).
    fn close(&self) {}
}

/// The kernel's `/dev/fuse`.
#[derive(Debug)]
pub struct FdDevice(OwnedFd);

impl Device for FdDevice {
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: read(2) into a buffer we own, of its length.
        #[allow(unsafe_code)]
        let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
    }
    fn write(&self, buf: &[u8]) -> std::io::Result<()> {
        // SAFETY: write(2) of a buffer we own.
        #[allow(unsafe_code)]
        let n = unsafe { libc::write(self.0.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// The fault-injection points of 9.9 and 22 item 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    ReadReturned,
    CallSent,
    CallAnswered,
    ReplyWritten,
}

/// A cc op that kept failing for `STUCK_OP` while magpie answered other calls (9.5 item 4): the agent reports the
/// turn `failed` with `cc.write_stuck` (path, offset, last error) and ends the generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StuckOp {
    pub what: String,
    pub message: String,
}

type Hook<A> = Arc<dyn Fn(A) + Send + Sync>;
/// Decides whether to die at a fault point.
pub type FaultHook = Arc<dyn Fn(Point, &proto::InHeader) -> bool + Send + Sync>;

/// The `fault-injection` build's fault point, read once at start from [`FAULT_FILE`] (`POINT:OPCODE:NTH`, e.g.
/// `call-answered:MKDIR:1`; points `read-returned`, `call-sent`, `call-answered`, `reply-written`) and removed, so the
/// supervisor the keeper restarts after the SIGKILL runs without it (supervisor 22 item 4 on the real build).
#[cfg(feature = "fault-injection")]
pub const FAULT_FILE: &str = "/run/fault-injection";

/// The hook for [`FAULT_FILE`], if it names a fault point; the file is removed either way.
#[cfg(feature = "fault-injection")]
#[must_use]
pub fn fault_from_file(path: &Path) -> Option<FaultHook> {
    let s = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    let mut it = s.trim().split(':');
    let point = match it.next()? {
        "read-returned" => Point::ReadReturned,
        "call-sent" => Point::CallSent,
        "call-answered" => Point::CallAnswered,
        "reply-written" => Point::ReplyWritten,
        _ => return None,
    };
    let opcode = it.next()?.to_owned();
    let nth: u64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    tracing::warn!(?point, %opcode, nth, "fault-injection armed");
    let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    Some(Arc::new(move |p, h| {
        p == point
            && proto::opname(h.opcode) == opcode
            && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == nth
    }))
}

/// What the FUSE layer tells the rest of the supervisor (the agent, WP-23, and the lifecycle's readiness).
#[derive(Clone)]
pub struct Hooks {
    /// `lease_lost`: kill the generation (SIGKILL), send no more reports for its turn, health `ready: false`.
    pub lease_lost: Arc<dyn Fn() + Send + Sync>,
    /// A new lease after a loss: ready again.
    pub lease_regained: Hook<magpie_api::sandbox::LeaseResponse>,
    /// `cc.write_stuck`.
    pub write_stuck: Hook<StuckOp>,
    /// The cache disagrees with magpie's bytes: stop the agent (a forked history must not continue).
    pub forked: Hook<String>,
    /// Fault injection (tests, and the `fault-injection` build): `true` kills the supervisor at that point.
    pub fault: Option<FaultHook>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            lease_lost: Arc::new(|| {}),
            lease_regained: Arc::new(|_| {}),
            write_stuck: Arc::new(|s| tracing::error!(what = %s.what, message = %s.message, "cc write stuck")),
            forked: Arc::new(|m| tracing::error!(message = %m, "history forked")),
            fault: None,
        }
    }
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

/// Why a mount failed.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    /// The keeper holds the connection's fd but no valid stored `bootId` exists (9.3): never draw a new one on the
    /// old connection.
    #[error("boot_id_unreadable: {0}")]
    BootIdUnreadable(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

const BOOT_ID: &str = "boot_id";

/// The exit of a start refused for `boot_id_unreadable` (the keeper restarts it with backoff, keeping the fd).
pub const EXIT_BOOT_ID: i32 = 75;

/// Writes the connection's `bootId` atomically (temporary file, `fsync`, `rename`, `fsync` of the directory).
///
/// # Errors
/// Any step fails.
pub fn store_boot_id(state: &Path, id: Uuid) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(state)?;
    let tmp = state.join("boot_id.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(id.to_string().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, state.join(BOOT_ID))?;
    std::fs::File::open(state)?.sync_all()
}

/// The stored `bootId`.
///
/// # Errors
/// Missing or not a UUID.
pub fn read_boot_id(state: &Path) -> Result<Uuid, MountError> {
    let s = std::fs::read_to_string(state.join(BOOT_ID)).map_err(|e| MountError::BootIdUnreadable(e.to_string()))?;
    Uuid::parse_str(s.trim()).map_err(|e| MountError::BootIdUnreadable(e.to_string()))
}

/// The `bootId` for a mount: fresh (and stored before the first request is read) on a new connection, the stored
/// one when the connection is the keeper's.
///
/// # Errors
/// [`MountError::BootIdUnreadable`] on a restart without a valid stored id.
pub fn boot_id_for(state: &Path, restart: bool) -> Result<Uuid, MountError> {
    if restart {
        return read_boot_id(state);
    }
    let id = Uuid::new_v4();
    store_boot_id(state, id)?;
    Ok(id)
}

fn cstring(s: &str) -> std::io::Result<CString> {
    CString::new(s).map_err(std::io::Error::other)
}

/// Opens `/dev/fuse` and mounts it at `mount` (supervisor 2 item 3's options).
///
/// # Errors
/// `open` or `mount(2)` fails.
pub fn mount_fresh(mount: &Path) -> std::io::Result<OwnedFd> {
    let dev = cstring("/dev/fuse")?;
    // SAFETY: open(2) of a NUL-terminated path.
    #[allow(unsafe_code)]
    let fd: RawFd = unsafe { libc::open(dev.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a fresh fd we own.
    #[allow(unsafe_code)]
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    std::fs::create_dir_all(mount)?;
    let data = format!("fd={fd},rootmode=40755,user_id=0,group_id=0,default_permissions,allow_other");
    let (src, tgt, ty, d) = (
        cstring("supervisor")?,
        cstring(&mount.to_string_lossy())?,
        cstring("fuse")?,
        cstring(&data)?,
    );
    // SAFETY: mount(2) with NUL-terminated strings that outlive the call.
    #[allow(unsafe_code)]
    let r = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            ty.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            d.as_ptr().cast(),
        )
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    tracing::info!(mount = %mount.display(), "mounted ~/.claude");
    Ok(owned)
}

/// The `~/.claude` filesystem the start sequence mounts.
pub struct FuseFs {
    cfg: Config,
    hooks: Mutex<Hooks>,
    core: Mutex<Option<Arc<Core>>>,
    remote: Arc<Remote>,
}

impl std::fmt::Debug for FuseFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseFs")
            .field("mount", &self.cfg.mount)
            .finish_non_exhaustive()
    }
}

impl FuseFs {
    #[must_use]
    pub fn new(cfg: Config, magpie: magpie_api::client::Client) -> Arc<Self> {
        let remote = Arc::new(Remote::new(magpie, cfg.timing));
        Arc::new(Self {
            cfg,
            hooks: Mutex::new(Hooks::default()),
            core: Mutex::new(None),
            remote,
        })
    }

    /// Sets the hooks (before or after the mount).
    pub fn set_hooks(&self, h: Hooks) {
        if let Some(c) = self.core() {
            c.set_hooks(h.clone());
        }
        *self.hooks.lock().unwrap_or_else(PoisonError::into_inner) = h;
    }

    /// The running server, once mounted.
    #[must_use]
    pub fn core(&self) -> Option<Arc<Core>> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// magpie as the FUSE layer sees it (WP-23's heartbeat records its successes here for the stuck rule).
    #[must_use]
    pub fn remote(&self) -> &Arc<Remote> {
        &self.remote
    }

    /// Starts the server on a device (the kernel's, or a test's).
    ///
    /// # Errors
    /// A local file cannot be opened.
    pub async fn serve(
        &self,
        dev: Arc<dyn Device>,
        boot: Uuid,
        restart: bool,
        lease: &magpie_api::sandbox::LeaseResponse,
    ) -> std::io::Result<Arc<Core>> {
        let cfg = self.cfg.clone();
        let rt = Handle::current();
        let args = StartArgs {
            dev,
            boot,
            restart,
            remote: self.remote.clone(),
            lease: lease.clone(),
            hooks: self.hooks.lock().unwrap_or_else(PoisonError::into_inner).clone(),
        };
        let core = tokio::task::spawn_blocking(move || Core::start(cfg, rt, args))
            .await
            .map_err(std::io::Error::other)??;
        *self.core.lock().unwrap_or_else(PoisonError::into_inner) = Some(core.clone());
        Ok(core)
    }

    async fn mount_kernel(
        &self,
        lease: &magpie_api::sandbox::LeaseResponse,
        keeper: &crate::keeper::Client,
        restart: bool,
    ) -> Result<(), MountError> {
        let k = keeper.clone();
        let held = if restart {
            tokio::task::spawn_blocking(move || k.get_fd(crate::keeper::FUSE_FD))
                .await
                .map_err(std::io::Error::other)?
                .map_err(|e| std::io::Error::other(e.to_string()))?
        } else {
            None
        };
        let (fd, restart, boot) = if let Some(fd) = held {
            (fd, true, boot_id_for(&self.cfg.state, true)?)
        } else {
            let mount = self.cfg.mount.clone();
            let fd = tokio::task::spawn_blocking(move || mount_fresh(&mount))
                .await
                .map_err(std::io::Error::other)??;
            // the new connection's id is stored before the keeper holds the fd: a crash in between leaves no
            // keeper fd (the next start mounts afresh), never a keeper fd without its id
            let boot = boot_id_for(&self.cfg.state, false)?;
            let raw = fd.as_raw_fd();
            let k = keeper.clone();
            tokio::task::spawn_blocking(move || k.put_fd(crate::keeper::FUSE_FD, raw))
                .await
                .map_err(std::io::Error::other)?
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            (fd, false, boot)
        };
        tracing::info!(restart, %boot, "fuse connection");
        self.serve(Arc::new(FdDevice(fd)), boot, restart, lease).await?;
        Ok(())
    }
}

impl Filesystem for FuseFs {
    fn mount(
        &self,
        lease: &magpie_api::sandbox::LeaseResponse,
        keeper: &crate::keeper::Client,
        restart: bool,
    ) -> BoxFuture<'_, Result<(), String>> {
        let lease = lease.clone();
        let keeper = keeper.clone();
        Box::pin(async move {
            #[allow(clippy::single_match_else)]
            match self.mount_kernel(&lease, &keeper, restart).await {
                Ok(()) => Ok(()),
                Err(MountError::BootIdUnreadable(e)) => {
                    // 9.3: refuse to start; the keeper keeps the fd and backs off
                    tracing::error!(event = "boot_id_unreadable", error = %e, "the keeper holds the connection but no stored bootId: exiting");
                    std::process::exit(EXIT_BOOT_ID);
                }
                Err(e) => Err(e.to_string()),
            }
        })
    }

    fn stats(&self) -> FsStats {
        let (blocked, oldest) = self.remote.blocked();
        FsStats {
            inflight: self.core().map_or(0, |c| c.inflight()),
            blocked,
            oldest_blocked_ms: oldest.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            lease_epoch: self.remote.lease().map(|l| l.epoch),
            magpie_reachable: self.remote.reachable(),
        }
    }

    fn settle(&self, deadline: tokio::time::Instant) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let Some(c) = self.core() else { return true };
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::task::spawn_blocking(move || c.settle(left))
                .await
                .unwrap_or(false)
        })
    }

    fn close_connections(&self) {}

    fn resumed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(c) = self.core() {
                let _ = tokio::task::spawn_blocking(move || c.resumed()).await;
            }
        })
    }

    fn unmount(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(c) = self.core() else { return };
            c.stop();
            if let Ok(t) = cstring(&self.cfg.mount.to_string_lossy()) {
                // SAFETY: umount2(2) of a NUL-terminated path.
                #[allow(unsafe_code)]
                let _ = unsafe { libc::umount2(t.as_ptr(), libc::MNT_DETACH) };
            }
        })
    }
}
