//! Small helpers without a heap: errors, time, IPv4, files and directories, JSON log lines.
use crate::fixed::{Buf, Str, cstr};
use crate::json::{self, JsonError};
use core::fmt::Write as _;
use std::io::{Read, Write};

/// An error answered to the controller: an HTTP status and a message.
#[derive(Clone, Copy, Debug)]
pub struct Error {
    pub status: u16,
    pub msg: Str<480>,
}

pub type Res<T> = Result<T, Error>;

impl Error {
    pub fn new(status: u16, msg: &str) -> Error {
        Error { status, msg: Str::lossy(msg) }
    }
    pub fn fmt(status: u16, args: core::fmt::Arguments) -> Error {
        let mut msg = Str::new();
        if msg.write_fmt(args).is_err() {
            // cut: keep what fit
        }
        Error { status, msg }
    }
}

impl From<JsonError> for Error {
    fn from(_: JsonError) -> Error {
        Error::new(400, "body is not JSON")
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::fmt(500, format_args!("{}", io_msg(&e)))
    }
}

/// An io::Error's message without allocating (its Display allocates for OS errors).
pub fn io_msg(e: &std::io::Error) -> Str<160> {
    let mut s = Str::new();
    match e.raw_os_error() {
        Some(code) => {
            let mut b = [0u8; 128];
            let r = unsafe { libc::strerror_r(code, b.as_mut_ptr() as *mut _, b.len()) };
            let n = b.iter().position(|c| *c == 0).unwrap_or(0);
            let msg = if r == 0 { core::str::from_utf8(&b[..n]).unwrap_or("error") } else { "error" };
            let _ = write!(s, "{msg} (os error {code})");
        }
        None => {
            let _ = write!(s, "{:?}", e.kind());
        }
    }
    s
}

#[macro_export]
macro_rules! err {
    ($status:expr, $($a:tt)*) => { $crate::util::Error::fmt($status, format_args!($($a)*)) };
}

// ---- time ---------------------------------------------------------------------------------------------------------

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// Monotonic milliseconds.
pub fn mono_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// ISO 8601 UTC with milliseconds, as JavaScript's toISOString().
pub fn iso(ms: u64) -> Str<24> {
    let secs = ms / 1000;
    let days = (secs / 86400) as i64;
    let sod = secs % 86400;
    // civil from days (H. Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let mut s = Str::new();
    let _ = write!(
        s,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        sod / 3600,
        sod / 60 % 60,
        sod % 60,
        ms % 1000
    );
    s
}

pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

// ---- IPv4 ---------------------------------------------------------------------------------------------------------

pub fn ip_to_int(ip: &str) -> Option<u32> {
    let mut n = 0u32;
    let mut parts = 0;
    for p in ip.split('.') {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let v: u32 = p.parse().ok()?;
        if v > 255 {
            return None;
        }
        n = (n << 8) | v;
        parts += 1;
    }
    if parts == 4 { Some(n) } else { None }
}

pub fn in_cidr(ip: &str, cidr: &str) -> bool {
    let Some((base, bits)) = cidr.split_once('/') else { return false };
    let Ok(b) = bits.parse::<u32>() else { return false };
    if b > 32 {
        return false;
    }
    let mask = if b == 0 { 0 } else { u32::MAX << (32 - b) };
    match (ip_to_int(ip), ip_to_int(base)) {
        (Some(a), Some(c)) => a & mask == c & mask,
        _ => false,
    }
}

/// a.b.c.d/n with numbers only (the old check: /^\d+\.\d+\.\d+\.\d+\/\d+$/)
pub fn looks_like_cidr(s: &str) -> bool {
    let Some((ip, bits)) = s.split_once('/') else { return false };
    let mut parts = 0;
    for p in ip.split('.') {
        if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    parts == 4 && !bits.is_empty() && bits.bytes().all(|c| c.is_ascii_digit())
}

/// a.b.c.d with numbers only (/^\d+\.\d+\.\d+\.\d+$/)
pub fn looks_like_ip(s: &str) -> bool {
    let mut parts = 0;
    for p in s.split('.') {
        if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    parts == 4
}

// ---- files --------------------------------------------------------------------------------------------------------

/// Reads all of `path` into `buf`; an error if it is larger than `buf`.
pub fn read_file<'a>(path: &str, buf: &'a mut [u8]) -> std::io::Result<&'a [u8]> {
    let mut f = std::fs::File::open(path)?;
    let mut n = 0;
    loop {
        if n == buf.len() {
            let mut one = [0u8; 1];
            if f.read(&mut one)? != 0 {
                return Err(std::io::Error::from_raw_os_error(libc::EFBIG));
            }
            break;
        }
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(&buf[..n])
}

/// Reads a small file (e.g. a cgroup or /proc value) into a stack buffer and returns it trimmed.
pub fn read_small<const N: usize>(path: &str) -> Option<Str<N>> {
    let mut b = [0u8; N];
    let s = read_file(path, &mut b).ok()?;
    let s = core::str::from_utf8(s).ok()?.trim();
    Str::from(s)
}

/// Writes `data` to `path` (mode `mode`) through a temporary file and a rename.
/// An io error with the path it happened at.
pub fn at(path: &str, e: std::io::Error) -> Error {
    Error::fmt(500, format_args!("{path}: {}", io_msg(&e)))
}

pub fn write_file_atomic(path: &str, data: &[u8], mode: u32) -> Res<()> {
    write_file_atomic_io(path, data, mode).map_err(|e| at(path, e))
}

fn write_file_atomic_io(path: &str, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp: Str<300> = Str::new();
    if write!(tmp, "{path}.tmp").is_err() {
        return Err(std::io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    {
        let mut f =
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(tmp.as_str())?;
        f.write_all(data)?;
    }
    std::fs::rename(tmp.as_str(), path)
}

pub fn remove_file(path: &str) {
    let _ = std::fs::remove_file(path);
}

pub fn exists(path: &str) -> bool {
    let mut b = [0u8; 512];
    match cstr(path, &mut b) {
        Some(c) => unsafe { libc::access(c.as_ptr(), libc::F_OK) == 0 },
        None => false,
    }
}

pub fn mkdir(path: &str, mode: u32) -> Res<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(mode).create(path).map_err(|e| at(path, e))
}

/// Calls `f` with each entry name of directory `path` (not . and ..), with libc only (std's read_dir allocates).
pub fn each_dir_entry(path: &str, mut f: impl FnMut(&str)) -> std::io::Result<()> {
    let mut b = [0u8; 512];
    let c = cstr(path, &mut b).ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
    let d = unsafe { libc::opendir(c.as_ptr()) };
    if d.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    loop {
        let e = unsafe { libc::readdir(d) };
        if e.is_null() {
            break;
        }
        let name = unsafe { core::ffi::CStr::from_ptr((*e).d_name.as_ptr()) };
        let Ok(n) = name.to_str() else { continue };
        if n == "." || n == ".." {
            continue;
        }
        f(n);
    }
    unsafe { libc::closedir(d) };
    Ok(())
}

/// sha256(s) as hex, the first 12 characters (the per-sandbox interface and table names).
pub fn h12(s: &str) -> Str<12> {
    let d = ring::digest::digest(&ring::digest::SHA256, s.as_bytes());
    let mut out = Str::new();
    for b in &d.as_ref()[..6] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

// ---- log lines ----------------------------------------------------------------------------------------------------

/// One JSON log line to stdout (journald), built in a 4 KiB stack buffer; longer lines are cut.
pub struct Log {
    mem: [u8; 4096],
    len: usize,
    stderr: bool,
}

impl Log {
    pub fn info(msg: &str) -> Log {
        Self::start(msg, false)
    }
    pub fn error(msg: &str) -> Log {
        Self::start(msg, true)
    }
    fn start(msg: &str, stderr: bool) -> Log {
        let mut l = Log { mem: [0; 4096], len: 0, stderr };
        let t = iso(now_ms());
        l.with(|w| {
            w.obj().kstr("t", t.as_str()).kstr("msg", msg);
        });
        l
    }
    fn with(&mut self, f: impl FnOnce(&mut json::W)) {
        let mut b = Buf::new(&mut self.mem[..4000]);
        b.len = self.len;
        let mut w = json::W::new(&mut b);
        // continue an open object: pretend a member was written already
        if self.len > 0 {
            w.obj_continue();
        }
        f(&mut w);
        if !b.full {
            self.len = b.len;
        }
    }
    pub fn s(mut self, k: &str, v: &str) -> Log {
        self.with(|w| {
            w.kstr(k, v);
        });
        self
    }
    pub fn n(mut self, k: &str, v: u64) -> Log {
        self.with(|w| {
            w.ku64(k, v);
        });
        self
    }
    /// Raw JSON value (must be valid).
    pub fn j(mut self, k: &str, v: &[u8]) -> Log {
        self.with(|w| {
            w.key(k).raw(v);
        });
        self
    }
    pub fn emit(mut self) {
        let n = self.len;
        self.mem[n] = b'}';
        self.mem[n + 1] = b'\n';
        let fd = if self.stderr { 2 } else { 1 };
        let mut off = 0;
        while off < n + 2 {
            let r = unsafe { libc::write(fd, self.mem[off..].as_ptr() as *const _, n + 2 - off) };
            if r <= 0 {
                break;
            }
            off += r as usize;
        }
    }
}
