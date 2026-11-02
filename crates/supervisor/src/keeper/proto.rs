//! The keeper protocol's wire format (supervisor 4.2): one `SOCK_SEQPACKET` message per request or answer, at most
//! [`KEEPER_PACKET`](crate::limits::KEEPER_PACKET) bytes, little-endian fixed-width integers and length-prefixed
//! byte strings. Encoding writes into a caller's buffer and decoding borrows from it, so the keeper parses and
//! answers without allocating.
//!
//! Request packets start with an [`Op`] byte, answers with a [`Status`] byte:
//!
//! | Op | Fields | Answer |
//! |---|---|---|
//! | `SPAWN` | name, argv[], env[], cwd, uid, gid, setsid, `oom_score_adj`, cgroup (empty: none), stdout, stderr, stdin (0 null, 1 pipe) | `OK pid since` (the exit counter at the fork) |
//! | `STDIN` | pid, `msg_id` (16), total, then the first bytes | after `total` bytes (this and `STDIN_MORE` packets): `OK msg_id` once written, `ALREADY msg_id` when nothing was written |
//! | `STDIN_MORE` | bytes | (none until the message is complete) |
//! | `CLOSE_STDIN` | pid | `OK` |
//! | `SIGNAL` | target (pid, or -pgid), signal | `OK` |
//! | `PUT_FD` | name, one fd as `SCM_RIGHTS` | `OK` |
//! | `GET_FD` | name | `OK` with the fd, or `NONE` |
//! | `EXITS` | after | `OK` count, then {n, pid, name, code?, signal?, `at_ms`} |
//! | `STATUS` | | `OK` `boot_id`, restarts, `started_at_ms`, count, then {pid, name}, orphans, released |
//! | `RELEASED` | | `OK`: the supervisor finished a release; its exit 0 is final |
//!
//! Every request may also be answered `ERR errno message`.

/// A request's first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    Spawn = 1,
    Stdin = 2,
    StdinMore = 3,
    CloseStdin = 4,
    Signal = 5,
    PutFd = 6,
    GetFd = 7,
    Exits = 8,
    Status = 9,
    Released = 10,
}

impl Op {
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            1 => Self::Spawn,
            2 => Self::Stdin,
            3 => Self::StdinMore,
            4 => Self::CloseStdin,
            5 => Self::Signal,
            6 => Self::PutFd,
            7 => Self::GetFd,
            8 => Self::Exits,
            9 => Self::Status,
            10 => Self::Released,
            _ => return None,
        })
    }
}

/// An answer's first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    Already = 1,
    None = 2,
    Err = 3,
}

impl Status {
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Ok,
            1 => Self::Already,
            2 => Self::None,
            3 => Self::Err,
            _ => return None,
        })
    }
}

/// The length of a `msg_id`.
pub const MSG_ID: usize = 16;
/// The bytes of a `STDIN` header before its data: op, pid, `msg_id`, total.
pub const STDIN_HEADER: usize = 1 + 4 + MSG_ID + 8;
/// The most argv or env entries one `SPAWN` carries.
pub const MAX_ARGS: usize = 256;
/// The longest child name the keeper keeps.
pub const NAME_MAX: usize = 16;

/// The packet did not fit or did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("malformed keeper packet")
    }
}

impl std::error::Error for Malformed {}

/// Writes fields into a fixed buffer.
#[derive(Debug)]
pub struct Writer<'a> {
    buf: &'a mut [u8],
    len: usize,
    overflow: bool,
}

impl<'a> Writer<'a> {
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            overflow: false,
        }
    }

    pub fn bytes_raw(&mut self, b: &[u8]) -> &mut Self {
        let end = self.len + b.len();
        if end > self.buf.len() {
            self.overflow = true;
        } else {
            self.buf[self.len..end].copy_from_slice(b);
            self.len = end;
        }
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.bytes_raw(&[v])
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes_raw(&v.to_le_bytes())
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.bytes_raw(&v.to_le_bytes())
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes_raw(&v.to_le_bytes())
    }

    /// A length-prefixed byte string.
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        let Ok(n) = u32::try_from(b.len()) else {
            self.overflow = true;
            return self;
        };
        self.u32(n).bytes_raw(b)
    }

    pub fn opt_i32(&mut self, v: Option<i32>) -> &mut Self {
        match v {
            Some(v) => self.u8(1).i32(v),
            None => self.u8(0).i32(0),
        }
    }

    /// The written packet, or [`Malformed`] when it did not fit.
    ///
    /// # Errors
    /// When a field did not fit the buffer.
    pub fn finish(&self) -> Result<&[u8], Malformed> {
        if self.overflow {
            Err(Malformed)
        } else {
            Ok(&self.buf[..self.len])
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Reads fields from a packet, borrowing byte strings.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// # Errors
    /// When fewer than `n` bytes are left.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        let end = self.pos.checked_add(n).ok_or(Malformed)?;
        let s = self.buf.get(self.pos..end).ok_or(Malformed)?;
        self.pos = end;
        Ok(s)
    }

    /// # Errors
    /// At the end of the packet.
    pub fn u8(&mut self) -> Result<u8, Malformed> {
        Ok(self.take(1)?[0])
    }

    /// # Errors
    /// When the packet is too short.
    pub fn u32(&mut self) -> Result<u32, Malformed> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// # Errors
    /// When the packet is too short.
    pub fn i32(&mut self) -> Result<i32, Malformed> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// # Errors
    /// When the packet is too short.
    pub fn u64(&mut self) -> Result<u64, Malformed> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    /// A length-prefixed byte string.
    ///
    /// # Errors
    /// When the packet is too short for the length it names.
    pub fn bytes(&mut self) -> Result<&'a [u8], Malformed> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    /// # Errors
    /// When the packet is too short.
    pub fn opt_i32(&mut self) -> Result<Option<i32>, Malformed> {
        let has = self.u8()?;
        let v = self.i32()?;
        Ok((has != 0).then_some(v))
    }

    /// # Errors
    /// When the packet is too short.
    pub fn msg_id(&mut self) -> Result<[u8; MSG_ID], Malformed> {
        let mut id = [0u8; MSG_ID];
        id.copy_from_slice(self.take(MSG_ID)?);
        Ok(id)
    }

    /// Everything not read yet.
    #[must_use]
    pub fn rest(&mut self) -> &'a [u8] {
        let r = &self.buf[self.pos.min(self.buf.len())..];
        self.pos = self.buf.len();
        r
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut buf = [0u8; 128];
        let mut w = Writer::new(&mut buf);
        w.u8(Op::Signal as u8)
            .i32(-812)
            .i32(15)
            .bytes(b"claude")
            .opt_i32(None)
            .opt_i32(Some(-9))
            .u64(7);
        let p = w.finish().unwrap().to_vec();
        let mut r = Reader::new(&p);
        assert_eq!(Op::from_byte(r.u8().unwrap()), Some(Op::Signal));
        assert_eq!(r.i32().unwrap(), -812);
        assert_eq!(r.i32().unwrap(), 15);
        assert_eq!(r.bytes().unwrap(), b"claude");
        assert_eq!(r.opt_i32().unwrap(), None);
        assert_eq!(r.opt_i32().unwrap(), Some(-9));
        assert_eq!(r.u64().unwrap(), 7);
        assert_eq!(r.remaining(), 0);
        assert!(r.u8().is_err());
    }

    #[test]
    fn overflow_and_short_reads_are_errors() {
        let mut buf = [0u8; 4];
        let mut w = Writer::new(&mut buf);
        w.bytes(b"too long");
        assert_eq!(w.finish(), Err(Malformed));
        // a length naming more than the packet holds
        let p = [10u8, 0, 0, 0, b'a'];
        assert!(Reader::new(&p).bytes().is_err());
        assert!(Reader::new(&[0xff_u8; 3]).u32().is_err());
        assert_eq!(Op::from_byte(0), None);
        assert_eq!(Status::from_byte(9), None);
    }
}
