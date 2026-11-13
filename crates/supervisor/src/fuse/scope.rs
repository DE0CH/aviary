//! The scope table (supervisor 9.1): the mount holds one home per harness (`claude/` is Claude Code's `~/.claude`,
//! `qwen/` is Qwen Code's `~/.qwen`); under a home, which paths go to magpie, which stay in the sandbox, which are
//! refused; and the path checks every call passes (9.3).

use std::collections::HashSet;
use std::sync::Mutex;

use aviary_core::harness::Harness;

use super::errno::{EINVAL, ENAMETOOLONG};

/// Where a path is served from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The mount itself: it holds the harness homes and nothing else.
    Top,
    /// A harness's home (`claude`, `qwen`): the union of its magpie and local entries.
    Root,
    /// The conversation's files in magpie.
    Magpie,
    /// The backing directory on scratch (passthrough).
    Local,
    /// Always absent; a create is EACCES (managed settings and instructions, A8.F28, A11.F12; any name at the top
    /// that is not a harness).
    Refused,
}

/// One harness's table: its top-level names (and their subtrees) by scope.
#[derive(Debug)]
pub struct Table {
    /// In magpie scope.
    pub magpie: &'static [&'static str],
    /// In local scope and known (anything else is local too, logged once as unclassified).
    pub local: &'static [&'static str],
    /// Always absent.
    pub refused: &'static [&'static str],
}

/// Claude Code's `~/.claude`.
pub const CLAUDE: Table = Table {
    magpie: &["projects", "todos", "plans", "tasks", "file-history"],
    local: &[
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
    ],
    refused: &[
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
    ],
};

/// Qwen Code's `~/.qwen` (measured on 0.27.3 with `QWEN_HOME`): `projects/<cwd>/chats/<sid>.jsonl` are the
/// transcripts (with `<sid>.title.json`), `projects/<cwd>/memory`, `meta.json`, `extract-cursor.json` and
/// `memories/` its memory. It makes an empty `skills/` at every start, so that name is local, not refused: the
/// managed skills are in `/etc/qwen-code/skills`, and what a conversation writes there goes with the sandbox.
pub const QWEN: Table = Table {
    magpie: &["projects", "memories"],
    local: &[
        "skills",
        "usage",
        "usage_record.jsonl",
        "tmp",
        "extensions",
        "extension-store",
        "model-registry.json",
        "output-language.md",
        "installation_id",
        "logs",
        "debug",
        "ide",
    ],
    refused: &[
        "settings.json",
        "QWEN.md",
        "AGENTS.md",
        "commands",
        "agents",
        "hooks",
        "output-styles",
    ],
};

/// The table of a harness.
#[must_use]
pub const fn table(h: Harness) -> &'static Table {
    match h {
        Harness::Claude => &CLAUDE,
        Harness::Qwen => &QWEN,
    }
}

/// A path split into its harness and the rest under that harness's home (`""` for the home itself). `None`: the
/// mount's root, or a top-level name that is no harness.
#[must_use]
pub fn split(path: &str) -> Option<(Harness, &str)> {
    let (top, rest) = path.split_once('/').unwrap_or((path, ""));
    Harness::parse(top).map(|h| (h, rest))
}

/// The home of a harness, as a path of the mount.
#[must_use]
pub const fn home(h: Harness) -> &'static str {
    h.as_str()
}

/// The longest path (bytes) and component.
pub const PATH_MAX: usize = 4096;
pub const NAME_MAX: usize = 255;

/// The scope of a path of the mount (`""` is the mount itself).
#[must_use]
pub fn scope_of(path: &str) -> Scope {
    if path.is_empty() {
        return Scope::Top;
    }
    let Some((h, rest)) = split(path) else {
        return Scope::Refused;
    };
    if rest.is_empty() {
        return Scope::Root;
    }
    let t = table(h);
    let top = rest.split('/').next().unwrap_or(rest);
    if t.magpie.contains(&top) {
        Scope::Magpie
    } else if t.refused.contains(&top) {
        Scope::Refused
    } else {
        Scope::Local
    }
}

/// Whether a path's name under its home is one the harness's table names (else a local path is "unclassified").
/// The mount, a home and a name that is no harness count as classified.
#[must_use]
pub fn classified(path: &str) -> bool {
    let Some((h, rest)) = split(path) else {
        return true;
    };
    if rest.is_empty() {
        return true;
    }
    let t = table(h);
    let top = rest.split('/').next().unwrap_or(rest);
    t.magpie.contains(&top) || t.local.contains(&top) || t.refused.contains(&top)
}

/// Logs `unclassified path` once per top-level name, so a Claude Code upgrade that adds state is noticed.
#[derive(Debug, Default)]
pub struct Unclassified(Mutex<HashSet<String>>);

impl Unclassified {
    pub fn note(&self, path: &str) {
        if classified(path) {
            return;
        }
        // the harness and the name under its home: `claude/something-new`
        let top = path.splitn(3, '/').take(2).collect::<Vec<_>>().join("/");
        let mut seen = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.insert(top.clone()) {
            tracing::warn!(path = %top, "unclassified path");
        }
    }
}

/// Logs `unanswered opcode` once per opcode the mount refuses as unsupported (ENOSYS, ENOTSUP, EOPNOTSUPP) or by
/// policy (LINK, MKNOD: EPERM), so `claude-code-gate` sees a Claude Code release that issues one the pinned release
/// did not (supervisor 22).
#[derive(Debug, Default)]
pub struct Unanswered(Mutex<HashSet<u32>>);

impl Unanswered {
    pub fn note(&self, opcode: u32, name: &str, errno: i32) {
        let mut seen = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.insert(opcode) {
            tracing::warn!(opcode = name, errno, "unanswered opcode");
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
            "claude/projects",
            "claude/projects/-workspace/abc.jsonl",
            "claude/projects/-workspace/abc/subagents/agent-1.jsonl",
            "claude/projects/-workspace/memory/MEMORY.md",
            "claude/todos/x.json",
            "claude/plans/p.md",
            "claude/tasks/t",
            "claude/file-history/s/h@v1",
            "qwen/projects",
            "qwen/projects/-workspace/chats/abc.jsonl",
            "qwen/projects/-workspace/chats/abc.ledger.jsonl",
            "qwen/projects/-workspace/memory/MEMORY.md",
            "qwen/memories/MEMORY.md",
        ] {
            assert_eq!(scope_of(p), Scope::Magpie, "{p}");
        }
        for p in [
            "claude/sessions/12.json",
            "claude/shell-snapshots/s.sh",
            "claude/session-env/x",
            "claude/backups/.claude.json.backup",
            "claude/.last-cleanup",
            "claude/statsig/x",
            "claude/plugins",
            "claude/something-new/x",
            "claude/.ccr-tip.json",
            "qwen/usage/token-usage-2026-11.jsonl",
            "qwen/usage_record.jsonl",
            "qwen/skills/x/SKILL.md",
            "qwen/.openai-logs-cleanup-b98b",
            "qwen/something-new",
            // one harness's names mean nothing in the other's home
            "qwen/file-history/x",
            "claude/memories/x",
        ] {
            assert_eq!(scope_of(p), Scope::Local, "{p}");
        }
        for p in [
            "claude/settings.json",
            "claude/CLAUDE.md",
            "claude/skills/x/SKILL.md",
            "claude/hooks",
            "claude/rules/a.md",
            "qwen/settings.json",
            "qwen/QWEN.md",
            "qwen/hooks/x",
            // the mount holds the harness homes and nothing else
            "projects",
            "projects/-workspace/abc.jsonl",
            "codex",
            "claudeX/projects",
        ] {
            assert_eq!(scope_of(p), Scope::Refused, "{p}");
        }
        assert_eq!(scope_of(""), Scope::Top);
        assert_eq!(scope_of("claude"), Scope::Root);
        assert_eq!(scope_of("qwen"), Scope::Root);
        assert!(!classified("claude/something-new/x"));
        assert!(classified("claude/sessions"));
        assert!(classified("claude"));
        assert!(classified(""));
        // a name that only starts like a magpie one is not one
        assert_eq!(scope_of("claude/projectsX"), Scope::Local);
        assert_eq!(split("qwen/projects/a"), Some((Harness::Qwen, "projects/a")));
        assert_eq!(split("claude"), Some((Harness::Claude, "")));
        assert_eq!(split(""), None);
        assert_eq!(split("projects/a"), None);
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
