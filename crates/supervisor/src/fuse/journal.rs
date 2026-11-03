//! The request journal (supervisor 9.9): `fuse-journal` on scratch, mapped `MAP_SHARED`; the FUSE reader reads each
//! request straight into a free slot's mapped memory, so once `read(2)` returned the request is in page cache that
//! outlives the process. A slot is occupied exactly when its header's `len` is non-zero; it is zeroed after the
//! reply is written. After the 64 request slots, a 64-entry area keeps the interrupted results of 9.3 (a result
//! whose reply the sentry refused with ENOENT, until the thread's resend takes it), so they survive a restart too.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::proto::{IN_HEADER, InHeader, u32_at};

/// Bytes of one kept result (header + reply payload; replies of mutating ops are at most 144 bytes).
pub const KEPT_BYTES: usize = 4096;
/// The kept area's entries.
pub const KEPT_SLOTS: usize = 64;
const KEPT_MAGIC: u32 = 0x4b45_5054; // "KEPT"

/// The mapping. Each worker owns one slot; the kept area is behind the caller's lock.
pub struct Journal {
    base: *mut u8,
    len: usize,
    slots: usize,
    slot_bytes: usize,
    file: File,
    path: PathBuf,
}

// SAFETY: the mapping lives as long as the Journal; slots are handed to one worker each, the kept area is only
// touched under the dispatcher's resend lock.
#[allow(unsafe_code)]
unsafe impl Send for Journal {}
// SAFETY: as above.
#[allow(unsafe_code)]
unsafe impl Sync for Journal {}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("path", &self.path)
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

/// A kept result, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptRecord {
    pub pid: u32,
    pub opcode: u32,
    pub nodeid: u64,
    pub key: [u8; 32],
    pub errno: i32,
    pub payload: Vec<u8>,
}

impl Journal {
    /// Opens (sparse, sized) and maps the journal.
    ///
    /// # Errors
    /// The file cannot be made or mapped.
    pub fn open(path: &Path, slots: usize, slot_bytes: usize) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let len = slots * slot_bytes + KEPT_SLOTS * KEPT_BYTES;
        file.set_len(len as u64)?;
        // SAFETY: a shared read/write mapping of a file just sized to `len`; unmapped in Drop.
        #[allow(unsafe_code)]
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            base: p.cast(),
            len,
            slots,
            slot_bytes,
            file,
            path: path.to_owned(),
        })
    }

    #[must_use]
    pub fn slots(&self) -> usize {
        self.slots
    }

    #[must_use]
    pub fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }

    /// Slot `i`'s memory.
    ///
    /// # Panics
    /// `i` is not a slot.
    ///
    /// # Safety
    /// The caller is the only user of slot `i` while the slice lives (one worker per slot; the replay runs before
    /// any worker starts).
    #[allow(unsafe_code, clippy::mut_from_ref)]
    #[must_use]
    pub unsafe fn slot(&self, i: usize) -> &mut [u8] {
        assert!(i < self.slots);
        // SAFETY: inside the mapping; exclusive by the caller's contract.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(i * self.slot_bytes), self.slot_bytes) }
    }

    /// The `len` field of slot `i`, read and written atomically (the reader zeroes it before each read).
    fn len_field(&self, i: usize) -> &AtomicU32 {
        // SAFETY: 4-byte aligned (the mapping is page-aligned, slot sizes are multiples of 4096), inside it.
        #[allow(unsafe_code, clippy::cast_ptr_alignment)]
        unsafe {
            &*self.base.add(i * self.slot_bytes).cast::<AtomicU32>()
        }
    }

    /// Frees slot `i` (the reply was written).
    pub fn free(&self, i: usize) {
        self.len_field(i).store(0, Ordering::SeqCst);
    }

    /// The occupied slots in `unique` order (the replay after a restart).
    #[must_use]
    pub fn occupied(&self) -> Vec<(u64, usize)> {
        let mut v = Vec::new();
        for i in 0..self.slots {
            if self.len_field(i).load(Ordering::SeqCst) != 0 {
                // SAFETY: before any worker runs.
                #[allow(unsafe_code)]
                let s = unsafe { self.slot(i) };
                if let Some(h) = InHeader::parse(s) {
                    v.push((h.unique, i));
                }
            }
        }
        v.sort_unstable();
        v
    }

    /// The short-read check of 9.9: `read(2)` returned `n`, the slot's header must say `len == n`. If not, logs
    /// `fuse.short_read` with both and the journal file's own bytes at that slot, and takes the file's copy when
    /// that one is whole. Never panics on a header.
    pub fn whole_request(&self, i: usize, slot: &mut [u8], n: usize) -> bool {
        let mlen = u32_at(slot, 0) as usize;
        if n >= IN_HEADER && mlen == n {
            return true;
        }
        let mut fb = vec![0u8; n.clamp(IN_HEADER, self.slot_bytes)];
        let fread = self.file.read_at(&mut fb, (i * self.slot_bytes) as u64).unwrap_or(0);
        let flen = u32_at(&fb, 0) as usize;
        tracing::error!(
            event = "fuse.short_read",
            slot = i,
            read = n,
            map_len = mlen,
            map_opcode = u32_at(slot, 4),
            file_read = fread,
            file_len = flen,
            file_opcode = u32_at(&fb, 4),
            "a read left no whole request in the slot"
        );
        if n >= IN_HEADER && flen == n && fread >= n {
            slot[..n].copy_from_slice(&fb[..n]);
            return true;
        }
        false
    }

    /// After a short read: the header of the request the slot should hold, from the mapping or else the journal
    /// file's copy, when one names a request that waits for a reply (a non-zero `unique`, not FORGET or INTERRUPT).
    #[must_use]
    pub fn lost_header(&self, i: usize, slot: &[u8]) -> Option<InHeader> {
        let mut fb = vec![0u8; IN_HEADER];
        let fread = self.file.read_at(&mut fb, (i * self.slot_bytes) as u64).unwrap_or(0);
        let file = (fread >= IN_HEADER).then(|| InHeader::parse(&fb)).flatten();
        [InHeader::parse(slot), file].into_iter().flatten().find(|h| {
            h.unique != 0
                && !matches!(
                    h.opcode,
                    super::proto::op::FORGET | super::proto::op::BATCH_FORGET | super::proto::op::INTERRUPT
                )
        })
    }

    fn kept_ptr(&self, k: usize) -> *mut u8 {
        assert!(k < KEPT_SLOTS);
        // SAFETY: inside the mapping (the kept area follows the slots).
        #[allow(unsafe_code)]
        unsafe {
            self.base.add(self.slots * self.slot_bytes + k * KEPT_BYTES)
        }
    }

    #[allow(clippy::mut_from_ref)] // the mapping is shared memory; the caller holds the resend lock
    fn kept_mut(&self, k: usize) -> &mut [u8] {
        // SAFETY: the kept area is only used under the dispatcher's resend lock.
        #[allow(unsafe_code)]
        unsafe {
            std::slice::from_raw_parts_mut(self.kept_ptr(k), KEPT_BYTES)
        }
    }

    /// Stores a kept result in entry `k`.
    pub fn keep(&self, k: usize, r: &KeptRecord) {
        let b = self.kept_mut(k);
        let n = r.payload.len().min(KEPT_BYTES - 64);
        b[4..8].copy_from_slice(&r.pid.to_le_bytes());
        b[8..12].copy_from_slice(&r.opcode.to_le_bytes());
        b[12..20].copy_from_slice(&r.nodeid.to_le_bytes());
        b[20..52].copy_from_slice(&r.key);
        b[52..56].copy_from_slice(&r.errno.to_le_bytes());
        #[allow(clippy::cast_possible_truncation)] // < 4096
        b[56..60].copy_from_slice(&(n as u32).to_le_bytes());
        b[64..64 + n].copy_from_slice(&r.payload[..n]);
        b[0..4].copy_from_slice(&KEPT_MAGIC.to_le_bytes());
    }

    /// Clears entry `k`.
    pub fn unkeep(&self, k: usize) {
        self.kept_mut(k)[0..4].copy_from_slice(&[0; 4]);
    }

    /// Clears every kept entry (a fresh connection: its results belong to the old one).
    pub fn clear_kept(&self) {
        for k in 0..KEPT_SLOTS {
            self.unkeep(k);
        }
    }

    /// The stored kept results (a restart that took the connection from the keeper).
    #[must_use]
    pub fn kept(&self) -> Vec<(usize, KeptRecord)> {
        let mut v = Vec::new();
        for k in 0..KEPT_SLOTS {
            let b = self.kept_mut(k);
            if u32_at(b, 0) != KEPT_MAGIC {
                continue;
            }
            let n = (u32_at(b, 56) as usize).min(KEPT_BYTES - 64);
            let mut key = [0u8; 32];
            key.copy_from_slice(&b[20..52]);
            #[allow(clippy::cast_possible_wrap)] // stored from an i32
            v.push((
                k,
                KeptRecord {
                    pid: u32_at(b, 4),
                    opcode: u32_at(b, 8),
                    nodeid: super::proto::u64_at(b, 12),
                    key,
                    errno: u32_at(b, 52) as i32,
                    payload: b[64..64 + n].to_vec(),
                },
            ));
        }
        v
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `open`, of `len` bytes.
        #[allow(unsafe_code)]
        unsafe {
            libc::munmap(self.base.cast(), self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_survive_the_process_view_and_kept_results_round_trip() {
        let dir = std::env::temp_dir().join(format!("journal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fuse-journal");
        {
            let j = Journal::open(&p, 4, 8192).unwrap();
            let h = InHeader {
                len: 44,
                opcode: 16,
                unique: 77,
                nodeid: 5,
                uid: 0,
                gid: 0,
                pid: 3,
            };
            // SAFETY: test, single user.
            #[allow(unsafe_code)]
            let s = unsafe { j.slot(2) };
            s[..40].copy_from_slice(&h.encode());
            s[40..44].copy_from_slice(b"abcd");
            j.keep(
                1,
                &KeptRecord {
                    pid: 3,
                    opcode: 9,
                    nodeid: 4,
                    key: [7; 32],
                    errno: 0,
                    payload: vec![1, 2, 3],
                },
            );
        }
        let j = Journal::open(&p, 4, 8192).unwrap();
        assert_eq!(j.occupied(), vec![(77, 2)]);
        let kept = j.kept();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1.payload, vec![1, 2, 3]);
        // a short read: the header says 44, read(2) said 40 → the file's copy is not whole either
        #[allow(unsafe_code)]
        let s = unsafe { j.slot(2) };
        assert!(j.whole_request(2, s, 44));
        assert!(!j.whole_request(2, s, 40));
        j.free(2);
        assert_eq!(j.occupied(), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
