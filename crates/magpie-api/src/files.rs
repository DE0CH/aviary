//! The files panel and downloads (M14, magpie 11.1, 11.3, 12.3).

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::uploads::BlobId;

/// Paths: at most this many bytes (magpie 17).
pub const PATH_MAX_BYTES: usize = 4_096;

/// `GET /v1/conversations/{id}/files`: newest send first, one entry per path (its latest send).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesPanel {
    /// The files.
    pub files: Vec<PanelFile>,
}

/// A sent file in the panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelFile {
    /// Relative to `/workspace`.
    pub path: String,
    /// File name.
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// MIME type.
    pub mime_type: String,
    /// Shown as an image.
    pub is_image: bool,
    /// When it was sent.
    pub sent_at: Timestamp,
    /// The send's caption.
    pub caption: Option<String>,
    /// The `SendUserFile` call (pass as `sent` to download this send's snapshot).
    pub tool_use_id: String,
    /// The snapshot's blob.
    pub blob_id: BlobId,
}

/// M14 and the share download query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFileQuery {
    /// Relative, normalised, no `..` or NUL, at most 4,096 bytes.
    pub path: String,
    /// A `SendUserFile` call: that send's snapshot; absent: the live (or, for a share, frozen) workspace file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<String>,
    /// Inline for images (not SVG) and PDF.
    #[serde(default, with = "crate::flag01", skip_serializing_if = "crate::flag01::is_false")]
    pub inline: bool,
}
