//! The scope table (supervisor 9.1): which paths under `~/.claude` go to magpie, which stay in the sandbox, which
//! are refused; and the path checks every call passes (9.3).

use std::collections::HashSet;
use std::sync::Mutex;

use super::errno::{EINVAL, ENAMETOOLONG};

/// Where a path is served from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `~/.claude` itself: the union of both.
    Root,
    /// The conversation's files in magpie.
    Magpie,
    /// The backing directory on scratch (passthrough).
    Local,
    /// Always absent; a create is EACCES (managed settings and instructions, A8.F28, A11.F12).
    Refused,
}

/// Top-level names (and their subtrees) in magpie scope.
pub const MAGPIE: &[&str] = &["projects", "todos", "plans", "tasks", "file-history"];
/// Top-level names in local scope that are known (anything else is local too, logged once as unclassified).
pub const LOCAL: &[&str] = &[
    "sessions",
    "shell-snapshots",
    "session-env",
    "backups",
    ".last-cleanup",
    "statsig",
    "telemetry",
    "debug",
    "ide",
    "cache",
    "mcp-discovery-cache",
    "uploads",
    "plugins",
];
/// Top-level names that are always absent.
pub const REFUSED: &[&str] = &[
    "settings.json",
    "settings.local.json",
    "CLAUDE.md",
    "CLAUDE.local.md",
    "commands",
    "agents",
    "skills",
    "output-styles",
    "hooks",
    "rules",
];

/// The longest path (bytes) and component.
pub const PATH_MAX: usize = 4096;
pub const NAME_MAX: usize = 255;

/// The scope of a path relative to `~/.claude` (`""` is the root).
#[must_use]
pub fn scope_of(path: &str) -> Scope {
    if path.is_empty() {
        return Scope::Root;
    }
    let top = path.split('/').next().unwrap_or(path);
    if MAGPIE.contains(&top) {
        Scope::Magpie
    } else if REFUSED.contains(&top) {
        Scope::Refused
    } else {
        Scope::Local
    }
}

/// Whether a top-level name is one the table names (else a local path is "unclassified").
#[must_use]
pub fn classified(path: &str) -> bool {
    let top = path.split('/').next().unwrap_or(path);
    MAGPIE.contains(&top) || LOCAL.contains(&top) || REFUSED.contains(&top)
}

/// Logs `unclassified path` once per top-level name, so a Claude Code upgrade that adds state is noticed.
#[derive(Debug, Default)]
pub struct Unclassified(Mutex<HashSet<String>>);

impl Unclassified {
    pub fn note(&self, path: &str) {
        if path.is_empty() || classified(path) {
            return;
        }
        let top = path.split('/').next().unwrap_or(path).to_owned();
        let mut seen = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.insert(top.clone()) {
            tracing::warn!(path = %top, "unclassified path");
        }
    }
}

/// A component as the kernel sends it: at most 255 bytes, UTF-8 (magpie stores text paths), no NUL, not `.`/`..`,
/// no `/`.
///
/// # Errors
/// ENAMETOOLONG or EINVAL.
pub fn check_name(name: &[u8]) -> Result<&str, i32> {
    if name.len() > NAME_MAX {
        return Err(ENAMETOOLONG);
    }
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0) {
        return Err(EINVAL);
    }
    std::str::from_utf8(name).map_err(|_| EINVAL)
}

/// `parent/name` with the 4,096-byte limit.
///
/// # Errors
/// ENAMETOOLONG or EINVAL.
pub fn join(parent: &str, name: &[u8]) -> Result<String, i32> {
    let name = check_name(name)?;
    let p = if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    };
    if p.len() > PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    Ok(p)
}

/// The parent of a path (`""` for a top-level name).
#[must_use]
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(a, _)| a)
}

/// The last component.
#[must_use]
pub fn base(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, b)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table() {
        for p in [
            "projects",
            "projects/-workspace/abc.jsonl",
            "projects/-workspace/abc/subagents/agent-1.jsonl",
            "projects/-workspace/memory/MEMORY.md",
            "todos/x.json",
            "plans/p.md",
            "tasks/t",
            "file-history/s/h@v1",
        ] {
            assert_eq!(scope_of(p), Scope::Magpie, "{p}");
        }
        for p in [
            "sessions/12.json",
            "shell-snapshots/s.sh",
            "session-env/x",
            "backups/.claude.json.backup",
            ".last-cleanup",
            "statsig/x",
            "plugins",
            "something-new/x",
            ".ccr-tip.json",
        ] {
            assert_eq!(scope_of(p), Scope::Local, "{p}");
        }
        for p in ["settings.json", "CLAUDE.md", "skills/x/SKILL.md", "hooks", "rules/a.md"] {
            assert_eq!(scope_of(p), Scope::Refused, "{p}");
        }
        assert_eq!(scope_of(""), Scope::Root);
        assert!(!classified("something-new/x"));
        assert!(classified("sessions"));
        // a name that only starts like a magpie one is not one
        assert_eq!(scope_of("projectsX"), Scope::Local);
    }

    #[test]
    fn path_checks() {
        assert_eq!(join("", b"projects"), Ok("projects".into()));
        assert_eq!(join("projects", b"a.jsonl"), Ok("projects/a.jsonl".into()));
        assert_eq!(join("", b".."), Err(EINVAL));
        assert_eq!(join("", b"."), Err(EINVAL));
        assert_eq!(join("", b"a/b"), Err(EINVAL));
        assert_eq!(join("", &[b'a'; 256]), Err(ENAMETOOLONG));
        let deep = "a/".repeat(2047) + "a";
        assert_eq!(join(&deep, b"bb"), Err(ENAMETOOLONG));
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
        assert_eq!(base("a/b/c"), "c");
    }
}
