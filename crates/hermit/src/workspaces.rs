//! `/workspace` on `JuiceFS` (hermit 3.8, 15): the YYC implementation of [`Workspaces`]. hermit's own mount of the
//! volume is `/jfs` (`all-squash=1000:1000`, so everything it makes is owned by the sandbox user); a workspace is
//! the folder `ws/<id>` with a directory quota. Quotas go through the `juicefs` CLI against the metadata database;
//! `clone` and `rmr` through the CLI on the mount; listing and reading are plain syscalls on the mount, every path
//! resolved with `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the workspace's folder so a symlink the
//! agent made never leads outside it. An absolute symlink is not followed even when it names a path inside the
//! workspace as the sandbox sees it (`/workspace/out/x`): `RESOLVE_BENEATH` refuses every absolute link, and such a
//! path answers 400 `bad_path` (a known gap of 3.8, recorded for the spec owner).
//!
//! Every filesystem call has a time limit ([`FS_TIMEOUT`], [`LIST_TIMEOUT`], [`READ_TIMEOUT`]): a FUSE mount that
//! hangs instead of failing is a broken mount (503 `workspace_unavailable`, retry-after 2, and a start hands over),
//! never a request that waits for ever. At most one `stat` of the mount is outstanding per replica.
//!
//! The CLI is behind [`JfsCli`] so the tests run the store on a plain directory with an in-memory quota table; no
//! configuration selects it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use hermit_api::Timestamp;
use hermit_api::error::{self, codes};
use hermit_api::workspaces::{
    Amount, ByteRange, ENTRIES_PAGE, Entries, Entry, EntryType, FileContent, FileMeta, Workspace,
};
use rustix::fs::{FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use tokio::sync::Semaphore;
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
/// One page of a listing (`readdir` and a `stat` per entry).
pub const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// One 64 KiB chunk of a file read (a cold chunk comes from the object store).
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
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
    /// The destination exists (`clone` to a path another clone made meanwhile).
    #[error("exists: {0}")]
    Exists(String),
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
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            Err(CliError::PermissionDenied(msg))
        } else if lower.contains("file exists") || lower.contains("already exists") {
            Err(CliError::Exists(msg))
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
    /// A `stat` of the mount has not answered yet (a hung FUSE mount keeps its thread; no second one is started).
    stat_pending: Arc<AtomicBool>,
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
            stat_pending: Arc::new(AtomicBool::new(false)),
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
    /// starts hand over. While a `stat` hangs, the next checks fail at once instead of starting another.
    async fn check(&self) -> Result<(), WorkspaceError> {
        let root = self.ws_root();
        if self.stat_pending.swap(true, Ordering::SeqCst) {
            return Err(self.not_mounted(format!("{}: the last stat has not answered", root.display())));
        }
        let pending = self.stat_pending.clone();
        let r = root.clone();
        let res = self
            .blocking("stat of the workspace store", FS_TIMEOUT, move || {
                let m = std::fs::metadata(&r);
                pending.store(false, Ordering::SeqCst);
                m
            })
            .await?;
        match res {
            Ok(m) if m.is_dir() => {
                self.health.lock().unwrap_or_else(PoisonError::into_inner).unmounted = None;
                Ok(())
            }
            Ok(_) => Err(self.not_mounted("ws is not a directory".into())),
            Err(e) => Err(self.not_mounted(format!("{}: {e}", root.display()))),
        }
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
        let mut all: Vec<(Vec<u8>, Entry)> = Vec::new();
        let d = rustix::fs::Dir::read_from(&fd).map_err(|e| path_error(e, true))?;
        for item in d {
            let item = item.map_err(|e| unavailable(format!("readdir: {e}")))?;
            let name = item.file_name().to_bytes().to_vec();
            if name == b"." || name == b".." {
                continue;
            }
            if cursor.is_some_and(|c| name.as_slice() <= c.as_bytes()) {
                continue;
            }
            // an entry removed between readdir and stat is left out
            let Ok(st) = rustix::fs::statat(&fd, item.file_name(), rustix::fs::AtFlags::SYMLINK_NOFOLLOW) else {
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
            all.push((
                name.clone(),
                Entry {
                    name: String::from_utf8_lossy(&name).into_owned(),
                    kind,
                    size: if kind == EntryType::Dir {
                        0
                    } else {
                        u64::try_from(st.st_size).unwrap_or(0)
                    },
                    mtime,
                },
            ));
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let more = all.len() > ENTRIES_PAGE;
        all.truncate(ENTRIES_PAGE);
        let next_cursor = more.then(|| all.last().map(|(_, e)| e.name.clone())).flatten();
        Ok(Entries {
            path: path.to_owned(),
            entries: all.into_iter().map(|(_, e)| e).collect(),
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

/// Streams `len` bytes of `file` from its current offset in 64 KiB chunks, holding `permit` until the end.
fn stream(file: tokio::fs::File, len: u64, permit: tokio::sync::OwnedSemaphorePermit) -> axum::body::Body {
    use tokio::io::AsyncReadExt as _;
    let s = futures_util::stream::unfold(
        (file, len, permit, vec![0u8; CHUNK]),
        |(mut file, left, permit, mut buf)| async move {
            if left == 0 {
                return None;
            }
            let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            let read = tokio::time::timeout(READ_TIMEOUT, file.read(&mut buf[..want]))
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "workspace read: no answer in 30 s",
                    ))
                });
            match read {
                Ok(0) => None,
                Ok(n) => {
                    let chunk = axum::body::Bytes::copy_from_slice(&buf[..n]);
                    Some((Ok::<_, io::Error>(chunk), (file, left - n as u64, permit, buf)))
                }
                Err(e) => Some((Err(e), (file, 0, permit, buf))),
            }
        },
    );
    axum::body::Body::from_stream(s)
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
            let dir = self.dir(wid);
            let (path, cursor) = (path.to_owned(), cursor.map(str::to_owned));
            self.blocking("a workspace listing", LIST_TIMEOUT, move || {
                Self::list(&dir, &path, cursor.as_deref())
            })
            .await?
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
            let permit = self
                .reads
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            let dir = self.dir(wid);
            let p = path.to_owned();
            let (file, st) = self
                .blocking("open of a workspace file", FS_TIMEOUT, move || {
                    Self::open_file(&dir, &p)
                })
                .await??;
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
            let mut file = tokio::fs::File::from_std(file);
            if let Some((first, _)) = served {
                use tokio::io::AsyncSeekExt as _;
                tokio::time::timeout(FS_TIMEOUT, file.seek(io::SeekFrom::Start(first)))
                    .await
                    .map_err(|_| self.not_mounted("seek in a workspace file: no answer in 2 s".into()))?
                    .map_err(|e| unavailable(format!("seek: {e}")))?;
            }
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
            // the check above and the clone are not one step: a clone that made the target meanwhile wins, and this
            // one answers what it would have answered a moment later
            self.cli
                .clone_tree(&self.dir(from), &self.dir(wid))
                .await
                .map_err(|e| match e {
                    CliError::Exists(_) => api(error::api(409, codes::EXISTS, "the workspace exists")),
                    other => unavailable(other.to_string()),
                })?;
            // the copy gets its own quota (a directory quota is not copied with the tree)
            self.set_quota(wid).await?;
            let created = self.created_at(wid).await?.unwrap_or_else(Timestamp::now);
            self.workspace(wid, created, false).await
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
                    Err(CliError::Failed(m) | CliError::Exists(m)) => return Err(unavailable(m)),
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
            let _ = self.check().await;
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

    // a FUSE call that hangs (here: a sleep) is a broken mount after the limit, and a stat still outstanding is
    // not joined by another
    #[tokio::test]
    async fn a_hung_filesystem_call_is_a_broken_mount() {
        let root = std::env::temp_dir().join(format!("hermit-hung-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(root.join("ws")).unwrap();
        let s = JuiceFs::new(&root, Arc::new(NoCli), 1, 1000);
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
        // a stat that has not answered: the next check fails at once
        s.stat_pending.store(true, Ordering::SeqCst);
        assert!(matches!(s.check().await, Err(WorkspaceError::NotMounted(_))));
        s.stat_pending.store(false, Ordering::SeqCst);
        s.check().await.unwrap();
        assert_eq!(s.health().unmounted, None, "a mount that answers again is fine again");
        let _ = std::fs::remove_dir_all(&root);
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
