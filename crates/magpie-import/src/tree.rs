//! A conversation's source files on `JuiceFS` and their cc paths (cutover 7.4.2): under
//! `/jfs/users/<key>/claude/`, the main transcript `projects/-workspace/<psid>.jsonl`, its `tool-results/**` and
//! `subagents/**`, and the person's auto-memory `projects/-workspace/memory/**`, which is copied into each of the
//! person's conversations (decision 6). The cc path is the path under `~/.claude`, the same as under the person's
//! `claude/` folder, so `claude --resume` reads the same names.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::Result;

/// The project folder every old conversation ran in (the cwd is always `/workspace`).
pub const PROJECT: &str = "projects/-workspace";

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

/// Every source file of the conversation with Claude Code session `psid`, by cc path; nothing for a folder that is
/// not there.
///
/// # Errors
/// An I/O error other than "not found".
pub fn sources(jfs: &Path, key: &str, psid: &str) -> Result<BTreeMap<String, Source>> {
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
    for dir in [
        format!("{PROJECT}/{psid}/tool-results"),
        format!("{PROJECT}/{psid}/subagents"),
        MEMORY.to_owned(),
    ] {
        walk(&base, &dir, &mut out)?;
    }
    Ok(out)
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
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
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
