//! Uploads (M13), blobs (M15 and the sandbox's blob routes) and the blob reference objects inside events.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A blob's public id: the SHA-256 of its stored bytes, lowercase hex (magpie 5.1).
pub type BlobId = String;

/// Upload size cap, bytes (50 MiB, the app's `FILE_UPLOAD_MAX_BYTES`; magpie 17).
pub const UPLOAD_MAX_BYTES: u64 = 52_428_800;
/// Images per turn (magpie 17).
pub const MAX_IMAGES_PER_TURN: usize = 10;
/// Files per turn (magpie 17).
pub const MAX_FILES_PER_TURN: usize = 10;

/// `Aviary-Upload-Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UploadKind {
    /// Any file; an image sent as a file stays a file.
    File,
    /// An image (JPEG, PNG, GIF, WebP, HEIC/HEIF, AVIF, TIFF, BMP by sniffed type); 415 otherwise.
    Image,
}

impl UploadKind {
    /// The header value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UploadKind::File => "file",
            UploadKind::Image => "image",
        }
    }

    /// From the header value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(UploadKind::File),
            "image" => Some(UploadKind::Image),
            _ => None,
        }
    }
}

/// M13's metadata headers, as the server reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadMeta {
    /// `Aviary-Upload-Name`, percent-decoded.
    pub name: String,
    /// `Aviary-Upload-Kind`.
    pub kind: UploadKind,
    /// `Content-Type` as sent.
    pub content_type: String,
}

/// M13 `POST /v1/uploads` → 201.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    /// The upload.
    pub upload_id: Uuid,
    /// The opaque reference the app sends back in the next turn's `files[].path`: `uploads/<uploadId>/<name>`.
    pub path: String,
    /// The file name as sent.
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// Sniffed type, else from the extension.
    pub mime_type: String,
    /// Image width in pixels (images only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Image height in pixels (images only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// M15 `variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BlobVariant {
    /// WebP, long edge 512 px, made on first request and kept.
    Thumb,
    /// Long edge <= 2000 px (an upload's model copy).
    Display,
    /// The original bytes (media decoded from base64, text unescaped).
    #[default]
    Full,
}

/// M15 query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobQuery {
    /// Which variant; `full` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<BlobVariant>,
}

/// `GET /v1/sandbox/blobs/{sha}` `as`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SandboxBlobAs {
    /// The stored bytes.
    #[default]
    Raw,
    /// Base64-decoded.
    Media,
    /// JSON-unescaped text.
    Text,
}

/// `GET /v1/sandbox/blobs/{sha}` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxBlobQuery {
    /// How to decode; `raw` when absent.
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub as_: Option<SandboxBlobAs>,
}

/// What kind of bytes a blob holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlobKind {
    /// Image or PDF.
    Media,
    /// Large text extracted from a transcript line.
    Text,
    /// Any other file.
    File,
}

/// A blob reference inside event data: `{"$blob": {…}}` (magpie 6.5), in place of an extracted string or as an
/// attachment's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPointer {
    /// The reference.
    #[serde(rename = "$blob")]
    pub blob: BlobRef,
}

/// The body of a [`BlobPointer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobRef {
    /// SHA-256 hex.
    pub blob_id: BlobId,
    /// Stored bytes.
    pub bytes: u64,
    /// Kind.
    pub kind: BlobKind,
    /// MIME type (media and files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Image width, read from the header at ingest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Image height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Text blobs: the first 4,000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Text blobs: the line count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
}

/// An attached file in `user.sent` (magpie 7.2 `Attachment`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// The upload it came from.
    pub upload_id: Uuid,
    /// The file name as sent.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
    /// The workspace path assigned at admission (`uploads/<name>`, de-duplicated), relative to `/workspace`.
    pub path: String,
    /// The bytes.
    pub blob: BlobPointer,
}

/// An attached image in `user.sent` (magpie 7.2 `Image`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedImage {
    /// The upload it came from.
    pub upload_id: Uuid,
    /// MIME type of the original.
    pub mime_type: String,
    /// Bytes of the original.
    pub size: u64,
    /// Width of the original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Height of the original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The workspace path of the original, relative to `/workspace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The original.
    pub blob: BlobPointer,
    /// The model copy (long edge <= 2000 px, <= 3,932,160 bytes; also the `display` variant).
    pub model_blob: BlobPointer,
}

/// A file of a sent-file snapshot in `files.sent` (magpie 7.2 `SentFile`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFile {
    /// Relative to `/workspace`.
    pub path: String,
    /// The last path component.
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// MIME type from the extension table.
    pub mime_type: String,
    /// Whether the app shows it as an image.
    pub is_image: bool,
    /// The snapshot's bytes.
    pub blob: BlobPointer,
}
