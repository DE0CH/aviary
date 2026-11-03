//! The keeper (supervisor 4.1, 4.2): PID 1 of the sandbox. It starts the supervisor and restarts it with backoff,
//! is the parent of every process the supervisor starts (so a supervisor restart never orphans or kills Claude
//! Code), owns their stdin pipes (a message is written whole or not at all, and at most once per `msg_id`), keeps
//! the `/dev/fuse` descriptor across supervisor restarts, reaps, and remembers the last exits of its own children
//! (reparented orphans are reaped and only counted, so a burst of them never pushes a tracked exit out of the ring).
//! A supervisor that exits 0 after it told the keeper `RELEASED` is not started again; a SIGTERM that arrives while
//! no supervisor runs (a restart backoff), or a supervisor that dies during the stop without having finished the
//! release, starts one at once, so the runtime stop is always a forced release that ends cleanly within the grace.
//!
//! One thread, `poll(2)` over a fixed set of descriptors, every table and buffer allocated by [`Keeper::new`]
//! before the loop; the loop itself does not allocate. A stdin message is staged whole before its first byte
//! reaches the pipe: in a pooled 64 KiB buffer when it fits, otherwise in a spool file on the scratch disk (the
//! supervisor can die mid-send, and a half-written NDJSON line must never reach Claude Code).

#![allow(unsafe_code)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)] // C integer types
#![allow(clippy::many_single_char_names, clippy::similar_names)] // c: connection, m: message slot, n: length

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::proto::{MAX_ARGS, MSG_ID, Malformed, NAME_MAX, Op, Reader, STDIN_HEADER, Status, Writer};
use super::sys;
use crate::limits::{
    KEEPER_BACKOFF, KEEPER_BACKOFF_RESET, KEEPER_EXITS, KEEPER_PACKET, KEEPER_STDIN_RING, KEEPER_STOP_GRACE,
};

/// Children the keeper tracks at once (Claude Code, dockerd, exec runs and the hook's siblings are few).
pub const MAX_CHILDREN: usize = 32;
/// Client connections at once.
pub const MAX_CONNS: usize = 16;
/// Stdin messages in flight at once (one per child at most); each holds one pooled buffer.
pub const STDIN_BUFFERS: usize = 4;
/// The pooled buffer: a message up to this size is staged in memory, a larger one in a spool file.
pub const STDIN_BUFFER: usize = 64 * 1024;
/// Descriptors kept by name (`PUT_FD`).
pub const HELD_FDS: usize = 4;
const POLL_MS: i32 = 100;
/// During a stop, a supervisor that dies is started again no sooner than this after the last start.
const STOP_RESTART_GAP_MS: u64 = 100;
/// The listener, the signal source, every connection, every stdin message being written, and the error pipe of
/// every spawn not yet answered.
const POLL_SLOTS: usize = 2 + MAX_CONNS + STDIN_BUFFERS + MAX_CHILDREN;

/// Where the keeper works and what it starts.
#[derive(Debug, Clone)]
pub struct Config {
    /// `/run/supervisor`: the control socket and `boot_id`.
    pub run_dir: PathBuf,
    /// Mount a tmpfs (0755, 1 MiB) on `run_dir` when it is not one already.
    pub mount_run_tmpfs: bool,
    /// Where messages larger than [`STDIN_BUFFER`] are staged.
    pub spool_dir: PathBuf,
    /// The supervisor's argv; `None` starts none (the protocol tests drive the keeper themselves).
    pub supervisor: Option<Vec<String>>,
    /// `mlockall` and `oom_score_adj -1000` for the keeper itself.
    pub protect_self: bool,
    /// Where the stop requests come from.
    pub signals: Signals,
    /// How long a stop waits for the supervisor to finish its release before it sends SIGKILL to everything
    /// ([`KEEPER_STOP_GRACE`]; tests whose stand-in never finishes one use less).
    pub stop_grace: std::time::Duration,
}

/// How the keeper learns it must stop.
#[derive(Debug, Clone, Copy)]
pub enum Signals {
    /// The process's own SIGTERM, SIGINT and SIGCHLD through a `signalfd` (PID 1).
    Process,
    /// Bytes on this descriptor: `T` is a SIGTERM, `I` a SIGINT (tests run the keeper on a thread).
    Pipe(RawFd),
}

impl Config {
    /// The sandbox's keeper (supervisor 4.2; the same in every deployment).
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            run_dir: PathBuf::from(super::RUN_DIR),
            mount_run_tmpfs: true,
            spool_dir: PathBuf::from(super::SPOOL_DIR),
            supervisor: Some(vec![super::SUPERVISOR_BIN.into(), "run".into()]),
            protect_self: true,
            signals: Signals::Process,
            stop_grace: KEEPER_STOP_GRACE,
        }
    }
}

#[derive(Clone, Copy)]
struct Name {
    b: [u8; NAME_MAX],
    len: u8,
}

impl Name {
    fn new(s: &[u8]) -> Self {
        let mut b = [0u8; NAME_MAX];
        let n = s.len().min(NAME_MAX);
        b[..n].copy_from_slice(&s[..n]);
        Self { b, len: n as u8 }
    }
    fn get(&self) -> &[u8] {
        &self.b[..self.len as usize]
    }
}

#[derive(Clone, Copy)]
struct Child {
    pid: i32,
    name: Name,
    stdin: RawFd,
    ring: [[u8; MSG_ID]; KEEPER_STDIN_RING],
    ring_len: usize,
    ring_next: usize,
    /// The in-flight stdin message's slot.
    msg: Option<usize>,
    /// A spawn not answered yet: the error pipe's read end (closed by the child's exec, or its errno), polled by the
    /// loop so a child stuck before exec (a stalled `/workspace` in its `chdir`) never stops the keeper.
    spawn_err: RawFd,
    /// The connection waiting for that answer.
    spawn_conn: Option<usize>,
    /// Its exec failed: it exits 127, and is reaped without an exit record.
    failed: bool,
    /// The exit counter when it was forked: its exit record is the first one with its pid above this (a pid the
    /// kernel reuses never matches an older record).
    since: u64,
}

impl Child {
    const EMPTY: Self = Self {
        pid: 0,
        name: Name {
            b: [0; NAME_MAX],
            len: 0,
        },
        stdin: -1,
        ring: [[0; MSG_ID]; KEEPER_STDIN_RING],
        ring_len: 0,
        ring_next: 0,
        msg: None,
        spawn_err: -1,
        since: 0,
        spawn_conn: None,
        failed: false,
    };

    fn seen(&self, id: &[u8; MSG_ID]) -> bool {
        self.ring[..self.ring_len].iter().any(|r| r == id)
    }

    fn remember(&mut self, id: [u8; MSG_ID]) {
        self.ring[self.ring_next] = id;
        self.ring_next = (self.ring_next + 1) % KEEPER_STDIN_RING;
        self.ring_len = (self.ring_len + 1).min(KEEPER_STDIN_RING);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Packets still arriving.
    Receiving,
    /// Every byte staged; copying to the pipe.
    Writing,
}

#[derive(Clone, Copy)]
struct Msg {
    child: usize,
    /// The connection waiting for the answer (`None` once it closed).
    conn: Option<usize>,
    id: [u8; MSG_ID],
    total: u64,
    got: u64,
    /// Bytes of the message already in the pipe.
    written: u64,
    /// A message already written (in the ring): its bytes are read and dropped, the answer is `ALREADY`.
    discard: bool,
    /// Staged in the spool file rather than the buffer.
    spooled: bool,
    /// Spool mode: the buffer holds `[chunk_start, chunk_start + chunk_len)` of the message.
    chunk_start: u64,
    chunk_len: usize,
    phase: Phase,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Free,
    /// Ready for the next request.
    Idle,
    /// Receiving the packets of message slot `n`.
    Receiving(usize),
    /// Waiting for message slot `n` to reach the pipe.
    Waiting(usize),
    /// A `STDIN` header is queued in the socket for a busy child or a full buffer pool.
    Blocked,
    /// Waiting for child `n`'s exec (its `SPAWN` answer).
    Spawning(usize),
}

#[derive(Clone, Copy)]
struct Exit {
    n: u64,
    pid: i32,
    name: Name,
    code: Option<i32>,
    signal: Option<i32>,
    at_ms: u64,
}

const NO_EXIT: Exit = Exit {
    n: 0,
    pid: 0,
    name: Name {
        b: [0; NAME_MAX],
        len: 0,
    },
    code: None,
    signal: None,
    at_ms: 0,
};

#[derive(Clone, Copy)]
enum Slot {
    Listener,
    Signals,
    Conn(usize),
    Pipe(usize),
    SpawnErr(usize),
}

/// The keeper's state; every buffer is allocated here.
#[allow(clippy::struct_excessive_bools)] // independent facts of the supervisor's lifecycle
pub struct Keeper {
    cfg: Config,
    listener: RawFd,
    sigfd: RawFd,
    boot_id: [u8; 16],
    started_at_ms: u64,
    reap_all: bool,

    supervisor_argv: Vec<CString>,
    supervisor_ptrs: Vec<*const libc::c_char>,
    supervisor_pid: Option<i32>,
    supervisor_started_mono: u64,
    supervisor_restarts: u32,
    backoff: usize,
    next_start_mono: Option<u64>,
    stopping_since: Option<u64>,
    /// The stop's SIGTERM has reached the running supervisor (cleared when it exits: a successor gets it again).
    stop_delivered: bool,
    /// The supervisor said `RELEASED`: once it has exited 0 it is not started again. Kept in memory, which a
    /// checkpoint keeps, so a restored released sandbox stays released and no file write can lose it.
    released: bool,
    supervisor_done: bool,
    /// Reparented processes reaped (PID 1 only); counted, never in the exits ring.
    orphans: u64,

    children: Box<[Child; MAX_CHILDREN]>,
    conns: [RawFd; MAX_CONNS],
    conn_state: [ConnState; MAX_CONNS],
    msgs: [Option<Msg>; STDIN_BUFFERS],
    buffers: Vec<Box<[u8]>>,
    spools: [RawFd; STDIN_BUFFERS],
    spool_paths: Vec<CString>,
    held: [(Name, RawFd); HELD_FDS],
    exits: Box<[Exit; KEEPER_EXITS]>,
    exit_n: u64,

    rx: Box<[u8]>,
    tx: Box<[u8]>,
    spawn_buf: Box<[u8]>,
    pollfds: Box<[libc::pollfd; POLL_SLOTS]>,
    slots: Box<[Slot; POLL_SLOTS]>,
}

// SAFETY: the only non-Send fields are `supervisor_ptrs`, pointers into the `CString`s of `supervisor_argv`, which
// the same struct owns and never shares; moving the struct to another thread moves both together.
unsafe impl Send for Keeper {}

/// A keeper that stopped closes its socket and every connection. As PID 1 its exit closes them anyway; a keeper on a
/// test's thread would otherwise leave them open in the test process, and a client that connected after the loop's
/// last poll (queued on the listener) or whose request was never read would wait in `recv` for ever, holding the
/// test's runtime at its shutdown.
impl Drop for Keeper {
    fn drop(&mut self) {
        for c in 0..MAX_CONNS {
            if self.conns[c] >= 0 {
                sys::close(self.conns[c]);
                self.conns[c] = -1;
            }
        }
        if self.listener >= 0 {
            sys::close(self.listener);
            self.listener = -1;
        }
    }
}

impl std::fmt::Debug for Keeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keeper")
            .field("run_dir", &self.cfg.run_dir)
            .field("supervisor_pid", &self.supervisor_pid)
            .finish_non_exhaustive()
    }
}

/// A zeroed buffer on the heap (allocated before the loop starts).
fn zeroed(n: usize) -> Box<[u8]> {
    vec![0u8; n].into_boxed_slice()
}

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// A log line's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// What happened.
    Info,
    /// Something degraded that the keeper carries on without.
    Warn,
    /// Why the keeper (and so the sandbox) failed: an exit or a supervisor that cannot be started.
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// One JSON log line at `info` on stderr ([`log_at`]).
pub fn log(msg: &str, fields: &[(&str, i64)]) {
    log_at(Level::Info, msg, fields);
}

/// One JSON log line on stderr, written from a stack buffer (supervisor 21.1: the sandbox's console log).
pub fn log_at(level: Level, msg: &str, fields: &[(&str, i64)]) {
    let mut buf = [0u8; 512];
    let mut w = Writer::new(&mut buf);
    w.bytes_raw(b"{\"t\":\"");
    let mut t = [0u8; 24];
    w.bytes_raw(rfc3339(sys::wall_ms(), &mut t));
    w.bytes_raw(b"\",\"level\":\"")
        .bytes_raw(level.as_str().as_bytes())
        .bytes_raw(b"\",\"svc\":\"supervisor-keeper\",\"msg\":\"");
    w.bytes_raw(msg.as_bytes());
    w.bytes_raw(b"\"");
    for (k, v) in fields {
        w.bytes_raw(b",\"").bytes_raw(k.as_bytes()).bytes_raw(b"\":");
        let mut d = [0u8; 24];
        w.bytes_raw(decimal(*v, &mut d));
    }
    w.bytes_raw(b"}\n");
    if let Ok(line) = w.finish() {
        let _ = sys::write_all(2, line);
    }
}

/// `v` in decimal, into `out`.
fn decimal(v: i64, out: &mut [u8; 24]) -> &[u8] {
    let mut i = out.len();
    let neg = v < 0;
    let mut u = v.unsigned_abs();
    loop {
        i -= 1;
        out[i] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    if neg {
        i -= 1;
        out[i] = b'-';
    }
    &out[i..]
}

/// `ms` since the epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ` (days to civil date: Howard Hinnant's algorithm).
fn rfc3339(ms: u64, out: &mut [u8; 24]) -> &[u8] {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let mut put = |at: usize, v: u64, width: usize| {
        let mut v = v;
        for k in (0..width).rev() {
            out[at + k] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    put(0, y as u64, 4);
    put(5, m as u64, 2);
    put(8, d as u64, 2);
    put(11, rem / 3600, 2);
    put(14, (rem / 60) % 60, 2);
    put(17, rem % 60, 2);
    put(20, ms % 1000, 3);
    out[4] = b'-';
    out[7] = b'-';
    out[10] = b'T';
    out[13] = b':';
    out[16] = b':';
    out[19] = b'.';
    out[23] = b'Z';
    &out[..]
}

impl Keeper {
    /// Prepares everything the loop needs: the run directory, `boot_id`, the control socket, the signal source and
    /// every buffer.
    ///
    /// # Errors
    /// When the control socket or the spool directory cannot be made.
    pub fn new(cfg: Config) -> io::Result<Self> {
        if cfg.protect_self {
            // SAFETY: mlockall has no memory-safety effects; a failure is logged and not fatal.
            if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } != 0 {
                log_at(Level::Warn, "mlockall failed", &[("errno", i64::from(sys::errno()))]);
            }
            if let Err(e) = std::fs::write("/proc/self/oom_score_adj", b"-1000") {
                log(
                    "oom_score_adj failed",
                    &[("errno", i64::from(e.raw_os_error().unwrap_or(0)))],
                );
            }
        }
        std::fs::create_dir_all(&cfg.run_dir)?;
        if cfg.mount_run_tmpfs {
            mount_tmpfs(&cfg.run_dir);
        }
        std::fs::create_dir_all(&cfg.spool_dir)?;
        let boot_id = boot_id(&cfg.run_dir)?;

        let sock = cfg.run_dir.join(super::KEEPER_SOCKET_NAME);
        let listener = sys::listen_seqpacket(sock.as_os_str().as_bytes(), &cstr(&sock)?)?;
        let sigfd = match cfg.signals {
            Signals::Process => process_signals()?,
            Signals::Pipe(fd) => fd,
        };
        // SAFETY: ignoring SIGPIPE: a write to a pipe whose reader died returns EPIPE instead of ending PID 1.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

        let supervisor_argv: Vec<CString> = cfg
            .supervisor
            .iter()
            .flatten()
            .map(|a| CString::new(a.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL)))
            .collect::<io::Result<_>>()?;
        let mut supervisor_ptrs: Vec<*const libc::c_char> = supervisor_argv.iter().map(|a| a.as_ptr()).collect();
        supervisor_ptrs.push(std::ptr::null());
        let spool_paths = (0..STDIN_BUFFERS)
            .map(|i| cstr(&cfg.spool_dir.join(format!("stdin-{i}.spool"))))
            .collect::<io::Result<Vec<_>>>()?;

        Ok(Self {
            reap_all: sys::getpid() == 1,
            started_at_ms: sys::wall_ms(),
            next_start_mono: cfg.supervisor.as_ref().map(|_| 0),
            cfg,
            listener,
            sigfd,
            boot_id,
            supervisor_argv,
            supervisor_ptrs,
            supervisor_pid: None,
            supervisor_started_mono: 0,
            supervisor_restarts: 0,
            backoff: 0,
            stopping_since: None,
            stop_delivered: false,
            released: false,
            supervisor_done: false,
            orphans: 0,
            children: Box::new([Child::EMPTY; MAX_CHILDREN]),
            conns: [-1; MAX_CONNS],
            conn_state: [ConnState::Free; MAX_CONNS],
            msgs: [None; STDIN_BUFFERS],
            buffers: (0..STDIN_BUFFERS).map(|_| zeroed(STDIN_BUFFER)).collect(),
            spools: [-1; STDIN_BUFFERS],
            spool_paths,
            held: [(Name::new(b""), -1); HELD_FDS],
            exits: Box::new([NO_EXIT; KEEPER_EXITS]),
            exit_n: 0,
            rx: zeroed(KEEPER_PACKET),
            tx: zeroed(KEEPER_PACKET),
            spawn_buf: zeroed(KEEPER_PACKET + 2 * (MAX_ARGS + 1)),
            pollfds: Box::new(
                [libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                }; POLL_SLOTS],
            ),
            slots: Box::new([Slot::Listener; POLL_SLOTS]),
        })
    }

    /// The loop. Returns when a SIGTERM's stop has finished (supervisor exited or the grace ran out and everything
    /// was killed).
    pub fn run(&mut self) -> i32 {
        log("keeper started", &[("pid", i64::from(sys::getpid()))]);
        // SIGTERM and SIGINT blocked in the loop's thread (PID 1 has them on the signalfd already; a test keeper on a
        // thread gets them through its pipe): a supervisor forked from it has them blocked from the first instant
        let stop = stop_set();
        // SAFETY: changing this thread's mask with a local set.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const stop, std::ptr::null_mut()) };
        loop {
            let now = sys::mono_ms();
            self.reap_children(now);
            if let Some(code) = self.check_stop(now) {
                return code;
            }
            self.maybe_start_supervisor(now);
            let n = self.fill_pollfds();
            // SAFETY: poll over the first `n` entries of the fixed pollfd array.
            let r = unsafe { libc::poll(self.pollfds.as_mut_ptr(), n as libc::nfds_t, POLL_MS) };
            if r <= 0 {
                continue;
            }
            for i in 0..n {
                let rev = self.pollfds[i].revents;
                if rev == 0 {
                    continue;
                }
                match self.slots[i] {
                    Slot::Listener => self.accept_all(),
                    Slot::Signals => self.read_signals(),
                    Slot::Conn(c) if self.conns[c] == self.pollfds[i].fd => self.conn_ready(c, rev),
                    Slot::Pipe(m) => self.pipe_ready(m),
                    Slot::SpawnErr(ch) if self.children[ch].spawn_err == self.pollfds[i].fd => self.resolve_spawn(ch),
                    Slot::Conn(_) | Slot::SpawnErr(_) => {}
                }
            }
        }
    }

    fn fill_pollfds(&mut self) -> usize {
        let mut n = 0;
        let mut push = |fd: RawFd, events: i16, slot: Slot, pf: &mut [libc::pollfd], sl: &mut [Slot]| {
            pf[n] = libc::pollfd { fd, events, revents: 0 };
            sl[n] = slot;
            n += 1;
        };
        let (pf, sl) = (&mut *self.pollfds, &mut *self.slots);
        // served while stopping too: the supervisor's forced release signals Claude Code and polls exits through it
        push(self.listener, libc::POLLIN, Slot::Listener, pf, sl);
        push(self.sigfd, libc::POLLIN, Slot::Signals, pf, sl);
        for c in 0..MAX_CONNS {
            let events = match self.conn_state[c] {
                ConnState::Free => continue,
                ConnState::Idle | ConnState::Receiving(_) => libc::POLLIN,
                // only hang-ups (always reported): the connection waits for an answer or a free child
                ConnState::Waiting(_) | ConnState::Blocked | ConnState::Spawning(_) => 0,
            };
            push(self.conns[c], events, Slot::Conn(c), pf, sl);
        }
        for (i, ch) in self.children.iter().enumerate() {
            if ch.spawn_err >= 0 {
                push(ch.spawn_err, libc::POLLIN, Slot::SpawnErr(i), pf, sl);
            }
        }
        for m in 0..STDIN_BUFFERS {
            if let Some(msg) = &self.msgs[m]
                && msg.phase == Phase::Writing
            {
                push(self.children[msg.child].stdin, libc::POLLOUT, Slot::Pipe(m), pf, sl);
            }
        }
        n
    }

    // signals and the supervisor

    fn read_signals(&mut self) {
        match self.cfg.signals {
            Signals::Process => {
                // SAFETY: an all-zero signalfd_siginfo is valid; read fills whole records.
                let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
                let size = std::mem::size_of::<libc::signalfd_siginfo>();
                loop {
                    // SAFETY: reading one record into the local struct.
                    let r = unsafe { libc::read(self.sigfd, (&raw mut info).cast(), size) };
                    if r != size as isize {
                        break;
                    }
                    self.on_signal(info.ssi_signo as i32);
                }
            }
            Signals::Pipe(fd) => {
                let mut b = [0u8; 16];
                if let Ok(n) = sys::read(fd, &mut b) {
                    for c in &b[..n] {
                        match c {
                            b'T' => self.on_signal(libc::SIGTERM),
                            b'I' => self.on_signal(libc::SIGINT),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn on_signal(&mut self, sig: i32) {
        match sig {
            libc::SIGTERM => {
                log("stopping", &[("signal", i64::from(sig))]);
                if self.stopping_since.is_none() {
                    self.stopping_since = Some(sys::mono_ms());
                }
                // no supervisor running (a restart backoff): the loop starts one at once and hands it the SIGTERM
                if let Some(pid) = self.supervisor_pid {
                    let _ = sys::kill(pid, libc::SIGTERM);
                    self.stop_delivered = true;
                }
            }
            libc::SIGINT => {
                if let Some(pid) = self.supervisor_pid {
                    let _ = sys::kill(pid, libc::SIGINT);
                }
            }
            _ => {} // SIGCHLD: the loop reaps on every pass
        }
    }

    /// `Some(exit code)` once a stop has finished.
    fn check_stop(&mut self, now: u64) -> Option<i32> {
        let since = self.stopping_since?;
        let grace = self.cfg.stop_grace.as_millis() as u64;
        // over when a supervisor has finished the release (exit 0 after RELEASED), or with no supervisor to run, or
        // at the grace; one that died before finishing is started again at once (maybe_start_supervisor)
        let finished = self.supervisor_pid.is_none() && (self.supervisor_done || !self.has_supervisor());
        if !finished && now.saturating_sub(since) < grace {
            return None;
        }
        if !finished {
            log("stop grace over, killing everything", &[]);
        }
        if self.reap_all {
            // PID 1: every other process in the sandbox
            let _ = sys::kill(-1, libc::SIGKILL);
        } else {
            for c in self.children.iter().filter(|c| c.pid > 0) {
                let _ = sys::kill(c.pid, libc::SIGKILL);
            }
            if let Some(pid) = self.supervisor_pid {
                let _ = sys::kill(pid, libc::SIGKILL);
            }
        }
        log("keeper stopped", &[]);
        Some(0)
    }

    fn has_supervisor(&self) -> bool {
        !self.supervisor_argv.is_empty()
    }

    fn maybe_start_supervisor(&mut self, now: u64) {
        if self.supervisor_pid.is_some() || self.supervisor_done || !self.has_supervisor() {
            return;
        }
        let stopping = self.stopping_since.is_some();
        if stopping {
            // a SIGTERM no running supervisor has (a backoff, or one that died mid-release): start one now, skipping
            // the backoff, so the stop is a forced release; a supervisor that keeps dying is started at most every
            // STOP_RESTART_GAP
            if self.stop_delivered || now < self.supervisor_started_mono.saturating_add(STOP_RESTART_GAP_MS) {
                return;
            }
        } else {
            let Some(at) = self.next_start_mono else { return };
            if now < at {
                return;
            }
        }
        self.next_start_mono = None;
        let ptrs = self.supervisor_ptrs.as_ptr();
        let exe = self.supervisor_argv[0].as_ptr();
        let stop = stop_set();
        // SAFETY: fork; the child only calls async-signal-safe functions (sigprocmask, signal, execv, _exit) on
        // memory prepared before the fork.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: the child after fork: restore the signal state and exec the supervisor with the keeper's own
            // environment (hermit's boot environment). SIGTERM and SIGINT stay blocked across the exec: one that
            // arrives before the supervisor's handlers exist waits as pending instead of killing it.
            unsafe {
                supervisor_child_signals(&stop);
                libc::execv(exe, ptrs);
                libc::_exit(127);
            }
        }
        if pid < 0 {
            log_at(
                Level::Error,
                "supervisor fork failed",
                &[("errno", i64::from(sys::errno()))],
            );
            self.schedule_restart(now);
            return;
        }
        self.supervisor_pid = Some(pid);
        self.supervisor_started_mono = now;
        if stopping {
            let _ = sys::kill(pid, libc::SIGTERM);
            self.stop_delivered = true;
        }
        log(
            "supervisor started",
            &[
                ("pid", i64::from(pid)),
                ("restarts", i64::from(self.supervisor_restarts)),
            ],
        );
    }

    fn schedule_restart(&mut self, now: u64) {
        if now.saturating_sub(self.supervisor_started_mono) >= KEEPER_BACKOFF_RESET.as_millis() as u64 {
            self.backoff = 0;
        }
        let delay = KEEPER_BACKOFF[self.backoff.min(KEEPER_BACKOFF.len() - 1)];
        self.backoff += 1;
        self.next_start_mono = Some(now + delay.as_millis() as u64);
    }

    // reaping

    fn reap_children(&mut self, now: u64) {
        if self.reap_all {
            while let Some(r) = sys::reap(-1) {
                self.on_reaped(r, now);
            }
        } else {
            if let Some(pid) = self.supervisor_pid
                && let Some(r) = sys::reap(pid)
            {
                self.on_reaped(r, now);
            }
            for i in 0..MAX_CHILDREN {
                let pid = self.children[i].pid;
                if pid > 0
                    && let Some(r) = sys::reap(pid)
                {
                    self.on_reaped(r, now);
                }
            }
        }
    }

    fn on_reaped(&mut self, r: sys::Reaped, now: u64) {
        let name = if Some(r.pid) == self.supervisor_pid {
            self.supervisor_pid = None;
            self.supervisor_restarts += 1;
            log(
                "supervisor exited",
                &[
                    ("pid", i64::from(r.pid)),
                    ("code", i64::from(r.code.unwrap_or(-1))),
                    ("signal", i64::from(r.signal.unwrap_or(0))),
                ],
            );
            // exit 0 after `RELEASED`: the release is final (4.3, 15.5); anything else is restarted
            let released = r.code == Some(0) && self.released;
            if released {
                self.supervisor_done = true;
                log("supervisor released, not restarted", &[]);
            } else if self.stopping_since.is_none() {
                self.schedule_restart(now);
            } else {
                // died during the stop without finishing the release: the next one is started at once and gets the
                // SIGTERM (its run.json and the keeper's state let it finish the forced release)
                self.stop_delivered = false;
                log("supervisor exited during the stop, starting another", &[]);
            }
            Name::new(b"supervisor")
        } else if let Some(i) = self.children.iter().position(|c| c.pid == r.pid) {
            if self.children[i].spawn_err >= 0 {
                // it ended before the loop read its error pipe: answer the SPAWN first
                self.resolve_spawn(i);
            }
            let name = self.children[i].name;
            let failed = self.children[i].failed;
            self.drop_child(i);
            if failed {
                return; // its exec failed and SPAWN said so: not an exit anyone waits for
            }
            name
        } else {
            // a reparented descendant (PID 1 reaps them all): counted, kept out of the ring of tracked exits
            self.orphans += 1;
            return;
        };
        self.exit_n += 1;
        self.exits[(self.exit_n as usize - 1) % KEEPER_EXITS] = Exit {
            n: self.exit_n,
            pid: r.pid,
            name,
            code: r.code,
            signal: r.signal,
            at_ms: sys::wall_ms(),
        };
    }

    fn drop_child(&mut self, i: usize) {
        if let Some(m) = self.children[i].msg {
            self.fail_msg(m, libc::EPIPE);
        }
        sys::close(self.children[i].stdin);
        self.children[i] = Child::EMPTY;
    }

    // connections

    fn accept_all(&mut self) {
        while let Ok(fd) = sys::accept(self.listener) {
            if let Some(c) = self.conn_state.iter().position(|s| *s == ConnState::Free) {
                self.conns[c] = fd;
                self.conn_state[c] = ConnState::Idle;
            } else {
                log("too many keeper connections", &[]);
                sys::close(fd);
            }
        }
    }

    fn close_conn(&mut self, c: usize) {
        match self.conn_state[c] {
            // the supervisor died mid-send: the incomplete message is discarded, never half-written
            ConnState::Receiving(m) => self.free_msg(m),
            ConnState::Waiting(m) => {
                if let Some(msg) = &mut self.msgs[m] {
                    msg.conn = None;
                }
            }
            ConnState::Spawning(i) => self.children[i].spawn_conn = None,
            _ => {}
        }
        sys::close(self.conns[c]);
        self.conns[c] = -1;
        self.conn_state[c] = ConnState::Free;
    }

    fn conn_ready(&mut self, c: usize, rev: i16) {
        if rev & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 && rev & libc::POLLIN == 0 {
            self.close_conn(c);
            return;
        }
        match self.conn_state[c] {
            ConnState::Idle => self.read_request(c),
            ConnState::Receiving(m) => self.read_more(c, m),
            _ => {}
        }
    }

    fn reply(&mut self, c: usize, len: usize, fd: Option<RawFd>) {
        if sys::send_packet(self.conns[c], &self.tx[..len], fd, false).is_err() {
            self.close_conn(c);
        }
    }

    fn reply_status(&mut self, c: usize, status: Status) {
        self.tx[0] = status as u8;
        self.reply(c, 1, None);
    }

    fn reply_err(&mut self, c: usize, errno: i32, what: &str) {
        let len = {
            let mut w = Writer::new(&mut self.tx[..]);
            w.u8(Status::Err as u8).i32(errno).bytes(what.as_bytes());
            w.len()
        };
        self.reply(c, len, None);
    }

    fn read_request(&mut self, c: usize) {
        // a STDIN header is peeked first: when its child is busy or no buffer is free it stays in the socket
        let mut head = [0u8; STDIN_HEADER];
        match sys::recv_packet(self.conns[c], &mut head, true, false) {
            Ok((0, _)) => return self.close_conn(c),
            Ok((n, _)) if n >= 1 && head[0] == Op::Stdin as u8 => {
                if n < STDIN_HEADER {
                    let _ = self.recv(c);
                    return self.reply_err(c, libc::EINVAL, "short STDIN header");
                }
                let pid = i32::from_le_bytes([head[1], head[2], head[3], head[4]]);
                if let Some(i) = self.child_index(pid)
                    && (self.children[i].msg.is_some() || !self.msgs.iter().any(Option::is_none))
                {
                    self.conn_state[c] = ConnState::Blocked;
                    return;
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => return self.close_conn(c),
        }
        let Some((n, fd)) = self.recv(c) else { return };
        let result = self.handle(c, n, fd);
        if let Err(Malformed) = result {
            self.reply_err(c, libc::EINVAL, "malformed request");
        }
    }

    /// Receives one packet into `rx`; `None` when the connection ended.
    fn recv(&mut self, c: usize) -> Option<(usize, Option<RawFd>)> {
        match sys::recv_packet(self.conns[c], &mut self.rx[..], false, false) {
            Ok((0, fd)) => {
                if let Some(fd) = fd {
                    sys::close(fd);
                }
                self.close_conn(c);
                None
            }
            Ok(r) => Some(r),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
            Err(_) => {
                self.close_conn(c);
                None
            }
        }
    }

    /// Wakes connections whose `STDIN` header waits for a child or a buffer.
    fn unblock(&mut self) {
        for c in 0..MAX_CONNS {
            if self.conn_state[c] == ConnState::Blocked {
                self.conn_state[c] = ConnState::Idle;
            }
        }
    }

    fn child_index(&self, pid: i32) -> Option<usize> {
        if pid <= 0 {
            return None;
        }
        self.children.iter().position(|ch| ch.pid == pid)
    }

    #[allow(clippy::too_many_lines)] // one arm per request
    fn handle(&mut self, c: usize, n: usize, fd: Option<RawFd>) -> Result<(), Malformed> {
        let op = Op::from_byte(*self.rx.first().ok_or(Malformed)?);
        if fd.is_some() && op != Some(Op::PutFd) {
            sys::close(fd.unwrap_or(-1));
        }
        match op {
            Some(Op::Spawn) => self.op_spawn(c, n),
            Some(Op::Stdin) => self.op_stdin(c, n),
            Some(Op::CloseStdin) => {
                let pid = Reader::new(&self.rx[1..n]).i32()?;
                match self.child_index(pid) {
                    Some(i) => {
                        if let Some(m) = self.children[i].msg {
                            self.fail_msg(m, libc::EPIPE);
                        }
                        sys::close(self.children[i].stdin);
                        self.children[i].stdin = -1;
                        self.reply_status(c, Status::Ok);
                    }
                    None => self.reply_err(c, libc::ESRCH, "no such child"),
                }
                Ok(())
            }
            Some(Op::Signal) => {
                let mut r = Reader::new(&self.rx[1..n]);
                let (target, sig) = (r.i32()?, r.i32()?);
                // only the keeper's own children or their process groups
                let known = self.child_index(target.checked_abs().unwrap_or(0)).is_some()
                    || Some(target) == self.supervisor_pid;
                if target == 0 || target == -1 || !known {
                    self.reply_err(c, libc::ESRCH, "not a child");
                } else {
                    match sys::kill(target, sig) {
                        Ok(()) => self.reply_status(c, Status::Ok),
                        Err(e) => self.reply_err(c, e.raw_os_error().unwrap_or(libc::EIO), "kill"),
                    }
                }
                Ok(())
            }
            Some(Op::PutFd) => {
                let name = Name::new(Reader::new(&self.rx[1..n]).bytes()?);
                let Some(fd) = fd else {
                    self.reply_err(c, libc::EINVAL, "no fd attached");
                    return Ok(());
                };
                let slot = self
                    .held
                    .iter()
                    .position(|(k, f)| *f >= 0 && k.get() == name.get())
                    .or_else(|| self.held.iter().position(|(_, f)| *f < 0));
                if let Some(s) = slot {
                    sys::close(self.held[s].1);
                    self.held[s] = (name, fd);
                    self.reply_status(c, Status::Ok);
                } else {
                    sys::close(fd);
                    self.reply_err(c, libc::ENOSPC, "fd table full");
                }
                Ok(())
            }
            Some(Op::GetFd) => {
                let name = Name::new(Reader::new(&self.rx[1..n]).bytes()?);
                match self.held.iter().find(|(k, f)| *f >= 0 && k.get() == name.get()) {
                    Some(&(_, fd)) => {
                        self.tx[0] = Status::Ok as u8;
                        self.reply(c, 1, Some(fd));
                    }
                    None => self.reply_status(c, Status::None),
                }
                Ok(())
            }
            Some(Op::Exits) => {
                let after = Reader::new(&self.rx[1..n]).u64()?;
                let len = {
                    let mut w = Writer::new(&mut self.tx[..]);
                    w.u8(Status::Ok as u8);
                    let first = self.exit_n.saturating_sub(KEEPER_EXITS as u64).max(after) + 1;
                    let count = (self.exit_n + 1).saturating_sub(first);
                    w.u32(count as u32);
                    for k in first..=self.exit_n {
                        let e = &self.exits[(k as usize - 1) % KEEPER_EXITS];
                        w.u64(e.n)
                            .i32(e.pid)
                            .bytes(e.name.get())
                            .opt_i32(e.code)
                            .opt_i32(e.signal)
                            .u64(e.at_ms);
                    }
                    w.len()
                };
                self.reply(c, len, None);
                Ok(())
            }
            Some(Op::Status) => {
                let len = {
                    let mut w = Writer::new(&mut self.tx[..]);
                    w.u8(Status::Ok as u8)
                        .bytes(&self.boot_id)
                        .u32(self.supervisor_restarts)
                        .u64(self.started_at_ms);
                    let live = |ch: &&Child| ch.pid > 0 && !ch.failed;
                    let count = self.children.iter().filter(live).count() + usize::from(self.supervisor_pid.is_some());
                    w.u32(count as u32);
                    if let Some(pid) = self.supervisor_pid {
                        w.i32(pid).bytes(b"supervisor");
                    }
                    for ch in self.children.iter().filter(live) {
                        w.i32(ch.pid).bytes(ch.name.get());
                    }
                    w.u64(self.orphans).u8(u8::from(self.released));
                    w.len()
                };
                self.reply(c, len, None);
                Ok(())
            }
            Some(Op::Released) => {
                if !self.released {
                    log("supervisor released", &[]);
                }
                self.released = true;
                self.reply_status(c, Status::Ok);
                Ok(())
            }
            Some(Op::StdinMore) | None => Err(Malformed),
        }
    }

    // SPAWN

    fn op_spawn(&mut self, c: usize, n: usize) -> Result<(), Malformed> {
        if self.stopping_since.is_some() {
            self.reply_err(c, libc::ESHUTDOWN, "stopping");
            return Ok(());
        }
        let Some(slot) = self.children.iter().position(|ch| ch.pid == 0) else {
            self.reply_err(c, libc::EAGAIN, "child table full");
            return Ok(());
        };
        let spec = parse_spawn(&self.rx[1..n], &mut self.spawn_buf[..])?;
        let mut stdin_pipe = None;
        if spec.stdin_pipe {
            match sys::pipe() {
                Ok(p) => stdin_pipe = Some(p),
                Err(e) => {
                    self.reply_err(c, e.raw_os_error().unwrap_or(libc::EIO), "pipe");
                    return Ok(());
                }
            }
        }
        let Ok((err_r, err_w)) = sys::pipe() else {
            if let Some((r, w)) = stdin_pipe {
                sys::close(r);
                sys::close(w);
            }
            self.reply_err(c, libc::EMFILE, "pipe");
            return Ok(());
        };
        // SAFETY: fork; the child runs `exec_child`, which only calls async-signal-safe functions on memory
        // prepared before the fork (the spawn buffer and its pointer arrays).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: the child after fork; exec_child never returns.
            unsafe { exec_child(&spec, stdin_pipe.map(|p| p.0), err_w) };
        }
        sys::close(err_w);
        if let Some((r, _)) = stdin_pipe {
            sys::close(r);
        }
        if pid < 0 {
            let e = sys::errno();
            sys::close(err_r);
            if let Some((_, w)) = stdin_pipe {
                sys::close(w);
            }
            self.reply_err(c, e, "fork");
            return Ok(());
        }
        // the child's exec closes the error pipe (success) or writes its errno into it; the loop polls it, so a
        // child slow to reach exec holds up only this SPAWN's answer
        let stdin = match stdin_pipe {
            Some((_, w)) => {
                let _ = sys::set_nonblocking(w);
                w
            }
            None => -1,
        };
        self.children[slot] = Child {
            pid,
            name: spec.name,
            stdin,
            spawn_err: err_r,
            spawn_conn: Some(c),
            since: self.exit_n,
            ..Child::EMPTY
        };
        self.conn_state[c] = ConnState::Spawning(slot);
        Ok(())
    }

    /// Reads child `i`'s error pipe (readable, or the child has ended) and answers its `SPAWN`.
    fn resolve_spawn(&mut self, i: usize) {
        let fd = self.children[i].spawn_err;
        if fd < 0 {
            return;
        }
        let mut eb = [0u8; 4];
        let got = loop {
            match sys::read(fd, &mut eb) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                r => break r.unwrap_or(0),
            }
        };
        sys::close(fd);
        self.children[i].spawn_err = -1;
        let conn = self.children[i].spawn_conn.take();
        if got > 0 {
            // exec failed: the child exits 127 and is reaped without an exit record
            let e = if got == 4 { i32::from_le_bytes(eb) } else { libc::EIO };
            self.children[i].failed = true;
            if let Some(m) = self.children[i].msg {
                self.fail_msg(m, libc::EPIPE);
            }
            sys::close(self.children[i].stdin);
            self.children[i].stdin = -1;
            if let Some(c) = conn {
                self.conn_state[c] = ConnState::Idle;
                self.reply_err(c, e, "exec");
            }
            return;
        }
        if let Some(c) = conn {
            self.conn_state[c] = ConnState::Idle;
            let (pid, since) = (self.children[i].pid, self.children[i].since);
            let len = {
                let mut w = Writer::new(&mut self.tx[..]);
                w.u8(Status::Ok as u8).i32(pid).u64(since);
                w.len()
            };
            self.reply(c, len, None);
        }
    }

    // STDIN

    fn op_stdin(&mut self, c: usize, n: usize) -> Result<(), Malformed> {
        let mut r = Reader::new(&self.rx[1..n]);
        let pid = r.i32()?;
        let id = r.msg_id()?;
        let total = r.u64()?;
        let first = r.rest().len();
        let Some(child) = self.child_index(pid) else {
            self.reply_err(c, libc::ESRCH, "no such child");
            return Ok(());
        };
        if self.children[child].stdin < 0 {
            self.reply_err(c, libc::EPIPE, "stdin closed");
            return Ok(());
        }
        let Some(m) = self.msgs.iter().position(Option::is_none) else {
            // cannot happen: read_request blocks the connection first
            self.reply_err(c, libc::EAGAIN, "busy");
            return Ok(());
        };
        let discard = self.children[child].seen(&id);
        let spooled = !discard && total > STDIN_BUFFER as u64;
        if spooled {
            sys::close(self.spools[m]);
            match sys::open(
                &self.spool_paths[m],
                libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
                0o600,
            ) {
                Ok(fd) => self.spools[m] = fd,
                Err(e) => {
                    self.spools[m] = -1;
                    self.reply_err(c, e.raw_os_error().unwrap_or(libc::EIO), "spool");
                    return Ok(());
                }
            }
        }
        self.msgs[m] = Some(Msg {
            child,
            conn: Some(c),
            id,
            total,
            got: 0,
            written: 0,
            discard,
            spooled,
            chunk_start: 0,
            chunk_len: 0,
            phase: Phase::Receiving,
        });
        self.children[child].msg = Some(m);
        self.conn_state[c] = ConnState::Receiving(m);
        self.stage(m, STDIN_HEADER, first);
        Ok(())
    }

    fn read_more(&mut self, c: usize, m: usize) {
        let Some((n, _)) = self.recv(c) else { return };
        if n == 0 || self.rx[0] != Op::StdinMore as u8 {
            self.free_msg(m);
            self.conn_state[c] = ConnState::Idle;
            self.reply_err(c, libc::EINVAL, "expected STDIN_MORE");
            return;
        }
        self.stage(m, 1, n - 1);
    }

    /// Adds `rx[at..at + len]` to message `m`; once complete, starts writing (or answers `ALREADY`).
    fn stage(&mut self, m: usize, at: usize, len: usize) {
        let Some(mut msg) = self.msgs[m] else { return };
        let Some(c) = msg.conn else { return };
        if msg.got + len as u64 > msg.total {
            self.free_msg(m);
            self.conn_state[c] = ConnState::Idle;
            self.reply_err(c, libc::EINVAL, "more bytes than announced");
            return;
        }
        if !msg.discard {
            let data = &self.rx[at..at + len];
            if msg.spooled {
                if let Err(e) = spool_write(self.spools[m], data, msg.got) {
                    self.free_msg(m);
                    self.conn_state[c] = ConnState::Idle;
                    self.reply_err(c, e.raw_os_error().unwrap_or(libc::EIO), "spool");
                    return;
                }
            } else {
                let start = msg.got as usize;
                self.buffers[m][start..start + len].copy_from_slice(data);
                msg.chunk_len = start + len;
            }
        }
        msg.got += len as u64;
        if msg.got < msg.total {
            self.msgs[m] = Some(msg);
            return;
        }
        if msg.discard {
            self.free_msg(m);
            self.conn_state[c] = ConnState::Idle;
            self.answer_msg(c, Status::Already, &msg.id);
            return;
        }
        msg.phase = Phase::Writing;
        self.msgs[m] = Some(msg);
        self.conn_state[c] = ConnState::Waiting(m);
        self.pipe_ready(m);
    }

    fn answer_msg(&mut self, c: usize, status: Status, id: &[u8; MSG_ID]) {
        self.tx[0] = status as u8;
        self.tx[1..=MSG_ID].copy_from_slice(id);
        self.reply(c, 1 + MSG_ID, None);
    }

    /// Copies as much of message `m` into its child's stdin as the pipe takes.
    fn pipe_ready(&mut self, m: usize) {
        let Some(mut msg) = self.msgs[m] else { return };
        if msg.phase != Phase::Writing {
            return; // the slot was reused within one poll pass
        }
        let fd = self.children[msg.child].stdin;
        while msg.written < msg.total {
            let (buf_from, buf_to) = if msg.spooled {
                if msg.written >= msg.chunk_start + msg.chunk_len as u64 {
                    let want = ((msg.total - msg.written) as usize).min(STDIN_BUFFER);
                    match sys::pread(self.spools[m], &mut self.buffers[m][..want], msg.written) {
                        Ok(k) if k > 0 => {
                            msg.chunk_start = msg.written;
                            msg.chunk_len = k;
                        }
                        _ => return self.fail_msg(m, libc::EIO),
                    }
                }
                let off = (msg.written - msg.chunk_start) as usize;
                (off, msg.chunk_len)
            } else {
                (msg.written as usize, msg.total as usize)
            };
            match sys::write(fd, &self.buffers[m][buf_from..buf_to]) {
                Ok(k) => msg.written += k as u64,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.msgs[m] = Some(msg);
                    return;
                }
                Err(e) => return self.fail_msg(m, e.raw_os_error().unwrap_or(libc::EPIPE)),
            }
        }
        // the whole message is in the pipe
        self.children[msg.child].remember(msg.id);
        let conn = msg.conn;
        self.free_msg(m);
        if let Some(c) = conn {
            self.conn_state[c] = ConnState::Idle;
            self.answer_msg(c, Status::Ok, &msg.id);
        }
    }

    fn fail_msg(&mut self, m: usize, errno: i32) {
        let Some(msg) = self.msgs[m] else { return };
        self.free_msg(m);
        if let Some(c) = msg.conn
            && self.conn_state[c] != ConnState::Free
        {
            self.conn_state[c] = ConnState::Idle;
            self.reply_err(c, errno, "stdin write failed");
        }
    }

    fn free_msg(&mut self, m: usize) {
        if let Some(msg) = self.msgs[m].take() {
            if self.children[msg.child].msg == Some(m) {
                self.children[msg.child].msg = None;
            }
            if msg.spooled {
                sys::close(self.spools[m]);
                self.spools[m] = -1;
            }
        }
        self.unblock();
    }
}

fn spool_write(fd: RawFd, data: &[u8], off: u64) -> io::Result<()> {
    let mut done = 0;
    while done < data.len() {
        // SAFETY: pwrite from a valid slice.
        let k = unsafe {
            libc::pwrite(
                fd,
                data[done..].as_ptr().cast(),
                data.len() - done,
                (off + done as u64) as libc::off_t,
            )
        };
        if k <= 0 {
            return Err(io::Error::last_os_error());
        }
        done += k as usize;
    }
    Ok(())
}

/// A parsed `SPAWN`, its strings NUL-terminated inside the spawn buffer.
struct Spawn {
    name: Name,
    argv: [*const libc::c_char; MAX_ARGS + 1],
    envp: [*const libc::c_char; MAX_ARGS + 1],
    cwd: *const libc::c_char,
    uid: u32,
    gid: u32,
    setsid: bool,
    oom: i32,
    cgroup_procs: *const libc::c_char,
    stdout: *const libc::c_char,
    stderr: *const libc::c_char,
    stdin_pipe: bool,
}

/// Copies `src` into `buf` at `*at` with a NUL; the pointer to it.
fn place(buf: &mut [u8], at: &mut usize, src: &[u8], suffix: &[u8]) -> Result<*const libc::c_char, Malformed> {
    if src.contains(&0) {
        return Err(Malformed);
    }
    let end = *at + src.len() + suffix.len() + 1;
    if end > buf.len() {
        return Err(Malformed);
    }
    buf[*at..*at + src.len()].copy_from_slice(src);
    buf[*at + src.len()..end - 1].copy_from_slice(suffix);
    buf[end - 1] = 0;
    let p = buf[*at..].as_ptr().cast();
    *at = end;
    Ok(p)
}

fn parse_spawn(pkt: &[u8], buf: &mut [u8]) -> Result<Spawn, Malformed> {
    let mut r = Reader::new(pkt);
    let mut at = 0;
    let name = Name::new(r.bytes()?);
    let mut argv = [std::ptr::null(); MAX_ARGS + 1];
    let argc = r.u32()? as usize;
    if argc == 0 || argc > MAX_ARGS {
        return Err(Malformed);
    }
    for a in argv.iter_mut().take(argc) {
        *a = place(buf, &mut at, r.bytes()?, b"")?;
    }
    let mut envp = [std::ptr::null(); MAX_ARGS + 1];
    let envc = r.u32()? as usize;
    if envc > MAX_ARGS {
        return Err(Malformed);
    }
    for e in envp.iter_mut().take(envc) {
        *e = place(buf, &mut at, r.bytes()?, b"")?;
    }
    let cwd = r.bytes()?;
    let cwd = if cwd.is_empty() {
        std::ptr::null()
    } else {
        place(buf, &mut at, cwd, b"")?
    };
    let uid = r.u32()?;
    let gid = r.u32()?;
    let setsid = r.u8()? != 0;
    let oom = r.i32()?;
    let cgroup = r.bytes()?;
    let cgroup_procs = if cgroup.is_empty() {
        std::ptr::null()
    } else {
        place(buf, &mut at, cgroup, b"/cgroup.procs")?
    };
    let out = r.bytes()?;
    let stdout = if out.is_empty() {
        std::ptr::null()
    } else {
        place(buf, &mut at, out, b"")?
    };
    let err = r.bytes()?;
    let stderr = if err.is_empty() {
        std::ptr::null()
    } else {
        place(buf, &mut at, err, b"")?
    };
    let stdin_pipe = r.u8()? != 0;
    Ok(Spawn {
        name,
        argv,
        envp,
        cwd,
        uid,
        gid,
        setsid,
        oom,
        cgroup_procs,
        stdout,
        stderr,
        stdin_pipe,
    })
}

/// Restores a default signal state in a child: nothing blocked, SIGPIPE back to its default.
///
/// # Safety
/// Only between fork and exec (async-signal-safe calls only).
unsafe fn child_signals() {
    // SAFETY: an all-zero sigset_t is a valid argument to sigemptyset; both calls are async-signal-safe.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const set, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The child half of `SPAWN`: never returns. Writes its errno into `err_w` when it cannot exec.
///
/// # Safety
/// Only between fork and exec: every call is async-signal-safe and every pointer in `s` points into memory the
/// parent prepared before the fork.
unsafe fn exec_child(s: &Spawn, stdin_r: Option<RawFd>, err_w: RawFd) -> ! {
    // SAFETY: async-signal-safe calls on memory prepared before the fork; every path ends in execve or _exit.
    unsafe {
        let fail = |what: libc::c_int| -> ! {
            let e = *libc::__errno_location();
            let b = if e == 0 { what } else { e }.to_le_bytes();
            libc::write(err_w, b.as_ptr().cast(), 4);
            libc::_exit(127)
        };
        child_signals();
        if s.setsid && libc::setsid() < 0 {
            fail(libc::EPERM);
        }
        if !s.cgroup_procs.is_null() {
            // the cgroup is best effort (supervisor 5.2): a failure leaves the child where it is
            let fd = libc::open(s.cgroup_procs, libc::O_WRONLY | libc::O_CLOEXEC);
            if fd >= 0 {
                let mut d = [0u8; 24];
                let txt = decimal(i64::from(libc::getpid()), &mut d);
                libc::write(fd, txt.as_ptr().cast(), txt.len());
                libc::close(fd);
            }
        }
        {
            let fd = libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd >= 0 {
                let mut d = [0u8; 24];
                let txt = decimal(i64::from(s.oom), &mut d);
                libc::write(fd, txt.as_ptr().cast(), txt.len());
                libc::close(fd);
            }
        }
        let devnull = c"/dev/null".as_ptr();
        let stdin_fd = match stdin_r {
            Some(fd) => fd,
            None => libc::open(devnull, libc::O_RDONLY | libc::O_CLOEXEC),
        };
        if stdin_fd < 0 || libc::dup2(stdin_fd, 0) < 0 {
            fail(libc::EBADF);
        }
        for (path, target) in [(s.stdout, 1), (s.stderr, 2)] {
            let p = if path.is_null() { devnull } else { path };
            let fd = libc::open(
                p,
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
                0o640,
            );
            if fd < 0 || libc::dup2(fd, target) < 0 {
                fail(libc::EBADF);
            }
        }
        if libc::getuid() == 0 {
            if libc::setgroups(0, std::ptr::null()) < 0 || libc::setgid(s.gid) < 0 || libc::setuid(s.uid) < 0 {
                fail(libc::EPERM);
            }
        } else if s.uid != libc::getuid() || s.gid != libc::getgid() {
            fail(libc::EPERM);
        }
        if !s.cwd.is_null() && libc::chdir(s.cwd) < 0 {
            fail(libc::ENOENT);
        }
        libc::execve(s.argv[0], s.argv.as_ptr(), s.envp.as_ptr());
        fail(libc::ENOEXEC)
    }
}

/// The set of SIGTERM and SIGINT.
fn stop_set() -> libc::sigset_t {
    // SAFETY: an all-zero sigset_t is a valid argument to sigemptyset; sigaddset on a local.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGTERM);
        libc::sigaddset(&raw mut set, libc::SIGINT);
        set
    }
}

/// The supervisor's child half: only SIGTERM and SIGINT blocked (inherited blocked from the keeper, so they never
/// are unblocked between the fork and the supervisor's handlers), SIGPIPE back to its default.
///
/// # Safety
/// Only between fork and exec (async-signal-safe calls only); `set` is prepared before the fork.
unsafe fn supervisor_child_signals(set: &libc::sigset_t) {
    // SAFETY: async-signal-safe calls on a set prepared before the fork.
    unsafe {
        libc::sigprocmask(libc::SIG_SETMASK, set, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Blocks SIGTERM, SIGINT and SIGCHLD and returns a `signalfd` for them.
fn process_signals() -> io::Result<RawFd> {
    // SAFETY: building a signal set in a local and blocking it; signalfd returns a new fd.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGCHLD] {
            libc::sigaddset(&raw mut set, s);
        }
        if libc::sigprocmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut()) < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = libc::signalfd(-1, &raw const set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }
}

/// `f_type` is `u64` on glibc and `i64` on musl; compared as bits.
fn same_magic<A: Copy + Into<i128>, B: Copy + Into<i128>>(a: A, b: B) -> bool {
    a.into() == b.into()
}

fn mount_tmpfs(dir: &Path) {
    let Ok(c) = cstr(dir) else { return };
    // SAFETY: statfs into a local struct, then mount with static strings.
    unsafe {
        let mut st: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &raw mut st) == 0 && same_magic(st.f_type, libc::TMPFS_MAGIC) {
            return;
        }
        let r = libc::mount(
            c"tmpfs".as_ptr(),
            c.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=0755,size=1m".as_ptr().cast(),
        );
        if r != 0 {
            log_at(Level::Warn, "tmpfs mount failed", &[("errno", i64::from(sys::errno()))]);
        }
    }
}

/// The sandbox's `boot_id` (16 random hex characters), created unless present: a restored sandbox keeps it, since
/// the tmpfs is part of the checkpoint.
fn boot_id(run_dir: &Path) -> io::Result<[u8; 16]> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let path = run_dir.join(super::BOOT_ID_NAME);
    if let Ok(b) = std::fs::read(&path)
        && let Ok(id) = <[u8; 16]>::try_from(b.trim_ascii())
        && id.iter().all(u8::is_ascii_hexdigit)
    {
        return Ok(id);
    }
    let mut raw = [0u8; 8];
    sys::random(&mut raw)?;
    let mut out = [0u8; 16];
    for (i, b) in raw.iter().enumerate() {
        out[2 * i] = HEX[(b >> 4) as usize];
        out[2 * i + 1] = HEX[(b & 15) as usize];
    }
    let tmp = run_dir.join(".boot_id.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &path)?;
    Ok(out)
}

impl Keeper {
    /// The `boot_id` this keeper reports.
    #[must_use]
    pub fn boot_id(&self) -> &[u8] {
        &self.boot_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_numbers_format_without_allocating() {
        let mut t = [0u8; 24];
        assert_eq!(rfc3339(1_793_620_800_123, &mut t), b"2026-11-02T12:00:00.123Z");
        assert_eq!(rfc3339(951_782_400_000, &mut t), b"2000-02-29T00:00:00.000Z");
        let mut d = [0u8; 24];
        assert_eq!(decimal(-1000, &mut d), b"-1000");
        assert_eq!(decimal(0, &mut d), b"0");
        assert_eq!(decimal(i64::MIN, &mut d), b"-9223372036854775808");
    }

    #[test]
    fn spawn_parsing_rejects_nul_and_empty_argv() {
        let mut pkt = [0u8; 256];
        let mut buf = [0u8; 1024];
        let mut w = Writer::new(&mut pkt);
        w.bytes(b"exec").u32(1).bytes(b"/bin/t\0rue").u32(0);
        let p = w.finish().unwrap().to_vec();
        assert!(parse_spawn(&p, &mut buf).is_err());
        let mut pkt = [0u8; 256];
        let mut w = Writer::new(&mut pkt);
        w.bytes(b"exec").u32(0);
        let p = w.finish().unwrap().to_vec();
        assert!(parse_spawn(&p, &mut buf).is_err());
    }

    #[test]
    fn orphans_are_counted_and_kept_out_of_the_exits_ring() {
        let dir = std::env::temp_dir().join(format!("keeper-orphans-{}", uuid::Uuid::new_v4().simple()));
        let (r, w) = sys::pipe().unwrap();
        let mut k = Keeper::new(Config {
            run_dir: dir.join("run"),
            mount_run_tmpfs: false,
            spool_dir: dir.join("spool"),
            supervisor: None,
            protect_self: false,
            signals: Signals::Pipe(r),
            stop_grace: KEEPER_STOP_GRACE,
        })
        .unwrap();
        k.children[0] = Child {
            pid: 4242,
            name: Name::new(b"claude"),
            ..Child::EMPTY
        };
        for pid in 10_000..10_100 {
            k.on_reaped(
                sys::Reaped {
                    pid,
                    code: Some(0),
                    signal: None,
                },
                0,
            );
        }
        k.on_reaped(
            sys::Reaped {
                pid: 4242,
                code: Some(3),
                signal: None,
            },
            0,
        );
        assert_eq!((k.orphans, k.exit_n), (100, 1));
        assert_eq!((k.exits[0].pid, k.exits[0].code), (4242, Some(3)));
        sys::close(w);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_ring_keeps_the_last_sixteen() {
        let mut c = Child::EMPTY;
        for i in 0..20u8 {
            c.remember([i; MSG_ID]);
        }
        assert!(!c.seen(&[3; MSG_ID]));
        assert!(c.seen(&[4; MSG_ID]));
        assert!(c.seen(&[19; MSG_ID]));
    }
}
