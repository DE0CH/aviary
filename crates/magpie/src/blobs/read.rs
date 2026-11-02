//! Reading blobs (magpie 5.4): the stored bytes (the read cache, then staging, then the bucket), decoded per the
//! blob's encoding, and the derived variants (thumbnail, display copy), made on first request and kept in the bucket.
//!
//! A staged blob (a cc write's, before the uploader has stored it) is served from `blob_staging`, previews included.

use aviary_pg::Pool;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use magpie_api::errors::ErrorCode;
use magpie_api::uploads::{BlobVariant, SandboxBlobAs};

use super::store::{self, StoreError};
use super::{BlobRow, Blobs, Sha, images, to_hex, unavailable};
use crate::error::{Error, Result};
use crate::limits;

/// Bytes to answer with, and their type.
#[derive(Debug, Clone)]
pub struct Served {
    /// The bytes.
    pub bytes: Bytes,
    /// `content-type`.
    pub content_type: String,
    /// The variant they are (for the etag).
    pub tag: &'static str,
}

fn unsupported(what: &str) -> Error {
    Error::code(ErrorCode::UnsupportedType, format!("{what} has no image variant"))
}

/// JSON-unescapes `bytes` (the content between a JSON string's quotes).
///
/// # Errors
/// [`Error::Internal`] when they are not a JSON string's content.
pub fn unescape(bytes: &[u8]) -> Result<String> {
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'"');
    quoted.extend_from_slice(bytes);
    quoted.push(b'"');
    serde_json::from_slice(&quoted).map_err(|e| Error::Internal(format!("blob is not a JSON string: {e}")))
}

/// The original bytes of a blob from its stored bytes: base64-json media decoded, json-string text unescaped, raw as
/// is.
///
/// # Errors
/// [`Error::Internal`] for stored bytes that do not decode.
pub fn original(row: &BlobRow, stored: Bytes) -> Result<Bytes> {
    match row.encoding.as_str() {
        "base64-json" => {
            let text = unescape(&stored)?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .map_err(|e| Error::Internal(format!("blob is not base64: {e}")))?;
            Ok(Bytes::from(raw))
        }
        "json-string" => Ok(Bytes::from(unescape(&stored)?.into_bytes())),
        _ => Ok(stored),
    }
}

/// The content type of a blob's original bytes.
#[must_use]
pub fn content_type(row: &BlobRow) -> String {
    if row.kind == "text" {
        return "text/plain; charset=utf-8".into();
    }
    row.mime.clone().unwrap_or_else(|| "application/octet-stream".into())
}

/// Whether `row` is an image magpie can make variants of.
#[must_use]
pub fn is_image(row: &BlobRow) -> bool {
    row.kind != "text" && row.mime.as_deref().is_some_and(images::is_image)
}

impl Blobs {
    /// The stored bytes of `row`: the read cache, staging, the bucket (cached afterwards).
    ///
    /// # Errors
    /// 404 when the bucket has no object (a database restored past a purge, magpie 18.6); 503 when it cannot be
    /// reached; a statement error.
    pub async fn stored_bytes(&self, pool: &Pool, row: &BlobRow) -> Result<Bytes> {
        if let Some(b) = self.cache.get(&row.sha).await {
            return Ok(b);
        }
        if !row.stored {
            let staged = pool
                .get()
                .await?
                .query_opt(
                    "select bytes from blob_staging where sha256 = $1",
                    &[&row.sha.as_slice()],
                )
                .await?;
            if let Some(r) = staged {
                let b = Bytes::from(r.get::<_, Vec<u8>>(0));
                self.cache.insert(row.sha, b.clone()).await;
                return Ok(b);
            }
            // the uploader stored it meanwhile: the bucket has it now
        }
        let b = match self.bucket.get(&store::blob_key(&to_hex(&row.sha))).await {
            Ok(b) => b,
            Err(StoreError::NotFound) => {
                tracing::warn!(blob = %to_hex(&row.sha), "blob object missing from the bucket");
                return Err(Error::not_found());
            }
            Err(e) => return Err(unavailable(&e)),
        };
        self.cache.insert(row.sha, b.clone()).await;
        Ok(b)
    }

    /// The original bytes of `row` (decoded).
    ///
    /// # Errors
    /// As [`Blobs::stored_bytes`].
    pub async fn original(&self, pool: &Pool, row: &BlobRow) -> Result<Bytes> {
        original(row, self.stored_bytes(pool, row).await?)
    }

    /// What `GET /v1/sandbox/blobs/{sha}?as=` serves.
    ///
    /// # Errors
    /// As [`Blobs::stored_bytes`]; 415 for `media` of a blob that is not base64 or `text` of one that is not text.
    pub async fn for_sandbox(&self, pool: &Pool, row: &BlobRow, as_: SandboxBlobAs) -> Result<Served> {
        let stored = self.stored_bytes(pool, row).await?;
        Ok(match as_ {
            SandboxBlobAs::Raw => Served {
                bytes: stored,
                content_type: "application/octet-stream".into(),
                tag: "raw",
            },
            SandboxBlobAs::Media => {
                if row.encoding == "json-string" {
                    return Err(Error::code(ErrorCode::UnsupportedType, "a text blob has no media form"));
                }
                Served {
                    bytes: original(row, stored)?,
                    content_type: content_type(row),
                    tag: "media",
                }
            }
            SandboxBlobAs::Text => {
                if row.encoding != "json-string" {
                    return Err(Error::code(ErrorCode::UnsupportedType, "not a text blob"));
                }
                Served {
                    bytes: original(row, stored)?,
                    content_type: content_type(row),
                    tag: "text",
                }
            }
        })
    }

    /// A derived object (`key`), made from `row` by `make` on first request and kept in the bucket. When the bucket
    /// cannot be reached the copy is made and served without being kept.
    async fn derived(
        &self,
        pool: &Pool,
        row: &BlobRow,
        key: String,
        make: impl FnOnce(Bytes) -> Result<Vec<u8>, images::ImageError> + Send + 'static,
    ) -> Result<Bytes> {
        let mime = row.mime.clone().unwrap_or_default();
        let reachable = match self.bucket.get(&key).await {
            Ok(b) => return Ok(b),
            Err(StoreError::NotFound) => true,
            Err(e) => {
                tracing::warn!(error = %e, "bucket unreachable for a derived copy; making it without keeping it");
                false
            }
        };
        let original = self.original(pool, row).await?;
        let memory = images::decode_memory(&original, &mime);
        let copy = match self.decode(memory, move || make(original)).await? {
            Ok(b) => Bytes::from(b),
            Err(e) => return Err(Error::code(ErrorCode::UnsupportedType, e.to_string())),
        };
        if reachable && let Err(e) = self.bucket.put(&key, copy.clone(), Some("image/webp")).await {
            tracing::warn!(error = %e, "derived copy not kept");
        }
        Ok(copy)
    }

    /// What M15 serves for `row` (whose references the caller checked). `model` is the upload's model copy when
    /// `row` is an upload's original (its `display` variant).
    ///
    /// # Errors
    /// 415 `unsupported_type` for `display`/`thumb` of a non-image or an image over the decode limit; as
    /// [`Blobs::stored_bytes`].
    pub async fn variant(
        &self,
        pool: &Pool,
        row: &BlobRow,
        variant: BlobVariant,
        model: Option<&BlobRow>,
    ) -> Result<Served> {
        match variant {
            BlobVariant::Full => Ok(Served {
                bytes: self.original(pool, row).await?,
                content_type: content_type(row),
                tag: "full",
            }),
            BlobVariant::Display => {
                if !is_image(row) {
                    return Err(unsupported("a blob that is not an image"));
                }
                if let Some(m) = model {
                    return Ok(Served {
                        bytes: self.original(pool, m).await?,
                        content_type: content_type(m),
                        tag: "display",
                    });
                }
                let mime = row.mime.clone().unwrap_or_default();
                let native = images::MODEL_NATIVE.contains(&mime.as_str());
                let fits = match (row.width, row.height) {
                    (Some(w), Some(h)) => u32::try_from(w.max(h)).unwrap_or(u32::MAX) <= limits::DISPLAY_EDGE,
                    _ => false,
                };
                if native && fits {
                    return Ok(Served {
                        bytes: self.original(pool, row).await?,
                        content_type: mime,
                        tag: "display",
                    });
                }
                let bytes = self
                    .derived(pool, row, store::display_key(&to_hex(&row.sha)), move |b| {
                        images::webp_fitted(&b, &mime, limits::DISPLAY_EDGE)
                    })
                    .await?;
                Ok(Served {
                    bytes,
                    content_type: "image/webp".into(),
                    tag: "display",
                })
            }
            BlobVariant::Thumb => {
                if !is_image(row) {
                    return Err(unsupported("a blob that is not an image"));
                }
                let mime = row.mime.clone().unwrap_or_default();
                let bytes = self
                    .derived(pool, row, store::thumb_key(&to_hex(&row.sha)), move |b| {
                        images::webp_fitted(&b, &mime, limits::THUMB_EDGE)
                    })
                    .await?;
                Ok(Served {
                    bytes,
                    content_type: "image/webp".into(),
                    tag: "thumb",
                })
            }
        }
    }
}

/// `filename*` of a `content-disposition` (RFC 5987).
#[must_use]
pub fn filename_star(name: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("UTF-8''");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Whether `content_type` is shown inline: images except SVG, and PDF.
#[must_use]
pub fn inline_type(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or("").trim();
    (base.starts_with("image/") && base != "image/svg+xml") || base == "application/pdf"
}

/// A blob read's etag: content-addressed, so known before the bytes are fetched.
#[must_use]
pub fn etag(sha: &Sha, tag: &str) -> String {
    if tag == "full" {
        format!("\"{}\"", to_hex(sha))
    } else {
        format!("\"{}-{tag}\"", to_hex(sha))
    }
}

fn base_headers(etag: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in [
        (header::CACHE_CONTROL, "private, max-age=31536000, immutable"),
        (header::ETAG, etag),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox"),
    ] {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    }
    h
}

/// 304 when the request's `If-None-Match` names this read's etag: answered before the blob is fetched or decoded.
#[must_use]
pub fn not_modified(sha: &Sha, tag: &str, request: &HeaderMap) -> Option<Response> {
    let etag = etag(sha, tag);
    request
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
        .then(|| (StatusCode::NOT_MODIFIED, base_headers(&etag)).into_response())
}

/// The answer for a blob read (magpie 5.4's headers): immutable, content-addressed, never sniffed or run.
#[must_use]
pub fn respond(sha: &Sha, served: Served, request: &HeaderMap) -> Response {
    if let Some(r) = not_modified(sha, served.tag, request) {
        return r;
    }
    let mut h = base_headers(&etag(sha, served.tag));
    let set = |h: &mut HeaderMap, k: header::HeaderName, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set(&mut h, header::CONTENT_TYPE, &served.content_type);
    let disposition = if inline_type(&served.content_type) {
        "inline".to_owned()
    } else {
        format!("attachment; filename*={}", filename_star(&to_hex(sha)))
    };
    set(&mut h, header::CONTENT_DISPOSITION, &disposition);
    (StatusCode::OK, h, Body::from(served.bytes)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(encoding: &str, kind: &str, mime: Option<&str>) -> BlobRow {
        BlobRow {
            sha: [0; 32],
            size: 0,
            kind: kind.into(),
            encoding: encoding.into(),
            mime: mime.map(Into::into),
            width: None,
            height: None,
            stored: true,
        }
    }

    #[test]
    fn decodes_by_encoding() {
        let media = row("base64-json", "media", Some("image/png"));
        assert_eq!(
            original(&media, Bytes::from_static(b"aGVsbG8=")).unwrap(),
            Bytes::from_static(b"hello")
        );
        let text = row("json-string", "text", None);
        assert_eq!(
            original(&text, Bytes::from_static(b"a\\nb \\\"q\\\" \xc3\xa9")).unwrap(),
            Bytes::from("a\nb \"q\" é".as_bytes())
        );
        assert_eq!(content_type(&text), "text/plain; charset=utf-8");
        let raw = row("raw", "file", Some("application/zip"));
        assert_eq!(
            original(&raw, Bytes::from_static(b"\0\x01")).unwrap(),
            Bytes::from_static(b"\0\x01")
        );
    }

    #[test]
    fn dispositions() {
        assert!(inline_type("image/png"));
        assert!(!inline_type("image/svg+xml"));
        assert!(inline_type("application/pdf"));
        assert!(!inline_type("text/html"));
        assert_eq!(filename_star("Q3 報告.pdf"), "UTF-8''Q3%20%E5%A0%B1%E5%91%8A.pdf");
    }
}
