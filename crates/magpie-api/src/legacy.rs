//! The legacy-path set (DESIGN 9.5, 9.7), until A7: the old asset path of a teed upload and the old asset path
//! rules. The switch window's tickets and its window state went in A6 (cutover 10.5 step 7) with `migrating`; this
//! goes in A7 with the rest of the legacy-path set (cutover 10.7).

use serde::{Deserialize, Serialize};

/// `PUT /v1/uploads/{uploadId}/legacy-path` → 204: the old asset path pigeon's T0→gate upload tee got back from the
/// old child for these bytes (DESIGN 9.5), so a send that carries that path resolves to this upload. 400
/// `bad_path` for a path that is not an old asset path of the person; 404 for an upload that is not the person's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyPathRequest {
    /// The path as the old child returned it (`…/<person key>/.claude/cloudcli-assets/<file name>`).
    pub legacy_path: String,
}

/// The old asset folder of a person's home on the old stack, as it appears in a composer's file path.
pub const LEGACY_ASSET_DIR: &str = ".claude/cloudcli-assets";

/// The file name inside the old asset folder that `path` names, when `path` is an old asset path: it ends in
/// `<LEGACY_ASSET_DIR>/<name>` (name non-empty, no `/`, not `.` or `..`), and the segment before `.claude`, when
/// there is one, is `person_key` (an old home is `<root>/<person key>`). Anything else is `None`.
#[must_use]
pub fn legacy_asset_name<'a>(path: &'a str, person_key: &str) -> Option<&'a str> {
    let marker = format!("{LEGACY_ASSET_DIR}/");
    let at = path.rfind(&marker)?;
    let name = &path[at + marker.len()..];
    if name.is_empty() || name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return None;
    }
    let before = &path[..at];
    if !before.is_empty() {
        let home = before.strip_suffix('/')?;
        let key = home.rsplit('/').next().unwrap_or(home);
        if key != person_key {
            return None;
        }
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_asset_paths_name_their_file() {
        let k = "u0123456789abcdef01234567";
        assert_eq!(
            legacy_asset_name(
                &format!("/var/lib/old/home/{k}/.claude/cloudcli-assets/1730000-a.pdf"),
                k
            ),
            Some("1730000-a.pdf")
        );
        assert_eq!(legacy_asset_name(".claude/cloudcli-assets/x.png", k), Some("x.png"));
        assert_eq!(
            legacy_asset_name("/h/uffff/.claude/cloudcli-assets/x.png", k),
            None,
            "another person's home"
        );
        assert_eq!(legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/"), k), None);
        assert_eq!(
            legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/a/b"), k),
            None
        );
        assert_eq!(
            legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/.."), k),
            None
        );
        assert_eq!(legacy_asset_name("uploads/0190/x.pdf", k), None);
        assert_eq!(legacy_asset_name(&format!("/h/{k}x.claude/cloudcli-assets/a"), k), None);
    }
}
