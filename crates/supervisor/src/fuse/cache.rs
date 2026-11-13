//! What is cached locally (supervisor 9.7): the tree (every magpie-scope path's kind, size, mtime, mode and
//! version, in memory; loaded from magpie at every lease, never from a local copy, so spec 9.7's `tree.json`, which
//! nothing would read, is not written) and the content cache (`cache/<node>.<version>`, a file's bytes as
//! last read or written; 256 MiB LRU for every file but the main transcript, which is never evicted).

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use magpie_api::sandbox::{CcEntry, CcKind, CcTree};
use serde::{Deserialize, Serialize};

/// One path of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: CcKind,
    pub size: u64,
    pub mtime_ns: i64,
    /// The last metadata change (the supervisor's; not in magpie).
    pub ctime_ns: i64,
    pub mode: u32,
    pub version: u64,
}

impl Entry {
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.kind == CcKind::Dir
    }
}

/// The tree cache. Paths are relative to `~/.claude`; the root is implicit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tree {
    pub files: BTreeMap<String, Entry>,
    pub cc_bytes: u64,
    pub quota_bytes: u64,
}

impl Tree {
    /// From magpie's answer.
    #[must_use]
    pub fn from_magpie(t: &CcTree) -> Self {
        let files = t
            .files
            .iter()
            .map(|e: &CcEntry| {
                (
                    e.path.clone(),
                    Entry {
                        kind: e.kind,
                        size: e.size,
                        mtime_ns: e.mtime_ns,
                        ctime_ns: e.mtime_ns,
                        mode: e.mode,
                        version: e.version,
                    },
                )
            })
            .collect();
        Self {
            files,
            cc_bytes: t.cc_bytes,
            quota_bytes: t.quota_bytes,
        }
    }

    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.files.get(path)
    }

    pub fn get_mut(&mut self, path: &str) -> Option<&mut Entry> {
        self.files.get_mut(path)
    }

    /// Whether `path` is a directory of the tree (`""`, the mount, and each harness's home always are).
    #[must_use]
    pub fn is_dir(&self, path: &str) -> bool {
        path.is_empty()
            || super::scope::split(path).is_some_and(|(_, rest)| rest.is_empty())
            || self.files.get(path).is_some_and(Entry::is_dir)
    }

    /// The direct children of a directory: (name, entry).
    #[must_use]
    pub fn children(&self, dir: &str) -> Vec<(String, Entry)> {
        let pre = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        self.files
            .range(pre.clone()..)
            .take_while(|(k, _)| k.starts_with(&pre))
            .filter(|(k, _)| !k[pre.len()..].contains('/') && k.len() > pre.len())
            .map(|(k, e)| (k[pre.len()..].to_owned(), *e))
            .collect()
    }

    #[must_use]
    pub fn has_children(&self, dir: &str) -> bool {
        let pre = format!("{dir}/");
        self.files
            .range(pre.clone()..)
            .next()
            .is_some_and(|(k, _)| k.starts_with(&pre))
    }

    pub fn insert(&mut self, path: &str, e: Entry) {
        let old = self.files.insert(path.to_owned(), e).map_or(0, |o| o.size);
        self.cc_bytes = (self.cc_bytes + e.size).saturating_sub(old);
    }

    /// Removes a path and its subtree.
    pub fn remove(&mut self, path: &str) -> Option<Entry> {
        let pre = format!("{path}/");
        let kids: Vec<String> = self
            .files
            .range(pre.clone()..)
            .take_while(|(k, _)| k.starts_with(&pre))
            .map(|(k, _)| k.clone())
            .collect();
        for k in kids {
            if let Some(e) = self.files.remove(&k) {
                self.cc_bytes = self.cc_bytes.saturating_sub(e.size);
            }
        }
        let e = self.files.remove(path)?;
        self.cc_bytes = self.cc_bytes.saturating_sub(e.size);
        Some(e)
    }

    /// Moves `from` and its subtree to `to`, replacing what was there.
    pub fn rename(&mut self, from: &str, to: &str, version: u64, now: i64) {
        if from == to || !self.files.contains_key(from) {
            // (a replayed rename whose effect the tree already shows)
            return;
        }
        self.remove(to);
        let pre = format!("{from}/");
        let kids: Vec<(String, Entry)> = self
            .files
            .range(pre.clone()..)
            .take_while(|(k, _)| k.starts_with(&pre))
            .map(|(k, e)| (k.clone(), *e))
            .collect();
        for (k, e) in kids {
            self.files.remove(&k);
            self.files.insert(format!("{to}{}", &k[from.len()..]), e);
        }
        if let Some(mut e) = self.files.remove(from) {
            e.version = version;
            e.ctime_ns = now;
            if e.kind != CcKind::Dir {
                e.kind = kind_of(to);
            }
            self.files.insert(to.to_owned(), e);
        }
    }
}

/// magpie 6.2: lines when the last component ends in `.jsonl` or contains `.jsonl.`.
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)] // magpie 6.2's rule is case-sensitive
pub fn kind_of(path: &str) -> CcKind {
    let last = super::scope::base(path);
    if last.ends_with(".jsonl") || last.contains(".jsonl.") {
        CcKind::Lines
    } else {
        CcKind::Bytes
    }
}

/// A change magpie acknowledged for a node whose whole-file fetch was streaming (applied to it when it ends).
#[derive(Clone, Debug)]
enum Pending {
    Write {
        old: u64,
        offset: u64,
        data: Vec<u8>,
        version: u64,
        size: u64,
    },
    Truncate {
        old: u64,
        version: u64,
        size: u64,
    },
    Reversion {
        old: u64,
        version: u64,
    },
}

impl Pending {
    fn old(&self) -> u64 {
        match self {
            Pending::Write { old, .. } | Pending::Truncate { old, .. } | Pending::Reversion { old, .. } => *old,
        }
    }
}

/// The most bytes of writes kept for one fetch in progress; past it the fetch is not cached.
const PENDING_MAX: usize = 64 << 20;

#[derive(Debug, Default)]
struct Fetching {
    ops: Vec<Pending>,
    bytes: usize,
    overflow: bool,
}

#[derive(Clone, Copy, Debug)]
struct Cached {
    version: u64,
    size: u64,
    used: u64,
}

/// The content cache. Every method takes the lock of the caller's choosing; file IO is short and per file.
#[derive(Debug)]
pub struct Content {
    dir: PathBuf,
    budget: u64,
    entries: HashMap<u64, Cached>,
    pinned: Option<u64>,
    total: u64,
    tick: u64,
    fetching: HashMap<u64, Fetching>,
}

impl Content {
    /// Opens the directory (made if missing).
    ///
    /// # Errors
    /// The directory cannot be made.
    pub fn open(dir: &Path, budget: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            budget,
            entries: HashMap::new(),
            pinned: None,
            total: 0,
            tick: 0,
            fetching: HashMap::new(),
        })
    }

    fn file(&self, node: u64, version: u64) -> PathBuf {
        self.dir.join(format!("{node}.{version}"))
    }

    /// The main transcript's node: never evicted, outside the budget.
    pub fn pin(&mut self, node: Option<u64>) {
        self.pinned = node;
    }

    /// At start (and on every new lease): keep the cache files whose node, version and size match the tree; hand
    /// back the ones newer and longer than magpie's copy (magpie may have lost acknowledged writes: the repair of
    /// 9.5 item 6 decides); drop the rest.
    pub fn reconcile(&mut self, current: impl Fn(u64) -> Option<(u64, u64)>) -> Vec<Candidate> {
        self.entries.clear();
        self.total = 0;
        let mut repair = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return repair;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let parsed = name
                .split_once('.')
                .and_then(|(n, v)| Some((n.parse::<u64>().ok()?, v.parse::<u64>().ok()?)));
            let len = e.metadata().map_or(u64::MAX, |m| m.len());
            let Some((node, version)) = parsed else {
                let _ = std::fs::remove_file(e.path());
                continue;
            };
            match current(node) {
                Some((v, s)) if v == version && s == len => {
                    self.tick += 1;
                    self.entries.insert(
                        node,
                        Cached {
                            version,
                            size: len,
                            used: self.tick,
                        },
                    );
                    if self.pinned != Some(node) {
                        self.total += len;
                    }
                }
                Some((v, s)) if version > v && len > s => repair.push(Candidate {
                    node,
                    version,
                    size: len,
                }),
                _ => {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        repair
    }

    /// The cache file for a node at exactly this version and size.
    #[must_use]
    pub fn valid(&mut self, node: u64, version: u64, size: u64) -> Option<PathBuf> {
        self.tick += 1;
        let t = self.tick;
        let c = self.entries.get_mut(&node)?;
        if c.version != version || c.size != size {
            return None;
        }
        c.used = t;
        Some(self.file(node, version))
    }

    /// A fresh file a fetch streams into before [`Content::filled`] (one per fetch, so two never mix).
    #[must_use]
    pub fn staging(&self, node: u64) -> PathBuf {
        self.dir.join(format!("{node}.{}.part", uuid::Uuid::new_v4().simple()))
    }

    /// A fetched file is complete: it becomes the node's cache entry at `version`.
    pub fn filled(&mut self, node: u64, version: u64, size: u64, staging: &Path) {
        self.forget(node);
        if std::fs::rename(staging, self.file(node, version)).is_err() {
            let _ = std::fs::remove_file(staging);
            return;
        }
        self.tick += 1;
        self.entries.insert(
            node,
            Cached {
                version,
                size,
                used: self.tick,
            },
        );
        if self.pinned != Some(node) {
            self.total += size;
        }
        self.evict();
    }

    /// A whole-file fetch of `node` starts: changes acknowledged until it ends are kept for it.
    pub fn begin_fetch(&mut self, node: u64) {
        self.fetching.insert(node, Fetching::default());
    }

    /// A fetch ended without a file.
    pub fn abort_fetch(&mut self, node: u64) {
        self.fetching.remove(&node);
    }

    /// A fetch of `node` at `version` and `size` completed into `staging`: the changes acknowledged meanwhile are
    /// applied to it in order, and it becomes the entry at the version they reached (returned with its size).
    /// `None` (and the staging file removed) when they do not chain from the fetched version or did not fit.
    pub fn fill_fetched(&mut self, node: u64, version: u64, size: u64, staging: &Path) -> Option<(u64, u64)> {
        let f = self.fetching.remove(&node).unwrap_or_default();
        let applied = (|| {
            if f.overflow {
                return None;
            }
            let mut cur = (version, size);
            if f.ops.is_empty() {
                return Some(cur);
            }
            let file = OpenOptions::new().write(true).open(staging).ok()?;
            for op in &f.ops {
                if op.old() != cur.0 {
                    return None;
                }
                cur = match op {
                    Pending::Write {
                        offset,
                        data,
                        version,
                        size,
                        ..
                    } => {
                        file.write_all_at(data, *offset).ok()?;
                        file.set_len(*size).ok()?;
                        (*version, *size)
                    }
                    Pending::Truncate { version, size, .. } => {
                        file.set_len(*size).ok()?;
                        (*version, *size)
                    }
                    Pending::Reversion { version, .. } => (*version, cur.1),
                };
            }
            Some(cur)
        })();
        let Some((v, s)) = applied else {
            let _ = std::fs::remove_file(staging);
            return None;
        };
        self.filled(node, v, s, staging);
        self.entries.contains_key(&node).then_some((v, s))
    }

    /// Keeps a change for the node's fetch in progress; whether there is one.
    fn pend(&mut self, node: u64, op: Pending) -> bool {
        let Some(f) = self.fetching.get_mut(&node) else {
            return false;
        };
        if let Pending::Write { data, .. } = &op {
            f.bytes += data.len();
        }
        if f.bytes > PENDING_MAX {
            f.overflow = true;
            f.ops.clear();
        } else if !f.overflow {
            f.ops.push(op);
        }
        true
    }

    /// An empty file just created at `version`.
    pub fn created(&mut self, node: u64, version: u64) {
        let st = self.staging(node);
        if File::create(&st).is_ok() {
            self.filled(node, version, 0, &st);
        }
    }

    /// magpie stored a write: apply it to the cached copy (if it was the current one) and rename it to the new
    /// version; a failure (disk full) drops the entry, never the write.
    pub fn wrote(&mut self, node: u64, old: (u64, u64), new_version: u64, offset: u64, data: &[u8], new_size: u64) {
        let Some(path) = self.valid(node, old.0, old.1) else {
            self.forget(node);
            self.pend(
                node,
                Pending::Write {
                    old: old.0,
                    offset,
                    data: data.to_vec(),
                    version: new_version,
                    size: new_size,
                },
            );
            return;
        };
        let ok = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| {
                f.write_all_at(data, offset)?;
                f.set_len(new_size)
            })
            .and_then(|()| std::fs::rename(&path, self.file(node, new_version)));
        self.moved(node, ok.is_ok(), new_version, new_size);
    }

    /// magpie stored a truncate.
    pub fn truncated(&mut self, node: u64, old: (u64, u64), new_version: u64, size: u64) {
        let Some(path) = self.valid(node, old.0, old.1) else {
            self.forget(node);
            self.pend(
                node,
                Pending::Truncate {
                    old: old.0,
                    version: new_version,
                    size,
                },
            );
            return;
        };
        let ok = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_len(size))
            .and_then(|()| std::fs::rename(&path, self.file(node, new_version)));
        self.moved(node, ok.is_ok(), new_version, size);
    }

    /// The path's version changed without its bytes (a rename, a setattr).
    pub fn reversion(&mut self, node: u64, old: (u64, u64), new_version: u64) {
        let Some(path) = self.valid(node, old.0, old.1) else {
            self.forget(node);
            self.pend(
                node,
                Pending::Reversion {
                    old: old.0,
                    version: new_version,
                },
            );
            return;
        };
        let ok = std::fs::rename(&path, self.file(node, new_version));
        self.moved(node, ok.is_ok(), new_version, old.1);
    }

    fn moved(&mut self, node: u64, ok: bool, version: u64, size: u64) {
        if !ok {
            tracing::warn!(node, "content cache update failed: entry dropped");
            self.forget(node);
            return;
        }
        if let Some(c) = self.entries.get_mut(&node) {
            if self.pinned != Some(node) {
                self.total = (self.total + size).saturating_sub(c.size);
            }
            c.version = version;
            c.size = size;
        }
        self.evict();
    }

    /// Drops a node's entry and file.
    pub fn forget(&mut self, node: u64) {
        if let Some(c) = self.entries.remove(&node) {
            if self.pinned != Some(node) {
                self.total = self.total.saturating_sub(c.size);
            }
            let _ = std::fs::remove_file(self.file(node, c.version));
        }
    }

    /// A repair candidate's file (its bytes are what magpie lost).
    #[must_use]
    pub fn candidate_file(&self, c: &Candidate) -> PathBuf {
        self.file(c.node, c.version)
    }

    /// Adopts a repaired candidate at magpie's new version.
    pub fn adopt(&mut self, c: &Candidate, version: u64) {
        if std::fs::rename(self.file(c.node, c.version), self.file(c.node, version)).is_ok() {
            self.tick += 1;
            self.entries.insert(
                c.node,
                Cached {
                    version,
                    size: c.size,
                    used: self.tick,
                },
            );
            if self.pinned != Some(c.node) {
                self.total += c.size;
            }
        }
    }

    /// A candidate whose repair stored part of the suffix (magpie is now at `magpie_version`): renamed above that
    /// version, so the next reconcile still finds it newer than magpie's copy.
    pub fn rekey_candidate(&self, c: &Candidate, magpie_version: u64) {
        if c.version <= magpie_version {
            let _ = std::fs::rename(self.file(c.node, c.version), self.file(c.node, magpie_version + 1));
        }
    }

    /// Drops a candidate that could not be repaired.
    pub fn discard(&self, c: &Candidate) {
        let _ = std::fs::remove_file(self.file(c.node, c.version));
    }

    fn evict(&mut self) {
        while self.total > self.budget {
            let victim = self
                .entries
                .iter()
                .filter(|(n, _)| Some(**n) != self.pinned)
                .min_by_key(|(_, c)| c.used)
                .map(|(n, _)| *n);
            let Some(v) = victim else { break };
            self.forget(v);
        }
    }

    /// Bytes held within the budget.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Whether a node has an entry.
    #[must_use]
    pub fn has(&self, node: u64) -> bool {
        self.entries.contains_key(&node)
    }

    /// Any entry of a node (for an unlinked node, whose tree entry is gone): its file and size.
    #[must_use]
    pub fn any(&self, node: u64) -> Option<(PathBuf, u64)> {
        self.entries.get(&node).map(|c| (self.file(node, c.version), c.size))
    }
}

/// A cache file newer and longer than magpie's copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub node: u64,
    pub version: u64,
    pub size: u64,
}

/// Reads `len` bytes at `offset` of a cache file (short at its end).
///
/// # Errors
/// The file cannot be read.
pub fn read_at(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let f = File::open(path)?;
    let size = f.metadata()?.len();
    #[allow(clippy::cast_possible_truncation)] // bounded by `len`
    let n = (size.saturating_sub(offset) as usize).min(len);
    let mut buf = vec![0u8; n];
    f.read_exact_at(&mut buf, offset)?;
    Ok(buf)
}

/// Writes a whole staging file.
///
/// # Errors
/// The file cannot be written.
pub fn write_staging(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: CcKind, size: u64, version: u64) -> Entry {
        Entry {
            kind,
            size,
            mtime_ns: 1,
            ctime_ns: 1,
            mode: 0o600,
            version,
        }
    }

    #[test]
    fn tree_children_rename_remove() {
        let mut t = Tree::default();
        t.insert("projects", e(CcKind::Dir, 0, 1));
        t.insert("projects/a", e(CcKind::Dir, 0, 1));
        t.insert("projects/a/x.jsonl", e(CcKind::Lines, 5, 1));
        t.insert("projects/ab", e(CcKind::Bytes, 3, 1));
        t.insert("todos", e(CcKind::Dir, 0, 1));
        assert_eq!(t.cc_bytes, 8);
        let names: Vec<String> = t.children("").into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["projects", "todos"]);
        let names: Vec<String> = t.children("projects").into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["a", "ab"]);
        assert!(t.has_children("projects/a"));
        assert!(!t.has_children("projects/ab"));
        t.rename("projects/a", "projects/b", 2, 9);
        assert!(t.get("projects/b/x.jsonl").is_some());
        assert!(t.get("projects/a/x.jsonl").is_none());
        t.rename("projects/b/x.jsonl", "projects/ab", 3, 9);
        assert_eq!(t.get("projects/ab").unwrap().kind, CcKind::Bytes);
        assert_eq!(t.get("projects/ab").unwrap().version, 3);
        assert_eq!(t.cc_bytes, 5);
        t.remove("projects");
        assert_eq!(t.cc_bytes, 0);
        assert_eq!(t.files.len(), 1);
    }

    #[test]
    fn content_cache_lru_pin_and_reconcile() {
        let dir = std::env::temp_dir().join(format!("cache-{}", uuid::Uuid::new_v4()));
        let mut c = Content::open(&dir, 10).unwrap();
        c.pin(Some(1));
        for (n, len) in [(1u64, 20usize), (2, 6), (3, 4)] {
            let st = c.staging(n);
            write_staging(&st, &vec![b'a'; len]).unwrap();
            c.filled(n, 1, len as u64, &st);
        }
        assert_eq!(c.total(), 10);
        let _ = c.valid(2, 1, 6); // 2 is now the most recently used
        let st = c.staging(4);
        write_staging(&st, b"zz").unwrap();
        c.filled(4, 1, 2, &st);
        assert!(c.has(1), "the pinned main transcript is never evicted");
        assert!(!c.has(3), "the least recently used goes");
        assert!(c.has(2) && c.has(4));
        c.wrote(2, (1, 6), 2, 6, b"bb", 8);
        assert_eq!(read_at(&c.valid(2, 2, 8).unwrap(), 4, 10).unwrap(), b"aabb");
        c.truncated(2, (2, 8), 3, 1);
        assert!(c.valid(2, 3, 1).is_some());
        // a stale write (the cached copy is not the current one) drops the entry
        c.wrote(4, (9, 2), 10, 0, b"x", 2);
        assert!(!c.has(4));
        // reconcile: equal kept, newer and longer handed back, others dropped
        let mut c = Content::open(&dir, 100).unwrap();
        let rep = c.reconcile(|n| match n {
            1 => Some((1, 20)),
            2 => Some((2, 0)),
            _ => None,
        });
        assert!(c.has(1));
        assert_eq!(
            rep,
            vec![Candidate {
                node: 2,
                version: 3,
                size: 1
            }]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
