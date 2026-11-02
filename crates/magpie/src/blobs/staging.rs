//! The blob staging writer (magpie 5.2): a blob a cc write introduces is stored in `blob_staging` in the write's own
//! transaction (`blobs.stored = false`), so the write is durable in Postgres alone and never waits for the object
//! store. The uploader (WP-15) moves staged bytes to the bucket afterwards; until then reads serve them from here.
//!
//! References (magpie 5.3): every blob a conversation's cc state uses gets a `blob_refs` row for the conversation;
//! inserting it bumps `blobs.last_ref_at` under the blob's row lock, which is what keeps the GC from freeing a blob
//! a transaction is about to reference.

use std::collections::HashMap;

use aviary_pg::Tx;
use magpie_api::errors::ErrorCode;
use uuid::Uuid;

use crate::ccfs::extract::Sha;
use crate::error::{Error, Result};
use crate::limits;

/// A blob to stage.
#[derive(Debug, Clone, Copy)]
pub struct NewBlob<'a> {
    /// SHA-256 of `bytes`.
    pub sha: Sha,
    /// The stored bytes.
    pub bytes: &'a [u8],
    /// `media`, `text` or `file`.
    pub kind: &'static str,
    /// `base64-json`, `json-string` or `raw`.
    pub encoding: &'static str,
    /// MIME type.
    pub mime: Option<&'a str>,
    /// Image width.
    pub width: Option<u32>,
    /// Image height.
    pub height: Option<u32>,
    /// Text: the preview, whether it is cut, and the line count.
    pub preview: Option<(&'a str, bool, u64)>,
}

fn i32_of(n: Option<u32>) -> Option<i32> {
    n.and_then(|n| i32::try_from(n).ok())
}

fn i64_of(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Bytes waiting in `blob_staging` (the sum of their blobs' sizes).
///
/// # Errors
/// When the statement fails.
pub async fn staged_bytes(tx: &Tx<'_>) -> Result<i64> {
    let row = tx
        .query_one(
            "select coalesce(sum(b.size), 0)::bigint from blob_staging s join blobs b on b.sha256 = s.sha256",
            &[],
        )
        .await?;
    Ok(row.get(0))
}

/// Stores `blob` (staging its bytes when it is new) and references it from `conversation`. Answers the bytes the
/// conversation's quota newly counts: the blob's size the first time this conversation references it, else 0.
///
/// # Errors
/// 503 `unavailable` when the blob is new and staging already holds [`limits::STAGING_MAX`] (the bucket has been
/// unreachable for long; the supervisor retries); a statement error.
pub async fn stage(tx: &Tx<'_>, conversation: Uuid, blob: &NewBlob<'_>) -> Result<i64> {
    let sha = blob.sha.as_slice();
    let size = i64_of(blob.bytes.len());
    // takes the blob's row lock (insert, or the update of an existing row), which the GC needs too
    let row = tx
        .query_one(
            "insert into blobs (sha256, size, kind, encoding, mime, width, height, stored, preview,
                                preview_truncated, lines)
             values ($1, $2, $3, $4, $5, $6, $7, false, $8, $9, $10)
             on conflict (sha256) do update set last_ref_at = now()
             returning (xmax = 0) as inserted, stored",
            &[
                &sha,
                &size,
                &blob.kind,
                &blob.encoding,
                &blob.mime,
                &i32_of(blob.width),
                &i32_of(blob.height),
                &blob.preview.map(|p| p.0),
                &blob.preview.map(|p| p.1),
                &blob.preview.map(|p| i64::try_from(p.2).unwrap_or(i64::MAX)),
            ],
        )
        .await?;
    let inserted: bool = row.get(0);
    let stored: bool = row.get(1);
    if inserted || !stored {
        let staged = tx
            .query_opt("select 1 from blob_staging where sha256 = $1", &[&sha])
            .await?
            .is_some();
        if !staged {
            if staged_bytes(tx).await? + size > limits::STAGING_MAX {
                return Err(Error::code(
                    ErrorCode::Unavailable,
                    "blob staging is full (the bucket has been unreachable)",
                ));
            }
            tx.execute(
                "insert into blob_staging (sha256, bytes) values ($1, $2)",
                &[&sha, &blob.bytes],
            )
            .await?;
        }
    }
    let first = tx
        .execute(
            "insert into blob_refs (owner, owner_kind, blob) values ($1, 'conversation', $2) on conflict do nothing",
            &[&conversation, &sha],
        )
        .await?;
    Ok(if first == 1 { size } else { 0 })
}

/// The staged bytes of `shas` that are staged.
///
/// # Errors
/// When the statement fails.
pub async fn read(db: &impl crate::db::Db, shas: &[Sha]) -> Result<HashMap<Sha, Vec<u8>>> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let keys: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
    let rows = db
        .query(
            "select sha256, bytes from blob_staging where sha256 = any($1)",
            &[&keys],
        )
        .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for r in rows {
        let sha: Vec<u8> = r.get(0);
        if let Ok(sha) = <Sha>::try_from(sha.as_slice()) {
            out.insert(sha, r.get(1));
        }
    }
    Ok(out)
}
