//! Running host commands (runsc, ip, nft, wg, crane, tar, curl) without a heap: fork + execve with argv/envp built
//! in a stack buffer, output read through poll into the caller's fixed buffer (the rest is drained and counted, never
//! stored). Children go into the helpers cgroup (their memory is capped there, not in sandboxd's), have SIGPIPE back
//! at its default and only fds 0-2. Nothing here ever waits for "any child": only for the pid it started.
use crate::err;
use crate::fixed::{Str, cstr};
use crate::util::{Error, Res, mono_ms};
use core::ffi::c_char;
use core::sync::atomic::{AtomicI32, Ordering::Relaxed};

/// fd of <helpers cgroup>/cgroup.procs, opened at start (-1: no cgroup placement).
static HELPERS_CG: AtomicI32 = AtomicI32::new(-1);
/// A replacement for the runsc binary (tests).
static mut RUNSC: Str<256> = Str::new();
/// $PATH at start (std::env::var allocates; read once).
static mut PATH: Str<2048> = Str::new();

pub fn set_helpers_cgroup(fd: i32) {
    HELPERS_CG.store(fd, Relaxed);
}

pub fn init_runsc() {
    let p = std::env::var_os("SBX_RUNSC");
    let v = p.as_ref().and_then(|s| s.to_str()).unwrap_or("runsc");
    // SAFETY: called once at start before any other thread exists.
    unsafe { (*core::ptr::addr_of_mut!(RUNSC)).set(v) };
    let path =
        std::env::var("PATH").unwrap_or_else(|_| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    unsafe { (*core::ptr::addr_of_mut!(PATH)).push_lossy(&path) };
}

pub fn runsc_bin() -> &'static str {
    // SAFETY: written once at start, read-only after.
    unsafe { (*core::ptr::addr_of!(RUNSC)).as_str() }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Io {
    Null,
    Pipe,
    Fd(i32),
}

const ARGS: usize = 64;
const ENVS: usize = 512;

/// argv + envp, prepared before fork (no allocation in the child).
pub struct Prepared {
    strings: [u8; 32768],
    used: usize,
    argv: [*const c_char; ARGS + 1],
    envp: [*const c_char; ENVS + 2],
    path: [u8; 512],
}

impl Prepared {
    fn add(&mut self, s: &str) -> Option<*const c_char> {
        if s.as_bytes().contains(&0) || self.used + s.len() + 1 > self.strings.len() {
            return None;
        }
        let p = self.used;
        self.strings[p..p + s.len()].copy_from_slice(s.as_bytes());
        self.strings[p + s.len()] = 0;
        self.used += s.len() + 1;
        Some(self.strings[p..].as_ptr() as *const c_char)
    }
}

/// Fills `p` for `argv` (argv[0] looked up in PATH) and the environment plus `extra_env` ("K=V").
fn prepare(p: &mut Prepared, argv: &[&str], extra_env: &[&str]) -> Res<()> {
    if argv.is_empty() || argv.len() > ARGS {
        return Err(Error::new(500, "bad command"));
    }
    p.used = 0;
    resolve(argv[0], &mut p.path).ok_or_else(|| err!(500, "{}: command not found", argv[0]))?;
    for (i, a) in argv.iter().enumerate() {
        p.argv[i] = p.add(a).ok_or_else(|| Error::new(500, "command line too long"))?;
    }
    p.argv[argv.len()] = core::ptr::null();
    // the environment sandboxd got (unit's PATH etc.), minus keys overridden by extra_env
    let mut n = 0;
    unsafe extern "C" {
        static environ: *const *const c_char;
    }
    // SAFETY: environ is NULL-terminated and not modified after start.
    unsafe {
        let mut e = environ;
        while !e.is_null() && !(*e).is_null() && n < ENVS {
            let entry = core::ffi::CStr::from_ptr(*e).to_bytes();
            let key = entry.split(|c| *c == b'=').next().unwrap_or(b"");
            if !extra_env.iter().any(|x| x.as_bytes().split(|c| *c == b'=').next() == Some(key)) {
                p.envp[n] = *e;
                n += 1;
            }
            e = e.add(1);
        }
        if !e.is_null() && !(*e).is_null() {
            return Err(Error::new(500, "too many environment variables"));
        }
    }
    for x in extra_env {
        if n >= ENVS {
            break;
        }
        p.envp[n] = p.add(x).ok_or_else(|| Error::new(500, "environment too long"))?;
        n += 1;
    }
    p.envp[n] = core::ptr::null();
    Ok(())
}

/// argv[0] → an executable path in `out` (NUL-terminated).
fn resolve(cmd: &str, out: &mut [u8]) -> Option<()> {
    if cmd.contains('/') {
        cstr(cmd, out)?;
        return Some(());
    }
    // SAFETY: written once at start.
    let path = unsafe { (*core::ptr::addr_of!(PATH)).as_str() };
    let path = if path.is_empty() { "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" } else { path };
    for dir in path.split(':') {
        let mut s: Str<500> = Str::new();
        if !(s.push(dir) && s.push("/") && s.push(cmd)) {
            continue;
        }
        let c = s.cstr(out)?;
        if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
            return Some(());
        }
    }
    None
}

pub struct Child {
    pub pid: i32,
    pub stdout: i32,
    pub stderr: i32,
}

fn pipe() -> Res<(i32, i32)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(err!(500, "pipe: {}", crate::util::io_msg(&std::io::Error::last_os_error())));
    }
    Ok((fds[0], fds[1]))
}

fn close(fd: i32) {
    if fd >= 0 {
        unsafe { libc::close(fd) };
    }
}

/// Starts a command. `stdin`/`stdout`/`stderr`: Null, Pipe (we read stdout/stderr; stdin Pipe is not offered) or an
/// fd (dup'ed into place; the caller keeps and closes its own).
pub fn spawn(argv: &[&str], extra_env: &[&str], stdin: Io, stdout: Io, stderr: Io) -> Res<Child> {
    let mut p = Prepared {
        strings: [0; 32768],
        used: 0,
        argv: [core::ptr::null(); ARGS + 1],
        envp: [core::ptr::null(); ENVS + 2],
        path: [0; 512],
    };
    prepare(&mut p, argv, extra_env)?;
    let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if devnull < 0 {
        return Err(Error::new(500, "open /dev/null"));
    }
    let mut out_r = -1;
    let mut err_r = -1;
    let target = |io: Io, is_out: bool, keep: &mut i32| -> Res<i32> {
        Ok(match io {
            Io::Null => devnull,
            Io::Fd(fd) => fd,
            Io::Pipe => {
                if !is_out {
                    return Ok(devnull);
                }
                let (r, w) = pipe()?;
                *keep = r;
                w
            }
        })
    };
    let fd0 = target(stdin, false, &mut -1)?;
    let fd1 = match target(stdout, true, &mut out_r) {
        Ok(f) => f,
        Err(e) => {
            close(devnull);
            return Err(e);
        }
    };
    let fd2 = match target(stderr, true, &mut err_r) {
        Ok(f) => f,
        Err(e) => {
            close(devnull);
            close(out_r);
            if stdout == Io::Pipe {
                close(fd1);
            }
            return Err(e);
        }
    };
    let cg = HELPERS_CG.load(Relaxed);
    // SAFETY: after fork the child only makes raw system calls (async-signal-safe) on memory prepared above, then
    // execs or _exits.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            if cg >= 0 {
                libc::write(cg, b"0".as_ptr() as *const _, 1);
            }
            // stdio: dup2 into 0..2 (sources are >= 3: 0..2 are always open in sandboxd)
            libc::dup2(fd0, 0);
            libc::dup2(fd1, 1);
            libc::dup2(fd2, 2);
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
            let mut set: libc::sigset_t = core::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, core::ptr::null_mut());
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::execve(p.path.as_ptr() as *const c_char, p.argv.as_ptr(), p.envp.as_ptr());
            libc::_exit(127);
        }
    }
    let fork_err = std::io::Error::last_os_error();
    close(devnull);
    if stdout == Io::Pipe {
        close(fd1);
    }
    if stderr == Io::Pipe {
        close(fd2);
    }
    if pid < 0 {
        close(out_r);
        close(err_r);
        return Err(err!(500, "fork: {}", crate::util::io_msg(&fork_err)));
    }
    Ok(Child { pid, stdout: out_r, stderr: err_r })
}

/// waitpid for this pid. `block` false: None while it runs. The exit code, or -1 for a signal (as Node's `code ?? -1`).
pub fn wait(pid: i32, block: bool) -> Option<i32> {
    let mut st = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut st, if block { 0 } else { libc::WNOHANG }) };
        if r == pid {
            return Some(if libc::WIFEXITED(st) { libc::WEXITSTATUS(st) } else { -1 });
        }
        if r == 0 {
            return None;
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Some(-1); // ECHILD: already reaped
    }
}

pub fn kill(pid: i32) {
    if pid > 0 {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

pub struct Output<'a> {
    pub code: i32,
    pub out: &'a [u8],
    /// stdout was longer than the buffer (the rest was read and dropped)
    pub truncated: bool,
    pub timed_out: bool,
    /// the last bytes of stderr
    pub err: Str<600>,
}

impl Output<'_> {
    pub fn out_str(&self) -> &str {
        core::str::from_utf8(self.out).unwrap_or("")
    }
    /// The old message: "<cmd>: exit N: <stderr or stdout, trimmed, 500 chars>"
    pub fn failure(&self, argv: &[&str]) -> Error {
        let mut m: Str<480> = Str::new();
        for (i, a) in argv.iter().enumerate() {
            if i > 0 {
                m.push_lossy(" ");
            }
            m.push_lossy(a);
            if m.len() > 200 {
                break;
            }
        }
        let detail =
            if !self.err.as_str().trim().is_empty() { self.err.as_str().trim() } else { self.out_str().trim() };
        let mut e = Error::new(500, m.as_str());
        let _ = core::fmt::Write::write_fmt(
            &mut e.msg,
            format_args!(": exit {}{}: ", self.code, if self.timed_out { " (timed out)" } else { "" }),
        );
        e.msg.push_lossy(detail);
        e
    }
}

/// Reads the child's pipes until both close or it exited (plus a short grace for the rest), into `out` (stdout) and
/// a stderr tail; kills it after `timeout_ms` (0 = none). Then reaps it.
pub fn collect<'a>(c: &Child, out: &'a mut [u8], timeout_ms: u64) -> Output<'a> {
    let start = mono_ms();
    let mut n = 0usize;
    let mut truncated = false;
    let mut err_tail = [0u8; 600];
    let mut err_len = 0usize;
    let mut fds = [
        libc::pollfd { fd: c.stdout, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: c.stderr, events: libc::POLLIN, revents: 0 },
    ];
    let mut code: Option<i32> = None;
    let mut exited_at = 0u64;
    let mut timed_out = false;
    let mut scratch = [0u8; 4096];
    loop {
        let open = fds.iter().any(|f| f.fd >= 0);
        if !open {
            break;
        }
        if code.is_none() {
            code = wait(c.pid, false);
            if code.is_some() {
                exited_at = mono_ms();
            }
        }
        // a grandchild may hold the pipes open (runsc sidecars): stop 200 ms after the exit
        if code.is_some() && mono_ms() - exited_at > 200 {
            break;
        }
        if timeout_ms > 0 && !timed_out && mono_ms() - start > timeout_ms {
            kill(c.pid);
            timed_out = true;
        }
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, 50) };
        if r <= 0 {
            continue;
        }
        for (i, f) in fds.iter_mut().enumerate() {
            if f.fd < 0 || f.revents == 0 {
                continue;
            }
            let k = if i == 0 && n < out.len() {
                unsafe { libc::read(f.fd, out[n..].as_mut_ptr() as *mut _, out.len() - n) }
            } else {
                unsafe { libc::read(f.fd, scratch.as_mut_ptr() as *mut _, scratch.len()) }
            };
            if k <= 0 {
                if k < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                close(f.fd);
                f.fd = -1;
                continue;
            }
            let k = k as usize;
            if i == 0 {
                if n < out.len() {
                    n += k;
                } else {
                    truncated = true;
                }
            } else {
                // keep the last 600 bytes of stderr
                let src = &scratch[..k];
                if src.len() >= err_tail.len() {
                    let l = err_tail.len();
                    err_tail.copy_from_slice(&src[src.len() - l..]);
                    err_len = err_tail.len();
                } else {
                    let keep = (err_tail.len() - src.len()).min(err_len);
                    err_tail.copy_within(err_len - keep..err_len, 0);
                    err_tail[keep..keep + src.len()].copy_from_slice(src);
                    err_len = keep + src.len();
                }
            }
        }
    }
    for f in fds {
        close(f.fd);
    }
    let code = match code {
        Some(c) => c,
        None => {
            if timeout_ms > 0 && mono_ms() - start > timeout_ms {
                kill(c.pid);
                timed_out = true;
            }
            wait(c.pid, true).unwrap_or(-1)
        }
    };
    let mut et = &err_tail[..err_len];
    while !et.is_empty() && core::str::from_utf8(et).is_err() {
        et = &et[1..];
    }
    Output { code, out: &out[..n], truncated, timed_out, err: Str::lossy(core::str::from_utf8(et).unwrap_or("")) }
}

/// Runs a command to completion; stdout into `out`.
pub fn run<'a>(argv: &[&str], out: &'a mut [u8], timeout_ms: u64) -> Res<Output<'a>> {
    run_env(argv, &[], out, timeout_ms)
}

pub fn run_env<'a>(argv: &[&str], env: &[&str], out: &'a mut [u8], timeout_ms: u64) -> Res<Output<'a>> {
    let c = spawn(argv, env, Io::Null, Io::Pipe, Io::Pipe)?;
    Ok(collect(&c, out, timeout_ms))
}

/// Runs a command whose output doesn't matter; an error if it fails.
pub fn sh(argv: &[&str]) -> Res<()> {
    let mut out = [0u8; 512];
    let o = run(argv, &mut out, 60_000)?;
    if o.code != 0 { Err(o.failure(argv)) } else { Ok(()) }
}

/// Runs and ignores the result (cleanup steps that may fail: "a missing one is fine").
pub fn quiet(argv: &[&str]) -> i32 {
    let mut out = [0u8; 256];
    match run(argv, &mut out, 60_000) {
        Ok(o) => o.code,
        Err(_) => -1,
    }
}

/// `a | b` (stdout of a into stdin of b, directly; nothing passes through sandboxd). Errors carry both stderrs.
pub fn pipeline(a: &[&str], a_env: &[&str], b: &[&str], timeout_ms: u64) -> Res<()> {
    let (r, w) = pipe()?;
    let ca = match spawn(a, a_env, Io::Null, Io::Fd(w), Io::Pipe) {
        Ok(c) => c,
        Err(e) => {
            close(r);
            close(w);
            return Err(e);
        }
    };
    close(w);
    let cb = match spawn(b, &[], Io::Fd(r), Io::Null, Io::Pipe) {
        Ok(c) => c,
        Err(e) => {
            close(r);
            kill(ca.pid);
            wait(ca.pid, true);
            return Err(e);
        }
    };
    close(r);
    // b first: it ends when a's output ends (or a dies); then a
    let mut none = [0u8; 0];
    let ob = collect(&cb, &mut none, timeout_ms);
    if ob.code != 0 {
        kill(ca.pid);
    }
    let mut none2 = [0u8; 0];
    let oa = collect(&ca, &mut none2, if ob.code != 0 { 5_000 } else { timeout_ms });
    if oa.code != 0 || ob.code != 0 {
        let mut e = Error::new(500, a[0]);
        let _ = core::fmt::Write::write_fmt(
            &mut e.msg,
            format_args!(
                " | {}: exit {}/{}: {} {}",
                b[0],
                oa.code,
                ob.code,
                oa.err.as_str().trim(),
                ob.err.as_str().trim()
            ),
        );
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_bounds_output() {
        let mut out = [0u8; 8];
        let o = run(&["sh", "-c", "echo hello-world-long; echo oops >&2; exit 3"], &mut out, 5000).ok().unwrap();
        assert_eq!(o.code, 3);
        assert_eq!(o.out, b"hello-wo");
        assert!(o.truncated);
        assert_eq!(o.err.as_str().trim(), "oops");
        let o = run(&["sh", "-c", "sleep 5"], &mut out, 200).ok().unwrap();
        assert!(o.timed_out);
        assert_eq!(o.code, -1);
        assert!(run(&["no-such-command-xyz"], &mut out, 100).is_err());
        // a grandchild keeping stdout open does not hang us
        let t = mono_ms();
        let o = run(&["sh", "-c", "sleep 3 & echo x"], &mut out, 10_000).ok().unwrap();
        assert_eq!(o.code, 0);
        assert!(mono_ms() - t < 2000);
        pipeline(&["sh", "-c", "seq 1 100000"], &[], &["sh", "-c", "wc -l >/dev/null"], 10_000).ok().unwrap();
        assert!(pipeline(&["sh", "-c", "seq 1 10"], &[], &["sh", "-c", "exit 4"], 10_000).is_err());
        // a big stderr only keeps its tail
        let o = run(&["sh", "-c", "seq 1 100000 >&2; echo END >&2"], &mut out, 10_000).ok().unwrap();
        assert!(o.err.as_str().ends_with("END\n"));
    }
}
#[cfg(test)]
mod t2 {
    #[test]
    fn fake_runsc() {
        unsafe { std::env::set_var("SBX_FAKE", "/tmp/ff2") };
        let mut out = [0u8; 1024];
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/runsc");
        let o = super::run(&[p, "--root", "/x", "--net-raw", "cpu-features"], &mut out, 30_000).ok().unwrap();
        assert_eq!((o.code, o.out_str(), o.err.as_str()), (0, "fpu,sse2,avx\n", ""));
    }
}
