//! The sealed stream a frozen sandbox travels in (jarvis4 docs/PLAN-fabric.md, "How a frozen image travels"): its
//! checkpoint image and scratch, cut into chunks of at most [`CHUNK`] bytes, each sealed with AES-256-GCM under the
//! freeze's own key. Pure: the caller does the reading, writing and uploading, in its own fixed buffers.
//!
//! A frame is `head ‖ ciphertext ‖ tag`: `head` is 4 bytes little-endian, the plaintext's length with
//! [`LAST`] set on the stream's final frame. The nonce is the frame's index (96 bits, big-endian), and the head is the
//! frame's additional data, so frames can be neither reordered, dropped, cut nor re-marked as the last: a stream that
//! ends before a frame marked last does not open ([`Opener::finish`]).
//!
//! `sandboxd seal` and `sandboxd open` ([`main`]) run it as a stage of a pipeline (stdin to stdout, the key in
//! `SBX_SEAL_KEY` as 64 hex digits): a freeze is `stream-sink | sandboxd seal | sandboxd s3-put …`, a thaw the reverse, so no byte
//! of an image passes through the daemon's fixed memory.

use crate::err;
use crate::util::Res;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

/// The longest plaintext of one frame.
pub const CHUNK: usize = 1 << 20;
/// The frame's head and the GCM tag around each chunk.
pub const HEAD: usize = 4;
pub const TAG: usize = 16;
/// The largest frame: a buffer this long holds any frame in place.
pub const FRAME_MAX: usize = HEAD + CHUNK + TAG;
/// The head's mark of the stream's final frame.
pub const LAST: u32 = 1 << 31;

fn key(k: &[u8; 32]) -> LessSafeKey {
    // a 32-byte key is always a valid AES-256 key
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, k).unwrap_or_else(|_| unreachable!()))
}

fn nonce(n: u64) -> Nonce {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_be_bytes());
    Nonce::assume_unique_for_key(b)
}

/// Seals a stream, frame by frame.
pub struct Sealer {
    key: LessSafeKey,
    n: u64,
    done: bool,
}

impl Sealer {
    pub fn new(k: &[u8; 32]) -> Self {
        Self { key: key(k), n: 0, done: false }
    }

    /// Seals the `len` plaintext bytes at `buf[HEAD..HEAD + len]` in place into the frame `buf[..HEAD + len + TAG]`
    /// and answers its length. `last` marks the stream's final frame (it may be empty); nothing follows it.
    pub fn seal(&mut self, buf: &mut [u8], len: usize, last: bool) -> Res<usize> {
        if self.done {
            return Err(err!(500, "a sealed stream has nothing after its last frame"));
        }
        if len > CHUNK || buf.len() < HEAD + len + TAG {
            return Err(err!(500, "a frame of {len} bytes does not fit"));
        }
        let head = len as u32 | if last { LAST } else { 0 };
        buf[..HEAD].copy_from_slice(&head.to_le_bytes());
        let (h, rest) = buf.split_at_mut(HEAD);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce(self.n), Aad::from(&h[..]), &mut rest[..len])
            .map_err(|_| err!(500, "sealing a frame failed"))?;
        rest[len..len + TAG].copy_from_slice(tag.as_ref());
        self.n += 1;
        self.done = last;
        Ok(HEAD + len + TAG)
    }
}

/// Opens a sealed stream, frame by frame.
pub struct Opener {
    key: LessSafeKey,
    n: u64,
    done: bool,
}

impl Opener {
    pub fn new(k: &[u8; 32]) -> Self {
        Self { key: key(k), n: 0, done: false }
    }

    /// The length of the frame whose head is `head` (to know how much to read).
    pub fn frame_len(head: [u8; HEAD]) -> Res<usize> {
        let len = (u32::from_le_bytes(head) & !LAST) as usize;
        if len > CHUNK {
            return Err(err!(422, "a sealed frame of {len} bytes is longer than any sealed frame"));
        }
        Ok(HEAD + len + TAG)
    }

    /// Opens the whole frame `frame` in place: its plaintext is `frame[HEAD..HEAD + n]`; answers `n` and whether it
    /// was the last frame. Anything wrong (another key, a changed, reordered or extra frame) is an error.
    pub fn open(&mut self, frame: &mut [u8]) -> Res<(usize, bool)> {
        if self.done {
            return Err(err!(422, "the sealed stream goes on after its last frame"));
        }
        if frame.len() < HEAD + TAG || Self::frame_len([frame[0], frame[1], frame[2], frame[3]])? != frame.len() {
            return Err(err!(422, "a sealed frame is cut"));
        }
        let last = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) & LAST != 0;
        let (h, rest) = frame.split_at_mut(HEAD);
        let plain = self
            .key
            .open_in_place(nonce(self.n), Aad::from(&h[..]), rest)
            .map_err(|_| err!(422, "a sealed frame does not open: another key, or the stream was changed"))?;
        let n = plain.len();
        self.n += 1;
        self.done = last;
        Ok((n, last))
    }

    /// The stream ended: an error unless its last frame was opened (a cut stream never passes for a whole one).
    pub fn finish(&self) -> Res<()> {
        if self.done { Ok(()) } else { Err(err!(422, "the sealed stream ends before its last frame")) }
    }
}

// ---- the pipeline stage -------------------------------------------------------------------------------------------

/// The key from `SBX_SEAL_KEY` (64 hex digits).
fn key_from_env() -> Option<[u8; 32]> {
    let hex = crate::env::seal_key()?;
    let b = hex.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, pair) in b.chunks(2).enumerate() {
        let d = |c: u8| (c as char).to_digit(16);
        k[i] = (d(pair[0])? * 16 + d(pair[1])?) as u8;
    }
    Some(k)
}

/// Reads until `buf` is full or the input ends; how much was read.
fn fill(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// stdin sealed to stdout: full chunks, then the rest (maybe empty) as the last frame.
fn seal_stream(k: &[u8; 32], r: &mut impl std::io::Read, w: &mut impl std::io::Write, buf: &mut [u8]) -> Res<()> {
    let mut s = Sealer::new(k);
    loop {
        let n = fill(r, &mut buf[HEAD..HEAD + CHUNK]).map_err(|e| err!(500, "reading: {e}"))?;
        let last = n < CHUNK;
        let f = s.seal(buf, n, last)?;
        w.write_all(&buf[..f]).map_err(|e| err!(500, "writing: {e}"))?;
        if last {
            return w.flush().map_err(|e| err!(500, "writing: {e}"));
        }
    }
}

/// stdin opened to stdout; an error for anything but a whole stream under this key.
fn open_stream(k: &[u8; 32], r: &mut impl std::io::Read, w: &mut impl std::io::Write, buf: &mut [u8]) -> Res<()> {
    let mut o = Opener::new(k);
    loop {
        let h = fill(r, &mut buf[..HEAD]).map_err(|e| err!(500, "reading: {e}"))?;
        if h == 0 {
            o.finish()?;
            return w.flush().map_err(|e| err!(500, "writing: {e}"));
        }
        if h < HEAD {
            return Err(err!(422, "a sealed frame is cut"));
        }
        let len = Opener::frame_len([buf[0], buf[1], buf[2], buf[3]])?;
        if fill(r, &mut buf[HEAD..len]).map_err(|e| err!(500, "reading: {e}"))? != len - HEAD {
            return Err(err!(422, "a sealed frame is cut"));
        }
        let (n, _) = o.open(&mut buf[..len])?;
        w.write_all(&buf[HEAD..HEAD + n]).map_err(|e| err!(500, "writing: {e}"))?;
    }
}

/// `sandboxd seal` / `sandboxd open`: the exit code (0, or 1 with the reason on stderr).
pub fn main(open: bool) -> i32 {
    use std::os::fd::FromRawFd as _;
    let Some(k) = key_from_env() else {
        eprintln!("sandboxd {}: SBX_SEAL_KEY must be 64 hex digits", if open { "open" } else { "seal" });
        return 2;
    };
    // the frame buffer, mapped here (this process only: the daemon's boot arena is far smaller)
    // SAFETY: an anonymous private mapping, checked for MAP_FAILED below, used by this process only.
    let p = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            FRAME_MAX,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        eprintln!("sandboxd seal: no memory for the frame buffer");
        return 1;
    }
    // SAFETY: the mapping is FRAME_MAX bytes, readable and writable, and lives until the process ends.
    let buf = unsafe { core::slice::from_raw_parts_mut(p.cast::<u8>(), FRAME_MAX) };
    // stdin and stdout as plain files (no std buffers, which would allocate); never closed here
    // SAFETY: fds 0 and 1 are open for the life of the process; ManuallyDrop keeps them open.
    let (mut r, mut w) = unsafe {
        (
            core::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(0)),
            core::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(1)),
        )
    };
    let res = if open { open_stream(&k, &mut *r, &mut *w, buf) } else { seal_stream(&k, &mut *r, &mut *w, buf) };
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sandboxd {}: {}", if open { "open" } else { "seal" }, e.msg.as_str());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: [u8; 32] = [7; 32];

    fn seal_all(k: &[u8; 32], data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let mut s = Sealer::new(k);
        let mut frames = Vec::new();
        let pieces: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(chunk).collect() };
        for (i, p) in pieces.iter().enumerate() {
            let mut buf = vec![0u8; HEAD + p.len() + TAG];
            buf[HEAD..HEAD + p.len()].copy_from_slice(p);
            let n = s.seal(&mut buf, p.len(), i + 1 == pieces.len()).unwrap();
            buf.truncate(n);
            frames.push(buf);
        }
        frames
    }

    fn open_all(k: &[u8; 32], frames: &mut [Vec<u8>]) -> Res<Vec<u8>> {
        let mut o = Opener::new(k);
        let mut out = Vec::new();
        for f in frames.iter_mut() {
            let (n, _) = o.open(f)?;
            out.extend_from_slice(&f[HEAD..HEAD + n]);
        }
        o.finish()?;
        Ok(out)
    }

    #[test]
    fn a_stream_opens_to_what_was_sealed() {
        let data: Vec<u8> = (0..2_500u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut frames = seal_all(&K, &data, 1000);
        assert_eq!(frames.len(), 3);
        assert_eq!(open_all(&K, &mut frames).unwrap(), data);
        // an empty stream is one empty last frame
        let mut empty = seal_all(&K, &[], CHUNK);
        assert_eq!(open_all(&K, &mut empty).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn a_changed_reordered_cut_or_extended_stream_does_not_open() {
        let data = vec![42u8; 10_000];
        let fresh = || seal_all(&K, &data, 1000);
        // another key
        assert!(open_all(&[8; 32], &mut fresh()).is_err());
        // a flipped byte
        let mut f = fresh();
        f[3][HEAD + 5] ^= 1;
        assert!(open_all(&K, &mut f).is_err());
        // two frames swapped
        let mut f = fresh();
        f.swap(1, 2);
        assert!(open_all(&K, &mut f).is_err());
        // the last frame dropped: every frame opens, the stream does not
        let mut f = fresh();
        f.pop();
        assert!(open_all(&K, &mut f).is_err());
        // a frame re-marked as the last
        let mut f = fresh();
        f[4][3] |= 0x80;
        assert!(open_all(&K, &mut f).is_err());
        // a frame after the last
        let mut f = fresh();
        let again = f[0].clone();
        f.push(again);
        assert!(open_all(&K, &mut f).is_err());
    }

    #[test]
    fn the_pipeline_stages_round_trip_and_refuse_a_cut_stream() {
        let data: Vec<u8> = (0..(CHUNK + 123)).map(|i| (i % 253) as u8).collect();
        let mut buf = vec![0u8; FRAME_MAX];
        let mut sealed = Vec::new();
        seal_stream(&K, &mut &data[..], &mut sealed, &mut buf).unwrap();
        let mut opened = Vec::new();
        open_stream(&K, &mut &sealed[..], &mut opened, &mut buf).unwrap();
        assert_eq!(opened, data);
        // exactly a whole number of chunks: an empty last frame follows
        let even = vec![1u8; CHUNK];
        let mut sealed2 = Vec::new();
        seal_stream(&K, &mut &even[..], &mut sealed2, &mut buf).unwrap();
        assert_eq!(sealed2.len(), FRAME_MAX + HEAD + TAG);
        let mut out = Vec::new();
        open_stream(&K, &mut &sealed2[..], &mut out, &mut buf).unwrap();
        assert_eq!(out, even);
        // cut anywhere: refused
        for cut in [1, HEAD, FRAME_MAX, sealed.len() - 1] {
            let mut out = Vec::new();
            assert!(open_stream(&K, &mut &sealed[..cut], &mut out, &mut buf).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn nothing_seals_after_the_last_frame_and_no_frame_is_too_long() {
        let mut s = Sealer::new(&K);
        let mut buf = vec![0u8; FRAME_MAX];
        s.seal(&mut buf, 0, true).unwrap();
        assert!(s.seal(&mut buf, 0, false).is_err());
        let mut s = Sealer::new(&K);
        assert!(s.seal(&mut buf, CHUNK + 1, false).is_err());
        assert!(Opener::frame_len(((CHUNK + 1) as u32).to_le_bytes()).is_err());
        assert_eq!(Opener::frame_len(((CHUNK as u32) | LAST).to_le_bytes()).unwrap(), FRAME_MAX);
    }
}
