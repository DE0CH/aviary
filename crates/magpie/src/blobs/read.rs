//! Reading blobs (magpie 5.4): the stored bytes (the read cache, then staging, then the bucket), decoded per the
//! blob's encoding, and the derived variants (thumbnail, display copy), made on first request and kept in the bucket.
//!
//! A staged blob (a cc write's, before the uploader has stored it) is served from `blob_staging`, previews included.

use std::sync::Arc;

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

/// Bytes holding a share of the buffered-body budget until dropped.
struct HeldBytes {
    bytes: Bytes,
    _held: Arc<super::BodyMemory>,
}

impl AsRef<[u8]> for HeldBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

/// `bytes` as a handle that gives `held` back when it (and every slice of it, and every other handle sharing `held`)
/// is dropped.
pub(crate) fn held_bytes(bytes: Bytes, held: Arc<super::BodyMemory>) -> Bytes {
    Bytes::from_owner(HeldBytes { bytes, _held: held })
}

fn u64_of(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Blobs {
    /// The stored bytes of `row`, and the memory held for them: the read cache or the recent uploads, else staging or
    /// the bucket (cached afterwards). Concurrent fetches of one blob are one fetch: the others wait for it and are
    /// answered from the cache.
    ///
    /// With a `deadline` (a read sent at its client's pace), a fetch takes the bytes it reads ([`Blobs::hold_read`];
    /// twice a staged blob's size while its row and its copy are both alive) plus `extra`, and an answer from the
    /// cache takes `extra` only (the cache counts what it keeps); waiting for that, or for the fetch in flight, past
    /// `deadline` answers 503. Without one (the cc store's batches, which took the batch's share already), nothing is
    /// taken.
    async fn fetch_with(
        &self,
        pool: &Pool,
        row: &BlobRow,
        deadline: Option<tokio::time::Instant>,
        extra: u64,
    ) -> Result<(Bytes, Option<super::BodyMemory>)> {
        let take = async |bytes: u64| match deadline {
            Some(d) if bytes > 0 => self.hold_read(bytes, d).await.map(Some),
            _ => Ok(None),
        };
        if let Some(b) = self.cached(&row.sha).await {
            return Ok((b, take(extra).await?));
        }
        let one = self.fetch_lock(&row.sha);
        let _one = match deadline {
            Some(d) => tokio::time::timeout_at(d, one.lock_owned())
                .await
                .map_err(|_| super::busy())?,
            None => one.lock_owned().await,
        };
        if let Some(b) = self.cached(&row.sha).await {
            return Ok((b, take(extra).await?));
        }
        let size = u64::try_from(row.size).unwrap_or(0);
        if !row.stored {
            let mut held = take(2 * size + extra).await?;
            let staged = pool
                .get()
                .await?
                .query_opt(
                    "select bytes from blob_staging where sha256 = $1",
                    &[&row.sha.as_slice()],
                )
                .await?
                .map(|r| Bytes::from(r.get::<_, Vec<u8>>(0)));
            if let Some(b) = staged {
                // the row is dropped: only the copy is alive
                if let Some(h) = held.as_mut() {
                    h.keep(u64_of(b.len()) + extra);
                }
                self.cache.insert(row.sha, b.clone()).await;
                return Ok((b, held));
            }
            // the uploader stored it meanwhile: the bucket has it now
            if let Some(h) = held.as_mut() {
                h.keep(size + extra);
            }
            return self.bucket_bytes(row).await.map(|b| (b, held));
        }
        let held = take(size + extra).await?;
        self.bucket_bytes(row).await.map(|b| (b, held))
    }

    async fn bucket_bytes(&self, row: &BlobRow) -> Result<Bytes> {
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

    /// The stored bytes of `row` for a batch that holds its share of the budget itself ([`super::fetch_stored`]).
    pub(crate) async fn fetch(&self, pool: &Pool, row: &BlobRow) -> Result<Bytes> {
        Ok(self.fetch_with(pool, row, None, 0).await?.0)
    }

    /// The stored bytes of `row`, to be sent: the read cache or the recent uploads, staging, the bucket (cached
    /// afterwards).
    ///
    /// A read the cache cannot answer takes its fetch's share of the reads' part of the buffered-body budget
    /// ([`Blobs::hold_read`]) before it fetches, and the bytes it answers keep that share until the last handle
    /// outside the cache is dropped (the response sent or cut off, [`send_body`]): the reads in flight are counted,
    /// so many concurrent reads of large blobs wait for each other instead of taking a replica past its 1 GiB
    /// (magpie 18.2), and they never hold more than [`limits::READ_MEMORY_BYTES`], so slow clients cannot keep
    /// uploads waiting. A read that waits [`limits::READ_WAIT`] answers 503. What the cache keeps is counted in the
    /// cache's own budget.
    ///
    /// # Errors
    /// 404 when the bucket has no object (a database restored past a purge, magpie 18.6); 503 when it cannot be
    /// reached or the read waited too long; a statement error.
    pub async fn stored_bytes(&self, pool: &Pool, row: &BlobRow) -> Result<Bytes> {
        let deadline = tokio::time::Instant::now() + limits::READ_WAIT;
        let (b, held) = self.fetch_with(pool, row, Some(deadline), 0).await?;
        Ok(match held {
            Some(h) => held_bytes(b, Arc::new(h)),
            None => b,
        })
    }

    /// The original bytes of `row` (decoded), to be sent. A base64-json or json-string blob's decoded copy is a new
    /// buffer: it takes its share of the reads' part as [`Blobs::stored_bytes`] does (twice the stored size while it
    /// decodes, its own size afterwards), in the same wait as the fetch.
    ///
    /// # Errors
    /// As [`Blobs::stored_bytes`].
    pub async fn original(&self, pool: &Pool, row: &BlobRow) -> Result<Bytes> {
        if !matches!(row.encoding.as_str(), "base64-json" | "json-string") {
            return self.stored_bytes(pool, row).await;
        }
        let deadline = tokio::time::Instant::now() + limits::READ_WAIT;
        let decoding = 2 * u64::try_from(row.size).unwrap_or(0);
        let (stored, held) = self.fetch_with(pool, row, Some(deadline), decoding).await?;
        let out = original(row, stored)?;
        Ok(match held {
            Some(mut h) => {
                h.keep(u64_of(out.len()));
                held_bytes(out, Arc::new(h))
            }
            None => out,
        })
    }

    /// What `GET /v1/sandbox/blobs/{sha}?as=` serves.
    ///
    /// # Errors
    /// As [`Blobs::stored_bytes`]; 415 for `media` of a blob that is not base64 or `text` of one that is not text.
    pub async fn for_sandbox(&self, pool: &Pool, row: &BlobRow, as_: SandboxBlobAs) -> Result<Served> {
        Ok(match as_ {
            SandboxBlobAs::Raw => Served {
                bytes: self.stored_bytes(pool, row).await?,
                content_type: "application/octet-stream".into(),
                tag: "raw",
            },
            SandboxBlobAs::Media => {
                if row.encoding == "json-string" {
                    return Err(Error::code(ErrorCode::UnsupportedType, "a text blob has no media form"));
                }
                Served {
                    bytes: self.original(pool, row).await?,
                    content_type: content_type(row),
                    tag: "media",
                }
            }
            SandboxBlobAs::Text => {
                if row.encoding != "json-string" {
                    return Err(Error::code(ErrorCode::UnsupportedType, "not a text blob"));
                }
                Served {
                    bytes: self.original(pool, row).await?,
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
    (StatusCode::OK, h, send_body(served.bytes)).into_response()
}

/// The body of a blob response: `bytes` sent in chunks of [`limits::SEND_CHUNK`], each a copy, by a task that gives
/// up when the client takes no chunk for [`limits::SEND_IDLE`]. The task then drops `bytes`, giving back the memory
/// they hold ([`Blobs::stored_bytes`]), and the connection is cut, so a client that stalls or vanished without closing
/// keeps a few chunks at most, not the blob, for as long as TCP keeps it. `content-length` is set (the body is a
/// stream).
pub fn send_body(bytes: Bytes) -> Body {
    send_body_idle(bytes, limits::SEND_IDLE)
}

/// [`send_body`] with an idle bound of `idle`.
pub fn send_body_idle(bytes: Bytes, idle: std::time::Duration) -> Body {
    use tokio::sync::mpsc::error::SendTimeoutError;
    let total = bytes.len();
    if total <= limits::SEND_CHUNK {
        return Body::from(bytes);
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(1);
    tokio::spawn(async move {
        let mut off = 0;
        while off < bytes.len() {
            let end = (off + limits::SEND_CHUNK).min(bytes.len());
            match tx.send_timeout(Bytes::copy_from_slice(&bytes[off..end]), idle).await {
                Ok(()) => off = end,
                Err(SendTimeoutError::Timeout(_)) => {
                    tracing::warn!(sent = off, size = bytes.len(), "blob response stalled; cut off");
                    return;
                }
                Err(SendTimeoutError::Closed(_)) => return,
            }
        }
    });
    let stream = futures_util::stream::unfold((rx, 0usize), move |(mut rx, sent)| async move {
        if sent >= total {
            return None;
        }
        match rx.recv().await {
            Some(c) => {
                let n = sent + c.len();
                Some((Ok::<_, std::io::Error>(c), (rx, n)))
            }
            // the sender gave up: an error, so the connection is cut rather than the body taken as whole
            None => Some((Err(std::io::Error::other("blob response cut off")), (rx, total))),
        }
    });
    Body::new(SizedBody {
        inner: Body::from_stream(stream),
        size: u64_of(total),
    })
}

/// A body with a known size (so `content-length` is sent for a streamed body).
struct SizedBody {
    inner: Body,
    size: u64,
}

impl http_body::Body for SizedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Bytes>, Self::Error>>> {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.size)
    }
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

    #[tokio::test]
    async fn a_stalled_send_is_cut_off_and_gives_its_memory_back() {
        use http_body_util::BodyExt as _;
        let cfg = super::super::store::BucketConfig {
            endpoint: "http://127.0.0.1:1".into(),
            region: "r".into(),
            bucket: "b".into(),
            access_key_id: "k".into(),
            secret_access_key: "s".into(),
        };
        let blobs = Blobs::new(&cfg, "postgres://u@127.0.0.1:1/d").unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        let held = blobs.hold_read(8 << 20, deadline).await.unwrap();
        assert_eq!(blobs.memory_free(), (256 - 8, 128 - 8));
        let bytes = held_bytes(Bytes::from(vec![1u8; 8 << 20]), Arc::new(held));
        // a client that takes one chunk and then nothing
        let mut body = send_body_idle(bytes, std::time::Duration::from_millis(200));
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first.len(), limits::SEND_CHUNK);
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert_eq!(blobs.memory_free(), (256, 128), "the stalled send gave its memory back");
        // what was queued before the cut is delivered, then the body fails (the connection is cut, not completed)
        let mut failed = false;
        while let Some(f) = body.frame().await {
            if f.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        // a client that takes everything gets every byte, with its size known
        let whole = send_body(Bytes::from(vec![2u8; (1 << 20) + 5]));
        assert_eq!(http_body::Body::size_hint(&whole).exact(), Some((1 << 20) + 5));
        assert_eq!(whole.collect().await.unwrap().to_bytes().len(), (1 << 20) + 5);
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
