//! The harness homes on the sandbox surface. From `magpie-sandbox` 2 a supervisor names every file with its
//! harness's home first: `claude/…` is Claude Code's `~/.claude`, `qwen/…` is Qwen Code's `~/.qwen`. A supervisor
//! on number 1 knows only Claude Code and sends paths relative to `~/.claude`.
//!
//! The stored form is what number 1 sent, so nothing stored before harnesses existed changes and a sandbox started
//! before this release keeps working against it: Claude Code's files are stored without a prefix, every other
//! harness's with its home (`qwen/…`). Claude Code never stores a top-level name that is a harness's (its stored
//! names are `projects`, `todos`, `plans`, `tasks` and `file-history`), so the two cannot collide. [`stored`] and
//! [`shown`] are the only places that know this; everything else in magpie works on stored paths.

use aviary_core::version::Protocol;
use magpie_api::Harness;
use magpie_api::server::Ctx;

use crate::error::{Error, Result};

/// Whether the caller names files by harness home (`magpie-sandbox` 2 or later). A request without the header is
/// read as the current number.
#[must_use]
pub fn by_home(ctx: &Ctx) -> bool {
    ctx.protocol
        .as_deref()
        .and_then(Protocol::parse)
        .is_none_or(|(surface, n)| surface != magpie_api::SURFACE_SANDBOX || n >= 2)
}

/// The stored path of a path the caller sent.
///
/// # Errors
/// 400 `bad_request` when a caller that names files by home sends a path under no harness's home, or a home itself.
pub fn stored(ctx: &Ctx, path: &str) -> Result<String> {
    if !by_home(ctx) {
        return Ok(path.to_owned());
    }
    let (top, rest) = path.split_once('/').unwrap_or((path, ""));
    match Harness::parse(top) {
        Some(_) if rest.is_empty() => Err(Error::bad_request("a harness's home is no file")),
        Some(Harness::Claude) => Ok(rest.to_owned()),
        Some(_) => Ok(path.to_owned()),
        None => Err(Error::bad_request("the path is under no harness's home")),
    }
}

/// A stored path as the caller names it.
#[must_use]
pub fn shown(ctx: &Ctx, path: &str) -> String {
    if !by_home(ctx) {
        return path.to_owned();
    }
    let top = path.split_once('/').map_or(path, |(t, _)| t);
    match Harness::parse(top) {
        Some(h) if h != Harness::Claude => path.to_owned(),
        _ => format!("{}/{path}", Harness::Claude),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx(protocol: Option<&str>) -> Ctx {
        Ctx {
            protocol: protocol.map(str::to_owned),
            ..Ctx::default()
        }
    }

    #[test]
    fn a_supervisor_on_number_2_names_files_by_home() {
        for c in [ctx(Some("magpie-sandbox=2")), ctx(Some("magpie-sandbox=3")), ctx(None)] {
            assert_eq!(
                stored(&c, "claude/projects/-workspace/s.jsonl").unwrap(),
                "projects/-workspace/s.jsonl"
            );
            assert_eq!(
                stored(&c, "qwen/projects/-workspace/chats/s.jsonl").unwrap(),
                "qwen/projects/-workspace/chats/s.jsonl"
            );
            assert!(stored(&c, "projects/-workspace/s.jsonl").is_err());
            assert!(stored(&c, "claude").is_err());
            assert!(stored(&c, "qwen").is_err());
            assert_eq!(
                shown(&c, "projects/-workspace/s.jsonl"),
                "claude/projects/-workspace/s.jsonl"
            );
            assert_eq!(
                shown(&c, "qwen/projects/-workspace/chats/s.jsonl"),
                "qwen/projects/-workspace/chats/s.jsonl"
            );
            // what is shown is stored as it was
            for p in ["projects/a", "todos/x.json", "qwen/memories/m.md"] {
                assert_eq!(stored(&c, &shown(&c, p)).unwrap(), p);
            }
        }
    }

    #[test]
    fn a_supervisor_on_number_1_sends_claude_codes_paths_as_they_are_stored() {
        let c = ctx(Some("magpie-sandbox=1"));
        assert_eq!(
            stored(&c, "projects/-workspace/s.jsonl").unwrap(),
            "projects/-workspace/s.jsonl"
        );
        assert_eq!(shown(&c, "projects/-workspace/s.jsonl"), "projects/-workspace/s.jsonl");
    }
}
