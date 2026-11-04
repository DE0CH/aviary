//! The managed Claude Code files (supervisor 6.4) in `/etc/claude-code/`, Claude Code's managed paths on Linux:
//! the descriptor's CLAUDE.md byte for byte, the constant settings file, and the descriptor's managed skills in
//! `.claude/skills/<skill>/<file>` (decision 15: Claude Code has no MCP servers; the skills teach it the REST APIs it
//! calls with `curl`). Root, 0644 (folders 0755), each file replaced atomically, skill files and folders the
//! descriptor no longer names removed, and no `managed-mcp.json`; written at the first start and whenever the
//! descriptor's `etag` changes.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

/// The managed directory.
pub const MANAGED_DIR: &str = "/etc/claude-code";

/// `managed-settings.json`: managed hooks only, transcripts never swept, the `SendUserFile` hook.
pub const MANAGED_SETTINGS: &str = r#"{
  "allowManagedHooksOnly": true,
  "cleanupPeriodDays": 36500,
  "hooks": {
    "PreToolUse": [
      {"matcher": "SendUserFile",
       "hooks": [{"type": "command", "command": "/usr/local/bin/supervisor hook send-user-file", "timeout": 120}]}
    ]
  }
}
"#;

/// The managed skills' folder under the managed directory.
pub const SKILLS: &str = ".claude/skills";

/// Writes the managed files into `dir` from the descriptor's `claude`.
///
/// # Errors
/// The first write error.
pub fn write(dir: &Path, claude: &hermit_api::sandbox_surface::Claude) -> std::io::Result<()> {
    mkdir(dir)?;
    put(dir, "CLAUDE.md", claude.managed_claude_md.as_bytes())?;
    put(dir, "managed-settings.json", MANAGED_SETTINGS.as_bytes())?;
    // decision 15: no MCP servers (a file left by an older supervisor would bring one back)
    match std::fs::remove_file(dir.join("managed-mcp.json")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    write_skills(&dir.join(SKILLS), &claude.managed_skills)
}

fn mkdir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
}

/// `skills/<skill>/<file>` for every valid key (an invalid one is logged and skipped), then whatever else is there
/// removed.
fn write_skills(skills: &Path, files: &std::collections::BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(parent) = skills.parent() {
        mkdir(parent)?;
    }
    mkdir(skills)?;
    let mut keep: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for (key, text) in files {
        let Some((skill, file)) = key
            .split_once('/')
            .filter(|_| hermit_api::sandbox_surface::valid_skill_path(key))
        else {
            tracing::error!(key = %key, "a managed skill path that is not <skill>/<file>: skipped");
            continue;
        };
        let sdir = skills.join(skill);
        mkdir(&sdir)?;
        put(&sdir, file, text.as_bytes())?;
        keep.entry(skill).or_default().push(file);
    }
    for e in std::fs::read_dir(skills)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(kept) = keep.get(name.as_str()) else {
            remove(&e.path())?;
            continue;
        };
        for f in std::fs::read_dir(e.path())? {
            let f = f?;
            if !kept.contains(&f.file_name().to_string_lossy().as_ref()) {
                remove(&f.path())?;
            }
        }
    }
    Ok(())
}

fn remove(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp"));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, dir.join(name))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hermit_api::sandbox_surface::Claude;

    use super::*;

    fn claude(md: &str, skills: &[(&str, &str)]) -> Claude {
        Claude {
            managed_claude_md: md.to_owned(),
            managed_skills: skills
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_files() {
        let d = std::env::temp_dir().join(format!("managed-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        // an older supervisor's MCP file goes
        std::fs::write(d.join("managed-mcp.json"), "{}").unwrap();
        write(
            &d,
            &claude(
                "規則\n",
                &[
                    ("web/SKILL.md", "網頁\n"),
                    ("labels/SKILL.md", "l"),
                    ("labels/design-format.md", "f"),
                ],
            ),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(d.join("CLAUDE.md")).unwrap(), "規則\n");
        let s: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("managed-settings.json")).unwrap()).unwrap();
        assert_eq!(s["allowManagedHooksOnly"], true);
        assert_eq!(s["cleanupPeriodDays"], 36500);
        assert_eq!(
            s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/supervisor hook send-user-file"
        );
        assert!(!d.join("managed-mcp.json").exists());
        let skills = d.join(SKILLS);
        assert_eq!(std::fs::read_to_string(skills.join("web/SKILL.md")).unwrap(), "網頁\n");
        assert_eq!(
            std::fs::read_to_string(skills.join("labels/design-format.md")).unwrap(),
            "f"
        );
        assert_eq!(mode(&d.join("CLAUDE.md")), 0o644);
        assert_eq!(mode(&skills.join("web/SKILL.md")), 0o644);
        for dir in [d.join(".claude"), skills.clone(), skills.join("web")] {
            assert_eq!(mode(&dir), 0o755, "{}", dir.display());
        }

        // a new descriptor: a changed file, a dropped file, a dropped skill, a new one; an invalid key skipped
        write(
            &d,
            &claude(
                "新\n",
                &[("labels/SKILL.md", "l2"), ("echo/SKILL.md", "e"), ("../escape", "x")],
            ),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(d.join("CLAUDE.md")).unwrap(), "新\n");
        assert_eq!(std::fs::read_to_string(skills.join("labels/SKILL.md")).unwrap(), "l2");
        assert!(!skills.join("labels/design-format.md").exists());
        assert!(!skills.join("web").exists());
        assert!(skills.join("echo/SKILL.md").exists());
        assert!(!d.join(".claude/escape").exists() && !d.join("escape").exists());
        let mut names: Vec<String> = std::fs::read_dir(&skills)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["echo", "labels"]);

        // none at all: the folder is there and empty
        write(&d, &claude("x", &[])).unwrap();
        assert_eq!(std::fs::read_dir(&skills).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(d);
    }
}
