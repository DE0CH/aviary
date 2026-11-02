//! The keeper: holds the `/dev/fuse` fd (PUT_FD) so the connection outlives a supervisor crash, hands it back
//! (GET_FD). Unix stream socket, one line per command, the fd as SCM_RIGHTS.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};

pub fn send_fd(s: &UnixStream, msg: &[u8], fd: Option<RawFd>) -> io::Result<()> {
    // SAFETY: plain sendmsg with a stack buffer for one fd's control message.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: msg.as_ptr() as *mut _,
            iov_len: msg.len(),
        };
        let mut cbuf = [0u8; 64];
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &mut iov;
        m.msg_iovlen = 1;
        if let Some(fd) = fd {
            m.msg_control = cbuf.as_mut_ptr().cast();
            m.msg_controllen = libc::CMSG_SPACE(4) as _;
            let c = libc::CMSG_FIRSTHDR(&m);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<i32>(), fd);
        }
        if libc::sendmsg(s.as_raw_fd(), &m, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn recv_fd(s: &UnixStream) -> io::Result<(Vec<u8>, Option<RawFd>)> {
    // SAFETY: recvmsg into stack buffers; the control message is parsed with the libc CMSG macros.
    unsafe {
        let mut buf = [0u8; 64];
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let mut cbuf = [0u8; 64];
        let mut m: libc::msghdr = std::mem::zeroed();
        m.msg_iov = &mut iov;
        m.msg_iovlen = 1;
        m.msg_control = cbuf.as_mut_ptr().cast();
        m.msg_controllen = cbuf.len() as _;
        let n = libc::recvmsg(s.as_raw_fd(), &mut m, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fd = None;
        let c = libc::CMSG_FIRSTHDR(&m);
        if !c.is_null() && (*c).cmsg_type == libc::SCM_RIGHTS {
            fd = Some(std::ptr::read_unaligned(libc::CMSG_DATA(c).cast::<i32>()));
        }
        Ok((buf[..n as usize].to_vec(), fd))
    }
}

pub fn run(sock: &str) -> io::Result<()> {
    let _ = std::fs::remove_file(sock);
    let l = UnixListener::bind(sock)?;
    let mut held: Option<RawFd> = None;
    eprintln!("keeper on {sock}");
    for s in l.incoming() {
        let Ok(s) = s else { continue };
        let Ok((msg, fd)) = recv_fd(&s) else { continue };
        match msg.as_slice() {
            b"PUT\n" => {
                if let Some(fd) = fd {
                    if let Some(old) = held.replace(fd) {
                        // SAFETY: an fd we own.
                        unsafe { libc::close(old) };
                    }
                    eprintln!("keeper: holding fd {fd}");
                    let _ = (&s).write_all(b"OK\n");
                }
            }
            b"GET\n" => {
                let _ = match held {
                    Some(fd) => send_fd(&s, b"FD\n", Some(fd)),
                    None => send_fd(&s, b"NONE\n", None),
                };
            }
            _ => {}
        }
    }
    Ok(())
}

/// Client side: the held fd, if any.
pub fn get(sock: &str) -> io::Result<Option<RawFd>> {
    let s = UnixStream::connect(sock)?;
    send_fd(&s, b"GET\n", None)?;
    let (msg, fd) = recv_fd(&s)?;
    Ok(if msg == b"FD\n" { fd } else { None })
}

pub fn put(sock: &str, fd: RawFd) -> io::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    send_fd(&s, b"PUT\n", Some(fd))?;
    let mut b = [0u8; 3];
    s.read_exact(&mut b)?;
    Ok(())
}
