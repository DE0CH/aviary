//! The managed Claude Code files (supervisor 6.4) in `/etc/claude-code/`, Claude Code's managed paths on Linux:
//! the descriptor's CLAUDE.md byte for byte, and the constant settings and MCP files. Root, 0644, replaced
//! atomically; written at the first start and whenever the descriptor's `etag` changes.

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

/// `managed-mcp.json`: the one MCP server, `tools`, at its fixed name (the gateway authenticates the sandbox).
pub const MANAGED_MCP: &str = r#"{"mcpServers": {"tools": {"type": "http", "url": "http://tools/mcp"}}}
"#;

/// Writes the three files into `dir`.
///
/// # Errors
/// The first write error.
pub fn write(dir: &Path, claude_md: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    put(dir, "CLAUDE.md", claude_md.as_bytes())?;
    put(dir, "managed-settings.json", MANAGED_SETTINGS.as_bytes())?;
    put(dir, "managed-mcp.json", MANAGED_MCP.as_bytes())
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
    use super::*;

    #[test]
    fn the_files() {
        let d = std::env::temp_dir().join(format!("managed-{}", uuid::Uuid::new_v4().simple()));
        write(&d, "規則\n").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("CLAUDE.md")).unwrap(), "規則\n");
        let s: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("managed-settings.json")).unwrap()).unwrap();
        assert_eq!(s["allowManagedHooksOnly"], true);
        assert_eq!(s["cleanupPeriodDays"], 36500);
        assert_eq!(
            s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/supervisor hook send-user-file"
        );
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("managed-mcp.json")).unwrap()).unwrap();
        assert_eq!(m["mcpServers"]["tools"]["url"], "http://tools/mcp");
        assert!(m["mcpServers"]["tools"].get("headers").is_none());
        let mode = std::fs::metadata(d.join("CLAUDE.md")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        write(&d, "新\n").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("CLAUDE.md")).unwrap(), "新\n");
        let _ = std::fs::remove_dir_all(d);
    }
}
