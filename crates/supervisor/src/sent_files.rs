//! The `SendUserFile` hook (supervisor 13): sent-file snapshots.
//!
//! - The hook process (`supervisor hook send-user-file`, run by Claude Code as the agent with the hook JSON on
//!   stdin) resolves each path beneath `/workspace` without following a symlink, opens each file as itself, and
//!   sends `{toolUseId, caption, files}` with the open descriptors (`SCM_RIGHTS`, at most 100) to `agent.sock`
//!   `POST /v1/hooks/send-user-file`. Exit 0 on 200; otherwise today's text on stderr and exit 2, which blocks the
//!   tool so the model knows.
//! - The supervisor hashes each descriptor, stages the send with magpie (`POST /v1/sandbox/sent-files` →
//!   `missing`), streams each missing blob from offset 0 (a `sha_mismatch`, the file changed between the passes:
//!   one full retry, then "the file changed while it was being sent"), and commits, all within 110 s. A file over
//!   64 MiB is refused by its size before it is read; 5xx, 429, `conversation_migrating` and a lease the filesystem
//!   re-took are retried within the budget.
//! - [`serve_with_fds`] is `agent.sock`'s acceptor: the first read of each connection is a `recvmsg` whose
//!   descriptors become the request's [`ReceivedFds`].

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::Router;
use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::{SENT_FILE_MAX_BYTES, SENT_FILES_MAX, SENT_TOTAL_MAX_BYTES, SentFileSpec, SentFilesRequest};
use serde_json::Value;
use sha2::Digest as _;
use supervisor_api::local::{
    HOOK_FAILED_EXIT, HOOK_WAIT, MAX_FDS, ReceivedFds, SEND_USER_FILE, SendUserFileReply, SendUserFileRequest,
    SentFile, hook_failure_text,
};
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf};

use crate::cc::report::Reporter;
use crate::limits::{HOOK_BUDGET, REPORT_RETRY_FIRST, REPORT_RETRY_MAX};

/// The hash pass's buffer.
const HASH_BUFFER: usize = 256 * 1024;
/// The workspace as the agent names it.
pub const WORKSPACE: &str = "/workspace";

/// Lowercase hex.
#[must_use]
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

// ---- the supervisor's side ----

fn pread(fd: RawFd, buf: &mut [u8], at: u64) -> io::Result<usize> {
    // SAFETY: reading into a local buffer from an fd we own.
    #[allow(unsafe_code)]
    let n = unsafe {
        libc::pread(
            fd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            i64::try_from(at).unwrap_or(i64::MAX),
        )
    };
    usize::try_from(n).map_err(|_| io::Error::last_os_error())
}

/// Size and sha256 of a regular file's descriptor, read from offset 0.
/// The size and sha256 of the regular file `fd`; `None` when it is (or grows while it is read) over `max` bytes,
/// told by its size before a byte is read.
fn hash_fd(fd: &OwnedFd, max: u64) -> io::Result<Option<(u64, String)>> {
    let f = fd.as_raw_fd();
    // SAFETY: fstat into a local struct.
    #[allow(unsafe_code)]
    let st = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(f, &raw mut st) != 0 {
            return Err(io::Error::last_os_error());
        }
        st
    };
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(io::Error::other("not a regular file"));
    }
    if u64::try_from(st.st_size).unwrap_or(u64::MAX) > max {
        return Ok(None);
    }
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; HASH_BUFFER];
    let mut at = 0u64;
    loop {
        let n = pread(f, &mut buf, at)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        at += n as u64;
        if at > max {
            return Ok(None);
        }
    }
    Ok(Some((at, hex(&h.finalize()))))
}

/// The bytes of `fd` from offset 0 as a request body, read as the request goes.
fn body_of(fd: Arc<OwnedFd>) -> reqwest::Body {
    let stream = futures_util::stream::unfold((fd, 0u64, false), |(fd, at, done)| async move {
        if done {
            return None;
        }
        let f = fd.clone();
        let r = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; HASH_BUFFER];
            pread(f.as_raw_fd(), &mut buf, at).map(|n| {
                buf.truncate(n);
                buf
            })
        })
        .await
        .unwrap_or_else(|e| Err(io::Error::other(e.to_string())));
        match r {
            Ok(b) if b.is_empty() => None,
            Ok(b) => {
                let n = b.len() as u64;
                Some((Ok::<_, io::Error>(hyper::body::Bytes::from(b)), (fd, at + n, false)))
            }
            Err(e) => Some((Err(e), (fd, at, true))),
        }
    });
    reqwest::Body::wrap_stream(stream)
}

/// Whether (and after how long) a refused call is tried again within the hook's budget: 5xx, transport errors,
/// 429 and `409 conversation_migrating` (a migration or a replica draining), as the reports are; `lease_lost` when the
/// lease changed meanwhile (the filesystem re-took it).
/// A retry, after at least the wait magpie named (if any).
struct Again(Option<Duration>);

fn again(e: &magpie_api::client::ClientError, lease_changed: bool) -> Option<Again> {
    let retry = match e.code() {
        Some(ErrorCode::ConversationMigrating | ErrorCode::RateLimited) => true,
        Some(ErrorCode::LeaseLost) => lease_changed,
        _ => e.is_retryable() || e.api().is_some_and(|r| r.status >= 500 || r.status == 429),
    };
    retry.then(|| Again(crate::cc::report::retry_after(e)))
}

/// One staged-and-committed send; `Err(true)` when a blob arrived with another sha (the file changed).
async fn attempt(reporter: &Reporter, req: &SendUserFileRequest, fds: &[Arc<OwnedFd>]) -> Result<u32, (bool, String)> {
    let mut specs = Vec::with_capacity(fds.len());
    let mut total = 0u64;
    for (f, fd) in req.files.iter().zip(fds) {
        let d = fd.clone();
        let hashed = tokio::task::spawn_blocking(move || hash_fd(&d, SENT_FILE_MAX_BYTES))
            .await
            .map_err(|e| (false, e.to_string()))?
            .map_err(|e| (false, format!("{}: {e}", f.path)))?;
        let Some((size, sha)) = hashed else {
            return Err((false, format!("{} is over {} MiB", f.path, SENT_FILE_MAX_BYTES >> 20)));
        };
        total += size;
        specs.push(SentFileSpec {
            path: f.path.clone(),
            size,
            sha256: sha,
            mime_type: magpie_api::mime::from_name(&f.path).to_owned(),
        });
    }
    if total > SENT_TOTAL_MAX_BYTES {
        return Err((
            false,
            format!("the files are over {} MiB together", SENT_TOTAL_MAX_BYTES >> 20),
        ));
    }
    let staged = SentFilesRequest {
        tool_use_id: req.tool_use_id.clone(),
        caption: req.caption.clone(),
        files: specs.clone(),
    };
    let missing = retrying(reporter, || async { reporter.calls().sent_files(&staged).await })
        .await
        .map_err(|e| (false, e.to_string()))?
        .missing;
    for sha in missing {
        let Some(i) = specs.iter().position(|s| s.sha256 == sha) else {
            continue;
        };
        let fd = fds[i].clone();
        let r = retrying(reporter, || async {
            reporter.calls().sandbox_blob_put(&sha, body_of(fd.clone())).await
        })
        .await;
        match r {
            Ok(_) => {}
            Err(e) if e.code() == Some(&ErrorCode::ShaMismatch) => return Err((true, e.to_string())),
            Err(e) => return Err((false, e.to_string())),
        }
    }
    let done = retrying(reporter, || async {
        reporter.calls().sent_files_commit(&req.tool_use_id).await
    })
    .await
    .map_err(|e| (false, e.to_string()))?;
    Ok(done.kept)
}

/// `f` until it succeeds or fails in a way no retry cures ([`again`]); the hook's budget bounds it.
async fn retrying<T, F, Fut>(reporter: &Reporter, mut f: F) -> Result<T, magpie_api::client::ClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, magpie_api::client::ClientError>>,
{
    let mut delay = REPORT_RETRY_FIRST;
    loop {
        let used = reporter.lease();
        match f().await {
            Err(e) => match again(&e, reporter.lease() != used) {
                Some(Again(after)) => {
                    tracing::info!(error = %e, "sent files: magpie refused, retried");
                    tokio::time::sleep(after.map_or(delay, |a| a.max(delay))).await;
                    delay = (delay * 2).min(REPORT_RETRY_MAX);
                }
                None => return Err(e),
            },
            ok => return ok,
        }
    }
}

/// Stores the hook's files with magpie (13; magpie 11.2). `Err` is the reason the hook prints.
///
/// # Errors
/// Why the files could not be kept.
pub async fn send(
    reporter: &Reporter,
    req: SendUserFileRequest,
    fds: Vec<OwnedFd>,
) -> Result<SendUserFileReply, String> {
    if req.files.is_empty() {
        return Err("no files".into());
    }
    if req.files.len() > SENT_FILES_MAX {
        return Err(format!("more than {SENT_FILES_MAX} files"));
    }
    if fds.len() != req.files.len() {
        return Err(format!("{} files but {} descriptors", req.files.len(), fds.len()));
    }
    let fds: Vec<Arc<OwnedFd>> = fds.into_iter().map(Arc::new).collect();
    tracing::info!(tool_use = %req.tool_use_id, files = req.files.len(), "sent files");
    let run = async {
        match attempt(reporter, &req, &fds).await {
            Ok(n) => Ok(n),
            Err((true, _)) => match attempt(reporter, &req, &fds).await {
                Ok(n) => Ok(n),
                Err((true, _)) => Err("the file changed while it was being sent".to_owned()),
                Err((false, e)) => Err(e),
            },
            Err((false, e)) => Err(e),
        }
    };
    match tokio::time::timeout(HOOK_BUDGET, run).await {
        Ok(Ok(sent)) => Ok(SendUserFileReply { sent }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("magpie did not store them in time".into()),
    }
}

// ---- agent.sock's acceptor ----

/// Receives up to `buf.len()` bytes and every `SCM_RIGHTS` descriptor of the message (close-on-exec).
fn recv_with_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    // room for MAX_FDS descriptors, aligned
    let mut cbuf = vec![0u64; (MAX_FDS * 4 + 64) / 8 + 8];
    let mut fds = Vec::new();
    // SAFETY: recvmsg into `buf` and the local control buffer; the control messages are walked with the libc CMSG
    // macros within `msg_controllen`, and every received fd is owned (and so closed) by `fds`.
    #[allow(
        unsafe_code,
        clippy::cast_possible_truncation,
        clippy::cast_ptr_alignment,
        clippy::cast_sign_loss
    )]
    let n = unsafe {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &raw mut iov;
        m.msg_iovlen = 1;
        m.msg_control = cbuf.as_mut_ptr().cast();
        m.msg_controllen = (cbuf.len() * 8) as _;
        let n = libc::recvmsg(sock, &raw mut m, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut c = libc::CMSG_FIRSTHDR(&raw const m);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c).cast::<i32>();
                let len = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for i in 0..len / 4 {
                    let fd = std::ptr::read_unaligned(data.add(i));
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            c = libc::CMSG_NXTHDR(&raw const m, c);
        }
        n as usize
    };
    Ok((n, fds))
}

/// A connection of `agent.sock`: its first read takes the descriptors sent with the request.
#[derive(Debug)]
pub struct FdStream {
    inner: tokio::net::UnixStream,
    first: bool,
    fds: ReceivedFds,
}

impl AsyncRead for FdStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.first {
            let raw = me.inner.as_raw_fd();
            loop {
                ready!(me.inner.poll_read_ready(cx))?;
                let unfilled = buf.initialize_unfilled();
                match me.inner.try_io(Interest::READABLE, || recv_with_fds(raw, unfilled)) {
                    Ok((n, fds)) => {
                        me.first = false;
                        me.fds.put(fds);
                        buf.advance(n);
                        return Poll::Ready(Ok(()));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }
        }
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for FdStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// `agent.sock`'s listener.
#[derive(Debug)]
pub struct FdListener(tokio::net::UnixListener);

impl axum::serve::Listener for FdListener {
    type Io = FdStream;
    type Addr = ReceivedFds;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((s, _)) => {
                    let fds = ReceivedFds::default();
                    return (
                        FdStream {
                            inner: s,
                            first: true,
                            fds: fds.clone(),
                        },
                        fds,
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "agent socket accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(ReceivedFds::default())
    }
}

/// The connection's descriptors as connect info.
#[derive(Clone, Debug)]
struct Fds(ReceivedFds);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, FdListener>> for Fds {
    fn connect_info(stream: axum::serve::IncomingStream<'_, FdListener>) -> Self {
        Fds(stream.remote_addr().clone())
    }
}

async fn attach_fds(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if let Some(axum::extract::ConnectInfo(Fds(f))) = req.extensions().get::<axum::extract::ConnectInfo<Fds>>().cloned()
    {
        req.extensions_mut().insert(f);
    }
    next.run(req).await
}

/// Serves `router` on a Unix socket at `path` with `mode` (an old socket removed first); each request carries the
/// descriptors sent with its connection's first message.
///
/// # Errors
/// The bind error.
pub fn serve_with_fds(path: &Path, mode: u32, router: Router) -> io::Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    let app = router.layer(axum::middleware::from_fn(attach_fds));
    Ok(tokio::spawn(async move {
        let _ = axum::serve(FdListener(listener), app.into_make_service_with_connect_info::<Fds>()).await;
    }))
}

// ---- the hook process ----

/// The workspace as the hook sees it: `root` is where `/workspace` (the logical prefix) really is.
#[derive(Clone, Debug)]
pub struct HookPaths {
    pub root: PathBuf,
    pub socket: PathBuf,
}

impl HookPaths {
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            root: WORKSPACE.into(),
            socket: supervisor_api::local::AGENT_SOCKET.into(),
        }
    }
}

/// What the hook decided.
#[derive(Debug, PartialEq, Eq)]
pub enum HookOutcome {
    /// Not a send (exit 0, no output).
    Skip,
    Sent(u32),
    /// Exit 2 with the reason in today's text.
    Failed(String),
}

/// `path` (relative to `cwd`, `file://` stripped) normalised and under `/workspace`: its components below it.
fn workspace_components(cwd: &str, path: &str) -> Result<Vec<String>, String> {
    let p = path.strip_prefix("file://").unwrap_or(path);
    let joined = if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        Path::new(cwd).join(p)
    };
    let mut parts: Vec<String> = Vec::new();
    for c in joined.components() {
        match c {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
        }
    }
    if parts.first().map(String::as_str) != Some("workspace") || parts.len() < 2 {
        return Err(format!("{path} is not under /workspace"));
    }
    Ok(parts.split_off(1))
}

fn openat(dir: RawFd, name: &str, flags: libc::c_int) -> io::Result<OwnedFd> {
    let c = std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: a valid dirfd and C string; the new fd is owned below.
    #[allow(unsafe_code)]
    let fd = unsafe { libc::openat(dir, c.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new fd owned by nothing else.
    #[allow(unsafe_code)]
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens the regular file `components` beneath `root`, one component at a time, never following a symlink (the
/// same guarantee as `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`, which some sandbox runtimes do not offer);
/// `root` itself must not be a symlink. The file is opened non-blocking and refused unless it is a regular file, so
/// a FIFO or device cannot hold the hook in `open(2)`.
fn open_beneath(root: &Path, components: &[String]) -> io::Result<OwnedFd> {
    let rc = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: a valid C string; the fd is owned below.
    #[allow(unsafe_code)]
    let fd = unsafe {
        libc::open(
            rc.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new fd owned by nothing else.
    #[allow(unsafe_code)]
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    let (last, dirs) = components
        .split_last()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    for d in dirs {
        dir = openat(dir.as_raw_fd(), d, libc::O_DIRECTORY | libc::O_RDONLY)?;
    }
    let f = openat(dir.as_raw_fd(), last, libc::O_RDONLY | libc::O_NONBLOCK)?;
    // SAFETY: fstat into a local struct, fcntl on an fd we own.
    #[allow(unsafe_code)]
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(f.as_raw_fd(), &raw mut st) != 0 {
            return Err(io::Error::last_os_error());
        }
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let fl = libc::fcntl(f.as_raw_fd(), libc::F_GETFL);
        if fl < 0 || libc::fcntl(f.as_raw_fd(), libc::F_SETFL, fl & !libc::O_NONBLOCK) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(f)
}

/// Sends one HTTP request with `fds` on its first message over the socket at `path`; the status and body.
fn post_with_fds(path: &Path, route: &str, body: &[u8], fds: &[OwnedFd]) -> io::Result<(u16, Vec<u8>)> {
    let mut s = std::os::unix::net::UnixStream::connect(path)?;
    s.set_read_timeout(Some(HOOK_WAIT))?;
    s.set_write_timeout(Some(HOOK_WAIT))?;
    let mut msg = format!(
        "POST {route} HTTP/1.1\r\nhost: supervisor\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    msg.extend_from_slice(body);
    let raw: Vec<RawFd> = fds.iter().map(std::os::fd::AsRawFd::as_raw_fd).collect();
    let mut cbuf = vec![0u64; (raw.len() * 4 + 64) / 8 + 8];
    // SAFETY: the iovec points into `msg` and the control buffer is a local, large enough for CMSG_SPACE of every
    // fd, filled with the libc CMSG macros.
    #[allow(unsafe_code, clippy::cast_possible_truncation)]
    let sent = unsafe {
        let mut iov = libc::iovec {
            iov_base: msg.as_ptr() as *mut libc::c_void,
            iov_len: msg.len(),
        };
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &raw mut iov;
        m.msg_iovlen = 1;
        if !raw.is_empty() {
            let bytes = raw.len() * 4;
            m.msg_control = cbuf.as_mut_ptr().cast();
            m.msg_controllen = libc::CMSG_SPACE(bytes as u32) as _;
            let c = libc::CMSG_FIRSTHDR(&raw const m);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(bytes as u32) as _;
            std::ptr::copy_nonoverlapping(raw.as_ptr().cast::<u8>(), libc::CMSG_DATA(c), bytes);
        }
        libc::sendmsg(s.as_raw_fd(), &raw const m, libc::MSG_NOSIGNAL)
    };
    let sent = usize::try_from(sent).map_err(|_| io::Error::last_os_error())?;
    s.write_all(&msg[sent..])?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp)?;
    let head_end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| io::Error::other("no HTTP answer"))?;
    let status = std::str::from_utf8(&resp[..head_end])
        .ok()
        .and_then(|h| h.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other("no HTTP status"))?;
    let mut body = resp.split_off(head_end + 4);
    let chunked = std::str::from_utf8(&resp[..head_end])
        .is_ok_and(|h| h.to_ascii_lowercase().contains("transfer-encoding: chunked"));
    if chunked {
        body = dechunk(&body);
    }
    Ok((status, body))
}

fn dechunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = b;
    while let Some(i) = rest.windows(2).position(|w| w == b"\r\n") {
        let n = std::str::from_utf8(&rest[..i])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .unwrap_or(0);
        if n == 0 {
            break;
        }
        let start = i + 2;
        let end = (start + n).min(rest.len());
        out.extend_from_slice(&rest[start..end]);
        rest = rest.get(end + 2..).unwrap_or(&[]);
    }
    out
}

/// The hook: `input` is Claude Code's hook JSON.
#[must_use]
pub fn hook(paths: &HookPaths, input: &Value) -> HookOutcome {
    if input.get("tool_name").and_then(Value::as_str) != Some("SendUserFile") {
        return HookOutcome::Skip;
    }
    let Some(tool_use_id) = input.get("tool_use_id").and_then(Value::as_str) else {
        return HookOutcome::Skip;
    };
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    let files: Vec<String> = tool_input
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if files.is_empty() {
        return HookOutcome::Skip;
    }
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .unwrap_or(WORKSPACE);
    let caption = tool_input.get("caption").and_then(Value::as_str);
    send_paths(paths, tool_use_id, cwd, &files, caption)
}

/// Opens `files` (as named from `cwd`) beneath the workspace and hands them to the supervisor for the tool call
/// `tool_use_id` (empty: the shell call that runs this command, which the supervisor knows).
#[must_use]
pub fn send_paths(
    paths: &HookPaths,
    tool_use_id: &str,
    cwd: &str,
    files: &[String],
    caption: Option<&str>,
) -> HookOutcome {
    if files.len() > MAX_FDS {
        return HookOutcome::Failed(format!("more than {MAX_FDS} files"));
    }
    let mut sent = Vec::new();
    let mut fds = Vec::new();
    for f in files {
        let comps = match workspace_components(cwd, f) {
            Ok(c) => c,
            Err(e) => return HookOutcome::Failed(e),
        };
        let fd = match open_beneath(&paths.root, &comps) {
            Ok(fd) => fd,
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) || e.raw_os_error() == Some(libc::ENOTDIR) => {
                return HookOutcome::Failed(format!("{f} has a symlink in its path"));
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                return HookOutcome::Failed(format!("{f} is not a regular file"));
            }
            Err(e) => return HookOutcome::Failed(format!("{f}: {e}")),
        };
        // SAFETY: fstat into a local struct.
        #[allow(unsafe_code)]
        let size = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd.as_raw_fd(), &raw mut st) != 0 {
                return HookOutcome::Failed(format!("{f}: {}", io::Error::last_os_error()));
            }
            u64::try_from(st.st_size).unwrap_or(0)
        };
        sent.push(SentFile {
            path: format!("{WORKSPACE}/{}", comps.join("/")),
            size,
        });
        fds.push(fd);
    }
    let req = SendUserFileRequest {
        tool_use_id: tool_use_id.to_owned(),
        caption: caption.map(str::to_owned),
        files: sent,
    };
    let body = serde_json::to_vec(&req).unwrap_or_default();
    match post_with_fds(&paths.socket, SEND_USER_FILE, &body, &fds) {
        Ok((200, b)) => HookOutcome::Sent(serde_json::from_slice::<SendUserFileReply>(&b).map_or(0, |r| r.sent)),
        Ok((status, b)) => {
            let msg = serde_json::from_slice::<Value>(&b)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| format!("HTTP {status}"));
            HookOutcome::Failed(msg)
        }
        Err(e) => HookOutcome::Failed(e.to_string()),
    }
}

/// `supervisor hook send-user-file`: the exit code.
#[must_use]
pub fn hook_cli() -> i32 {
    let mut raw = String::new();
    let _ = io::stdin().read_to_string(&mut raw);
    let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    match hook(&HookPaths::sandbox(), &input) {
        HookOutcome::Skip | HookOutcome::Sent(_) => 0,
        HookOutcome::Failed(reason) => {
            let _ = writeln!(io::stderr(), "{}", hook_failure_text(&reason));
            HOOK_FAILED_EXIT
        }
    }
}

/// `send-file [--caption TEXT] PATH…` (Qwen Code has no `SendUserFile` tool: its shell tool runs this command, and
/// the files land on that shell call's row): the exit code. `args` are the words after the command's name.
#[must_use]
pub fn send_file_cli(args: &[String]) -> i32 {
    let mut caption = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--caption" {
            caption = it.next().cloned();
        } else if let Some(c) = a.strip_prefix("--caption=") {
            caption = Some(c.to_owned());
        } else {
            files.push(a.clone());
        }
    }
    if files.is_empty() {
        let _ = writeln!(io::stderr(), "usage: send-file [--caption TEXT] PATH…");
        return HOOK_FAILED_EXIT;
    }
    let cwd = std::env::current_dir().map_or_else(|_| WORKSPACE.to_owned(), |d| d.to_string_lossy().into_owned());
    match send_paths(&HookPaths::sandbox(), "", &cwd, &files, caption.as_deref()) {
        HookOutcome::Skip => 0,
        HookOutcome::Sent(n) => {
            let _ = writeln!(
                io::stdout(),
                "sent {n} file{} to the user",
                if n == 1 { "" } else { "s" }
            );
            0
        }
        HookOutcome::Failed(reason) => {
            let _ = writeln!(io::stderr(), "{}", hook_failure_text(&reason));
            HOOK_FAILED_EXIT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_beneath_the_workspace() {
        assert_eq!(
            workspace_components("/workspace", "out/a.png").unwrap(),
            vec!["out".to_owned(), "a.png".to_owned()]
        );
        assert_eq!(
            workspace_components("/workspace/sub", "file:///workspace/x/../y.txt").unwrap(),
            vec!["y.txt".to_owned()]
        );
        assert!(workspace_components("/workspace", "../etc/passwd").is_err());
        assert!(workspace_components("/workspace", "/etc/passwd").is_err());
        assert!(workspace_components("/workspace", "/workspace").is_err());
    }

    #[test]
    fn symlink_components_are_refused() {
        let d = std::env::temp_dir().join(format!("hook-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(d.join("real")).unwrap();
        std::fs::write(d.join("real/f.txt"), b"x").unwrap();
        std::fs::write(d.join("outside.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        std::os::unix::fs::symlink(d.join("outside.txt"), d.join("real/l.txt")).unwrap();
        assert!(open_beneath(&d, &["real".into(), "f.txt".into()]).is_ok());
        assert!(open_beneath(&d, &["link".into(), "f.txt".into()]).is_err());
        assert!(open_beneath(&d, &["real".into(), "l.txt".into()]).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    /// A FIFO, a directory or a device is refused at once (the hook never blocks in `open`), the file's descriptor
    /// is blocking again, and a symlinked root is refused.
    #[test]
    fn only_regular_files_are_opened() {
        let d = std::env::temp_dir().join(format!("hook-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(d.join("dir")).unwrap();
        std::fs::write(d.join("f.txt"), b"x").unwrap();
        let fifo = std::ffi::CString::new(d.join("pipe").to_string_lossy().into_owned()).unwrap();
        // SAFETY: a valid C string.
        #[allow(unsafe_code)]
        let r = unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) };
        assert_eq!(r, 0);
        let started = std::time::Instant::now();
        let e = open_beneath(&d, &["pipe".into()]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(open_beneath(&d, &["dir".into()]).is_err());
        let f = open_beneath(&d, &["f.txt".into()]).unwrap();
        // SAFETY: fcntl on an fd we own.
        #[allow(unsafe_code)]
        let fl = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(fl & libc::O_NONBLOCK, 0);
        std::os::unix::fs::symlink(&d, d.with_extension("link")).unwrap();
        assert!(open_beneath(&d.with_extension("link"), &["f.txt".into()]).is_err());
        let _ = std::fs::remove_file(d.with_extension("link"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn dechunks() {
        assert_eq!(dechunk(b"5\r\nhello\r\n3\r\n wo\r\n0\r\n\r\n"), b"hello wo");
    }
}
