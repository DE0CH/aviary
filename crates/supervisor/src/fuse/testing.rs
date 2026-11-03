//! The sentry stand-in: a `/dev/fuse` in memory. Tests submit requests as the runtime's client would (header with
//! `unique`, node, the calling thread's id), wait for replies, forget a request as a checkpoint does (its reply is
//! then refused with ENOENT and the test resends it under a new `unique`), and hand each server instance its own
//! handle (a "killed" instance's handle is closed; the next takes the connection, as from the keeper).

#![allow(clippy::many_single_char_names, clippy::similar_names)] // FUSE argument names: h(eader), b(ody), p(ath)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::Device;
use super::proto::{IN_HEADER, InHeader, Reply, op};

#[derive(Default)]
struct S {
    /// (request, the byte range a read copies into the buffer: all of it unless a test asks for a short read).
    queue: VecDeque<(Vec<u8>, std::ops::Range<usize>)>,
    /// unique → replies delivered (a request is waited for while present).
    waiting: HashMap<u64, Vec<Reply>>,
    refused: Vec<u64>,
    next: u64,
    closed: Vec<u64>,
    gens: u64,
}

/// The connection.
#[derive(Default)]
pub struct Sentry {
    s: Mutex<S>,
    cv: Condvar,
}

/// One server instance's view of it.
pub struct Conn {
    sentry: Arc<Sentry>,
    generation: u64,
}

impl Device for Conn {
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut s = self.sentry.s.lock().unwrap();
        loop {
            if s.closed.contains(&self.generation) {
                return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
            }
            if let Some((r, copy)) = s.queue.pop_front() {
                // a read returns the whole request's length whatever it copied (9.9's short read: the count says
                // n bytes, the slot does not hold them)
                buf[copy.clone()].copy_from_slice(&r[copy]);
                return Ok(r.len());
            }
            s = self.sentry.cv.wait(s).unwrap();
        }
    }

    fn write(&self, buf: &[u8]) -> std::io::Result<()> {
        let r = Reply::parse(buf).ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let mut s = self.sentry.s.lock().unwrap();
        if s.closed.contains(&self.generation) {
            return Err(std::io::Error::from_raw_os_error(libc::ENODEV));
        }
        match s.waiting.get_mut(&r.unique) {
            Some(v) if v.is_empty() => {
                v.push(r);
                self.sentry.cv.notify_all();
                Ok(())
            }
            _ => {
                s.refused.push(r.unique);
                Err(std::io::Error::from_raw_os_error(libc::ENOENT))
            }
        }
    }

    fn close(&self) {
        let mut s = self.sentry.s.lock().unwrap();
        s.closed.push(self.generation);
        self.sentry.cv.notify_all();
    }
}

impl Sentry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Closes every handle (the end of a test: the workers exit).
    pub fn close_all(&self) {
        let mut s = self.s.lock().unwrap();
        let all: Vec<u64> = (1..=s.gens).collect();
        s.closed.extend(all);
        self.cv.notify_all();
    }

    /// A handle for a new server instance.
    #[must_use]
    pub fn conn(self: &Arc<Self>) -> Arc<Conn> {
        let mut s = self.s.lock().unwrap();
        s.gens += 1;
        Arc::new(Conn {
            sentry: self.clone(),
            generation: s.gens,
        })
    }

    /// Queues a request; its `unique`.
    pub fn submit(&self, opcode: u32, nodeid: u64, pid: u32, body: &[u8]) -> u64 {
        self.submit_copying(opcode, nodeid, pid, body, None)
    }

    /// Queues a request whose read returns its whole length but copies only `copy` of its bytes into the reader's
    /// buffer (`0..0`: nothing lands); its `unique`.
    pub fn submit_short(&self, opcode: u32, nodeid: u64, pid: u32, body: &[u8], copy: std::ops::Range<usize>) -> u64 {
        self.submit_copying(opcode, nodeid, pid, body, Some(copy))
    }

    fn submit_copying(
        &self,
        opcode: u32,
        nodeid: u64,
        pid: u32,
        body: &[u8],
        copy: Option<std::ops::Range<usize>>,
    ) -> u64 {
        let mut s = self.s.lock().unwrap();
        s.next += 2;
        let unique = s.next;
        let h = InHeader {
            len: u32::try_from(IN_HEADER + body.len()).unwrap(),
            opcode,
            unique,
            nodeid,
            uid: 1000,
            gid: 1000,
            pid,
        };
        let mut r = h.encode().to_vec();
        r.extend_from_slice(body);
        if opcode != op::INTERRUPT && opcode != op::FORGET {
            s.waiting.insert(unique, Vec::new());
        }
        let copy = copy.unwrap_or(0..r.len());
        s.queue.push_back((r, copy));
        self.cv.notify_all();
        unique
    }

    /// The reply to `unique`, waiting at most `t`.
    pub fn wait(&self, unique: u64, t: Duration) -> Option<Reply> {
        let deadline = Instant::now() + t;
        let mut s = self.s.lock().unwrap();
        loop {
            if let Some(r) = s.waiting.get(&unique).and_then(|v| v.first().cloned()) {
                return Some(r);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            s = self.cv.wait_timeout(s, left).unwrap().0;
        }
    }

    /// Submit and wait (10 s).
    pub fn call(&self, opcode: u32, nodeid: u64, pid: u32, body: &[u8]) -> Reply {
        let u = self.submit(opcode, nodeid, pid, body);
        self.wait(u, Duration::from_secs(10))
            .unwrap_or_else(|| panic!("no reply to {} (unique {u})", super::proto::opname(opcode)))
    }

    /// A checkpoint forgets `unique`: its reply is refused with ENOENT from now on.
    pub fn forget(&self, unique: u64) {
        self.s.lock().unwrap().waiting.remove(&unique);
    }

    /// How many times a reply to `unique` was refused.
    #[must_use]
    pub fn refused(&self, unique: u64) -> usize {
        self.s.lock().unwrap().refused.iter().filter(|u| **u == unique).count()
    }

    /// Waits until a reply to `unique` was refused (the original finished after its forget).
    pub fn wait_refused(&self, unique: u64, t: Duration) -> bool {
        let deadline = Instant::now() + t;
        while Instant::now() < deadline {
            if self.refused(unique) > 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

/// Request bodies.
pub mod body {
    fn cs(name: &str) -> Vec<u8> {
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        v
    }
    fn le(vals: &[u64]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    fn le32(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    #[must_use]
    pub fn init() -> Vec<u8> {
        le32(&[7, 31, 131_072, 0x0040_0028])
    }
    #[must_use]
    pub fn name(n: &str) -> Vec<u8> {
        cs(n)
    }
    #[must_use]
    pub fn getattr() -> Vec<u8> {
        vec![0; 16]
    }
    #[must_use]
    pub fn mkdir(n: &str, mode: u32) -> Vec<u8> {
        let mut v = le32(&[mode, 0o022]);
        v.extend(cs(n));
        v
    }
    #[must_use]
    pub fn create(n: &str, flags: i32, mode: u32) -> Vec<u8> {
        #[allow(clippy::cast_sign_loss)]
        let mut v = le32(&[flags as u32, mode, 0o022, 0]);
        v.extend(cs(n));
        v
    }
    #[must_use]
    pub fn open(flags: i32) -> Vec<u8> {
        #[allow(clippy::cast_sign_loss)]
        le32(&[flags as u32, 0])
    }
    #[must_use]
    pub fn write(fh: u64, off: u64, data: &[u8]) -> Vec<u8> {
        let mut v = le(&[fh, off]);
        v.extend(le32(&[u32::try_from(data.len()).unwrap(), 0]));
        v.extend(le(&[0]));
        v.extend(le32(&[0, 0]));
        v.extend_from_slice(data);
        v
    }
    #[must_use]
    pub fn read(fh: u64, off: u64, size: u32) -> Vec<u8> {
        let mut v = le(&[fh, off]);
        v.extend(le32(&[size, 0]));
        v.extend(le(&[0]));
        v.extend(le32(&[0, 0]));
        v
    }
    #[must_use]
    pub fn readdir(off: u64) -> Vec<u8> {
        read(0, off, 65_536)
    }
    /// SETATTR with `valid`, size, mode, uid, mtime (seconds, nanoseconds).
    #[must_use]
    pub fn setattr(valid: u32, size: u64, mode: u32, uid: u32, mtime: (u64, u32)) -> Vec<u8> {
        let mut v = vec![0u8; 88];
        v[0..4].copy_from_slice(&valid.to_le_bytes());
        v[16..24].copy_from_slice(&size.to_le_bytes());
        v[40..48].copy_from_slice(&mtime.0.to_le_bytes());
        v[60..64].copy_from_slice(&mtime.1.to_le_bytes());
        v[68..72].copy_from_slice(&mode.to_le_bytes());
        v[76..80].copy_from_slice(&uid.to_le_bytes());
        v[80..84].copy_from_slice(&uid.to_le_bytes());
        v
    }
    #[must_use]
    pub fn rename(newdir: u64, old: &str, new: &str) -> Vec<u8> {
        let mut v = le(&[newdir]);
        v.extend(cs(old));
        v.extend(cs(new));
        v
    }
    /// RENAME2 (`fuse_rename2_in`: newdir, flags, padding, then the names).
    #[must_use]
    pub fn rename2(newdir: u64, flags: u32, old: &str, new: &str) -> Vec<u8> {
        let mut v = le(&[newdir]);
        v.extend(le32(&[flags, 0]));
        v.extend(cs(old));
        v.extend(cs(new));
        v
    }
    #[must_use]
    pub fn symlink(name: &str, target: &str) -> Vec<u8> {
        let mut v = cs(name);
        v.extend(cs(target));
        v
    }
    #[must_use]
    pub fn link(old: u64, name: &str) -> Vec<u8> {
        let mut v = le(&[old]);
        v.extend(cs(name));
        v
    }
    #[must_use]
    pub fn interrupt(unique: u64) -> Vec<u8> {
        le(&[unique])
    }
    #[must_use]
    pub fn fh(fh: u64) -> Vec<u8> {
        let mut v = le(&[fh]);
        v.extend(vec![0u8; 16]);
        v
    }
}

/// Fields of replies.
pub mod out {
    use super::super::proto::{u32_at, u64_at};
    use super::Reply;

    /// The node id of an entry reply.
    #[must_use]
    pub fn node(r: &Reply) -> u64 {
        u64_at(&r.payload, 0)
    }
    /// The handle of a CREATE reply (after the 128-byte entry).
    #[must_use]
    pub fn create_fh(r: &Reply) -> u64 {
        u64_at(&r.payload, 128)
    }
    /// The handle of an OPEN reply.
    #[must_use]
    pub fn open_fh(r: &Reply) -> u64 {
        u64_at(&r.payload, 0)
    }
    /// Size from an attr reply (104 bytes) or an entry reply (128).
    #[must_use]
    pub fn size(r: &Reply) -> u64 {
        let base = if r.payload.len() >= 128 { 40 } else { 16 };
        u64_at(&r.payload, base + 8)
    }
    /// Mode from an attr or entry reply.
    #[must_use]
    pub fn mode(r: &Reply) -> u32 {
        let base = if r.payload.len() >= 128 { 40 } else { 16 };
        u32_at(&r.payload, base + 60)
    }
    /// The names of a READDIR reply.
    #[must_use]
    pub fn names(r: &Reply) -> Vec<String> {
        let mut v = Vec::new();
        let mut o = 0;
        let b = &r.payload;
        while o + 24 <= b.len() {
            let n = u32_at(b, o + 16) as usize;
            v.push(String::from_utf8_lossy(&b[o + 24..o + 24 + n]).into_owned());
            o += (24 + n).div_ceil(8) * 8;
        }
        v
    }
}
