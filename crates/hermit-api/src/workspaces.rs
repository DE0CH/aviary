//! Workspaces (hermit 3.8): storage hermit provides; a conversation's workspace id is its conversation id, a share's
//! is `uuidv5(SHARE_NS, "share:" + shareId)` (magpie owns the convention).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time::Timestamp;

/// The namespace of share workspace ids.
pub const SHARE_NS: Uuid = Uuid::from_u128(0x6f1c_9d2e_0b7a_4f53_9a43_5b2f_3f1e_8c61);

/// The workspace id of a share's frozen copy.
#[must_use]
pub fn share_workspace(share_id: Uuid) -> Uuid {
    Uuid::new_v5(&SHARE_NS, format!("share:{share_id}").as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: Uuid,
    pub created_at: Timestamp,
    pub quota: Amount,
    pub usage: Amount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Amount {
    pub bytes: u64,
    pub inodes: u64,
}

/// `PUT /v1/workspaces/{wid}` body (empty object).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutWorkspaceRequest {}

/// 201 made (quota set) or 200 existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PutWorkspace {
    /// 201.
    Created(Workspace),
    /// 200.
    Existed(Workspace),
}

impl PutWorkspace {
    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        match self {
            Self::Created(w) | Self::Existed(w) => w,
        }
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::Created(_) => 201,
            Self::Existed(_) => 200,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EntryType,
    pub size: u64,
    pub mtime: Timestamp,
}

/// `GET /v1/workspaces/{wid}/entries?path=&cursor=`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesQuery {
    /// Relative; default `""` (the root).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// One page (sorted by name, [`ENTRIES_PAGE`] per page).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entries {
    pub path: String,
    pub entries: Vec<Entry>,
    pub next_cursor: Option<String>,
}

pub const ENTRIES_PAGE: usize = 1000;

/// `GET /v1/workspaces/{wid}/files?path=`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesQuery {
    pub path: String,
}

/// `POST /v1/workspaces/{wid}/clone` body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneRequest {
    pub from: Uuid,
}

/// One inclusive byte range (`Range: bytes=start-end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// Inclusive; `None` = to the end.
    pub end: Option<u64>,
}

impl ByteRange {
    /// Parses a single-range `Range` header (`bytes=0-99`, `bytes=100-`). Suffix ranges and lists are not part of
    /// the contract (`None`).
    #[must_use]
    pub fn parse(header: &str) -> Option<Self> {
        let spec = header.trim().strip_prefix("bytes=")?;
        if spec.contains(',') {
            return None;
        }
        let (a, b) = spec.split_once('-')?;
        let start = a.trim().parse().ok()?;
        let end = match b.trim() {
            "" => None,
            e => Some(e.parse().ok()?),
        };
        if end.is_some_and(|e| e < start) {
            return None;
        }
        Some(Self { start, end })
    }

    /// The header value.
    #[must_use]
    pub fn header(self) -> String {
        match self.end {
            Some(e) => format!("bytes={}-{e}", self.start),
            None => format!("bytes={}-", self.start),
        }
    }
}

/// A file read: 200 with the whole file, or 206 with `served` set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContent {
    /// The file's full size.
    pub size: u64,
    /// Modification time (`last-modified`, second resolution on the wire).
    pub mtime: Timestamp,
    /// `"<size>-<mtime ns>"` with the quotes.
    pub etag: String,
    /// `Some((first, last))` inclusive when this is a 206.
    pub served: Option<(u64, u64)>,
    pub body: axum::body::Bytes,
}

impl FileContent {
    /// The etag rule of 3.8: `"<size>-<mtime ns>"`.
    #[must_use]
    pub fn etag_for(size: u64, mtime_ns: i128) -> String {
        format!("\"{size}-{mtime_ns}\"")
    }
}

/// The path rules of 3.8: relative, UTF-8, no NUL, no `.` or `..` components, at most 4096 bytes and 255 bytes per
/// component. `""` is the root. Empty components (`a//b`, a trailing `/`) are refused too.
#[must_use]
pub fn valid_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if path.len() > 4096 || path.starts_with('/') || path.contains('\0') {
        return false;
    }
    path.split('/')
        .all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_ids_are_stable() {
        let share = Uuid::from_u128(1);
        assert_eq!(share_workspace(share), share_workspace(share));
        assert_eq!(share_workspace(share).get_version_num(), 5);
        assert_ne!(share_workspace(share), share_workspace(Uuid::from_u128(2)));
    }

    #[test]
    fn paths() {
        assert!(valid_path(""));
        assert!(valid_path("uploads/a b.pdf"));
        assert!(!valid_path("/etc/passwd"));
        assert!(!valid_path("a/../b"));
        assert!(!valid_path("./a"));
        assert!(!valid_path("a//b"));
        assert!(!valid_path("a\0b"));
        assert!(!valid_path(&"x".repeat(256)));
        assert!(!valid_path(&vec!["y"; 2049].join("/")));
    }

    #[test]
    fn ranges() {
        assert_eq!(
            ByteRange::parse("bytes=0-99"),
            Some(ByteRange {
                start: 0,
                end: Some(99)
            })
        );
        assert_eq!(
            ByteRange::parse("bytes=100-"),
            Some(ByteRange { start: 100, end: None })
        );
        assert_eq!(ByteRange::parse("bytes=-5"), None);
        assert_eq!(ByteRange::parse("bytes=0-1,4-5"), None);
        assert_eq!(ByteRange::parse("bytes=9-1"), None);
        assert_eq!(ByteRange { start: 3, end: None }.header(), "bytes=3-");
    }
}
