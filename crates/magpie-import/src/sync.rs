//! The sync of one conversation's cc files (cutover 7.4.2), each operation its own magpie transaction through the
//! importer's cc writes ([`magpie::ccfs::Caller::importer`]: the lease check replaced by the `migrating` check):
//!
//! 1. stat unchanged (size, mtime, inode) and magpie's copy the recorded size → nothing;
//! 2. a new file → created and written from offset 0;
//! 3. the source grew and its bytes up to magpie's size are the ones synced last (same sha256) → an append of the
//!    rest at magpie's size;
//! 4. otherwise (compaction, tombstone removal) → for a lines file, the source's lines are compared with magpie's
//!    stored lines (sha256 and offset) and the source is written from the first one that differs, then truncated to
//!    its size; a bytes file is written whole and truncated;
//! 5. a source gone → unlinked;
//! 6. a gone file and a new one with the same sha256 in one pass → a rename (Claude Code's relocation keeps its
//!    events).
//!
//! Writes are cut at 16 MiB (magpie's write cap). Each operation's `opId` is a uuidv5 of the conversation, the path,
//! the offset, the chunk's sha256 and the file's version before it: a pass cut by a crash repeats harmlessly (an
//! operation that committed moved the version, and magpie's rule 0 takes an identical range as done), and a later
//! write of the same bytes at the same offset (a file rewritten back) is a new operation rather than a replay.
//! (Cutover 7.4.2's `opId` has no version; without it such a write would replay the first one's stored answer and
//! write nothing.)

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::Path;

use magpie::ccfs::{self, Caller, Kind, ops};
use magpie_api::sandbox::{CcMkdir, CcPathOp, CcRename, CcTruncate, CcWriteQuery};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use uuid::Uuid;

use crate::tree::{Source, Stat};
use crate::{Error, Importer, NS, Result, from_magpie};

/// magpie's write cap.
pub const CHUNK: usize = magpie::limits::CC_WRITE_MAX;

/// What the importer recorded of a source file at its last sync (`import_sources.files`, cutover 7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    /// The path under `/jfs`.
    pub src: String,
    /// Bytes.
    pub size: u64,
    /// Modification time, ns.
    pub mtime_ns: i64,
    /// Inode.
    pub ino: u64,
    /// SHA-256 of the whole source file as synced (hex).
    pub sha256: String,
}

impl Recorded {
    fn same_stat(&self, s: &Stat) -> bool {
        self.size == s.size && self.mtime_ns == s.mtime_ns && self.ino == s.ino
    }
}

/// `import_sources.files`.
pub type Files = BTreeMap<String, Recorded>;

/// One conversation's cc writer.
#[derive(Debug)]
pub struct Writer<'a> {
    imp: &'a Importer,
    caller: Caller,
    dirs: BTreeSet<String>,
}

fn op_id(parts: &[&str]) -> Uuid {
    Uuid::new_v5(&NS, parts.join(":").as_bytes())
}

impl<'a> Writer<'a> {
    /// The importer writing `conversation` of `person`, with the workspace files its prompts can name.
    #[must_use]
    pub fn new(imp: &'a Importer, conversation: Uuid, person: &str, workspace: BTreeMap<String, u64>) -> Self {
        Self {
            imp,
            caller: Caller::importer(
                conversation,
                person,
                ccfs::Importer {
                    workspace_files: workspace,
                },
            ),
            dirs: BTreeSet::new(),
        }
    }

    fn conv(&self) -> Uuid {
        self.caller.conversation
    }

    /// magpie's file at `path`.
    ///
    /// # Errors
    /// A statement error.
    pub async fn file(&self, path: &str) -> Result<Option<ccfs::File>> {
        let conn = self.imp.magpie.pool.get().await?;
        Ok(ccfs::load_file(&conn, self.conv(), path).await?)
    }

    async fn parents(&mut self, path: &str) -> Result<()> {
        let mut acc = String::new();
        let comps: Vec<&str> = path.split('/').collect();
        for c in &comps[..comps.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(c);
            if self.dirs.contains(&acc) {
                continue;
            }
            match self.file(&acc).await? {
                Some(f) if f.kind == Kind::Dir => {}
                Some(_) => {
                    return Err(Error::Source(format!(
                        "{acc} is a file in magpie, a folder in the source"
                    )));
                }
                None => {
                    let conv = self.conv().to_string();
                    let b = CcMkdir {
                        path: acc.clone(),
                        mode: 0o755,
                        mtime_ns: 0,
                        op_id: op_id(&[&conv, "mkdir", &acc]),
                    };
                    match ops::mkdir(&self.imp.magpie, &self.caller, &b).await {
                        Ok(()) => {}
                        Err(e) if e.is(&magpie_api::errors::ErrorCode::Exists) => {}
                        Err(e) => return Err(from_magpie(e)),
                    }
                }
            }
            self.dirs.insert(acc.clone());
        }
        Ok(())
    }

    /// Writes `data` at `offset`, creating the file when `create`; the file's version after.
    async fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
        stat: &Stat,
        create: bool,
        version: u64,
    ) -> Result<u64> {
        let conv = self.conv().to_string();
        let q = CcWriteQuery {
            path: path.to_owned(),
            offset,
            mtime_ns: stat.mtime_ns,
            op_id: op_id(&[
                &conv,
                path,
                &offset.to_string(),
                &crate::sha_hex(data),
                &version.to_string(),
            ]),
            create,
            mode: stat.mode,
            turn_id: None,
            append: false,
        };
        let r = ops::write(&self.imp.magpie, &self.caller, &q, data)
            .await
            .map_err(from_magpie)?;
        Ok(r.version)
    }

    async fn truncate(&self, path: &str, size: u64, stat: &Stat, version: u64) -> Result<u64> {
        let conv = self.conv().to_string();
        let b = CcTruncate {
            path: path.to_owned(),
            size,
            mtime_ns: stat.mtime_ns,
            op_id: op_id(&[&conv, "truncate", path, &size.to_string(), &version.to_string()]),
        };
        let r = ops::truncate(&self.imp.magpie, &self.caller, &b)
            .await
            .map_err(from_magpie)?;
        Ok(r.version)
    }

    /// Unlinks `path` in magpie (nothing when it is not there).
    ///
    /// # Errors
    /// magpie's.
    pub async fn unlink(&self, path: &str) -> Result<()> {
        let Some(f) = self.file(path).await? else {
            return Ok(());
        };
        let conv = self.conv().to_string();
        let b = CcPathOp {
            path: path.to_owned(),
            op_id: op_id(&[&conv, "unlink", path, &f.version.to_string()]),
        };
        match ops::unlink(&self.imp.magpie, &self.caller, &b).await {
            Ok(()) => Ok(()),
            Err(e) if e.is(&magpie_api::errors::ErrorCode::NotFound) => Ok(()),
            Err(e) => Err(from_magpie(e)),
        }
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.parents(to).await?;
        let version = self.file(from).await?.map_or(0, |f| f.version);
        let conv = self.conv().to_string();
        let b = CcRename {
            from: from.to_owned(),
            to: to.to_owned(),
            op_id: op_id(&[&conv, "rename", from, to, &version.to_string()]),
        };
        ops::rename(&self.imp.magpie, &self.caller, &b)
            .await
            .map_err(from_magpie)?;
        Ok(())
    }

    /// Syncs every source file of the conversation against what was recorded (rules 1-6); the new record and whether
    /// anything was written. For a new main transcript (`main`), `breaks` is called at each of its offsets (failed
    /// turns placed by time on the first import, cutover 7.4.3).
    ///
    /// # Errors
    /// A file the importer cannot sync; magpie's refusals ([`Error::Released`] when the conversation stopped
    /// migrating).
    pub async fn sync_all(
        &mut self,
        sources: &BTreeMap<String, Source>,
        recorded: &Files,
        main: Option<&str>,
        mut breaks: Option<&mut dyn Breaks>,
    ) -> Result<(Files, bool)> {
        let mut out = Files::new();
        let mut changed = false;
        let mut gone: Vec<(&String, &Recorded)> = recorded.iter().filter(|(p, _)| !sources.contains_key(*p)).collect();
        // rule 6: a gone file and a new one with the same bytes in this pass is a rename
        let mut renamed_to: BTreeMap<String, Recorded> = BTreeMap::new();
        for (path, src) in sources {
            if recorded.contains_key(path) {
                continue;
            }
            let Some(pos) = gone.iter().position(|(_, r)| r.size == src.stat.size) else {
                continue;
            };
            let (sha, n) = crate::tree::hash_prefix(&src.abs, src.stat.size)?;
            if n != src.stat.size || sha != gone[pos].1.sha256 {
                continue;
            }
            let (from, r) = gone.remove(pos);
            if self.file(from).await?.is_some() && self.file(path).await?.is_none() {
                self.rename(from, path).await?;
                changed = true;
                renamed_to.insert(
                    path.clone(),
                    Recorded {
                        src: src.abs.to_string_lossy().into_owned(),
                        ..r.clone()
                    },
                );
            }
        }
        for (path, src) in sources {
            let rec = recorded.get(path).or_else(|| renamed_to.get(path));
            let breaks = if main == Some(path.as_str()) {
                breaks.take()
            } else {
                None
            };
            let (r, wrote) = self.sync_one(path, src, rec, breaks).await?;
            changed |= wrote;
            out.insert(path.clone(), r);
        }
        for (path, _) in gone {
            self.unlink(path).await?;
            changed = true;
        }
        Ok((out, changed))
    }

    /// Syncs one file (rules 1-4).
    async fn sync_one(
        &mut self,
        path: &str,
        src: &Source,
        rec: Option<&Recorded>,
        breaks: Option<&mut dyn Breaks>,
    ) -> Result<(Recorded, bool)> {
        let m = self.file(path).await?;
        if let (Some(r), Some(f)) = (rec, &m)
            && r.same_stat(&src.stat)
            && u64::try_from(f.size).ok() == Some(r.size)
        {
            return Ok((r.clone(), false));
        }
        self.parents(path).await?;
        let mut rd = Hashed::open(&src.abs, src.stat.size)?;
        match m {
            None => {
                let mut version = 0;
                let mut first = true;
                if let Some(b) = breaks {
                    for (off, at) in b.offsets(&src.abs, src.stat.size)? {
                        version = self.copy(path, &mut rd, off, &src.stat, &mut first, version).await?;
                        b.at(self.imp, at).await?;
                    }
                }
                self.copy(path, &mut rd, src.stat.size, &src.stat, &mut first, version)
                    .await?;
            }
            Some(f) => {
                let msize = u64::try_from(f.size).unwrap_or(0);
                let appendable = rec.is_some_and(|r| r.size == msize) && src.stat.size >= msize && {
                    rd.skip_hash(msize)?;
                    rec.is_some_and(|r| rd.peek_hex() == r.sha256)
                };
                if appendable {
                    let mut first = false;
                    self.copy(
                        path,
                        &mut rd,
                        src.stat.size,
                        &src.stat,
                        &mut first,
                        u64::try_from(f.version).unwrap_or(0),
                    )
                    .await?;
                } else {
                    let mut rd2 = Hashed::open(&src.abs, src.stat.size)?;
                    if f.kind == Kind::Lines {
                        self.first_difference(&f, &mut rd2).await?;
                    }
                    let mut first = false;
                    let mut version = u64::try_from(f.version).unwrap_or(0);
                    version = self
                        .copy(path, &mut rd2, src.stat.size, &src.stat, &mut first, version)
                        .await?;
                    let now = self.file(path).await?.map_or(0, |f| u64::try_from(f.size).unwrap_or(0));
                    if now > src.stat.size {
                        self.truncate(path, src.stat.size, &src.stat, version).await?;
                    }
                    rd = rd2;
                }
            }
        }
        let (sha, n) = rd.finish()?;
        if n != src.stat.size {
            return Err(Error::Source(format!("{path} changed while it was read")));
        }
        Ok((
            Recorded {
                src: src.abs.to_string_lossy().into_owned(),
                size: src.stat.size,
                mtime_ns: src.stat.mtime_ns,
                ino: src.stat.ino,
                sha256: sha,
            },
            true,
        ))
    }

    /// Writes the whole source again from offset 0 and truncates to its size (verification's re-ingest, cutover 7.7
    /// step 2: magpie 6.6 with `C = 0`); the new record.
    ///
    /// # Errors
    /// As [`Writer::sync_all`].
    pub async fn replace(&mut self, path: &str, src: &Source) -> Result<Recorded> {
        self.parents(path).await?;
        let m = self.file(path).await?;
        let mut rd = Hashed::open(&src.abs, src.stat.size)?;
        let mut first = m.is_none();
        let mut version = m.as_ref().map_or(0, |f| u64::try_from(f.version).unwrap_or(0));
        version = self
            .copy(path, &mut rd, src.stat.size, &src.stat, &mut first, version)
            .await?;
        let now = self.file(path).await?.map_or(0, |f| u64::try_from(f.size).unwrap_or(0));
        if now > src.stat.size {
            self.truncate(path, src.stat.size, &src.stat, version).await?;
        }
        let (sha, n) = rd.finish()?;
        if n != src.stat.size {
            return Err(Error::Source(format!("{path} changed while it was read")));
        }
        Ok(Recorded {
            src: src.abs.to_string_lossy().into_owned(),
            size: src.stat.size,
            mtime_ns: src.stat.mtime_ns,
            ino: src.stat.ino,
            sha256: sha,
        })
    }

    /// For a rewrite of a lines file: reads the source's lines while they equal magpie's stored lines at the same
    /// ordinal and offset; leaves the reader at the first one that differs (or at the end of the last complete line)
    /// and answers that offset.
    async fn first_difference(&self, f: &ccfs::File, rd: &mut Hashed) -> Result<u64> {
        let conn = self.imp.magpie.pool.get().await?;
        let rows = ccfs::lines::rows_from(&conn, f.id, 0, None).await?;
        drop(conn);
        let mut i = 0;
        loop {
            let Some(line) = rd.peek_line()? else { break };
            if !line.ends_with(b"\n") {
                break;
            }
            let Some(r) = rows.get(i) else { break };
            let sha: [u8; 32] = sha2::Sha256::digest(line).into();
            if r.sha != sha || u64::try_from(r.start_off).ok() != Some(rd.pos) {
                break;
            }
            rd.consume_peeked();
            i += 1;
        }
        Ok(rd.pos)
    }

    /// Copies the source from the reader's position up to `to` into magpie at the same offsets, in chunks of at most
    /// 16 MiB; the file's version after.
    async fn copy(
        &self,
        path: &str,
        rd: &mut Hashed,
        to: u64,
        stat: &Stat,
        first: &mut bool,
        mut version: u64,
    ) -> Result<u64> {
        loop {
            let off = rd.pos;
            if off >= to && !*first {
                return Ok(version);
            }
            let chunk = rd.read_upto(to, CHUNK)?;
            if chunk.is_empty() && !*first {
                return Ok(version);
            }
            version = self.write(path, off, &chunk, stat, *first, version).await?;
            *first = false;
            if chunk.is_empty() {
                return Ok(version);
            }
        }
    }
}

/// Where failed turns go in a new main transcript (cutover 7.4.3, magpie 19.1): each break is an offset at which the
/// importer stops writing and adds the events of the failed turns that came before the line there.
pub trait Breaks: Send {
    /// The offsets (ascending) and an opaque index for [`Breaks::at`].
    ///
    /// # Errors
    /// An I/O error reading the transcript.
    fn offsets(&mut self, abs: &Path, size: u64) -> Result<Vec<(u64, usize)>>;

    /// Writes what belongs at break `at`.
    fn at<'s>(
        &'s mut self,
        imp: &'s Importer,
        at: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 's>>;
}

/// A source file read once, front to back, hashing every byte read.
#[derive(Debug)]
struct Hashed {
    r: BufReader<std::io::Take<std::fs::File>>,
    h: sha2::Sha256,
    pos: u64,
    peeked: Option<Vec<u8>>,
}

impl Hashed {
    fn open(abs: &Path, size: u64) -> Result<Self> {
        Ok(Self {
            r: BufReader::with_capacity(1 << 20, std::fs::File::open(abs)?.take(size)),
            h: sha2::Sha256::new(),
            pos: 0,
            peeked: None,
        })
    }

    /// Hashes the next `n` bytes without keeping them.
    fn skip_hash(&mut self, n: u64) -> Result<()> {
        let mut left = n;
        let mut buf = vec![0u8; 1 << 20];
        while left > 0 {
            let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let k = self.r.read(&mut buf[..want])?;
            if k == 0 {
                break;
            }
            self.h.update(&buf[..k]);
            self.pos += k as u64;
            left -= k as u64;
        }
        Ok(())
    }

    /// The hash of what was read so far.
    fn peek_hex(&self) -> String {
        crate::hex(&self.h.clone().finalize())
    }

    /// The next line (with its `\n`, or the tail without one) without consuming it from the hash's view.
    fn peek_line(&mut self) -> Result<Option<&[u8]>> {
        if self.peeked.is_none() {
            let mut line = Vec::new();
            let n = self.r.read_until(b'\n', &mut line)?;
            if n == 0 {
                return Ok(None);
            }
            self.peeked = Some(line);
        }
        Ok(self.peeked.as_deref())
    }

    fn consume_peeked(&mut self) {
        if let Some(l) = self.peeked.take() {
            self.h.update(&l);
            self.pos += l.len() as u64;
        }
    }

    /// Up to `max` bytes, stopping at offset `to`.
    fn read_upto(&mut self, to: u64, max: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if let Some(l) = self.peeked.take() {
            out = l;
        }
        let room = usize::try_from(to.saturating_sub(self.pos))
            .unwrap_or(usize::MAX)
            .min(max);
        if out.len() > room {
            // a peeked line longer than the room: keep the rest for the next read
            let rest = out.split_off(room);
            self.peeked = Some(rest);
        } else {
            let mut more = vec![0u8; room - out.len()];
            let mut filled = 0;
            while filled < more.len() {
                let k = self.r.read(&mut more[filled..])?;
                if k == 0 {
                    break;
                }
                filled += k;
            }
            more.truncate(filled);
            out.extend_from_slice(&more);
        }
        self.h.update(&out);
        self.pos += out.len() as u64;
        Ok(out)
    }

    /// The whole file's hash and length (reads what is left).
    fn finish(mut self) -> Result<(String, u64)> {
        if let Some(l) = self.peeked.take() {
            self.h.update(&l);
            self.pos += l.len() as u64;
        }
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let k = self.r.read(&mut buf)?;
            if k == 0 {
                break;
            }
            self.h.update(&buf[..k]);
            self.pos += k as u64;
        }
        Ok((crate::hex(&self.h.finalize()), self.pos))
    }
}
