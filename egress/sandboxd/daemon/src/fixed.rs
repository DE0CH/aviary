//! Fixed-capacity strings and buffers, and the one region every fixed table and buffer is carved from at start.
use core::fmt;

/// A UTF-8 string of at most N bytes, inline. Pushing past N fails (never allocates, never truncates silently).
#[derive(Clone, Copy)]
pub struct Str<const N: usize> {
    len: usize,
    b: [u8; N],
}

impl<const N: usize> Default for Str<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Str<N> {
    pub const fn new() -> Self {
        Str { len: 0, b: [0; N] }
    }
    pub fn from(s: &str) -> Option<Self> {
        let mut x = Self::new();
        if x.push(s) { Some(x) } else { None }
    }
    /// Like `from`, cutting at a character boundary when `s` is too long (for messages).
    pub fn lossy(s: &str) -> Self {
        let mut x = Self::new();
        x.push_lossy(s);
        x
    }
    pub fn as_str(&self) -> &str {
        // SAFETY: only whole &str values (or char-boundary prefixes of them) are ever copied in.
        unsafe { core::str::from_utf8_unchecked(&self.b[..self.len]) }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.b[..self.len]
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn push(&mut self, s: &str) -> bool {
        if self.len + s.len() > N {
            return false;
        }
        self.b[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        true
    }
    pub fn push_lossy(&mut self, s: &str) {
        let room = N - self.len;
        let mut n = s.len().min(room);
        while n > 0 && !s.is_char_boundary(n) {
            n -= 1;
        }
        self.push(&s[..n]);
    }
    pub fn set(&mut self, s: &str) -> bool {
        self.clear();
        self.push(s)
    }
    /// For C calls: the string followed by NUL in `out`.
    pub fn cstr<'a>(&self, out: &'a mut [u8]) -> Option<&'a core::ffi::CStr> {
        cstr(self.as_str(), out)
    }
}

impl<const N: usize> fmt::Write for Str<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.push(s) { Ok(()) } else { Err(fmt::Error) }
    }
}

impl<const N: usize> fmt::Display for Str<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> fmt::Debug for Str<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> PartialEq<str> for Str<N> {
    fn eq(&self, o: &str) -> bool {
        self.as_str() == o
    }
}
impl<const N: usize> PartialEq for Str<N> {
    fn eq(&self, o: &Self) -> bool {
        self.as_str() == o.as_str()
    }
}

/// `s` + NUL into `out` (None if it doesn't fit or `s` has a NUL).
pub fn cstr<'a>(s: &str, out: &'a mut [u8]) -> Option<&'a core::ffi::CStr> {
    if s.len() + 1 > out.len() || s.as_bytes().contains(&0) {
        return None;
    }
    out[..s.len()].copy_from_slice(s.as_bytes());
    out[s.len()] = 0;
    core::ffi::CStr::from_bytes_with_nul(&out[..s.len() + 1]).ok()
}

/// A writer over a caller's byte slice. Overflow sets `full` and drops the rest (callers check `full`).
pub struct Buf<'a> {
    pub b: &'a mut [u8],
    pub len: usize,
    pub full: bool,
}

impl<'a> Buf<'a> {
    pub fn new(b: &'a mut [u8]) -> Self {
        Buf { b, len: 0, full: false }
    }
    pub fn bytes(&mut self, s: &[u8]) {
        if self.full || self.len + s.len() > self.b.len() {
            self.full = true;
            return;
        }
        self.b[self.len..self.len + s.len()].copy_from_slice(s);
        self.len += s.len();
    }
    pub fn s(&mut self, s: &str) {
        self.bytes(s.as_bytes())
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.b[..self.len]
    }
    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.b[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.s(s);
        if self.full { Err(fmt::Error) } else { Ok(()) }
    }
}

/// Types whose all-zero bit pattern is a valid value (plain bytes, integers, bools, floats, arrays and structs of
/// those), so they can live in the zeroed region.
/// # Safety
/// Only for such types.
pub unsafe trait Zeroed {}
unsafe impl<const N: usize> Zeroed for Str<N> {}
unsafe impl<const N: usize> Zeroed for [u8; N] {}
unsafe impl Zeroed for u8 {}

/// One anonymous mapping, made at start and never grown, from which every fixed table and buffer is carved.
pub struct Region {
    base: *mut u8,
    size: usize,
    used: usize,
}

unsafe impl Send for Region {}

impl Region {
    /// Maps `size` bytes, populated (no page is faulted in later).
    pub fn map(size: usize) -> std::io::Result<Region> {
        let size = (size + 4095) & !4095;
        // SAFETY: an anonymous private mapping; the result is checked.
        let p = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Region { base: p as *mut u8, size, used: 0 })
    }

    fn take(&mut self, bytes: usize, align: usize) -> *mut u8 {
        let start = (self.used + align - 1) & !(align - 1);
        assert!(start + bytes <= self.size, "region too small (budget arithmetic is wrong)");
        self.used = start + bytes;
        // SAFETY: within the mapping.
        unsafe { self.base.add(start) }
    }

    /// `n` zeroed values of T, for the life of the process.
    pub fn slice<T: Zeroed>(&mut self, n: usize) -> &'static mut [T] {
        let p = self.take(core::mem::size_of::<T>() * n, core::mem::align_of::<T>().max(64)) as *mut T;
        // SAFETY: fresh, zeroed (anonymous mapping), aligned memory that is never handed out again; zero is a valid T.
        unsafe { core::slice::from_raw_parts_mut(p, n) }
    }

    /// Raw bytes (for the heap arenas).
    pub fn raw(&mut self, bytes: usize) -> *mut u8 {
        self.take(bytes, 4096)
    }
}

/// Bytes needed in the region for `n` values of T (with the alignment `slice` uses).
pub const fn region_bytes<T>(n: usize) -> usize {
    core::mem::size_of::<T>() * n + 64
}
