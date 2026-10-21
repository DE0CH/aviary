//! Sockets with bounded kernel buffers: the kernel charges a socket's buffers to sandboxd's cgroup, and autotuning
//! would let each grow to megabytes (tcp_rmem/tcp_wmem max). Fixed SO_RCVBUF/SO_SNDBUF switch autotuning off.
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};

/// Each direction's buffer as requested (the kernel doubles it for bookkeeping: 2 x 2 x 64 KiB per socket).
pub const SOCK_BUF: i32 = 64 << 10;
/// Kernel memory one socket can hold at most (both buffers, doubled).
pub const SOCK_KERNEL_MAX: usize = 4 * SOCK_BUF as usize;

fn setopt(fd: i32, level: i32, opt: i32, v: i32) {
    unsafe { libc::setsockopt(fd, level, opt, &v as *const i32 as *const _, core::mem::size_of::<i32>() as u32) };
}

pub fn bound_buffers(s: &TcpStream) {
    setopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, SOCK_BUF);
    setopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, SOCK_BUF);
}

/// A listening TCP socket on ip:port with bounded buffers (inherited by accepted sockets, so the window scale they
/// negotiate fits) and a bounded accept queue.
pub fn listen(ip: &str, port: u16, backlog: i32) -> std::io::Result<TcpListener> {
    let addr: std::net::Ipv4Addr = ip.parse().map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let l = TcpListener::from_raw_fd(fd);
        setopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
        setopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, SOCK_BUF);
        setopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, SOCK_BUF);
        let sa = libc::sockaddr_in {
            sin_family: libc::AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr { s_addr: u32::from(addr).to_be() },
            sin_zero: [0; 8],
        };
        if libc::bind(fd, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32)
            != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::listen(fd, backlog) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(l)
    }
}
