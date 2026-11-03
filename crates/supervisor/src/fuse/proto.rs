//! The FUSE wire protocol as the sandbox runtime's client speaks it (protocol 7.31, supervisor 2.5 and 9.3): the request header,
//! the argument structs the supervisor reads, and the replies it writes. Little-endian, 64-bit layout, no libfuse.

#![allow(clippy::many_single_char_names, clippy::similar_names)] // FUSE argument names: h(eader), b(ody), p(ath)

use std::time::Duration;

/// The kernel's request header (`fuse_in_header`).
pub const IN_HEADER: usize = 40;
/// The reply header (`fuse_out_header`).
pub const OUT_HEADER: usize = 16;
/// The protocol the supervisor answers (7.31; minor >= 28 is needed for `FUSE_MAX_PAGES`).
pub const MAJOR: u32 = 7;
pub const MINOR: u32 = 31;

/// `FUSE_ATOMIC_O_TRUNC`: OPEN carries `O_TRUNC` (the supervisor truncates before replying).
pub const INIT_ATOMIC_O_TRUNC: u32 = 1 << 3;
/// `FUSE_BIG_WRITES`.
pub const INIT_BIG_WRITES: u32 = 1 << 5;
/// `FUSE_MAX_PAGES` (7.28).
pub const INIT_MAX_PAGES: u32 = 1 << 22;
/// What the supervisor accepts of the kernel's offer; never `FUSE_WRITEBACK_CACHE` (writes are write-through).
pub const INIT_FLAGS: u32 = INIT_ATOMIC_O_TRUNC | INIT_BIG_WRITES | INIT_MAX_PAGES;

/// `FATTR_*` bits of SETATTR.
pub mod fattr {
    pub const MODE: u32 = 1;
    pub const UID: u32 = 1 << 1;
    pub const GID: u32 = 1 << 2;
    pub const SIZE: u32 = 1 << 3;
    pub const ATIME: u32 = 1 << 4;
    pub const MTIME: u32 = 1 << 5;
    pub const FH: u32 = 1 << 6;
    pub const ATIME_NOW: u32 = 1 << 7;
    pub const MTIME_NOW: u32 = 1 << 8;
}

/// The opcodes.
#[allow(missing_docs)]
pub mod op {
    pub const LOOKUP: u32 = 1;
    pub const FORGET: u32 = 2;
    pub const GETATTR: u32 = 3;
    pub const SETATTR: u32 = 4;
    pub const READLINK: u32 = 5;
    pub const SYMLINK: u32 = 6;
    pub const MKNOD: u32 = 8;
    pub const MKDIR: u32 = 9;
    pub const UNLINK: u32 = 10;
    pub const RMDIR: u32 = 11;
    pub const RENAME: u32 = 12;
    pub const LINK: u32 = 13;
    pub const OPEN: u32 = 14;
    pub const READ: u32 = 15;
    pub const WRITE: u32 = 16;
    pub const STATFS: u32 = 17;
    pub const RELEASE: u32 = 18;
    pub const FSYNC: u32 = 20;
    pub const SETXATTR: u32 = 21;
    pub const GETXATTR: u32 = 22;
    pub const LISTXATTR: u32 = 23;
    pub const REMOVEXATTR: u32 = 24;
    pub const FLUSH: u32 = 25;
    pub const INIT: u32 = 26;
    pub const OPENDIR: u32 = 27;
    pub const READDIR: u32 = 28;
    pub const RELEASEDIR: u32 = 29;
    pub const FSYNCDIR: u32 = 30;
    pub const GETLK: u32 = 31;
    pub const SETLK: u32 = 32;
    pub const SETLKW: u32 = 33;
    pub const ACCESS: u32 = 34;
    pub const CREATE: u32 = 35;
    pub const INTERRUPT: u32 = 36;
    pub const DESTROY: u32 = 38;
    pub const BATCH_FORGET: u32 = 42;
    pub const FALLOCATE: u32 = 43;
    pub const READDIRPLUS: u32 = 44;
    pub const RENAME2: u32 = 45;
}

/// A name for logs.
#[must_use]
pub fn opname(opcode: u32) -> &'static str {
    match opcode {
        op::LOOKUP => "LOOKUP",
        op::FORGET => "FORGET",
        op::GETATTR => "GETATTR",
        op::SETATTR => "SETATTR",
        op::READLINK => "READLINK",
        op::SYMLINK => "SYMLINK",
        op::MKNOD => "MKNOD",
        op::MKDIR => "MKDIR",
        op::UNLINK => "UNLINK",
        op::RMDIR => "RMDIR",
        op::RENAME => "RENAME",
        op::LINK => "LINK",
        op::OPEN => "OPEN",
        op::READ => "READ",
        op::WRITE => "WRITE",
        op::STATFS => "STATFS",
        op::RELEASE => "RELEASE",
        op::FSYNC => "FSYNC",
        op::SETXATTR => "SETXATTR",
        op::GETXATTR => "GETXATTR",
        op::LISTXATTR => "LISTXATTR",
        op::REMOVEXATTR => "REMOVEXATTR",
        op::FLUSH => "FLUSH",
        op::INIT => "INIT",
        op::OPENDIR => "OPENDIR",
        op::READDIR => "READDIR",
        op::RELEASEDIR => "RELEASEDIR",
        op::FSYNCDIR => "FSYNCDIR",
        op::GETLK => "GETLK",
        op::SETLK => "SETLK",
        op::SETLKW => "SETLKW",
        op::ACCESS => "ACCESS",
        op::CREATE => "CREATE",
        op::INTERRUPT => "INTERRUPT",
        op::DESTROY => "DESTROY",
        op::BATCH_FORGET => "BATCH_FORGET",
        op::FALLOCATE => "FALLOCATE",
        op::READDIRPLUS => "READDIRPLUS",
        op::RENAME2 => "RENAME2",
        _ => "UNKNOWN",
    }
}

/// Whether an opcode changes state (the ones the resend rule of 9.3 keys).
#[must_use]
pub fn mutates(opcode: u32) -> bool {
    matches!(
        opcode,
        op::SETATTR
            | op::MKDIR
            | op::UNLINK
            | op::RMDIR
            | op::RENAME
            | op::RENAME2
            | op::OPEN
            | op::WRITE
            | op::CREATE
            | op::SYMLINK
    )
}

pub(crate) fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

pub(crate) fn u64_at(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// `fuse_in_header`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    /// The calling thread (the sentry puts the thread id here).
    pub pid: u32,
}

impl InHeader {
    /// The header at the start of `b`, if `b` holds one.
    #[must_use]
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < IN_HEADER {
            return None;
        }
        Some(Self {
            len: u32_at(b, 0),
            opcode: u32_at(b, 4),
            unique: u64_at(b, 8),
            nodeid: u64_at(b, 16),
            uid: u32_at(b, 24),
            gid: u32_at(b, 28),
            pid: u32_at(b, 32),
        })
    }

    /// Encodes the header (tests: the sentry stand-in).
    #[must_use]
    pub fn encode(&self) -> [u8; IN_HEADER] {
        let mut o = [0u8; IN_HEADER];
        o[0..4].copy_from_slice(&self.len.to_le_bytes());
        o[4..8].copy_from_slice(&self.opcode.to_le_bytes());
        o[8..16].copy_from_slice(&self.unique.to_le_bytes());
        o[16..24].copy_from_slice(&self.nodeid.to_le_bytes());
        o[24..28].copy_from_slice(&self.uid.to_le_bytes());
        o[28..32].copy_from_slice(&self.gid.to_le_bytes());
        o[32..36].copy_from_slice(&self.pid.to_le_bytes());
        o
    }
}

/// A NUL-terminated name at the start of `b`, and what follows it.
#[must_use]
pub fn cstr(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = b.iter().position(|&c| c == 0)?;
    Some((&b[..n], &b[n + 1..]))
}

/// What a file is, for attributes and dirents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileType {
    Dir,
    File,
    Symlink,
}

/// The attributes the supervisor answers (supervisor 9.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    /// Permission bits only.
    pub perm: u32,
    pub kind: FileType,
}

/// Owner of every file (9.2).
pub const OWNER: u32 = 1000;

fn split_ns(ns: i64) -> (u64, u32) {
    let ns = ns.max(0);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // non-negative; the remainder is < 1e9
    ((ns / 1_000_000_000) as u64, (ns % 1_000_000_000) as u32)
}

impl Attr {
    /// `fuse_attr` (88 bytes).
    pub fn encode(&self, o: &mut Vec<u8>) {
        let (ms, mns) = split_ns(self.mtime_ns);
        let (cs, cns) = split_ns(self.ctime_ns);
        let (fmt, nlink) = match self.kind {
            FileType::Dir => (libc::S_IFDIR, 2),
            FileType::File => (libc::S_IFREG, 1),
            FileType::Symlink => (libc::S_IFLNK, 1),
        };
        // st_atime = st_mtime (9.2)
        for v in [self.ino, self.size, self.size.div_ceil(512), ms, ms, cs] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            mns,
            mns,
            cns,
            fmt | (self.perm & 0o7777),
            nlink,
            OWNER,
            OWNER,
            0,
            4096,
            0,
        ] {
            o.extend_from_slice(&v.to_le_bytes());
        }
    }
}

fn valid_parts(d: Duration) -> (u64, u32) {
    (d.as_secs(), d.subsec_nanos())
}

/// `fuse_entry_out` (128 bytes).
#[must_use]
pub fn entry_out(attr: &Attr, valid: Duration) -> Vec<u8> {
    let (s, ns) = valid_parts(valid);
    let mut o = Vec::with_capacity(128 + 16);
    for v in [attr.ino, 0, s, s] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&ns.to_le_bytes());
    o.extend_from_slice(&ns.to_le_bytes());
    attr.encode(&mut o);
    o
}

/// `fuse_attr_out` (104 bytes).
#[must_use]
pub fn attr_out(attr: &Attr, valid: Duration) -> Vec<u8> {
    let (s, ns) = valid_parts(valid);
    let mut o = Vec::with_capacity(104);
    o.extend_from_slice(&s.to_le_bytes());
    o.extend_from_slice(&ns.to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    attr.encode(&mut o);
    o
}

/// `fuse_open_out`.
#[must_use]
pub fn open_out(fh: u64) -> Vec<u8> {
    let mut o = fh.to_le_bytes().to_vec();
    o.extend_from_slice(&[0u8; 8]);
    o
}

/// `fuse_write_out`.
#[must_use]
pub fn write_out(size: u32) -> Vec<u8> {
    let mut o = size.to_le_bytes().to_vec();
    o.extend_from_slice(&[0u8; 4]);
    o
}

/// The INIT reply (`fuse_init_out`, 64 bytes): 7.31, the kernel's readahead, the accepted flags, 1 MiB writes in
/// 256 pages. The minor is the kernel's when it offers less (it is refused later if under 28, by the kernel).
#[must_use]
pub fn init_out(init_in: &[u8], max_write: u32, max_pages: u16) -> Vec<u8> {
    let minor = u32_at(init_in, 4).min(MINOR);
    let readahead = u32_at(init_in, 8);
    let flags = u32_at(init_in, 12) & INIT_FLAGS;
    let mut o = Vec::with_capacity(64);
    for v in [MAJOR, minor, readahead, flags] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&64u16.to_le_bytes()); // max_background
    o.extend_from_slice(&48u16.to_le_bytes()); // congestion_threshold
    o.extend_from_slice(&max_write.to_le_bytes());
    o.extend_from_slice(&1u32.to_le_bytes()); // time_gran: nanoseconds
    o.extend_from_slice(&max_pages.to_le_bytes());
    o.extend_from_slice(&[0u8; 2 + 32]);
    o
}

/// `fuse_kstatfs` inside `fuse_statfs_out` (80 bytes).
#[must_use]
pub fn statfs_out(blocks: u64, free: u64, files: u64, ffree: u64) -> Vec<u8> {
    let mut o = Vec::with_capacity(80);
    for v in [blocks, free, free, files, ffree] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in [4096u32, 255, 4096, 0, 0, 0, 0, 0, 0, 0] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o
}

/// Appends one `fuse_dirent`; `false` when it does not fit in `max` bytes.
pub fn push_dirent(o: &mut Vec<u8>, max: usize, ino: u64, next_off: u64, kind: FileType, name: &[u8]) -> bool {
    let rec = 24 + name.len();
    let padded = rec.div_ceil(8) * 8;
    if o.len() + padded > max {
        return false;
    }
    let dtype: u32 = match kind {
        FileType::Dir => 4,
        FileType::File => 8,
        FileType::Symlink => 10,
    };
    o.extend_from_slice(&ino.to_le_bytes());
    o.extend_from_slice(&next_off.to_le_bytes());
    #[allow(clippy::cast_possible_truncation)] // names are at most 255 bytes
    o.extend_from_slice(&(name.len() as u32).to_le_bytes());
    o.extend_from_slice(&dtype.to_le_bytes());
    o.extend_from_slice(name);
    o.resize(o.len() + padded - rec, 0);
    true
}

/// A whole reply: header and payload. `errno` 0 is success.
#[must_use]
pub fn reply(unique: u64, errno: i32, payload: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(OUT_HEADER + payload.len());
    #[allow(clippy::cast_possible_truncation)] // replies are at most 1 MiB + the header
    o.extend_from_slice(&((OUT_HEADER + payload.len()) as u32).to_le_bytes());
    o.extend_from_slice(&(-errno).to_le_bytes());
    o.extend_from_slice(&unique.to_le_bytes());
    o.extend_from_slice(payload);
    o
}

/// A parsed reply (tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub unique: u64,
    pub errno: i32,
    pub payload: Vec<u8>,
}

impl Reply {
    /// Parses a reply written by the server.
    #[must_use]
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < OUT_HEADER {
            return None;
        }
        #[allow(clippy::cast_possible_wrap)] // the wire carries the negative errno as u32 bits
        let err = u32_at(b, 4) as i32;
        Some(Self {
            unique: u64_at(b, 8),
            errno: -err,
            payload: b[OUT_HEADER..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let a = Attr {
            ino: 7,
            size: 1025,
            mtime_ns: 1_500_000_000,
            ctime_ns: 2_000_000_001,
            perm: 0o600,
            kind: FileType::File,
        };
        let mut o = Vec::new();
        a.encode(&mut o);
        assert_eq!(o.len(), 88);
        assert_eq!(u64_at(&o, 16), 3); // blocks
        assert_eq!(u64_at(&o, 32), 1); // mtime s
        assert_eq!(u32_at(&o, 52), 500_000_000); // mtime ns
        assert_eq!(u32_at(&o, 60), libc::S_IFREG | 0o600);
        assert_eq!(u32_at(&o, 64), 1);
        assert_eq!(u32_at(&o, 68), 1000);
        assert_eq!(entry_out(&a, Duration::from_secs(1)).len(), 128);
        assert_eq!(attr_out(&a, Duration::from_secs(1)).len(), 104);
        assert_eq!(statfs_out(1, 2, 3, 4).len(), 80);
    }

    #[test]
    fn init_reply_is_7_31_with_1mib_writes() {
        let mut init_in = Vec::new();
        for v in [7u32, 31, 131_072, 0x0040_0028 | 1 | (1 << 10)] {
            init_in.extend_from_slice(&v.to_le_bytes());
        }
        let o = init_out(&init_in, 1 << 20, 256);
        assert_eq!(o.len(), 64);
        assert_eq!((u32_at(&o, 0), u32_at(&o, 4)), (7, 31));
        assert_eq!(u32_at(&o, 12), INIT_FLAGS);
        assert_eq!(u32_at(&o, 20), 1 << 20);
        assert_eq!(u16::from_le_bytes([o[28], o[29]]), 256);
    }

    #[test]
    fn header_round_trip_and_reply() {
        let h = InHeader {
            len: 41,
            opcode: op::WRITE,
            unique: 9,
            nodeid: 2,
            uid: 1000,
            gid: 1000,
            pid: 4242,
        };
        assert_eq!(InHeader::parse(&h.encode()), Some(h));
        let r = Reply::parse(&reply(9, libc::ENOENT, b"")).unwrap();
        assert_eq!((r.unique, r.errno), (9, libc::ENOENT));
        let mut d = Vec::new();
        assert!(push_dirent(&mut d, 4096, 5, 1, FileType::Dir, b"abc"));
        assert_eq!(d.len(), 32);
        assert!(!push_dirent(&mut d, 40, 5, 2, FileType::Dir, b"abc"));
    }
}
