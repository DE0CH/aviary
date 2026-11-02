//! Local scope (supervisor 9.1, 9.3): passthrough to a backing directory on scratch (`/var/lib/supervisor/local/`)
//! that lives and dies with the sandbox. Handles index a table of backing fds; a restarted supervisor rebuilds an
//! entry by reopening the node's path with the flags recorded in the handle (9.8).

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::errno::{EIO, EPERM};
use super::proto::{Attr, FileType, OWNER};

fn errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

/// The handle bits of 9.8: `node << 8 | flags`.
pub mod fh {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
    pub const APPEND: u64 = 4;

    /// The handle of an open with these `open(2)` flags.
    #[must_use]
    pub fn of(node: u64, flags: i32) -> u64 {
        let acc = flags & libc::O_ACCMODE;
        let mut b = 0;
        if acc == libc::O_RDONLY || acc == libc::O_RDWR {
            b |= READ;
        }
        if acc == libc::O_WRONLY || acc == libc::O_RDWR {
            b |= WRITE;
        }
        if flags & libc::O_APPEND != 0 {
            b |= APPEND;
        }
        node << 8 | b
    }

    /// The node a handle names.
    #[must_use]
    pub fn node(fh: u64) -> u64 {
        fh >> 8
    }
}

struct Open {
    file: Arc<File>,
    refs: u32,
}

/// The backing directory and its fd table.
pub struct Local {
    root: PathBuf,
    fds: Mutex<HashMap<u64, Open>>,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn ns(s: i64, n: i64) -> i64 {
    s.saturating_mul(1_000_000_000).saturating_add(n)
}

fn attr_of(ino: u64, m: &std::fs::Metadata) -> Attr {
    let ft = m.file_type();
    Attr {
        ino,
        size: m.len(),
        mtime_ns: ns(m.mtime(), m.mtime_nsec()),
        ctime_ns: ns(m.ctime(), m.ctime_nsec()),
        perm: m.mode() & 0o7777,
        kind: if ft.is_dir() {
            FileType::Dir
        } else if ft.is_symlink() {
            FileType::Symlink
        } else {
            FileType::File
        },
    }
}

fn cpath(p: &Path) -> Result<CString, i32> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| libc::EINVAL)
}

impl Local {
    /// The backing directory (made if missing).
    ///
    /// # Errors
    /// It cannot be made.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_owned(),
            fds: Mutex::new(HashMap::new()),
        })
    }

    /// The backing path of a path under `~/.claude`.
    #[must_use]
    pub fn backing(&self, path: &str) -> PathBuf {
        if path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(path)
        }
    }

    /// Whether a path exists in the backing directory.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(self.backing(path)).is_ok()
    }

    /// `lstat`, or `fstat` of the open handle when the path is gone (an unlinked open file).
    ///
    /// # Errors
    /// The errno.
    pub fn getattr(&self, ino: u64, path: &str, handle: Option<u64>) -> Result<Attr, i32> {
        match std::fs::symlink_metadata(self.backing(path)) {
            Ok(m) => Ok(attr_of(ino, &m)),
            Err(e) => {
                if let Some(h) = handle
                    && let Some(f) = self.table_get(h)
                {
                    return f.metadata().map(|m| attr_of(ino, &m)).map_err(|e| errno(&e));
                }
                Err(errno(&e))
            }
        }
    }

    /// The entries of a directory: (name, type).
    ///
    /// # Errors
    /// The errno.
    pub fn readdir(&self, path: &str) -> Result<Vec<(String, FileType)>, i32> {
        let rd = std::fs::read_dir(self.backing(path)).map_err(|e| errno(&e))?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let kind = match e.file_type() {
                Ok(t) if t.is_dir() => FileType::Dir,
                Ok(t) if t.is_symlink() => FileType::Symlink,
                _ => FileType::File,
            };
            out.push((name, kind));
        }
        out.sort();
        Ok(out)
    }

    fn table_get(&self, h: u64) -> Option<Arc<File>> {
        self.fds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&h)
            .map(|o| o.file.clone())
    }

    fn table_put(&self, h: u64, f: File) {
        let mut t = self.fds.lock().unwrap_or_else(PoisonError::into_inner);
        let e = t.entry(h).or_insert(Open {
            file: Arc::new(f),
            refs: 0,
        });
        e.refs += 1;
    }

    fn options(bits: u64) -> OpenOptions {
        let mut o = OpenOptions::new();
        o.read(bits & fh::READ != 0).write(bits & fh::WRITE != 0);
        o
    }

    /// The backing file of a handle; reopened by path after a restart (the table is empty then).
    fn file(&self, h: u64, path: &str) -> Result<Arc<File>, i32> {
        if let Some(f) = self.table_get(h) {
            return Ok(f);
        }
        let f = Self::options(h & 0xff)
            .open(self.backing(path))
            .map_err(|e| errno(&e))?;
        self.table_put(h, f);
        self.table_get(h).ok_or(EIO)
    }

    /// `open(2)` (`O_APPEND` is not passed: the sentry computes append offsets itself, 2.7).
    ///
    /// # Errors
    /// The errno.
    pub fn open_file(&self, node: u64, path: &str, flags: i32) -> Result<u64, i32> {
        let h = fh::of(node, flags);
        let mut o = Self::options(h & 0xff);
        if flags & libc::O_TRUNC != 0 {
            o.truncate(true);
        }
        let f = o.open(self.backing(path)).map_err(|e| errno(&e))?;
        self.table_put(h, f);
        Ok(h)
    }

    /// `open(2)` with `O_CREAT`.
    ///
    /// # Errors
    /// The errno.
    pub fn create(&self, node_for: impl FnOnce() -> u64, path: &str, flags: i32, mode: u32) -> Result<u64, i32> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).mode(mode & 0o7777);
        if flags & libc::O_EXCL != 0 {
            o.create_new(true);
        } else {
            o.create(true);
        }
        if flags & libc::O_TRUNC != 0 {
            o.truncate(true);
        }
        let f = o.open(self.backing(path)).map_err(|e| errno(&e))?;
        let h = fh::of(node_for(), flags);
        self.table_put(h, f);
        Ok(h)
    }

    /// RELEASE.
    pub fn release(&self, h: u64) {
        let mut t = self.fds.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = t.get_mut(&h) {
            e.refs = e.refs.saturating_sub(1);
            if e.refs == 0 {
                t.remove(&h);
            }
        }
    }

    /// READ.
    ///
    /// # Errors
    /// The errno.
    pub fn read(&self, h: u64, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, i32> {
        let f = self.file(h, path)?;
        let mut buf = vec![0u8; size as usize];
        let mut got = 0;
        while got < buf.len() {
            match f.read_at(&mut buf[got..], offset + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(errno(&e)),
            }
        }
        buf.truncate(got);
        Ok(buf)
    }

    /// WRITE.
    ///
    /// # Errors
    /// The errno.
    pub fn write(&self, h: u64, path: &str, offset: u64, data: &[u8]) -> Result<u32, i32> {
        let f = self.file(h, path)?;
        f.write_all_at(data, offset).map_err(|e| errno(&e))?;
        #[allow(clippy::cast_possible_truncation)] // at most 1 MiB
        Ok(data.len() as u32)
    }

    /// FSYNC.
    ///
    /// # Errors
    /// The errno.
    pub fn fsync(&self, h: u64, path: &str) -> Result<(), i32> {
        self.file(h, path)?.sync_all().map_err(|e| errno(&e))
    }

    /// MKDIR.
    ///
    /// # Errors
    /// The errno.
    pub fn mkdir(&self, path: &str, mode: u32) -> Result<(), i32> {
        let p = self.backing(path);
        std::fs::create_dir(&p).map_err(|e| errno(&e))?;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode & 0o7777));
        Ok(())
    }

    /// RMDIR.
    ///
    /// # Errors
    /// The errno.
    pub fn rmdir(&self, path: &str) -> Result<(), i32> {
        std::fs::remove_dir(self.backing(path)).map_err(|e| errno(&e))
    }

    /// UNLINK.
    ///
    /// # Errors
    /// The errno.
    pub fn unlink(&self, path: &str) -> Result<(), i32> {
        std::fs::remove_file(self.backing(path)).map_err(|e| errno(&e))
    }

    /// RENAME (replaces the target, as `rename(2)`).
    ///
    /// # Errors
    /// The errno.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), i32> {
        std::fs::rename(self.backing(from), self.backing(to)).map_err(|e| errno(&e))
    }

    /// SYMLINK.
    ///
    /// # Errors
    /// The errno.
    pub fn symlink(&self, target: &[u8], path: &str) -> Result<(), i32> {
        let t = std::ffi::OsStr::from_bytes(target);
        std::os::unix::fs::symlink(t, self.backing(path)).map_err(|e| errno(&e))
    }

    /// READLINK.
    ///
    /// # Errors
    /// The errno.
    pub fn readlink(&self, path: &str) -> Result<Vec<u8>, i32> {
        std::fs::read_link(self.backing(path))
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .map_err(|e| errno(&e))
    }

    /// SETATTR: size (`ftruncate` on the handle when given), mode, mtime; uid/gid only 1000.
    ///
    /// # Errors
    /// The errno.
    pub fn setattr(&self, path: &str, set: &SetAttr) -> Result<(), i32> {
        if set.uid.is_some_and(|u| u != OWNER) || set.gid.is_some_and(|g| g != OWNER) {
            return Err(EPERM);
        }
        let p = self.backing(path);
        if let Some(size) = set.size {
            match set.handle.map(|h| self.file(h, path)) {
                Some(Ok(f)) => f.set_len(size).map_err(|e| errno(&e))?,
                _ => OpenOptions::new()
                    .write(true)
                    .open(&p)
                    .and_then(|f| f.set_len(size))
                    .map_err(|e| errno(&e))?,
            }
        }
        if let Some(m) = set.mode {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(m & 0o7777)).map_err(|e| errno(&e))?;
        }
        if set.mtime_ns.is_some() || set.atime_ns.is_some() {
            let c = cpath(&p)?;
            let ts = |v: Option<i64>| match v {
                Some(n) => libc::timespec {
                    tv_sec: n.div_euclid(1_000_000_000),
                    tv_nsec: n.rem_euclid(1_000_000_000),
                },
                None => libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
            };
            let times = [ts(set.atime_ns), ts(set.mtime_ns)];
            // SAFETY: a NUL-terminated path and two timespecs.
            #[allow(unsafe_code)]
            let r = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
            if r != 0 {
                return Err(errno(&std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

/// What a SETATTR asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetAttr {
    pub handle: Option<u64>,
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime_ns: Option<i64>,
    pub mtime_ns: Option<i64>,
}
