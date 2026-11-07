//! `/workspace` on `JuiceFS` (hermit 3.8, 15): the YYC implementation of [`Workspaces`]. hermit's own mount of the
//! volume is `/jfs` (`all-squash=1000:1000`, so everything it makes is owned by the sandbox user); a workspace is
//! the folder `ws/<id>` with a directory quota. Quotas go through the `juicefs` CLI against the metadata database;
//! `clone` and `rmr` through the CLI on the mount; listing and reading are plain syscalls on the mount, every path
//! resolved with `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the workspace's folder so a symlink the
//! agent made never leads outside it. An absolute symlink is not followed even when it names a path inside the
//! workspace as the sandbox sees it (`/workspace/out/x`): `RESOLVE_BENEATH` refuses every absolute link, and such a
//! path answers 400 `bad_path` (a known gap of 3.8, recorded for the spec owner).
//!
//! Every filesystem call has a time limit. A `stat`, `mkdir`, `open` or `rename` with no answer in [`FS_TIMEOUT`] is a
//! hung mount (15.6: 503 `workspace_unavailable`, retry-after 2, and a start hands over). At most one `stat` of the
//! mount runs per replica: callers that arrive while it runs share its answer, and once it is older than
//! [`FS_TIMEOUT`] the mount is hung and every check fails at once until it returns. A listing slower than
//! [`LIST_TIMEOUT`] or a read chunk slower than [`READ_TIMEOUT`] is a slow call, not a hung mount: 503 (the listing)
//! or a body that ends with an error (the read), and the mount's health is left to the `stat`. A listing still running
//! is joined by the next request for the same folder (never a second scan of it), and its answer is kept
//! [`LIST_KEEP`] for the retry: a request for the same page in that time gets that scan's page, not a new one. A
//! read holds one of [`MAX_READS`] permits from before its `open` until its last chunk (or its blocked thread)
//! returns; a client that takes no chunk for [`READ_IDLE`] loses the body.
//!
//! A clone is made under a temporary name in `ws/` ([`CLONE_PREFIX`]) and renamed into place with
//! `RENAME_NOREPLACE` (`JuiceFS` 1.4.1 supports the flag: `pkg/meta/base.go` `Rename`), so `ws/<wid>` is never a
//! partial tree: a failed, timed-out or abandoned clone removes its temporary tree, and the probe sweeps the ones a
//! dead replica left (older than [`CLONE_STALE`]). The temporary name is unique to one clone call (a random part
//! after the time), so two clones of the same id at once (a retry, two magpie replicas freezing one share) never
//! share a tree, and one's cleanup never removes the other's copy. A rename with no answer in [`FS_TIMEOUT`] (a hung
//! mount) answers 503 and leaves the temporary tree alone while the rename's thread is blocked: if the rename lands
//! later, `ws/<wid>` gets its quota then (the next ensure sets it too, if that fails); if it fails, the tree is
//! removed then. A copy whose rename never returns stays under its temporary name until the sweep.
//!
//! The CLI is behind [`JfsCli`] so the tests run the store on a plain directory with an in-memory quota table; no
//! configuration selects it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use futures_util::FutureExt as _;
use futures_util::future::{BoxFuture, Shared};
use hermit_api::Timestamp;
use hermit_api::error::{self, codes};
use hermit_api::workspaces::{
    Amount, ByteRange, ENTRIES_PAGE, Entries, Entry, EntryType, FileContent, FileMeta, Workspace,
};
use rustix::fs::{FileType, Mode, OFlags, RenameFlags, ResolveFlags};
use rustix::io::Errno;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::fabric::{Fut, StoreHealth, WorkspaceError, Workspaces};

/// hermit's mount of the volume (the `hermit-jfs` PVC, hermit 15.2).
pub const MOUNT: &str = "/jfs";
/// At most this many file reads at once per replica (15.5).
pub const MAX_READS: usize = 64;
/// `GET /v1/workspaces/{wid}`'s usage is cached this long (15.5).
pub const USAGE_CACHE: Duration = Duration::from_secs(10);
/// Every CLI call's timeout (15.5) but `clone`'s.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);
/// `juicefs clone` copies every entry's metadata: a tree of hundreds of thousands of files (a `node_modules`) takes
/// minutes. hermit-api's client waits [`hermit_api::client::CLONE_TIMEOUT`], a minute longer.
pub const CLONE_TIMEOUT: Duration = Duration::from_mins(15);
/// A `stat`, `mkdir` or `open` on the mount: longer and the mount is taken as hung (15.6).
pub const FS_TIMEOUT: Duration = Duration::from_secs(2);
/// One page of a listing (`readdir` of the folder and a `stat` per entry of the page): longer answers 503, and the
/// scan goes on for the retry.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// A finished listing that nobody waited for is kept this long for the retry (the 503's retry-after is 2 s), and served
/// to any request for the same folder and cursor meanwhile: such a page shows the folder as it was during that scan,
/// at most this long after the scan ended.
pub const LIST_KEEP: Duration = Duration::from_secs(5);
/// One 64 KiB chunk of a file read (a cold chunk comes from the object store).
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A read waits this long for one of the [`MAX_READS`] permits, then answers 503.
pub const READ_WAIT: Duration = Duration::from_secs(5);
/// A client that takes no chunk of a file for this long loses the body (and its read permit).
pub const READ_IDLE: Duration = Duration::from_secs(60);
/// A clone's temporary name in `ws/`: `.clone-<wid>-<unix seconds>-<random>` (the random part keeps two clones of
/// the same id in the same second apart).
pub const CLONE_PREFIX: &str = ".clone-";
/// A temporary clone tree older than this belongs to no running clone ([`CLONE_TIMEOUT`] kills every clone).
pub const CLONE_STALE: Duration = Duration::from_mins(30);
/// The probe sweeps stale temporary clone trees at most this often.
pub const SWEEP_EVERY: Duration = Duration::from_hours(1);
const CHUNK: usize = 64 * 1024;
const GIB: u64 = 1 << 30;

/// A directory quota as `juicefs quota get` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    pub capacity_bytes: u64,
    pub inodes: u64,
    pub used_bytes: u64,
    pub used_inodes: u64,
}

/// Why a CLI call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    /// The call met EACCES/EPERM on an entry (`rmr` of a tree with a root-owned entry, 15.4).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("{0}")]
    Failed(String),
}

/// The `juicefs` commands hermit runs (15.5). Paths of quota calls are within the volume (`/ws/<id>`); paths of
/// `clone` and `rmr` are on the mount.
pub trait JfsCli: Send + Sync + 'static {
    /// `None` when the directory has no quota.
    fn quota_get<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>>;
    fn quota_set<'a>(&'a self, path: &'a str, capacity_gib: u32, inodes: u64) -> Fut<'a, Result<(), CliError>>;
    fn quota_delete<'a>(&'a self, path: &'a str) -> Fut<'a, Result<(), CliError>>;
    /// A metadata-only copy-on-write copy (`juicefs clone`).
    fn clone_tree<'a>(&'a self, from: &'a Path, to: &'a Path) -> Fut<'a, Result<(), CliError>>;
    /// Removes the tree server side (`juicefs rmr`).
    fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>>;
}

/// The `juicefs` 1.4.1 CLI of hermit's image. The metadata URL is an argument; its password reaches the child
/// only as `META_PASSWORD` (`JuiceFS` reads it from there), never on the command line.
pub struct JuiceCli {
    bin: PathBuf,
    meta_url: String,
    meta_password: String,
}

impl std::fmt::Debug for JuiceCli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JuiceCli")
            .field("bin", &self.bin)
            .finish_non_exhaustive()
    }
}

impl JuiceCli {
    #[must_use]
    pub fn new(meta_url: String, meta_password: String) -> Self {
        Self {
            bin: PathBuf::from("juicefs"),
            meta_url,
            meta_password,
        }
    }

    async fn run(&self, args: Vec<OsString>) -> Result<String, CliError> {
        self.run_for(args, CLI_TIMEOUT).await
    }

    async fn run_for(&self, args: Vec<OsString>, limit: Duration) -> Result<String, CliError> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.args(&args)
            .env_clear()
            .env("META_PASSWORD", &self.meta_password)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(limit, cmd.output())
            .await
            .map_err(|_| CliError::Failed(format!("juicefs {}: no answer in {} s", verb(&args), limit.as_secs())))?
            .map_err(|e| CliError::Failed(format!("juicefs: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        // JuiceFS logs FATAL and still exits 1; some failures exit 0 with a FATAL line
        let fatal = stderr.lines().find(|l| l.contains("<FATAL>") || l.contains("<ERROR>"));
        if out.status.success() && fatal.is_none() {
            return Ok(stdout);
        }
        let msg = fatal.map_or_else(|| stderr.trim().to_owned(), str::to_owned);
        let msg = crate::db::clip(&msg, 300);
        let lower = msg.to_ascii_lowercase();
        // (a clone's target always is a fresh temporary name: "exists" is the rename's EEXIST, never CLI text)
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            Err(CliError::PermissionDenied(msg))
        } else {
            Err(CliError::Failed(format!("juicefs {}: {msg}", verb(&args))))
        }
    }

    fn quota_args(&self, sub: &str, path: &str) -> Vec<OsString> {
        vec![
            "quota".into(),
            sub.into(),
            self.meta_url.clone().into(),
            "--path".into(),
            path.into(),
        ]
    }
}

fn verb(args: &[OsString]) -> String {
    args.iter()
        .take(2)
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

impl JfsCli for JuiceCli {
    fn quota_get<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
        Box::pin(async move { Ok(parse_quota_table(&self.run(self.quota_args("get", path)).await?, path)) })
    }

    fn quota_set<'a>(&'a self, path: &'a str, capacity_gib: u32, inodes: u64) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            let mut a = self.quota_args("set", path);
            a.extend([
                "--capacity".into(),
                capacity_gib.to_string().into(),
                "--inodes".into(),
                inodes.to_string().into(),
            ]);
            self.run(a).await.map(drop)
        })
    }

    fn quota_delete<'a>(&'a self, path: &'a str) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move { self.run(self.quota_args("delete", path)).await.map(drop) })
    }

    fn clone_tree<'a>(&'a self, from: &'a Path, to: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.run_for(vec!["clone".into(), from.into(), to.into()], CLONE_TIMEOUT)
                .await
                .map(drop)
        })
    }

    fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move { self.run(vec!["rmr".into(), path.into()]).await.map(drop) })
    }
}

/// Parses `juicefs quota get`'s table (1.4.1):
///
/// ```text
/// | Path  |   Size  | Used | Use% | Inodes | IUsed | IUse% |
/// | /ws/a | 1.0 GiB |  0 B |   0% |  1,000 |     0 |    0% |
/// ```
///
/// No table (no quota on the path) is `None`. Sizes are `JuiceFS`'s human form (`1.5 MiB`), so usage is exact to the
/// unit's first decimal; the capacity is always a whole number of GiB here.
#[must_use]
pub fn parse_quota_table(out: &str, path: &str) -> Option<Quota> {
    let mut header: Option<Vec<String>> = None;
    for line in out.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line.trim_matches('|').split('|').map(|c| c.trim().to_owned()).collect();
        match &header {
            None => header = Some(cells),
            Some(h) => {
                let get = |name: &str| h.iter().position(|c| c == name).and_then(|i| cells.get(i));
                if get("Path").map(String::as_str) != Some(path) {
                    continue;
                }
                return Some(Quota {
                    capacity_bytes: parse_size(get("Size")?)?,
                    inodes: parse_count(get("Inodes")?)?,
                    used_bytes: parse_size(get("Used")?)?,
                    used_inodes: parse_count(get("IUsed")?)?,
                });
            }
        }
    }
    None
}

fn parse_count(s: &str) -> Option<u64> {
    s.replace(',', "").parse().ok()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn parse_size(s: &str) -> Option<u64> {
    let (num, unit) = s.trim().split_once(' ').unwrap_or((s.trim(), "B"));
    let mult: u64 = match unit {
        "B" | "Bytes" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        "PiB" => 1 << 50,
        _ => return None,
    };
    let n: f64 = num.replace(',', "").parse().ok()?;
    (n >= 0.0).then(|| (n * mult as f64).round() as u64)
}

/// The `JuiceFS` workspace store of one replica.
pub struct JuiceFs {
    /// The mount (`/jfs`); workspaces are `ws/<id>` under it.
    root: PathBuf,
    cli: Arc<dyn JfsCli>,
    quota_gib: u32,
    quota_inodes: u64,
    usage: Mutex<HashMap<Uuid, (Instant, Option<Quota>)>>,
    reads: Arc<Semaphore>,
    health: Mutex<StoreHealth>,
    /// The `stat` of the mount now running, if one is (shared by every check meanwhile; a hung one keeps its thread).
    stat: Arc<Mutex<Option<StatFlight>>>,
    /// Listings running (or finished and not yet collected), by workspace and folder.
    listings: Arc<Mutex<HashMap<(Uuid, String), ListFlight>>>,
    flights: AtomicU64,
    last_sweep: Mutex<Option<Instant>>,
    sweeping: Arc<AtomicBool>,
    /// Work spawned past its request (an unfinished clone's removal, a late rename, the sweep): a one-shot command
    /// waits for it before it exits ([`JuiceFs::settle`]).
    background: Arc<Background>,
}

/// Tasks a [`JuiceFs`] spawned that outlive the call that started them.
#[derive(Default)]
struct Background(Mutex<Vec<tokio::task::JoinHandle<()>>>);

impl Background {
    fn spawn_on(&self, rt: &tokio::runtime::Handle, f: impl std::future::Future<Output = ()> + Send + 'static) {
        let h = rt.spawn(f);
        let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|h| !h.is_finished());
        all.push(h);
    }

    fn spawn(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        self.spawn_on(&tokio::runtime::Handle::current(), f);
    }
}

/// `stat /jfs/ws`'s answer: whether it is a directory, or the error's text; `Err` outside when the thread panicked.
type StatAnswer = Result<Result<bool, String>, String>;

struct StatFlight {
    id: u64,
    started: Instant,
    answer: Shared<BoxFuture<'static, StatAnswer>>,
}

/// A listing's answer and when it was ready.
type ListAnswer = (Result<Entries, WorkspaceError>, Instant);

struct ListFlight {
    id: u64,
    cursor: Option<String>,
    answer: Shared<BoxFuture<'static, ListAnswer>>,
}

impl std::fmt::Debug for JuiceFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JuiceFs")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn unavailable(m: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Unavailable(m.into())
}

fn api(e: hermit_api::error::ApiError) -> WorkspaceError {
    WorkspaceError::Api(e)
}

fn no_workspace() -> WorkspaceError {
    api(error::not_found(codes::NO_WORKSPACE, "no such workspace"))
}

fn no_path() -> WorkspaceError {
    api(error::not_found(codes::NO_PATH, "no such path"))
}

fn ts(t: SystemTime) -> Timestamp {
    Timestamp::from(time::OffsetDateTime::from(t))
}

/// A path error inside a workspace as the contract answers it.
fn path_error(e: Errno, want_dir: bool) -> WorkspaceError {
    match e {
        Errno::NOTDIR if want_dir => api(error::api(409, codes::NOT_A_DIR, "not a directory")),
        Errno::NOENT | Errno::NOTDIR => no_path(),
        // RESOLVE_BENEATH refused a component (an absolute symlink, `..` past the root): an escape
        Errno::XDEV => api(error::api(400, codes::BAD_PATH, "the path leads outside the workspace")),
        Errno::LOOP => api(error::api(400, codes::BAD_PATH, "too many symlinks")),
        Errno::NAMETOOLONG => api(error::api(400, codes::BAD_PATH, "path too long")),
        Errno::ACCESS | Errno::PERM => unavailable(format!("workspace read refused: {e}")),
        other => unavailable(format!("workspace store: {other}")),
    }
}

impl JuiceFs {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, cli: Arc<dyn JfsCli>, quota_gib: u32, quota_inodes: u64) -> Self {
        Self {
            root: root.into(),
            cli,
            quota_gib,
            quota_inodes,
            usage: Mutex::new(HashMap::new()),
            reads: Arc::new(Semaphore::new(MAX_READS)),
            health: Mutex::new(StoreHealth::default()),
            stat: Arc::new(Mutex::new(None)),
            listings: Arc::new(Mutex::new(HashMap::new())),
            flights: AtomicU64::new(0),
            last_sweep: Mutex::new(None),
            sweeping: Arc::new(AtomicBool::new(false)),
            background: Arc::new(Background::default()),
        }
    }

    /// Waits up to `wait` for the work this store spawned in the background (an unfinished clone's removal, a late
    /// rename's quota, the sweep), as a one-shot command must before it exits: the runtime drops what still runs.
    /// Returns how many tasks were still running at the end.
    pub async fn settle(&self, wait: Duration) -> usize {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let next = {
                let mut all = self.background.0.lock().unwrap_or_else(PoisonError::into_inner);
                all.retain(|h| !h.is_finished());
                all.pop()
            };
            let Some(mut h) = next else { return 0 };
            if tokio::time::timeout_at(deadline, &mut h).await.is_err() {
                let mut all = self.background.0.lock().unwrap_or_else(PoisonError::into_inner);
                all.push(h);
                all.retain(|h| !h.is_finished());
                return all.len();
            }
        }
    }

    fn ws_root(&self) -> PathBuf {
        self.root.join("ws")
    }

    fn dir(&self, wid: Uuid) -> PathBuf {
        self.ws_root().join(wid.to_string())
    }

    /// The path of a workspace within the volume (quota calls).
    fn volume_path(wid: Uuid) -> String {
        format!("/ws/{wid}")
    }

    fn wanted(&self) -> Amount {
        Amount {
            bytes: u64::from(self.quota_gib) * GIB,
            inodes: self.quota_inodes,
        }
    }

    /// Marks this replica's mount broken (15.6) and answers [`WorkspaceError::NotMounted`].
    fn not_mounted(&self, m: String) -> WorkspaceError {
        let mut h = self.health.lock().unwrap_or_else(PoisonError::into_inner);
        if h.unmounted.is_none() {
            tracing::warn!(error = %m, "workspace store not mounted");
        }
        h.unmounted = Some(m.clone());
        WorkspaceError::NotMounted(m)
    }

    /// Runs a blocking filesystem call on the mount within `limit`; no answer in time is a hung mount.
    async fn blocking<T: Send + 'static>(
        &self,
        what: &str,
        limit: Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        match tokio::time::timeout(limit, tokio::task::spawn_blocking(f)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(unavailable(e.to_string())),
            Err(_) => Err(self.not_mounted(format!("{what}: no answer in {} s", limit.as_secs()))),
        }
    }

    /// The mount check of 15.6: `stat /jfs/ws` within [`FS_TIMEOUT`]. `ENOTCONN`/`EIO` (a broken FUSE mount), a
    /// missing `ws` (nothing mounted) or no answer make every workspace call of this replica answer 503 and its
    /// starts hand over. One `stat` runs at a time: a check while it runs waits for its answer (within what is left
    /// of its [`FS_TIMEOUT`]); once it is older than that, the mount is hung and the check fails at once.
    async fn check(&self) -> Result<(), WorkspaceError> {
        let root = self.ws_root();
        let (answer, started) = {
            let mut slot = self.stat.lock().unwrap_or_else(PoisonError::into_inner);
            match &*slot {
                Some(f) if f.started.elapsed() >= FS_TIMEOUT => {
                    let secs = f.started.elapsed().as_secs();
                    drop(slot);
                    return Err(self.not_mounted(format!(
                        "{}: the stat started {secs} s ago has not answered",
                        root.display()
                    )));
                }
                Some(f) => (f.answer.clone(), f.started),
                None => {
                    let id = self.flights.fetch_add(1, Ordering::Relaxed);
                    let (r, done) = (root.clone(), self.stat.clone());
                    let h = tokio::task::spawn_blocking(move || {
                        let m = std::fs::metadata(&r).map(|m| m.is_dir()).map_err(|e| e.to_string());
                        let mut slot = done.lock().unwrap_or_else(PoisonError::into_inner);
                        if slot.as_ref().is_some_and(|f| f.id == id) {
                            *slot = None;
                        }
                        m
                    });
                    let answer = async move { h.await.map_err(|e| e.to_string()) }.boxed().shared();
                    let started = Instant::now();
                    *slot = Some(StatFlight {
                        id,
                        started,
                        answer: answer.clone(),
                    });
                    (answer, started)
                }
            }
        };
        let left = FS_TIMEOUT.saturating_sub(started.elapsed());
        match tokio::time::timeout(left, answer).await {
            Err(_) => Err(self.not_mounted(format!(
                "stat of the workspace store: no answer in {} s",
                FS_TIMEOUT.as_secs()
            ))),
            Ok(Err(e)) => Err(unavailable(e)),
            Ok(Ok(Ok(true))) => {
                self.health.lock().unwrap_or_else(PoisonError::into_inner).unmounted = None;
                Ok(())
            }
            Ok(Ok(Ok(false))) => Err(self.not_mounted("ws is not a directory".into())),
            Ok(Ok(Err(e))) => Err(self.not_mounted(format!("{}: {e}", root.display()))),
        }
    }

    /// One page of folder `path` of workspace `wid`. A listing of the same folder still running is joined (same
    /// cursor) or makes this one wait its turn (503); one slower than [`LIST_TIMEOUT`] answers 503 and goes on, and
    /// its answer is kept [`LIST_KEEP`] for the retry.
    async fn listing(&self, wid: Uuid, path: &str, cursor: Option<&str>) -> Result<Entries, WorkspaceError> {
        let key = (wid, path.to_owned());
        let (id, answer) = {
            let mut all = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
            // finished answers nobody came back for
            all.retain(|_, f| f.answer.peek().is_none_or(|(_, at)| at.elapsed() < LIST_KEEP));
            match all.get(&key) {
                Some(f) if f.cursor.as_deref() == cursor => (f.id, f.answer.clone()),
                Some(f) if f.answer.peek().is_none() => {
                    return Err(unavailable("a listing of this folder is still running"));
                }
                _ => {
                    let id = self.flights.fetch_add(1, Ordering::Relaxed);
                    let dir = self.dir(wid);
                    let (p, c) = (path.to_owned(), cursor.map(str::to_owned));
                    let h = tokio::task::spawn_blocking(move || (Self::list(&dir, &p, c.as_deref()), Instant::now()));
                    let answer = async move {
                        h.await
                            .unwrap_or_else(|e| (Err(unavailable(e.to_string())), Instant::now()))
                    }
                    .boxed()
                    .shared();
                    // the scan runs to its end even when nobody waits, so its answer is there for the retry
                    tokio::spawn(answer.clone());
                    all.insert(
                        key.clone(),
                        ListFlight {
                            id,
                            cursor: cursor.map(str::to_owned),
                            answer: answer.clone(),
                        },
                    );
                    (id, answer)
                }
            }
        };
        match tokio::time::timeout(LIST_TIMEOUT, answer).await {
            Ok((res, _)) => {
                let mut all = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
                if all.get(&key).is_some_and(|f| f.id == id) {
                    all.remove(&key);
                }
                res
            }
            Err(_) => Err(unavailable(format!(
                "the listing took longer than {} s; it goes on, and a retry gets its answer",
                LIST_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Starts a sweep of abandoned clone trees in the background, at most every [`SWEEP_EVERY`] and one at a time.
    fn maybe_sweep(&self) {
        {
            let mut last = self.last_sweep.lock().unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|t| t.elapsed() < SWEEP_EVERY) || self.sweeping.swap(true, Ordering::SeqCst) {
                return;
            }
            *last = Some(Instant::now());
        }
        let (root, cli, sweeping) = (self.ws_root(), self.cli.clone(), self.sweeping.clone());
        self.background.spawn(async move {
            sweep_clones(root, cli).await;
            sweeping.store(false, Ordering::SeqCst);
        });
    }

    async fn quota(&self, wid: Uuid) -> Result<Option<Quota>, WorkspaceError> {
        self.cli
            .quota_get(&Self::volume_path(wid))
            .await
            .map_err(|e| unavailable(e.to_string()))
    }

    async fn workspace(&self, wid: Uuid, created_at: Timestamp, cached: bool) -> Result<Workspace, WorkspaceError> {
        let hit = cached
            .then(|| {
                self.usage
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&wid)
                    .filter(|(at, _)| at.elapsed() < USAGE_CACHE)
                    .map(|(_, q)| *q)
            })
            .flatten();
        let q = if let Some(q) = hit {
            q
        } else {
            let q = self.quota(wid).await?;
            self.usage
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(wid, (Instant::now(), q));
            q
        };
        Ok(Workspace {
            id: wid,
            created_at,
            quota: q.map_or(Amount { bytes: 0, inodes: 0 }, |q| Amount {
                bytes: q.capacity_bytes,
                inodes: q.inodes,
            }),
            usage: q.map_or(Amount { bytes: 0, inodes: 0 }, |q| Amount {
                bytes: q.used_bytes,
                inodes: q.used_inodes,
            }),
        })
    }

    /// Whether the folder exists, with a stand-in creation time: its birth time where the filesystem keeps one,
    /// else its modification time (`JuiceFS` keeps no birth time, and a folder's mtime moves with every entry added
    /// or removed). The stable `createdAt` is hermit's `workspaces` row ([`crate::db::workspace_created`]); this time only
    /// seeds the row of a workspace made before hermit kept one.
    async fn created_at(&self, wid: Uuid) -> Result<Option<Timestamp>, WorkspaceError> {
        let dir = self.dir(wid);
        let res = self
            .blocking("stat of a workspace", FS_TIMEOUT, move || {
                std::fs::symlink_metadata(&dir)
            })
            .await?;
        match res {
            Ok(m) if m.is_dir() => Ok(Some(ts(m
                .created()
                .or_else(|_| m.modified())
                .unwrap_or(SystemTime::now())))),
            Ok(_) => Err(unavailable(format!("ws/{wid} is not a directory"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(unavailable(format!("ws/{wid}: {e}"))),
        }
    }

    /// Copies the tree `src` (a workspace's folder, or another folder on the volume) to `ws/<wid>` (15.5): a
    /// `juicefs clone` under a temporary name, renamed into place without replacing anything, then ensure's quota.
    /// 409 `exists` when `ws/<wid>` appeared meanwhile.
    async fn clone_dir(&self, wid: Uuid, src: PathBuf) -> Result<Workspace, WorkspaceError> {
        // the copy is made under a temporary name and renamed into place without replacing anything: ws/<wid> is
        // never a partial tree, and a clone that made the target meanwhile wins (this one answers what it would
        // have answered a moment later)
        let tmp = self.ws_root().join(format!(
            "{CLONE_PREFIX}{wid}-{}-{}",
            unix_now(),
            Uuid::new_v4().simple()
        ));
        let guard = TempTree {
            cli: self.cli.clone(),
            path: Some(tmp.clone()),
            bg: self.background.clone(),
        };
        self.cli
            .clone_tree(&src, &tmp)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let target = self.dir(wid);
        let t = tmp.clone();
        let mut rename = tokio::task::spawn_blocking(move || rename_noreplace(&t, &target));
        let renamed = match tokio::time::timeout(FS_TIMEOUT, &mut rename).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(unavailable(e.to_string())),
            Err(_) => {
                // the blocked rename may still land: its tree is not removed under it
                guard.keep();
                self.background.spawn(settle_late_rename(
                    rename,
                    self.cli.clone(),
                    self.background.clone(),
                    tmp,
                    wid,
                    self.quota_gib,
                    self.quota_inodes,
                ));
                return Err(self.not_mounted(format!(
                    "rename of a clone into place: no answer in {} s",
                    FS_TIMEOUT.as_secs()
                )));
            }
        };
        match renamed {
            Ok(()) => guard.keep(),
            Err(Errno::EXIST | Errno::NOTEMPTY) => {
                return Err(api(error::api(409, codes::EXISTS, "the workspace exists")));
            }
            Err(e) => return Err(unavailable(format!("rename of a clone into ws/{wid}: {e}"))),
        }
        // the copy gets its own quota (a directory quota is not copied with the tree)
        self.set_quota(wid).await?;
        let created = self.created_at(wid).await?.unwrap_or_else(Timestamp::now);
        self.workspace(wid, created, false).await
    }

    /// Sets the quota when absent or different (every ensure, A18.F2-F3).
    async fn set_quota(&self, wid: Uuid) -> Result<(), WorkspaceError> {
        let want = self.wanted();
        let have = self.quota(wid).await?;
        if have.is_some_and(|q| q.capacity_bytes == want.bytes && q.inodes == want.inodes) {
            return Ok(());
        }
        self.cli
            .quota_set(&Self::volume_path(wid), self.quota_gib, self.quota_inodes)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        self.usage.lock().unwrap_or_else(PoisonError::into_inner).remove(&wid);
        Ok(())
    }

    /// Opens the workspace folder (`O_DIRECTORY | O_NOFOLLOW`): `no_workspace` when absent.
    fn open_root(dir: &Path) -> Result<OwnedFd, WorkspaceError> {
        rustix::fs::open(
            dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| match e {
            Errno::NOENT | Errno::NOTDIR | Errno::LOOP => no_workspace(),
            other => unavailable(format!("workspace store: {other}")),
        })
    }

    /// Resolves `path` beneath the workspace folder (3.8): symlinks inside are followed, nothing outside is
    /// reachable, no magic links.
    fn open_beneath(root: &OwnedFd, path: &str, flags: OFlags, want_dir: bool) -> Result<OwnedFd, WorkspaceError> {
        rustix::fs::openat2(
            root,
            path,
            flags | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|e| path_error(e, want_dir))
    }

    fn list(dir: &Path, path: &str, cursor: Option<&str>) -> Result<Entries, WorkspaceError> {
        let root = Self::open_root(dir)?;
        let fd = if path.is_empty() {
            root
        } else {
            Self::open_beneath(&root, path, OFlags::RDONLY | OFlags::DIRECTORY, true)?
        };
        // the names first (one readdir), then a stat for the page's entries only: a folder of 100 000 entries costs
        // 1 001 stats per page, not 100 000
        let mut names: Vec<Vec<u8>> = Vec::new();
        let d = rustix::fs::Dir::read_from(&fd).map_err(|e| path_error(e, true))?;
        for item in d {
            let item = item.map_err(|e| unavailable(format!("readdir: {e}")))?;
            let name = item.file_name().to_bytes();
            if name == b"." || name == b".." || cursor.is_some_and(|c| name <= c.as_bytes()) {
                continue;
            }
            names.push(name.to_vec());
        }
        let more = names.len() > ENTRIES_PAGE;
        if more {
            names.select_nth_unstable(ENTRIES_PAGE);
            names.truncate(ENTRIES_PAGE);
        }
        names.sort_unstable();
        // the cursor is the page's last name even when that entry went away before its stat
        let next_cursor = more
            .then(|| names.last().map(|n| String::from_utf8_lossy(n).into_owned()))
            .flatten();
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let Ok(cname) = std::ffi::CString::new(name.clone()) else {
                continue;
            };
            // an entry removed between readdir and stat is left out
            let Ok(st) = rustix::fs::statat(&fd, cname.as_c_str(), rustix::fs::AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            let kind = match FileType::from_raw_mode(st.st_mode) {
                FileType::RegularFile => EntryType::File,
                FileType::Directory => EntryType::Dir,
                FileType::Symlink => EntryType::Symlink,
                _ => EntryType::Other,
            };
            let mtime = Timestamp::from_unix_millis(
                st.st_mtime.saturating_mul(1000) + i64::try_from(st.st_mtime_nsec / 1_000_000).unwrap_or(0),
            )
            .unwrap_or_else(Timestamp::now);
            entries.push(Entry {
                name: String::from_utf8_lossy(&name).into_owned(),
                kind,
                size: if kind == EntryType::Dir {
                    0
                } else {
                    u64::try_from(st.st_size).unwrap_or(0)
                },
                mtime,
            });
        }
        Ok(Entries {
            path: path.to_owned(),
            entries,
            next_cursor,
        })
    }

    /// Opens a file for reading: `O_NONBLOCK` so a FIFO the agent made never blocks the open; anything but a
    /// regular file is `not_a_file`.
    fn open_file(dir: &Path, path: &str) -> Result<(std::fs::File, rustix::fs::Stat), WorkspaceError> {
        let root = Self::open_root(dir)?;
        let fd = Self::open_beneath(&root, path, OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY, false)?;
        let st = rustix::fs::fstat(&fd).map_err(|e| path_error(e, false))?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            return Err(api(error::api(409, codes::NOT_A_FILE, "not a regular file")));
        }
        Ok((std::fs::File::from(fd), st))
    }
}

/// Streams `len` bytes of `file` from its current offset in 64 KiB chunks. A task reads each chunk on a blocking
/// thread that owns the file and `permit` while it runs (a read that hangs keeps its permit until it returns) and
/// hands it over a channel of one: a chunk with no answer in [`READ_TIMEOUT`] ends the body with an error, and a
/// client that takes no chunk for [`READ_IDLE`] loses the body; either way the permit goes back.
fn stream(file: std::fs::File, len: u64, permit: OwnedSemaphorePermit) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<io::Result<axum::body::Bytes>>(1);
    tokio::spawn(async move {
        let mut held = Some((file, permit, vec![0u8; CHUNK]));
        let mut left = len;
        while left > 0 {
            let Some((mut file, permit, mut buf)) = held.take() else {
                break;
            };
            let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            let h = tokio::task::spawn_blocking(move || {
                use std::io::Read as _;
                let r = file.read(&mut buf[..want]);
                (r, file, permit, buf)
            });
            let item = match tokio::time::timeout(READ_TIMEOUT, h).await {
                Ok(Ok((Ok(0), ..))) => break,
                Ok(Ok((Ok(n), file, permit, buf))) => {
                    left -= n as u64;
                    let chunk = axum::body::Bytes::copy_from_slice(&buf[..n]);
                    held = Some((file, permit, buf));
                    Ok(chunk)
                }
                Ok(Ok((Err(e), ..))) => Err(e),
                Ok(Err(e)) => Err(io::Error::other(e.to_string())),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("workspace read: no answer in {} s", READ_TIMEOUT.as_secs()),
                )),
            };
            let last = item.is_err();
            if tx.send_timeout(item, READ_IDLE).await.is_err() || last {
                break;
            }
        }
    });
    let s = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    axum::body::Body::from_stream(s)
}

/// `rename(from, to)` that never replaces `to` (`RENAME_NOREPLACE`; a filesystem without the flag gets a plain
/// rename, which still refuses a non-empty `to`).
fn rename_noreplace(from: &Path, to: &Path) -> Result<(), Errno> {
    match rustix::fs::renameat_with(rustix::fs::CWD, from, rustix::fs::CWD, to, RenameFlags::NOREPLACE) {
        Err(Errno::INVAL | Errno::NOSYS) => rustix::fs::rename(from, to),
        other => other,
    }
}

/// Removes a clone's temporary tree when the clone does not finish (an error, a timeout, a dropped request), unless
/// [`TempTree::keep`] was called: best effort, in the background.
struct TempTree {
    cli: Arc<dyn JfsCli>,
    path: Option<PathBuf>,
    bg: Arc<Background>,
}

impl TempTree {
    fn keep(mut self) {
        self.path = None;
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        let cli = self.cli.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            self.bg.spawn_on(&rt, async move {
                match cli.rmr(&path).await {
                    Ok(()) => tracing::info!(path = %path.display(), "unfinished clone removed"),
                    Err(e) => tracing::info!(path = %path.display(), error = %e, "unfinished clone not removed (the sweep retries)"),
                }
            });
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A temporary clone name's time, from `<wid>-<unix seconds>-<random>` (what follows [`CLONE_PREFIX`]).
fn clone_started(rest: &str) -> Option<u64> {
    let wid = rest.get(..36)?;
    Uuid::try_parse(wid).ok()?;
    let after = rest.get(36..)?.strip_prefix('-')?;
    after.split('-').next()?.parse().ok()
}

/// A rename of a clone into place that had no answer in [`FS_TIMEOUT`]: its thread may still finish. The temporary
/// tree is left to it: a rename that lands gives `ws/<wid>` its quota (a directory quota is not copied with the
/// tree), one that fails removes the tree.
async fn settle_late_rename(
    rename: tokio::task::JoinHandle<Result<(), Errno>>,
    cli: Arc<dyn JfsCli>,
    bg: Arc<Background>,
    tmp: PathBuf,
    wid: Uuid,
    gib: u32,
    inodes: u64,
) {
    match rename.await {
        Ok(Ok(())) => match cli.quota_set(&JuiceFs::volume_path(wid), gib, inodes).await {
            Ok(()) => tracing::warn!(%wid, "a late clone rename landed; its quota is set"),
            Err(e) => {
                tracing::warn!(%wid, error = %e, "a late clone rename landed; quota not set (the next ensure sets it)");
            }
        },
        Ok(Err(e)) => {
            tracing::info!(%wid, error = %e, "a late clone rename failed");
            drop(TempTree {
                cli,
                path: Some(tmp),
                bg,
            });
        }
        Err(e) => tracing::info!(%wid, error = %e, "a late clone rename's thread failed (the sweep removes its tree)"),
    }
}

/// The temporary clone trees in `ws_root` older than [`CLONE_STALE`] (by the time in their name).
fn stale_clones(ws_root: &Path, now: u64) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(ws_root)? {
        let e = e?;
        let name = e.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(CLONE_PREFIX)) else {
            continue;
        };
        let Some(at) = clone_started(rest) else {
            continue;
        };
        if now.saturating_sub(at) > CLONE_STALE.as_secs() {
            out.push(e.path());
        }
    }
    Ok(out)
}

/// Removes the temporary trees of clones that ended with their replica (no request left to remove them).
async fn sweep_clones(ws_root: PathBuf, cli: Arc<dyn JfsCli>) {
    let scan = tokio::task::spawn_blocking(move || stale_clones(&ws_root, unix_now()));
    let stale = match tokio::time::timeout(LIST_TIMEOUT, scan).await {
        Ok(Ok(Ok(v))) => v,
        Ok(Ok(Err(e))) => return tracing::info!(error = %e, "clone sweep: ws/ not read"),
        Ok(Err(e)) => return tracing::info!(error = %e, "clone sweep: ws/ not read"),
        Err(_) => return tracing::info!("clone sweep: ws/ not read in time"),
    };
    for path in stale {
        match cli.rmr(&path).await {
            Ok(()) => tracing::warn!(path = %path.display(), "an abandoned clone's tree removed"),
            Err(e) => tracing::info!(path = %path.display(), error = %e, "an abandoned clone's tree not removed"),
        }
    }
}

impl Workspaces for JuiceFs {
    fn ensure(&self, wid: Uuid) -> Fut<'_, Result<(Workspace, bool), WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            let dir = self.dir(wid);
            let made = self
                .blocking("mkdir of a workspace", FS_TIMEOUT, move || {
                    use std::os::unix::fs::DirBuilderExt as _;
                    match std::fs::DirBuilder::new().mode(0o755).create(&dir) {
                        Ok(()) => Ok(true),
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                        Err(e) => Err(e),
                    }
                })
                .await?
                .map_err(|e| unavailable(format!("mkdir ws/{wid}: {e}")))?;
            self.set_quota(wid).await?;
            let created = self.created_at(wid).await?.unwrap_or_else(Timestamp::now);
            Ok((self.workspace(wid, created, false).await?, made))
        })
    }

    fn get(&self, wid: Uuid) -> Fut<'_, Result<Option<Workspace>, WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            match self.created_at(wid).await? {
                None => Ok(None),
                Some(created) => Ok(Some(self.workspace(wid, created, true).await?)),
            }
        })
    }

    fn entries<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        cursor: Option<&'a str>,
    ) -> Fut<'a, Result<Entries, WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            self.listing(wid, path, cursor).await
        })
    }

    fn file<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        range: Option<ByteRange>,
    ) -> Fut<'a, Result<FileContent, WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            let permit = tokio::time::timeout(READ_WAIT, self.reads.clone().acquire_owned())
                .await
                .map_err(|_| unavailable(format!("{MAX_READS} file reads are running on this replica")))?
                .map_err(|e| unavailable(e.to_string()))?;
            let dir = self.dir(wid);
            let p = path.to_owned();
            // the permit goes into the open's thread and comes back with the file: an open that hangs keeps it
            let (opened, permit) = self
                .blocking("open of a workspace file", FS_TIMEOUT, move || {
                    (Self::open_file(&dir, &p), permit)
                })
                .await?;
            let (file, st) = opened?;
            let size = u64::try_from(st.st_size).unwrap_or(0);
            let served = match range {
                Some(r) => Some(r.resolve(size).ok_or_else(|| api(error::range_not_satisfiable(size)))?),
                None => None,
            };
            let mtime_ns = i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec);
            let mtime = Timestamp::from_unix_millis(i64::try_from(mtime_ns / 1_000_000).unwrap_or(0))
                .unwrap_or_else(Timestamp::now);
            let meta = FileMeta {
                size,
                mtime,
                etag: FileMeta::etag_for(size, mtime_ns),
                served,
            };
            let (file, permit) = match served {
                Some((first, _)) => {
                    let (sought, file, permit) = self
                        .blocking("seek in a workspace file", FS_TIMEOUT, move || {
                            use std::io::Seek as _;
                            let mut file = file;
                            (file.seek(io::SeekFrom::Start(first)), file, permit)
                        })
                        .await?;
                    sought.map_err(|e| unavailable(format!("seek: {e}")))?;
                    (file, permit)
                }
                None => (file, permit),
            };
            let len = meta.content_length();
            Ok(FileContent::streamed(meta, stream(file, len, permit)))
        })
    }

    fn clone_workspace(&self, wid: Uuid, from: Uuid) -> Fut<'_, Result<Workspace, WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            if self.created_at(from).await?.is_none() {
                return Err(no_workspace());
            }
            if self.created_at(wid).await?.is_some() {
                return Err(api(error::api(409, codes::EXISTS, "the workspace exists")));
            }
            self.clone_dir(wid, self.dir(from)).await
        })
    }

    fn delete(&self, wid: Uuid) -> Fut<'_, Result<(), WorkspaceError>> {
        Box::pin(async move {
            self.check().await?;
            let dir = self.dir(wid);
            if self.created_at(wid).await?.is_some() {
                let refused = match self.cli.rmr(&dir).await {
                    Ok(()) => None,
                    Err(CliError::PermissionDenied(m)) => Some(m),
                    Err(CliError::Failed(m)) => return Err(unavailable(m)),
                };
                // an entry the ownership fix-up missed leaves the tree in place (15.4)
                let refused = match refused {
                    Some(m) => Some(m),
                    None if self.created_at(wid).await?.is_some() => Some("the tree is still there".into()),
                    None => None,
                };
                if let Some(m) = refused {
                    let path = format!("ws/{wid}");
                    tracing::warn!(path = %path, error = %m, "workspace delete refused");
                    self.health
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .delete_refused = Some((Timestamp::now(), path.clone()));
                    return Err(unavailable(format!("workspace delete refused: {path}: {m}")));
                }
            }
            if let Err(e) = self.cli.quota_delete(&Self::volume_path(wid)).await {
                // a quota left behind on a removed path is harmless (the path is gone; a new ensure sets it again)
                tracing::info!(workspace = %wid, error = %e, "quota delete after rmr");
            }
            self.usage.lock().unwrap_or_else(PoisonError::into_inner).remove(&wid);
            Ok(())
        })
    }

    fn health(&self) -> StoreHealth {
        self.health.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn probe(&self) -> Fut<'_, StoreHealth> {
        Box::pin(async {
            if self.check().await.is_ok() {
                self.maybe_sweep();
            }
            self.health()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "+-------+---------+------+------+--------+-------+-------+
|  Path |   Size  | Used | Use% | Inodes | IUsed | IUse% |
+-------+---------+------+------+--------+-------+-------+
| /ws/a | 1.0 GiB | 1.5 MiB |   0% |  1,000 |     12 |    1% |
+-------+---------+------+------+--------+-------+-------+
";

    struct NoCli;

    impl JfsCli for NoCli {
        fn quota_get<'a>(&'a self, _: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
            Box::pin(async { Ok(None) })
        }
        fn quota_set<'a>(&'a self, _: &'a str, _: u32, _: u64) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn quota_delete<'a>(&'a self, _: &'a str) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn clone_tree<'a>(&'a self, _: &'a Path, _: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn rmr<'a>(&'a self, _: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn temp_store() -> (PathBuf, JuiceFs) {
        let root = std::env::temp_dir().join(format!("hermit-hung-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(root.join("ws")).unwrap();
        let s = JuiceFs::new(&root, Arc::new(NoCli), 1, 1000);
        (root, s)
    }

    fn flight(s: &JuiceFs, started: Instant, answer: BoxFuture<'static, StatAnswer>) -> u64 {
        let id = s.flights.fetch_add(1, Ordering::Relaxed);
        *s.stat.lock().unwrap() = Some(StatFlight {
            id,
            started,
            answer: answer.shared(),
        });
        id
    }

    // a FUSE call that hangs (here: a sleep) is a broken mount after the limit
    #[tokio::test]
    async fn a_hung_filesystem_call_is_a_broken_mount() {
        let (root, s) = temp_store();
        let t = Instant::now();
        let e = s
            .blocking("a test call", Duration::from_millis(100), || {
                std::thread::sleep(Duration::from_millis(400));
            })
            .await
            .unwrap_err();
        assert!(t.elapsed() < Duration::from_millis(300));
        assert!(
            matches!(&e, WorkspaceError::NotMounted(m) if m == "a test call: no answer in 0 s"),
            "{e:?}"
        );
        assert!(s.health().unmounted.is_some());
        let api: hermit_api::error::ApiError = e.into();
        assert_eq!(api.status().as_u16(), 503);
        // a stat older than FS_TIMEOUT that has not answered: the mount is hung, the next check fails at once
        let long_ago = Instant::now().checked_sub(FS_TIMEOUT + Duration::from_secs(1)).unwrap();
        flight(&s, long_ago, futures_util::future::pending().boxed());
        let t = Instant::now();
        assert!(matches!(s.check().await, Err(WorkspaceError::NotMounted(_))));
        assert!(t.elapsed() < Duration::from_millis(200));
        // one that started just now and never answers: the check waits out FS_TIMEOUT, then the mount is hung
        flight(&s, Instant::now(), futures_util::future::pending().boxed());
        let t = Instant::now();
        assert!(matches!(s.check().await, Err(WorkspaceError::NotMounted(_))));
        assert!(t.elapsed() >= FS_TIMEOUT.checked_sub(Duration::from_millis(50)).unwrap());
        *s.stat.lock().unwrap() = None;
        s.check().await.unwrap();
        assert_eq!(s.health().unmounted, None, "a mount that answers again is fine again");
        let _ = std::fs::remove_dir_all(&root);
    }

    // a stat that is simply still running is shared by every check that arrives meanwhile: none fails, and no
    // second stat starts
    #[tokio::test]
    async fn checks_during_a_healthy_stat_share_its_answer() {
        let (root, s) = temp_store();
        let id = flight(
            &s,
            Instant::now(),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(Ok(true))
            }
            .boxed(),
        );
        let t = Instant::now();
        let all = futures_util::future::join_all((0..50).map(|_| s.check())).await;
        assert!(all.iter().all(Result::is_ok), "{all:?}");
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(
            s.stat.lock().unwrap().as_ref().map(|f| f.id),
            Some(id),
            "no second stat was started"
        );
        assert_eq!(s.health().unmounted, None);
        // and real stats run one at a time, shared under concurrency
        *s.stat.lock().unwrap() = None;
        let all = futures_util::future::join_all((0..200).map(|_| s.check())).await;
        assert!(all.iter().all(Result::is_ok), "{all:?}");
        assert_eq!(s.health().unmounted, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // a slow listing is not a hung mount: 503 without marking the mount, the scan is joined by the retry (same
    // cursor), and a listing of the same folder from elsewhere waits its turn
    #[tokio::test]
    async fn a_slow_listing_answers_503_and_leaves_the_mount_alone() {
        let (root, s) = temp_store();
        let wid = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let answer = async move {
            let _ = rx.await;
            (
                Ok(Entries {
                    path: String::new(),
                    entries: vec![],
                    next_cursor: None,
                }),
                Instant::now(),
            )
        }
        .boxed()
        .shared();
        tokio::spawn(answer.clone());
        s.listings.lock().unwrap().insert(
            (wid, String::new()),
            ListFlight {
                id: 99,
                cursor: None,
                answer,
            },
        );
        let e = s.listing(wid, "", Some("b")).await.unwrap_err();
        assert!(
            matches!(&e, WorkspaceError::Unavailable(m) if m.contains("still running")),
            "{e:?}"
        );
        let t = Instant::now();
        let e = s.listing(wid, "", None).await.unwrap_err();
        assert!(t.elapsed() >= LIST_TIMEOUT.checked_sub(Duration::from_millis(50)).unwrap());
        assert!(
            matches!(&e, WorkspaceError::Unavailable(m) if m.contains("goes on")),
            "{e:?}"
        );
        assert_eq!(s.health().unmounted, None, "a slow listing is not a broken mount");
        // the scan finishes with nobody waiting; the retry gets its answer, not a new scan
        tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let page = s.listing(wid, "", None).await.unwrap();
        assert_eq!(page.entries.len(), 0);
        assert!(s.listings.lock().unwrap().is_empty(), "the collected answer is gone");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_clone_trees_are_found_by_the_time_in_their_name() {
        let root = std::env::temp_dir().join(format!("hermit-sweep-{}", Uuid::new_v4().simple()));
        let now = 2_000_000_000;
        let (old, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        for n in [
            format!(
                "{CLONE_PREFIX}{old}-{}-{}",
                now - CLONE_STALE.as_secs() - 1,
                Uuid::new_v4().simple()
            ),
            format!("{CLONE_PREFIX}{fresh}-{}-{}", now - 60, Uuid::new_v4().simple()),
            format!("{CLONE_PREFIX}garbage"),
            format!("{CLONE_PREFIX}garbage-{}", now - CLONE_STALE.as_secs() - 1),
            Uuid::new_v4().to_string(),
        ] {
            std::fs::create_dir_all(root.join(n)).unwrap();
        }
        let found = stale_clones(&root, now).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].to_string_lossy().contains(&old.to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_clone_name_s_time_is_read_from_its_fixed_place() {
        let wid = Uuid::new_v4();
        let r = Uuid::new_v4().simple();
        assert_eq!(clone_started(&format!("{wid}-1700000000-{r}")), Some(1_700_000_000));
        assert_eq!(clone_started(&format!("{wid}-1700000000")), Some(1_700_000_000));
        assert_eq!(clone_started(&format!("{wid}-x-{r}")), None);
        assert_eq!(clone_started("not-a-uuid-at-all-not-a-uuid-at-all1-1700000000"), None);
        assert_eq!(clone_started(""), None);
    }

    /// A CLI that records quota sets and removals.
    #[derive(Default)]
    struct Rec(Mutex<Vec<String>>);
    impl JfsCli for Rec {
        fn quota_get<'a>(&'a self, _: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
            Box::pin(async { Ok(None) })
        }
        fn quota_set<'a>(&'a self, path: &'a str, _: u32, _: u64) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(format!("quota {path}"));
                Ok(())
            })
        }
        fn quota_delete<'a>(&'a self, _: &'a str) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn clone_tree<'a>(&'a self, _: &'a Path, _: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(format!("rmr {}", path.display()));
                Ok(())
            })
        }
    }

    /// A CLI whose `rmr` takes `.1` before it records.
    #[derive(Default)]
    struct Slow(Mutex<Vec<String>>, Duration);
    impl JfsCli for Slow {
        fn quota_get<'a>(&'a self, _: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
            Box::pin(async { Ok(None) })
        }
        fn quota_set<'a>(&'a self, _: &'a str, _: u32, _: u64) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn quota_delete<'a>(&'a self, _: &'a str) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn clone_tree<'a>(&'a self, _: &'a Path, _: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async { Ok(()) })
        }
        fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
            Box::pin(async move {
                tokio::time::sleep(self.1).await;
                self.0.lock().unwrap().push(format!("rmr {}", path.display()));
                Ok(())
            })
        }
    }

    // a one-shot command's store: the removal an unfinished clone left in the background is waited for before the
    // command exits (the runtime would drop it), and one that outlasts the wait is counted
    #[tokio::test]
    async fn settle_waits_for_what_the_store_spawned() {
        let cli = Arc::new(Slow(Mutex::default(), Duration::from_millis(200)));
        let fs = JuiceFs::new("/nonexistent", cli.clone(), 1, 10);
        drop(TempTree {
            cli: cli.clone(),
            path: Some("/jfs/ws/.clone-a".into()),
            bg: fs.background.clone(),
        });
        assert!(cli.0.lock().unwrap().is_empty());
        assert_eq!(fs.settle(Duration::from_secs(10)).await, 0);
        assert_eq!(*cli.0.lock().unwrap(), vec!["rmr /jfs/ws/.clone-a".to_owned()]);

        let stuck = Arc::new(Slow(Mutex::default(), Duration::from_secs(600)));
        let fs = JuiceFs::new("/nonexistent", stuck.clone(), 1, 10);
        drop(TempTree {
            cli: stuck.clone(),
            path: Some("/jfs/ws/.clone-b".into()),
            bg: fs.background.clone(),
        });
        assert_eq!(fs.settle(Duration::from_millis(50)).await, 1);
    }

    // a rename that answered after its request gave up: landed, the workspace gets its quota and its tree is kept;
    // failed, the temporary tree is removed (and never while the rename may still run)
    #[tokio::test]
    async fn a_late_clone_rename_is_settled_when_it_returns() {
        let wid = Uuid::new_v4();
        let rec = Arc::new(Rec::default());
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let rename = tokio::task::spawn_blocking(move || {
            let _ = wait.recv();
            Ok(())
        });
        let settle = tokio::spawn(settle_late_rename(
            rename,
            rec.clone(),
            Arc::default(),
            "/jfs/ws/t".into(),
            wid,
            1,
            10,
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "nothing is done while the rename is blocked"
        );
        go.send(()).unwrap();
        settle.await.unwrap();
        assert_eq!(*rec.0.lock().unwrap(), vec![format!("quota /ws/{wid}")]);

        let rec = Arc::new(Rec::default());
        let rename = tokio::task::spawn_blocking(|| Err(Errno::IO));
        settle_late_rename(rename, rec.clone(), Arc::default(), "/jfs/ws/t".into(), wid, 1, 10).await;
        assert!(
            crate_eventually(|| rec.0.lock().unwrap().clone() == vec!["rmr /jfs/ws/t".to_owned()]).await,
            "{:?}",
            rec.0.lock().unwrap()
        );
    }

    async fn crate_eventually(f: impl Fn() -> bool) -> bool {
        for _ in 0..100 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[test]
    fn quota_table() {
        assert_eq!(
            parse_quota_table(TABLE, "/ws/a"),
            Some(Quota {
                capacity_bytes: 1 << 30,
                inodes: 1000,
                used_bytes: 1_572_864,
                used_inodes: 12
            })
        );
        assert_eq!(parse_quota_table(TABLE, "/ws/b"), None);
        assert_eq!(parse_quota_table("", "/ws/a"), None);
        assert_eq!(parse_size("0 B"), Some(0));
        assert_eq!(parse_size("2.0 KiB"), Some(2048));
        assert_eq!(parse_size("3 parsecs"), None);
    }
}
