//! The destination's end of a migration's scratch copy (docs/CLAUDE-CODE.md "Storage in the sandbox", docs/SANDBOX-VM.md
//! "Scratch"): `sandboxd migrate-relay`, a helper process (in the helpers cgroup, not sandboxd's budget) that owns the
//! receive's port on the overlay address. The source makes these connections to it, one at a time:
//!
//! - rsync connections (`rsync://<addr>:<port>/scratch/`, they begin with "@RSYNCD:"): served by `rsync --daemon` in
//!   inetd mode on the connection, writing into the sandbox's scratch filesystem (module `scratch`);
//! - the pre-copy stream of `runsc checkpoint --migrate-to` (it begins with the fork's magic "gVprcp01"): relayed to
//!   the local `runsc restore --migrate-listen`, record by record, except its End record, which is held back: the
//!   restore would start as soon as it reads End, before the scratch's final delta is here;
//! - "SBXDONE!" from the source once its final delta (copied after the stream ended, i.e. while the sandbox is
//!   stopped) is written: End goes to the restore, which then restores; "SBXFAIL!": the restore gets no End and fails.
//!
//! Exit 0 = End delivered after SBXDONE; otherwise non-zero (the restore then fails or is killed by sandboxd).
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub const MAGIC: &[u8; 8] = b"gVprcp01";
pub const DONE: &[u8; 8] = b"SBXDONE!";
pub const FAIL: &[u8; 8] = b"SBXFAIL!";
const RSYNC: &[u8; 8] = b"@RSYNCD:";
const REC_PAGES: u8 = 1;
const REC_ZERO: u8 = 2;
const REC_FILE: u8 = 3;
const REC_ROUND: u8 = 4;
pub const REC_END: u8 = 5;

/// A streaming parser of the pre-copy stream's framing (gVisor fork, pkg/sentry/state/precopy): finds the End record.
/// Fixed state, any chunking.
pub struct Parser {
    magic: usize,
    hdr: [u8; 17],
    have: usize,
    need: usize,
    payload: u64,
    pub end: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser { magic: 0, hdr: [0; 17], have: 0, need: 0, payload: 0, end: false }
    }

    /// Consumes `data`; Ok(Some(i)): data[i] is the End record's byte. Err: not a pre-copy stream / bytes after End.
    pub fn feed(&mut self, data: &[u8]) -> Result<Option<usize>, &'static str> {
        let mut i = 0;
        while i < data.len() {
            if self.end {
                return Err("bytes after the End record");
            }
            if self.magic < MAGIC.len() {
                if data[i] != MAGIC[self.magic] {
                    return Err("not a pre-copy stream");
                }
                self.magic += 1;
                i += 1;
                continue;
            }
            if self.payload > 0 {
                let k = (self.payload).min((data.len() - i) as u64) as usize;
                self.payload -= k as u64;
                i += k;
                continue;
            }
            if self.have == 0 {
                // a record's type byte
                let t = data[i];
                self.need = match t {
                    REC_PAGES | REC_ZERO => 16,
                    REC_FILE => 5,
                    REC_ROUND => 12,
                    REC_END => {
                        self.end = true;
                        return if i + 1 == data.len() { Ok(Some(i)) } else { Err("bytes after the End record") };
                    }
                    _ => return Err("unknown pre-copy record"),
                };
                self.hdr[0] = t;
                self.have = 1;
                i += 1;
                continue;
            }
            let k = (1 + self.need - self.have).min(data.len() - i);
            self.hdr[self.have..self.have + k].copy_from_slice(&data[i..i + k]);
            self.have += k;
            i += k;
            if self.have == 1 + self.need {
                self.payload = match self.hdr[0] {
                    REC_PAGES => u64::from_le_bytes(self.hdr[9..17].try_into().unwrap_or([0; 8])),
                    REC_FILE => u32::from_le_bytes(self.hdr[2..6].try_into().unwrap_or([0; 4])) as u64,
                    _ => 0,
                };
                self.have = 0;
            }
        }
        Ok(None)
    }
}

fn log(msg: &str) {
    let mut s = crate::fixed::Str::<300>::new();
    let _ = s.push("{\"msg\":\"migrate-relay: ");
    s.push_lossy(msg);
    let _ = s.push("\"}\n");
    unsafe { libc::write(2, s.as_str().as_ptr() as *const _, s.len()) };
}

/// Reads exactly 8 bytes without consuming them (MSG_PEEK), within the socket's read timeout.
fn peek8(c: &TcpStream) -> Option<[u8; 8]> {
    let mut b = [0u8; 8];
    let t0 = Instant::now();
    loop {
        let n = unsafe { libc::recv(c.as_raw_fd(), b.as_mut_ptr() as *mut _, 8, libc::MSG_PEEK | libc::MSG_WAITALL) };
        if n == 8 {
            return Some(b);
        }
        if n == 0 || t0.elapsed() > Duration::from_secs(30) {
            return None;
        }
        if n < 0 {
            let e = std::io::Error::last_os_error().raw_os_error();
            if e != Some(libc::EINTR) && e != Some(libc::EAGAIN) {
                return None;
            }
        }
    }
}

/// `sandboxd migrate-relay <listen ip:port> <restore ip:port> <rsyncd.conf> <timeout s>`
pub fn main(args: &[String]) -> i32 {
    if args.len() != 4 {
        log("usage: migrate-relay <listen ip:port> <restore ip:port> <rsyncd.conf> <timeout s>");
        return 2;
    }
    let (listen, restore, conf) = (args[0].as_str(), args[1].as_str(), args[2].as_str());
    let deadline = Instant::now() + Duration::from_secs(args[3].parse().unwrap_or(900));
    let l = match TcpListener::bind(listen) {
        Ok(l) => l,
        Err(_) => {
            log("listen failed");
            return 1;
        }
    };
    // 1 MiB relay buffer, mapped here (this process only)
    let buf = unsafe {
        let p = libc::mmap(
            core::ptr::null_mut(),
            1 << 20,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if p == libc::MAP_FAILED {
            return 1;
        }
        core::slice::from_raw_parts_mut(p as *mut u8, 1 << 20)
    };
    let mut held: Option<TcpStream> = None; // the restore connection, End held back
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            log("timed out");
            return 3;
        }
        let mut pfd = libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = left.as_millis().min(60_000) as i32;
        if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
            continue;
        }
        let Ok((c, _)) = l.accept() else { continue };
        let _ = c.set_read_timeout(Some(Duration::from_secs(120)));
        let Some(tag) = peek8(&c) else { continue };
        if &tag == RSYNC {
            let mut cf: crate::fixed::Str<300> = crate::fixed::Str::new();
            let _ = cf.push("--config=") && cf.push(conf);
            let fd = c.as_raw_fd();
            let io = crate::proc::Io::Fd(fd);
            match crate::proc::spawn(&["rsync", "--daemon", cf.as_str()], &[], io, io, crate::proc::Io::Null) {
                Ok(ch) => {
                    drop(c);
                    crate::proc::wait(ch.pid, true);
                }
                Err(_) => log("rsync --daemon did not start"),
            }
        } else if &tag == MAGIC {
            if held.is_some() {
                log("a second stream");
                return 1;
            }
            match relay(c, restore, buf) {
                Ok(r) => held = Some(r),
                Err(e) => {
                    log(e);
                    return 1;
                }
            }
        } else if &tag == DONE || &tag == FAIL {
            let mut c = c;
            let mut t = [0u8; 8];
            let _ = c.read_exact(&mut t);
            let Some(mut r) = held.take() else {
                log("SBXDONE/SBXFAIL without a finished stream");
                return 1;
            };
            if &tag == FAIL {
                log("the source failed after the stream; no End for the restore");
                return 1;
            }
            if r.write_all(&[REC_END]).and_then(|_| r.flush()).is_err() {
                log("writing End to the restore failed");
                return 1;
            }
            let _ = r.shutdown(std::net::Shutdown::Write);
            let _ = c.write_all(b"OK");
            // End is in the restore's socket (it never writes back, so closing can't reset the connection); the
            // restore goes on by itself, and sandboxd routes the sandbox here once it has exited
            return 0;
        }
    }
}

/// Relays a pre-copy stream to the restore up to (not including) its End record; returns the restore connection.
fn relay(mut src: TcpStream, restore: &str, buf: &mut [u8]) -> Result<TcpStream, &'static str> {
    let t0 = Instant::now();
    let mut dst = loop {
        match TcpStream::connect(restore) {
            Ok(d) => break d,
            Err(_) if t0.elapsed() < Duration::from_secs(30) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err("the restore does not accept"),
        }
    };
    let _ = dst.set_nodelay(true);
    let _ = src.set_read_timeout(Some(Duration::from_secs(300)));
    let mut p = Parser::new();
    loop {
        let n = match src.read(buf) {
            Ok(0) => return Err("the stream ended before its End record (the source failed)"),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("reading the stream failed"),
        };
        match p.feed(&buf[..n]) {
            Ok(Some(i)) => {
                dst.write_all(&buf[..i]).map_err(|_| "writing to the restore failed")?;
                return Ok(dst);
            }
            Ok(None) => dst.write_all(&buf[..n]).map_err(|_| "writing to the restore failed")?,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> Vec<u8> {
        let mut s = MAGIC.to_vec();
        // Pages: offset, length 5000, payload
        s.push(REC_PAGES);
        s.extend_from_slice(&4096u64.to_le_bytes());
        s.extend_from_slice(&5000u64.to_le_bytes());
        s.extend(std::iter::repeat_n(REC_END, 5000)); // payload bytes that look like End
        s.push(REC_ZERO);
        s.extend_from_slice(&0u64.to_le_bytes());
        s.extend_from_slice(&8192u64.to_le_bytes());
        s.push(REC_ROUND);
        s.extend_from_slice(&3u32.to_le_bytes());
        s.extend_from_slice(&99u64.to_le_bytes());
        s.push(REC_FILE);
        s.push(0);
        s.extend_from_slice(&300u32.to_le_bytes());
        s.extend(std::iter::repeat_n(REC_END, 300));
        s.push(REC_END);
        s
    }

    #[test]
    fn finds_end_in_any_chunking() {
        let s = stream();
        for chunk in [1usize, 2, 3, 7, 16, 17, 64, 4999, 5000, 5001, s.len()] {
            let mut p = Parser::new();
            let mut found = None;
            let mut off = 0;
            for c in s.chunks(chunk) {
                if let Some(i) = p.feed(c).unwrap() {
                    found = Some(off + i);
                }
                off += c.len();
            }
            assert_eq!(found, Some(s.len() - 1), "chunk {chunk}");
        }
    }

    #[test]
    fn rejects_garbage_and_trailing_bytes() {
        assert!(Parser::new().feed(b"gVprcp02").is_err());
        let mut s = stream();
        s.push(1);
        assert!(Parser::new().feed(&s).is_err());
        let mut s = MAGIC.to_vec();
        s.push(9);
        assert!(Parser::new().feed(&s).is_err());
        // a stream cut short: no End
        let s = stream();
        assert_eq!(Parser::new().feed(&s[..s.len() - 1]).unwrap(), None);
    }
}
