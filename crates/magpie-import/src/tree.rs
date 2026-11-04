//! A conversation's source files on `JuiceFS` and their cc paths (cutover 7.4.2): under
//! `/jfs/users/<key>/claude/`, the main transcript `projects/-workspace/<psid>.jsonl`, its `tool-results/**` and
//! `subagents/**`, and the person's auto-memory `projects/-workspace/memory/**`, which is copied into each of the
//! person's conversations (decision 6). The cc path is the path under `~/.claude`, the same as under the person's
//! `claude/` folder, so `claude --resume` reads the same names.
//!
//! A conversation the reverse export wrote back (cutover 14.3) has its own memory in `aviary-memory/<conversation>/`,
//! which the old stack does not read: once that folder is there, the conversation's memory (cc paths
//! `projects/-workspace/memory/**` still) is that folder's files, and over them every file of the person's shared
//! folder written after the export (the folder's newest write). A conversation reopened after a rollback so keeps
//! the memory it had on the new side, files made there included, and also gets what its old turns write, which can
//! only go to the shared folder; what other conversations' old turns write there after the export comes with it, as
//! nothing tells the writers apart, and a file deleted from the shared folder stays in the conversation. Verification
//! finds each file in its source. Every other `aviary-memory/` folder is ignored.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::Result;

/// The project folder every old conversation ran in (the cwd is always `/workspace`).
pub const PROJECT: &str = "projects/-workspace";

/// The parts of a session folder `projects/-workspace/<psid>/` that are the conversation's (imported, and deleted
/// on the old side by the export when magpie's scope no longer has them); anything else there is left alone.
pub const SESSION_PARTS: [&str; 2] = ["tool-results", "subagents"];

/// Claude Code's auto-memory folder of the project.
pub const MEMORY: &str = "projects/-workspace/memory";

/// What a source file looked like at a stat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    /// Bytes.
    pub size: u64,
    /// Modification time, ns since the epoch.
    pub mtime_ns: i64,
    /// Inode (a rename over the file changes it).
    pub ino: u64,
    /// Mode bits.
    pub mode: u32,
}

impl Stat {
    fn of(m: &std::fs::Metadata) -> Self {
        Self {
            size: m.len(),
            mtime_ns: m.mtime().saturating_mul(1_000_000_000).saturating_add(m.mtime_nsec()),
            ino: m.ino(),
            mode: m.mode() & 0o7777,
        }
    }
}

/// One source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Absolute path on the mount.
    pub abs: PathBuf,
    /// Its stat.
    pub stat: Stat,
}

/// A person's `claude/` folder.
#[must_use]
pub fn claude_dir(jfs: &Path, key: &str) -> PathBuf {
    jfs.join("users").join(key).join("claude")
}

/// The main transcript's cc path.
#[must_use]
pub fn main_path(psid: &str) -> String {
    format!("{PROJECT}/{psid}.jsonl")
}

/// The folder the reverse export writes conversation `id`'s memory to.
#[must_use]
pub fn exported_memory(jfs: &Path, key: &str, id: uuid::Uuid) -> PathBuf {
    claude_dir(jfs, key).join("aviary-memory").join(id.to_string())
}

/// Every source file of conversation `id` (when known) with Claude Code session `psid`, by cc path; nothing for a
/// folder that is not there. Its memory comes from [`exported_memory`] when that folder is there.
///
/// # Errors
/// An I/O error other than "not found".
pub fn sources(jfs: &Path, key: &str, psid: &str, id: Option<uuid::Uuid>) -> Result<BTreeMap<String, Source>> {
    let base = claude_dir(jfs, key);
    let mut out = BTreeMap::new();
    let main = main_path(psid);
    if let Some(m) = stat_opt(&base.join(&main))? {
        out.insert(
            main,
            Source {
                abs: base.join(main_path(psid)),
                stat: m,
            },
        );
    }
    for part in SESSION_PARTS {
        walk(&base, &format!("{PROJECT}/{psid}/{part}"), &mut out)?;
    }
    let own = id.filter(|id| std::fs::symlink_metadata(exported_memory(jfs, key, *id)).is_ok_and(|m| m.is_dir()));
    match own {
        Some(id) => {
            let name = id.to_string();
            let mut mine = BTreeMap::new();
            walk(&base.join("aviary-memory"), &name, &mut mine)?;
            // the export's time: the newest write in the folder (only the export writes it)
            let exported = newest(&exported_memory(jfs, key, id))?;
            for (rel, src) in mine {
                out.insert(format!("{MEMORY}{}", &rel[name.len()..]), src);
            }
            // what old turns wrote to the shared folder after it (the reopened conversation's own among them)
            let mut shared = BTreeMap::new();
            walk(&base, MEMORY, &mut shared)?;
            for (path, src) in shared {
                if src.stat.mtime_ns > exported {
                    out.insert(path, src);
                }
            }
        }
        None => walk(&base, MEMORY, &mut out)?,
    }
    Ok(out)
}

/// The newest modification time (ns) of `dir` and everything in it; 0 for a folder that is not there.
fn newest(dir: &Path) -> Result<i64> {
    let mut t = match std::fs::symlink_metadata(dir) {
        Ok(m) => Stat::of(&m).mtime_ns,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries {
            let e = e?;
            let m = e.metadata()?;
            t = t.max(if m.is_dir() {
                newest(&e.path())?
            } else {
                Stat::of(&m).mtime_ns
            });
        }
    }
    Ok(t)
}

/// The person's auto-memory files (for `plan`).
///
/// # Errors
/// An I/O error other than "not found".
pub fn memory(jfs: &Path, key: &str) -> Result<BTreeMap<String, Source>> {
    let mut out = BTreeMap::new();
    walk(&claude_dir(jfs, key), MEMORY, &mut out)?;
    Ok(out)
}

fn stat_opt(p: &Path) -> Result<Option<Stat>> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_file() => Ok(Some(Stat::of(&m))),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn walk(base: &Path, rel: &str, out: &mut BTreeMap<String, Source>) -> Result<()> {
    let dir = base.join(rel);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let mut names: Vec<(String, std::fs::Metadata)> = Vec::new();
    for e in entries {
        let e = e?;
        let name = utf8_name(&e, rel)?;
        if is_export_temp(&name) {
            continue;
        }
        names.push((name, e.metadata()?));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, m) in names {
        let child = format!("{rel}/{name}");
        if m.is_dir() {
            walk(base, &child, out)?;
        } else if m.is_file() {
            out.insert(
                child.clone(),
                Source {
                    abs: base.join(&child),
                    stat: Stat::of(&m),
                },
            );
        }
    }
    Ok(())
}

/// Whether `name` is one of the export's temporary files or share folders (`.<name>.aviary-export-<hex>`, renamed
/// into place when complete): never a source, whatever a crash left behind.
#[must_use]
pub fn is_export_temp(name: &str) -> bool {
    name.starts_with('.') && name.contains(".aviary-export-")
}

/// A folder entry's name, which must be UTF-8 (a cc path is text): another name is the conversation's error, never
/// a file skipped without a word.
///
/// # Errors
/// [`crate::Error::Source`] naming the file.
pub fn utf8_name(e: &std::fs::DirEntry, rel: &str) -> Result<String> {
    e.file_name().to_str().map(str::to_owned).ok_or_else(|| {
        crate::Error::Source(format!(
            "{rel}/{} has a name that is not UTF-8",
            e.file_name().to_string_lossy()
        ))
    })
}

/// Checks that the `JuiceFS` volume is mounted at `jfs` before anything reads it as the truth: `users/` is there and
/// holds at least one entry. Without the check an empty mount point (a failed or late mount) reads as "every
/// conversation's files deleted", and a pass would unlink them in magpie, or the export would write into it.
///
/// # Errors
/// [`crate::Error::Source`] when it does not look mounted.
pub fn check_mount(jfs: &Path) -> Result<()> {
    let users = jfs.join("users");
    let mut entries = std::fs::read_dir(&users).map_err(|e| {
        crate::Error::Source(format!(
            "the JuiceFS volume does not look mounted ({}: {e})",
            users.display()
        ))
    })?;
    if entries.next().is_none() {
        return Err(crate::Error::Source(format!(
            "the JuiceFS volume does not look mounted ({} is empty)",
            users.display()
        )));
    }
    Ok(())
}

/// Re-stats one source (`None` when it is gone).
///
/// # Errors
/// An I/O error other than "not found".
pub fn restat(abs: &Path) -> Result<Option<Stat>> {
    stat_opt(abs)
}

/// The first `len` bytes of a file, read in pieces, hashed: (sha256 hex, bytes read).
///
/// # Errors
/// An I/O error.
pub fn hash_prefix(abs: &Path, len: u64) -> Result<(String, u64)> {
    let mut reader = std::fs::File::open(abs)?.take(len);
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let got = reader.read(&mut buf)?;
        if got == 0 {
            break;
        }
        hasher.update(&buf[..got]);
        total += got as u64;
    }
    Ok((crate::hex(&hasher.finalize()), total))
}

/// The files of a conversation's workspace that an imported prompt can name: `uploads/<name>` (relative to
/// `/workspace`) and their sizes, from `/jfs/ws/<conversation>/uploads/`.
///
/// # Errors
/// An I/O error other than "not found".
pub fn workspace_uploads(jfs: &Path, conversation: uuid::Uuid) -> Result<BTreeMap<String, u64>> {
    let dir = jfs.join("ws").join(conversation.to_string()).join("uploads");
    let mut out = BTreeMap::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let e = e?;
        let m = e.metadata()?;
        if let (true, Some(name)) = (m.is_file(), e.file_name().to_str()) {
            out.insert(format!("uploads/{name}"), m.len());
        }
    }
    Ok(out)
}
