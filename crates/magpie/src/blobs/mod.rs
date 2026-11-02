//! Blobs (magpie 5). WP-12 ships the staging writer ([`staging`]) and what the cc store needs to read blob bytes;
//! WP-15 adds the S3 store, the uploader, the read cache, previews and the GC here.
//!
//! A transaction never calls the bucket (platform 4.3). The cc store reads blob bytes inside its transaction only
//! from `blob_staging` or from bytes fetched before the transaction ([`Bytes`]); when it needs a blob that is only
//! in the bucket it fails with [`crate::error::Error::NeedBlobs`], and the route fetches those ([`fetch_stored`])
//! and runs the transaction again.

pub mod staging;

use std::collections::HashMap;

use magpie_api::errors::ErrorCode;

use crate::app::Magpie;
use crate::ccfs::extract::{Sha, hex};
use crate::db::Db;
use crate::error::{Error, Result};

/// A blob's metadata, as an event's blob reference shows it (magpie 6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// Stored bytes.
    pub size: u64,
    /// `media`, `text` or `file`.
    pub kind: String,
    /// MIME type.
    pub mime: Option<String>,
    /// Image width.
    pub width: Option<u32>,
    /// Image height.
    pub height: Option<u32>,
    /// Text: the preview.
    pub preview: Option<String>,
    /// Text: the line count.
    pub lines: Option<u64>,
}

/// The metadata of `shas` (missing ones left out).
///
/// # Errors
/// When the statement fails.
pub async fn meta(db: &impl Db, shas: &[Sha]) -> Result<HashMap<Sha, Meta>> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let keys: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
    let rows = db
        .query(
            "select sha256, size, kind, mime, width, height, preview, lines from blobs where sha256 = any($1)",
            &[&keys],
        )
        .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for r in rows {
        let sha: Vec<u8> = r.get(0);
        let Ok(sha) = <Sha>::try_from(sha.as_slice()) else {
            continue;
        };
        out.insert(
            sha,
            Meta {
                size: u64::try_from(r.get::<_, i64>(1)).unwrap_or(0),
                kind: r.get(2),
                mime: r.get(3),
                width: r.get::<_, Option<i32>>(4).and_then(|n| u32::try_from(n).ok()),
                height: r.get::<_, Option<i32>>(5).and_then(|n| u32::try_from(n).ok()),
                preview: r.get(6),
                lines: r.get::<_, Option<i64>>(7).and_then(|n| u64::try_from(n).ok()),
            },
        );
    }
    Ok(out)
}

/// Blob bytes a transaction can read: those fetched from the bucket before it (`pre`), and whatever it loads from
/// staging or stages itself.
#[derive(Debug)]
pub struct Bytes<'p> {
    pre: &'p HashMap<Sha, Vec<u8>>,
    own: HashMap<Sha, Vec<u8>>,
}

impl<'p> Bytes<'p> {
    /// With the bytes fetched before the transaction.
    #[must_use]
    pub fn with(pre: &'p HashMap<Sha, Vec<u8>>) -> Self {
        Self {
            pre,
            own: HashMap::new(),
        }
    }

    /// Makes the bytes of `shas` available: from what is known, else from staging. A blob that is stored only in the
    /// bucket fails the transaction with [`Error::NeedBlobs`] (the route fetches it and retries).
    ///
    /// # Errors
    /// [`Error::NeedBlobs`]; [`Error::Internal`] for a sha with no blob row; a statement error.
    pub async fn load(&mut self, db: &impl Db, shas: &[Sha]) -> Result<()> {
        let mut missing: Vec<Sha> = shas.iter().filter(|s| self.get(s).is_none()).copied().collect();
        missing.sort_unstable();
        missing.dedup();
        if missing.is_empty() {
            return Ok(());
        }
        let staged = staging::read(db, &missing).await?;
        let absent: Vec<Sha> = missing.iter().filter(|s| !staged.contains_key(*s)).copied().collect();
        self.own.extend(staged);
        if absent.is_empty() {
            return Ok(());
        }
        let known = meta(db, &absent).await?;
        if let Some(lost) = absent.iter().find(|s| !known.contains_key(*s)) {
            return Err(Error::Internal(format!("blob {} has no row", hex(lost))));
        }
        Err(Error::NeedBlobs(absent))
    }

    /// The bytes of `sha`, once loaded.
    #[must_use]
    pub fn get(&self, sha: &Sha) -> Option<&[u8]> {
        self.own.get(sha).or_else(|| self.pre.get(sha)).map(Vec::as_slice)
    }

    /// Adds bytes (a blob the transaction itself staged).
    pub fn put(&mut self, sha: Sha, bytes: Vec<u8>) {
        self.own.insert(sha, bytes);
    }
}

/// Fetches blobs stored in the bucket, outside any transaction. The bucket is WP-15's; until it exists every stored
/// blob answers 503 `unavailable` (a cc write never needs one: what it introduces is staged, and nothing uploads yet).
///
/// # Errors
/// 503 `unavailable`.
#[allow(clippy::unused_async)] // WP-15's S3 reads are awaited
pub async fn fetch_stored(_app: &Magpie, shas: &[Sha]) -> Result<HashMap<Sha, Vec<u8>>> {
    Err(Error::code(
        ErrorCode::Unavailable,
        format!("{} blob(s) are in the bucket, which is not readable yet", shas.len()),
    ))
}
