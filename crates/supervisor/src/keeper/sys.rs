//! Thin wrappers over the system calls the keeper and its client use, each one `unsafe` block with its `SAFETY`
//! note. Nothing here allocates.

#![allow(unsafe_code)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)] // C integer types
#![allow(clippy::cast_ptr_alignment)] // CMSG_DATA is read with read_unaligned

use std::io;
use std::os::fd::RawFd;

/// The last OS error.
#[must_use]
pub fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn check_size(r: libc::ssize_t) -> io::Result<usize> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as usize)
    }
}

/// `close(2)`, errors ignored (the fd is gone either way).
pub fn close(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: closing an fd this process owns; callers never use it afterwards.
        unsafe { libc::close(fd) };
    }
}

/// Sends `msg` with an optional fd as `SCM_RIGHTS`, never blocking and never raising SIGPIPE.
///
/// # Errors
/// The `sendmsg(2)` error.
pub fn send_packet(sock: RawFd, msg: &[u8], fd: Option<RawFd>, block: bool) -> io::Result<usize> {
    let mut cbuf = [0u64; 4]; // aligned room for one fd's control message
    // SAFETY: the iovec points into `msg`, which outlives the call; the control buffer is a local array large enough
    // for CMSG_SPACE(4) (24 bytes on 64-bit Linux) and is filled with the libc CMSG macros.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: msg.as_ptr() as *mut libc::c_void,
            iov_len: msg.len(),
        };
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &raw mut iov;
        m.msg_iovlen = 1;
        if let Some(fd) = fd {
            m.msg_control = cbuf.as_mut_ptr().cast();
            m.msg_controllen = libc::CMSG_SPACE(4) as _;
            let c = libc::CMSG_FIRSTHDR(&raw const m);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<i32>(), fd);
        }
        let flags = libc::MSG_NOSIGNAL | if block { 0 } else { libc::MSG_DONTWAIT };
        check_size(libc::sendmsg(sock, &raw const m, flags))
    }
}

/// Receives one packet into `buf`, and the first `SCM_RIGHTS` fd if one came (opened close-on-exec; any further fds
/// in the same message are closed). `Ok((0, None))` is the peer's end of the connection.
///
/// # Errors
/// The `recvmsg(2)` error (`WouldBlock` when non-blocking and nothing is there).
pub fn recv_packet(sock: RawFd, buf: &mut [u8], peek: bool, block: bool) -> io::Result<(usize, Option<RawFd>)> {
    let mut cbuf = [0u64; 8];
    // SAFETY: recvmsg into `buf` and a local control buffer; the control messages are walked with the libc CMSG
    // macros within `msg_controllen`, and every received fd is either returned or closed.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &raw mut iov;
        m.msg_iovlen = 1;
        if !peek {
            // a peek gets no room for descriptors, so none is installed until the real receive
            m.msg_control = cbuf.as_mut_ptr().cast();
            m.msg_controllen = std::mem::size_of_val(&cbuf) as _;
        }
        let mut flags = libc::MSG_CMSG_CLOEXEC;
        if peek {
            flags |= libc::MSG_PEEK;
        }
        if !block {
            flags |= libc::MSG_DONTWAIT;
        }
        let n = check_size(libc::recvmsg(sock, &raw mut m, flags))?;
        let mut first = None;
        if !peek {
            let mut c = libc::CMSG_FIRSTHDR(&raw const m);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(c).cast::<i32>();
                    let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / 4;
                    for i in 0..count {
                        let fd = std::ptr::read_unaligned(data.add(i));
                        if first.is_none() {
                            first = Some(fd);
                        } else {
                            libc::close(fd);
                        }
                    }
                }
                c = libc::CMSG_NXTHDR(&raw const m, c);
            }
        }
        Ok((n, first))
    }
}

/// A `sockaddr_un` for `path` (at most 107 bytes plus the NUL).
fn sockaddr(path: &[u8]) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: an all-zero sockaddr_un is valid; the path is copied within sun_path's bounds.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.len() >= addr.sun_path.len() || path.contains(&0) {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in addr.sun_path.iter_mut().zip(path) {
        *d = *s as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sa_family_t>() + path.len() + 1;
    Ok((addr, len as libc::socklen_t))
}

/// A listening `SOCK_SEQPACKET` socket at `path` (an old one removed first), mode 0600.
///
/// # Errors
/// The socket, bind, chmod or listen error.
pub fn listen_seqpacket(path: &[u8], path_c: &std::ffi::CStr) -> io::Result<RawFd> {
    let (addr, len) = sockaddr(path)?;
    // SAFETY: plain socket calls on a new fd and a valid address; the fd is closed on every error path.
    unsafe {
        libc::unlink(path_c.as_ptr());
        let fd = check(libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        ))?;
        let r = check(libc::bind(fd, (&raw const addr).cast(), len))
            .and_then(|_| check(libc::chmod(path_c.as_ptr(), 0o600)))
            .and_then(|_| check(libc::listen(fd, 16)));
        if let Err(e) = r {
            libc::close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

/// A blocking client connection to the `SOCK_SEQPACKET` socket at `path`.
///
/// # Errors
/// The socket or connect error.
pub fn connect_seqpacket(path: &[u8]) -> io::Result<RawFd> {
    let (addr, len) = sockaddr(path)?;
    // SAFETY: plain socket calls on a new fd and a valid address; the fd is closed on error.
    unsafe {
        let fd = check(libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
        ))?;
        if let Err(e) = check(libc::connect(fd, (&raw const addr).cast(), len)) {
            libc::close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

/// `accept4` with close-on-exec and non-blocking set.
///
/// # Errors
/// The accept error (`WouldBlock` when none is waiting).
pub fn accept(listener: RawFd) -> io::Result<RawFd> {
    // SAFETY: accept4 with null address pointers is allowed.
    unsafe {
        check(libc::accept4(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        ))
    }
}

/// A pipe `(read, write)`, both close-on-exec.
///
/// # Errors
/// The `pipe2(2)` error.
pub fn pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2 writes two fds into the local array.
    unsafe { check(libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC))? };
    Ok((fds[0], fds[1]))
}

/// Sets `O_NONBLOCK` on `fd`.
///
/// # Errors
/// The fcntl error.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on an fd this process owns.
    unsafe {
        let flags = check(libc::fcntl(fd, libc::F_GETFL))?;
        check(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
    }
    Ok(())
}

/// `write(2)` once.
///
/// # Errors
/// The write error (`WouldBlock` on a full non-blocking pipe, `BrokenPipe` when the reader is gone).
pub fn write(fd: RawFd, b: &[u8]) -> io::Result<usize> {
    // SAFETY: write from a valid slice.
    unsafe { check_size(libc::write(fd, b.as_ptr().cast(), b.len())) }
}

/// `read(2)` once.
///
/// # Errors
/// The read error.
pub fn read(fd: RawFd, b: &mut [u8]) -> io::Result<usize> {
    // SAFETY: read into a valid slice.
    unsafe { check_size(libc::read(fd, b.as_mut_ptr().cast(), b.len())) }
}

/// `pread(2)` once.
///
/// # Errors
/// The read error.
pub fn pread(fd: RawFd, b: &mut [u8], off: u64) -> io::Result<usize> {
    // SAFETY: pread into a valid slice.
    unsafe { check_size(libc::pread(fd, b.as_mut_ptr().cast(), b.len(), off as libc::off_t)) }
}

/// Writes all of `b` (blocking fds).
///
/// # Errors
/// The first write error.
pub fn write_all(fd: RawFd, mut b: &[u8]) -> io::Result<()> {
    while !b.is_empty() {
        match write(fd, b) {
            Ok(0) => return Err(io::Error::from_raw_os_error(libc::EIO)),
            Ok(n) => b = &b[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `open(2)` with `O_CLOEXEC` added.
///
/// # Errors
/// The open error.
pub fn open(path: &std::ffi::CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<RawFd> {
    // SAFETY: path is a valid NUL-terminated string.
    unsafe {
        check(libc::open(
            path.as_ptr(),
            flags | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        ))
    }
}

/// `kill(2)`.
///
/// # Errors
/// The kill error (`ESRCH` when the target is gone).
pub fn kill(target: i32, sig: i32) -> io::Result<()> {
    // SAFETY: kill has no memory effects.
    unsafe { check(libc::kill(target, sig)).map(|_| ()) }
}

/// What `waitpid` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reaped {
    pub pid: i32,
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// `waitpid(pid, WNOHANG)`: `None` when nothing (more) has exited.
#[must_use]
pub fn reap(pid: i32) -> Option<Reaped> {
    let mut status = 0;
    // SAFETY: waitpid writes the status into a local.
    let r = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
    if r <= 0 {
        return None;
    }
    let (code, signal) = if libc::WIFEXITED(status) {
        (Some(libc::WEXITSTATUS(status)), None)
    } else if libc::WIFSIGNALED(status) {
        (None, Some(libc::WTERMSIG(status)))
    } else {
        (None, None)
    };
    Some(Reaped { pid: r, code, signal })
}

/// Milliseconds on `clock`.
#[must_use]
pub fn clock_ms(clock: libc::clockid_t) -> u64 {
    // SAFETY: an all-zero timespec is valid; clock_gettime fills it.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: clock_gettime writes into the local timespec.
    unsafe { libc::clock_gettime(clock, &raw mut ts) };
    (ts.tv_sec as u64).saturating_mul(1000) + (ts.tv_nsec as u64) / 1_000_000
}

/// Monotonic milliseconds.
#[must_use]
pub fn mono_ms() -> u64 {
    clock_ms(libc::CLOCK_MONOTONIC)
}

/// Wall-clock milliseconds since the Unix epoch.
#[must_use]
pub fn wall_ms() -> u64 {
    clock_ms(libc::CLOCK_REALTIME)
}

/// `getpid(2)`.
#[must_use]
pub fn getpid() -> i32 {
    // SAFETY: no arguments, no memory effects.
    unsafe { libc::getpid() }
}

/// Random bytes from `getrandom(2)`.
///
/// # Errors
/// The getrandom error.
pub fn random(buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        // SAFETY: getrandom writes into the unfilled tail of `buf`.
        let n = unsafe { libc::getrandom(buf[done..].as_mut_ptr().cast(), buf.len() - done, 0) };
        done += check_size(n)?;
    }
    Ok(())
}
