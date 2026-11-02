//! Uploads into `/workspace/uploads` (supervisor 12.1). The supervisor is the only writer of uploads: each file and
//! each image original is streamed from magpie's blob into `.upload.part-<rand>` (a fixed-length name, so an upload
//! name of up to 255 bytes still fits beside it; created `O_EXCL|O_NOFOLLOW` as the agent, so the file is the
//! agent's), checked against its sha256, and renamed onto its name. The bytes are written on blocking threads whose
//! filesystem ids are root's: the workspace quota still applies because it is the workspace directory's own
//! (hermit's `JuiceFS` directory quota, hermit 15.5), which counts every writer; a per-uid quota would not bind those
//! writes. The tests inject `EDQUOT` rather than run on a quota-enabled filesystem. A symlinked `/workspace` or `/workspace/uploads` is refused. A blob read that fails in a
//! way a retry may cure (no answer, a reset mid-stream, 429 or 5xx: a magpie or gateway replica draining, spec 19)
//! is retried with the reports' backoff until the start's deadline (inside magpie's 120 s `starting` watchdog),
//! the `.part` file starting again from zero; a 404, a sha mismatch or a refused write fails the turn at once.

use std::ffi::CString;
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};

use std::io::Seek as _;
use std::time::Duration;

use futures_util::StreamExt as _;
use magpie_api::client::ClientError;
use magpie_api::uploads::{SandboxBlobAs, SandboxBlobQuery};
use sha2::Digest as _;
use tokio::time::Instant;

use crate::limits::{REPORT_RETRY_FIRST, REPORT_RETRY_MAX};

/// The name of an upload being written (`.upload.part-<32 hex>`: 45 bytes whatever the upload's name).
pub const PART_PREFIX: &str = ".upload.part-";

/// How long a start retries its blob reads (magpie's `starting` watchdog is 120 s).
pub const BLOB_RETRY_FOR: Duration = Duration::from_secs(90);

/// Whether a retry may cure a failed blob read.
#[must_use]
pub fn retryable(e: &ClientError) -> bool {
    e.is_retryable() || e.api().is_some_and(|r| r.status >= 500 || r.status == 429)
}

/// How a blob read failed.
enum Failure {
    Retry(String),
    Fatal(String),
}

/// Retries `f` on [`Failure::Retry`] until `deadline`.
async fn retrying<T, F, Fut>(what: &str, deadline: Instant, mut f: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Failure>>,
{
    let mut delay = REPORT_RETRY_FIRST;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(Failure::Fatal(e)) => return Err(e),
            Err(Failure::Retry(e)) => {
                if Instant::now() + delay > deadline {
                    return Err(e);
                }
                tracing::info!(blob = what, error = %e, "blob read failed: retried");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(REPORT_RETRY_MAX);
            }
        }
    }
}

fn get_failure(sha: &str, e: &ClientError) -> Failure {
    let m = format!("blob {sha}: {e}");
    if retryable(e) {
        Failure::Retry(m)
    } else {
        Failure::Fatal(m)
    }
}

/// A blob's bytes (the model copy of an image), retried as above until `deadline`.
///
/// # Errors
/// A read no retry cured, or one that cannot succeed.
pub async fn blob(
    magpie: &magpie_api::client::SandboxCalls<'_>,
    sha: &str,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    retrying(sha, deadline, || async {
        let res = magpie
            .sandbox_blob_get(
                sha,
                &SandboxBlobQuery {
                    as_: Some(SandboxBlobAs::Raw),
                },
            )
            .await
            .map_err(|e| get_failure(sha, &e))?;
        let b = res
            .bytes()
            .await
            .map_err(|e| Failure::Retry(format!("blob {sha}: {e}")))?;
        Ok(b.to_vec())
    })
    .await
}

/// The message a full workspace fails the turn with.
pub const NO_SPACE: &str = "the attachment does not fit in the workspace";

/// One file to write: `uploads/<name>` (relative to the workspace) from blob `sha`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub workspace_path: String,
    pub sha: String,
}

/// Where uploads go, and as whom.
#[derive(Clone, Debug)]
pub struct Uploads {
    pub workspace: PathBuf,
    pub uid: u32,
    pub gid: u32,
}

#[cfg(test)]
pub(crate) static FAIL_WRITES_WITH: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

fn errno() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn reason(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EDQUOT | libc::ENOSPC) => NO_SPACE.to_owned(),
        _ => e.to_string(),
    }
}

/// Runs `f` with this thread's filesystem ids set to `uid`/`gid` (only effective as root), then restores them.
fn as_agent<T>(uid: u32, gid: u32, f: impl FnOnce() -> T) -> T {
    // SAFETY: setfsuid/setfsgid change this thread's filesystem ids only; no memory effects.
    #[allow(unsafe_code)]
    let (pu, pg) = unsafe { (libc::setfsuid(uid), libc::setfsgid(gid)) };
    let r = f();
    // SAFETY: as above, restoring the previous ids.
    #[allow(unsafe_code)]
    unsafe {
        libc::setfsgid(u32::try_from(pg).unwrap_or(0));
        libc::setfsuid(u32::try_from(pu).unwrap_or(0));
    }
    r
}

fn cstr(s: &str) -> std::io::Result<CString> {
    CString::new(s).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

/// `uploads/<name>` with one safe component.
fn upload_name(workspace_path: &str) -> Option<&str> {
    let name = workspace_path.strip_prefix("uploads/")?;
    let ok = !name.is_empty()
        && name.len() <= magpie_api::sandbox::WORKSPACE_NAME_MAX_BYTES
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\0');
    ok.then_some(name)
}

impl Uploads {
    /// The sandbox's.
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            workspace: crate::cc::launch::WORKSPACE.into(),
            uid: crate::cc::launch::AGENT_UID,
            gid: crate::cc::launch::AGENT_GID,
        }
    }

    /// The `uploads` directory, opened without following a symlink (created as the agent when missing).
    fn open_dir(&self) -> Result<OwnedFd, String> {
        let ws = std::fs::symlink_metadata(&self.workspace).map_err(|e| format!("workspace: {e}"))?;
        if ws.file_type().is_symlink() || !ws.is_dir() {
            return Err("the workspace is not a directory".into());
        }
        let dir = self.workspace.join("uploads");
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err("/workspace/uploads is not a directory".into());
            }
            Ok(_) => {}
            Err(_) => {
                let c = cstr(&dir.to_string_lossy()).map_err(|e| e.to_string())?;
                let r = as_agent(self.uid, self.gid, || {
                    // SAFETY: a valid C string; mkdir has no memory effects.
                    #[allow(unsafe_code)]
                    unsafe {
                        libc::mkdir(c.as_ptr(), 0o755)
                    }
                });
                if r != 0 {
                    let e = errno();
                    if e.raw_os_error() != Some(libc::EEXIST) {
                        return Err(reason(&e));
                    }
                }
            }
        }
        let ws_c = cstr(&self.workspace.to_string_lossy()).map_err(|e| e.to_string())?;
        // SAFETY: valid C strings; the fds are checked and owned below.
        #[allow(unsafe_code)]
        unsafe {
            let wfd = libc::open(
                ws_c.as_ptr(),
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if wfd < 0 {
                return Err(format!("workspace: {}", errno()));
            }
            let wfd = OwnedFd::from_raw_fd(wfd);
            let ufd = libc::openat(
                wfd.as_raw_fd(),
                c"uploads".as_ptr(),
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if ufd < 0 {
                return Err("/workspace/uploads is not a directory".into());
            }
            Ok(OwnedFd::from_raw_fd(ufd))
        }
    }

    fn create_part(&self, dir: &OwnedFd, part: &str) -> std::io::Result<std::fs::File> {
        let c = cstr(part)?;
        let fd = as_agent(self.uid, self.gid, || {
            // SAFETY: a valid dirfd and C string.
            #[allow(unsafe_code)]
            unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_WRONLY | libc::O_CLOEXEC,
                    0o644,
                )
            }
        });
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: a new fd owned by nothing else.
        #[allow(unsafe_code)]
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn rename(&self, dir: &OwnedFd, from: &str, to: &str) -> std::io::Result<()> {
        let (f, t) = (cstr(from)?, cstr(to)?);
        let r = as_agent(self.uid, self.gid, || {
            // SAFETY: a valid dirfd and C strings.
            #[allow(unsafe_code)]
            unsafe {
                libc::renameat(dir.as_raw_fd(), f.as_ptr(), dir.as_raw_fd(), t.as_ptr())
            }
        });
        if r == 0 { Ok(()) } else { Err(errno()) }
    }

    fn unlink(dir: &OwnedFd, name: &str) {
        if let Ok(c) = cstr(name) {
            // SAFETY: a valid dirfd and C string.
            #[allow(unsafe_code)]
            unsafe {
                libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0);
            }
        }
    }

    /// Removes the `.part` files a preparation left behind (one stopped mid-write; the supervisor is the only writer
    /// of uploads and prepares one turn at a time).
    pub async fn sweep(&self) {
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if std::fs::symlink_metadata(me.workspace.join("uploads")).is_err() {
                return;
            }
            let Ok(dir) = me.open_dir() else { return };
            let Ok(rd) = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())) else {
                return;
            };
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str()
                    && n.starts_with(PART_PREFIX)
                {
                    Self::unlink(&dir, n);
                }
            }
        })
        .await;
    }

    /// Writes every item; the absolute paths. `Err` is the turn's `run.error` message.
    ///
    /// # Errors
    /// The first item that could not be written.
    pub async fn materialise(
        &self,
        magpie: &magpie_api::client::SandboxCalls<'_>,
        items: &[Item],
        deadline: Instant,
    ) -> Result<Vec<String>, String> {
        if !items.is_empty() {
            self.sweep().await;
        }
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(self.one(magpie, it, deadline).await?);
        }
        Ok(out)
    }

    /// One attempt: the blob streamed into `file` (emptied first), checked against its sha.
    async fn fill(
        magpie: &magpie_api::client::SandboxCalls<'_>,
        it: &Item,
        file: &std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    ) -> Result<(), Failure> {
        let f = file.clone();
        tokio::task::spawn_blocking(move || {
            let mut f = f.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.set_len(0)?;
            f.rewind()
        })
        .await
        .map_err(|e| Failure::Fatal(e.to_string()))?
        .map_err(|e| Failure::Fatal(reason(&e)))?;
        let res = magpie
            .sandbox_blob_get(
                &it.sha,
                &SandboxBlobQuery {
                    as_: Some(SandboxBlobAs::Raw),
                },
            )
            .await
            .map_err(|e| get_failure(&it.sha, &e))?;
        let mut hash = sha2::Sha256::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Failure::Retry(format!("blob {}: {e}", it.sha)))?;
            hash.update(&chunk);
            let f = file.clone();
            tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                {
                    let e = FAIL_WRITES_WITH.load(std::sync::atomic::Ordering::SeqCst);
                    if e != 0 {
                        return Err(std::io::Error::from_raw_os_error(e));
                    }
                }
                f.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .write_all(&chunk)
            })
            .await
            .map_err(|e| Failure::Fatal(e.to_string()))?
            .map_err(|e| Failure::Fatal(reason(&e)))?;
        }
        let got = crate::sent_files::hex(&hash.finalize());
        if got != it.sha {
            return Err(Failure::Fatal(format!("blob {} arrived as {got}", it.sha)));
        }
        Ok(())
    }

    async fn one(
        &self,
        magpie: &magpie_api::client::SandboxCalls<'_>,
        it: &Item,
        deadline: Instant,
    ) -> Result<String, String> {
        let name = upload_name(&it.workspace_path)
            .ok_or_else(|| format!("bad workspace path {:?}", it.workspace_path))?
            .to_owned();
        let me = self.clone();
        let dir = tokio::task::spawn_blocking(move || me.open_dir())
            .await
            .map_err(|e| e.to_string())??;
        let dir = std::sync::Arc::new(dir);
        let part = format!("{PART_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let me = self.clone();
        let (d, p) = (dir.clone(), part.clone());
        let file = tokio::task::spawn_blocking(move || me.create_part(&d, &p))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| reason(&e))?;
        let file = std::sync::Arc::new(std::sync::Mutex::new(file));
        let written = retrying(&it.sha, deadline, || Self::fill(magpie, it, &file)).await;
        drop(file);
        let me = self.clone();
        let d = dir.clone();
        let p = part.clone();
        let n = name.clone();
        let r = tokio::task::spawn_blocking(move || match written {
            Ok(()) => me
                .rename(&d, &p, &n)
                .map_err(|e| reason(&e))
                .inspect_err(|_| Self::unlink(&d, &p)),
            Err(e) => {
                Self::unlink(&d, &p);
                Err(e)
            }
        })
        .await
        .map_err(|e| e.to_string())?;
        r?;
        Ok(Path::new(&self.workspace)
            .join("uploads")
            .join(&name)
            .to_string_lossy()
            .into_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(upload_name("uploads/Q3 report.pdf"), Some("Q3 report.pdf"));
        for bad in [
            "uploads/",
            "uploads/..",
            "uploads/a/b",
            "x/a",
            "/workspace/uploads/a",
            "uploads/a\0",
        ] {
            assert_eq!(upload_name(bad), None, "{bad}");
        }
        // a name of one full directory entry (85 CJK characters) is accepted; one byte more is not
        let full = format!("uploads/{}", "港".repeat(85));
        assert_eq!(upload_name(&full).map(str::len), Some(255));
        assert_eq!(upload_name(&format!("{full}x")), None);
        assert_eq!(reason(&std::io::Error::from_raw_os_error(libc::EDQUOT)), NO_SPACE);
        assert_eq!(reason(&std::io::Error::from_raw_os_error(libc::ENOSPC)), NO_SPACE);
    }

    #[test]
    fn a_symlinked_uploads_or_workspace_is_refused() {
        let d = std::env::temp_dir().join(format!("uploads-{}", uuid::Uuid::new_v4().simple()));
        let ws = d.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(d.join("elsewhere")).unwrap();
        // SAFETY: getuid/getgid have no memory effects.
        #[allow(unsafe_code)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let u = Uploads {
            workspace: ws.clone(),
            uid,
            gid,
        };
        assert!(u.open_dir().is_ok(), "created when missing");
        std::fs::remove_dir(ws.join("uploads")).unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere"), ws.join("uploads")).unwrap();
        assert!(u.open_dir().is_err());
        let link = Uploads {
            workspace: d.join("wslink"),
            uid,
            gid,
        };
        std::os::unix::fs::symlink(&ws, d.join("wslink")).unwrap();
        assert!(link.open_dir().is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
