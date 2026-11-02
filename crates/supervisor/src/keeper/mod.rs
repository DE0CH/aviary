//! The keeper (PID 1) and the supervisor's client of it (supervisor 4.1, 4.2).
//!
//! - [`server`]: the keeper itself, run by the `supervisor-keeper` binary.
//! - [`Client`]: the supervisor's side, one connection per request (so a stdin message waiting for Claude Code to
//!   read never holds up a signal or an exit poll). Blocking; the async callers wrap it in `spawn_blocking`.
//! - [`proto`]: the wire format both use.

pub mod proto;
pub mod server;
mod sys;
#[cfg(test)]
mod tests;

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use proto::{MSG_ID, Op, Reader, STDIN_HEADER, Status, Writer};

use crate::limits::KEEPER_PACKET;

/// The keeper's directory: a tmpfs, in the checkpoint of a paused sandbox.
pub const RUN_DIR: &str = "/run/supervisor";
/// The control socket's name in [`RUN_DIR`].
pub const KEEPER_SOCKET_NAME: &str = "keeper.sock";
/// The boot id's file name in [`RUN_DIR`].
pub const BOOT_ID_NAME: &str = "boot_id";
/// Where stdin messages larger than a pooled buffer are staged (the scratch disk).
pub const SPOOL_DIR: &str = "/var/lib/supervisor/keeper";
/// The supervisor binary the keeper starts as `supervisor run`.
pub const SUPERVISOR_BIN: &str = "/usr/local/bin/supervisor";

/// The fd name of the `/dev/fuse` connection (supervisor 4.3 step 7).
pub const FUSE_FD: &str = "fuse";

/// A keeper answer that was not `OK`.
#[derive(Debug, thiserror::Error)]
pub enum KeeperError {
    #[error("keeper socket: {0}")]
    Io(#[from] io::Error),
    #[error("keeper refused: {what} (errno {errno})")]
    Refused { errno: i32, what: String },
    #[error("malformed keeper answer")]
    Malformed,
}

impl From<proto::Malformed> for KeeperError {
    fn from(_: proto::Malformed) -> Self {
        Self::Malformed
    }
}

impl KeeperError {
    /// The errno of a refusal.
    #[must_use]
    pub fn errno(&self) -> Option<i32> {
        match self {
            Self::Refused { errno, .. } => Some(*errno),
            Self::Io(e) => e.raw_os_error(),
            Self::Malformed => None,
        }
    }
}

type R<T> = Result<T, KeeperError>;

/// What `SPAWN` starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    /// `claude`, `dockerd` or `exec` (at most 16 bytes kept).
    pub name: String,
    pub argv: Vec<String>,
    /// `KEY=value` entries: exactly the child's environment.
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub uid: u32,
    pub gid: u32,
    /// Its own session and process group.
    pub setsid: bool,
    pub oom_score_adj: i32,
    /// A cgroup directory whose `cgroup.procs` gets the child's pid before exec.
    pub cgroup: Option<String>,
    /// Files the child's stdout and stderr are appended to (`/dev/null` when `None`).
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    /// A pipe owned by the keeper (else `/dev/null`).
    pub stdin_pipe: bool,
}

/// What `STDIN` came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdinOutcome {
    /// Every byte is in the pipe.
    Written,
    /// That `msg_id` was already written to this child: nothing was written.
    Already,
}

/// One exit the keeper recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitRecord {
    /// The keeper's exit counter (pass the last one seen as `after`).
    pub n: u64,
    pub pid: i32,
    pub name: String,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// Unix milliseconds.
    pub at_ms: u64,
}

/// The keeper's `STATUS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeeperStatus {
    pub boot_id: String,
    pub supervisor_restarts: u32,
    pub started_at_ms: u64,
    pub children: Vec<(i32, String)>,
}

/// The supervisor's client of the keeper.
#[derive(Clone, Debug)]
pub struct Client {
    path: PathBuf,
}

struct Conn(OwnedFd);

impl Conn {
    fn send(&self, b: &[u8], fd: Option<RawFd>) -> R<()> {
        sys::send_packet(self.0.as_raw_fd(), b, fd, true)?;
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> R<(usize, Option<OwnedFd>)> {
        let (n, fd) = sys::recv_packet(self.0.as_raw_fd(), buf, false, true)?;
        // SAFETY: the descriptor was just received with SCM_RIGHTS and is owned by nothing else.
        #[allow(unsafe_code)]
        let fd = fd.map(|f| unsafe { OwnedFd::from_raw_fd(f) });
        if n == 0 {
            return Err(KeeperError::Io(io::Error::from(io::ErrorKind::UnexpectedEof)));
        }
        Ok((n, fd))
    }
}

/// Splits an answer into its status and payload, turning `ERR` into [`KeeperError::Refused`].
fn status(buf: &[u8]) -> R<(Status, Reader<'_>)> {
    let st = Status::from_byte(*buf.first().ok_or(KeeperError::Malformed)?).ok_or(KeeperError::Malformed)?;
    let mut r = Reader::new(&buf[1..]);
    if st == Status::Err {
        let errno = r.i32()?;
        let what = String::from_utf8_lossy(r.bytes()?).into_owned();
        return Err(KeeperError::Refused { errno, what });
    }
    Ok((st, r))
}

impl Client {
    /// The keeper at `run_dir`/keeper.sock.
    #[must_use]
    pub fn new(run_dir: &Path) -> Self {
        Self {
            path: run_dir.join(KEEPER_SOCKET_NAME),
        }
    }

    fn connect(&self) -> R<Conn> {
        use std::os::unix::ffi::OsStrExt as _;
        let fd = sys::connect_seqpacket(self.path.as_os_str().as_bytes())?;
        // SAFETY: a new socket owned by nothing else.
        #[allow(unsafe_code)]
        Ok(Conn(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn call(&self, req: &[u8], fd: Option<RawFd>) -> R<(Vec<u8>, Option<OwnedFd>)> {
        let c = self.connect()?;
        c.send(req, fd)?;
        let mut buf = vec![0u8; KEEPER_PACKET];
        let (n, fd) = c.recv(&mut buf)?;
        buf.truncate(n);
        Ok((buf, fd))
    }

    fn simple(&self, req: &[u8]) -> R<()> {
        let (b, _) = self.call(req, None)?;
        status(&b)?;
        Ok(())
    }

    /// `SPAWN`: the child's pid.
    ///
    /// # Errors
    /// The socket's error, or the keeper's (fork or exec failed, table full).
    pub fn spawn(&self, s: &SpawnSpec) -> R<i32> {
        let mut buf = vec![0u8; KEEPER_PACKET];
        let len = {
            let mut w = Writer::new(&mut buf);
            w.u8(Op::Spawn as u8).bytes(s.name.as_bytes());
            w.u32(u32::try_from(s.argv.len()).map_err(|_| KeeperError::Malformed)?);
            for a in &s.argv {
                w.bytes(a.as_bytes());
            }
            w.u32(u32::try_from(s.env.len()).map_err(|_| KeeperError::Malformed)?);
            for e in &s.env {
                w.bytes(e.as_bytes());
            }
            w.bytes(s.cwd.as_deref().unwrap_or("").as_bytes())
                .u32(s.uid)
                .u32(s.gid)
                .u8(u8::from(s.setsid))
                .i32(s.oom_score_adj)
                .bytes(s.cgroup.as_deref().unwrap_or("").as_bytes())
                .bytes(s.stdout.as_deref().unwrap_or("").as_bytes())
                .bytes(s.stderr.as_deref().unwrap_or("").as_bytes())
                .u8(u8::from(s.stdin_pipe));
            w.finish()?.len()
        };
        let (b, _) = self.call(&buf[..len], None)?;
        let (_, mut r) = status(&b)?;
        Ok(r.i32()?)
    }

    /// `STDIN`: writes `data` to `pid`'s stdin whole, at most once per `msg_id`. Blocks until the last byte is in
    /// the pipe (Claude Code reads it at its own pace).
    ///
    /// # Errors
    /// The socket's error, or the keeper's (no such child, stdin closed, the child exited mid-write).
    pub fn stdin(&self, pid: i32, msg_id: [u8; MSG_ID], data: &[u8]) -> R<StdinOutcome> {
        let c = self.connect()?;
        let mut buf = vec![0u8; KEEPER_PACKET];
        let first = data.len().min(KEEPER_PACKET - STDIN_HEADER);
        let len = {
            let mut w = Writer::new(&mut buf);
            w.u8(Op::Stdin as u8)
                .i32(pid)
                .bytes_raw(&msg_id)
                .u64(data.len() as u64)
                .bytes_raw(&data[..first]);
            w.finish()?.len()
        };
        c.send(&buf[..len], None)?;
        for chunk in data[first..].chunks(KEEPER_PACKET - 1) {
            buf[0] = Op::StdinMore as u8;
            buf[1..=chunk.len()].copy_from_slice(chunk);
            c.send(&buf[..=chunk.len()], None)?;
        }
        let (n, _) = c.recv(&mut buf)?;
        let (st, mut r) = status(&buf[..n])?;
        let id = r.msg_id()?;
        if id != msg_id {
            return Err(KeeperError::Malformed);
        }
        Ok(if st == Status::Already {
            StdinOutcome::Already
        } else {
            StdinOutcome::Written
        })
    }

    /// `CLOSE_STDIN`.
    ///
    /// # Errors
    /// The socket's or the keeper's error.
    pub fn close_stdin(&self, pid: i32) -> R<()> {
        let mut b = [0u8; 8];
        let mut w = Writer::new(&mut b);
        w.u8(Op::CloseStdin as u8).i32(pid);
        self.simple(w.finish()?)
    }

    /// `SIGNAL` to a child (`target > 0`) or its process group (`-pgid`).
    ///
    /// # Errors
    /// The socket's or the keeper's error (`ESRCH` for a target that is not the keeper's child).
    pub fn signal(&self, target: i32, sig: i32) -> R<()> {
        let mut b = [0u8; 16];
        let mut w = Writer::new(&mut b);
        w.u8(Op::Signal as u8).i32(target).i32(sig);
        self.simple(w.finish()?)
    }

    /// `PUT_FD`: the keeper keeps a duplicate of `fd` under `name`.
    ///
    /// # Errors
    /// The socket's or the keeper's error.
    pub fn put_fd(&self, name: &str, fd: RawFd) -> R<()> {
        let mut b = [0u8; 64];
        let mut w = Writer::new(&mut b);
        w.u8(Op::PutFd as u8).bytes(name.as_bytes());
        let (r, _) = self.call(w.finish()?, Some(fd))?;
        status(&r)?;
        Ok(())
    }

    /// `GET_FD`: the descriptor kept under `name`, if any.
    ///
    /// # Errors
    /// The socket's or the keeper's error.
    pub fn get_fd(&self, name: &str) -> R<Option<OwnedFd>> {
        let mut b = [0u8; 64];
        let mut w = Writer::new(&mut b);
        w.u8(Op::GetFd as u8).bytes(name.as_bytes());
        let (r, fd) = self.call(w.finish()?, None)?;
        match status(&r)?.0 {
            Status::None => Ok(None),
            _ => fd.map(Some).ok_or(KeeperError::Malformed),
        }
    }

    /// `EXITS` after counter `after` (the last 64 at most).
    ///
    /// # Errors
    /// The socket's or the keeper's error.
    pub fn exits(&self, after: u64) -> R<Vec<ExitRecord>> {
        let mut b = [0u8; 16];
        let mut w = Writer::new(&mut b);
        w.u8(Op::Exits as u8).u64(after);
        let (buf, _) = self.call(w.finish()?, None)?;
        let (_, mut r) = status(&buf)?;
        let n = r.u32()?;
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            out.push(ExitRecord {
                n: r.u64()?,
                pid: r.i32()?,
                name: String::from_utf8_lossy(r.bytes()?).into_owned(),
                code: r.opt_i32()?,
                signal: r.opt_i32()?,
                at_ms: r.u64()?,
            });
        }
        Ok(out)
    }

    /// `STATUS`.
    ///
    /// # Errors
    /// The socket's or the keeper's error.
    pub fn status(&self) -> R<KeeperStatus> {
        let (buf, _) = self.call(&[Op::Status as u8], None)?;
        let (_, mut r) = status(&buf)?;
        let boot_id = String::from_utf8_lossy(r.bytes()?).into_owned();
        let supervisor_restarts = r.u32()?;
        let started_at_ms = r.u64()?;
        let n = r.u32()?;
        let mut children = Vec::with_capacity(n as usize);
        for _ in 0..n {
            children.push((r.i32()?, String::from_utf8_lossy(r.bytes()?).into_owned()));
        }
        Ok(KeeperStatus {
            boot_id,
            supervisor_restarts,
            started_at_ms,
            children,
        })
    }
}

/// A fresh random `msg_id`.
#[must_use]
pub fn new_msg_id() -> [u8; MSG_ID] {
    *uuid::Uuid::new_v4().as_bytes()
}
