//! Blobs and the bucket (magpie 5): the S3 store ([`store`]), the staging writer of cc writes ([`staging`]), the
//! read cache, reads of every variant ([`read`]), the uploader of staged blobs ([`uploader`]), image facts and
//! derived copies ([`images`]), and the GC ([`gc`]).
//!
//! Blobs are content-addressed: a blob's id is the SHA-256 of its stored bytes. Its row (`blobs`) says what the bytes
//! are; the bytes are in the bucket (`stored`), or in `blob_staging` while a cc write's new blob waits for the
//! uploader (magpie 5.2).
//!
//! A transaction never calls the bucket (platform 4.3). The cc store reads blob bytes inside its transaction only
//! from `blob_staging` or from bytes fetched before the transaction ([`Bytes`]); when it needs a blob that is only
//! in the bucket it fails with [`crate::error::Error::NeedBlobs`], and the route fetches those ([`fetch_stored`])
//! and runs the transaction again.
//!
//! **The bucket and the GC.** A transaction never calls the bucket (platform 4.3), so "the bytes are in the bucket"
//! and "the row says so" cannot change together. One lock keeps them consistent: every write of a blob's object (an
//! upload, a sent file, the uploader) holds the blob's shared advisory lock ([`Blobs::shared`]) from before its `PUT`
//! until its row commits; the GC holds the exclusive one from before it deletes a row until it has deleted the
//! objects. A GC therefore never deletes an object a writer has just put, and a writer never commits a row whose
//! object a GC is deleting. The locks sit on lock sessions of their own ([`locks`]), never on a connection of the
//! request pool, so a bucket that hangs pins no request connection.

pub mod gc;
pub mod images;
pub mod locks;
pub mod read;
pub mod staging;
pub mod store;
pub mod uploader;

use std::collections::HashMap;
use std::sync::Arc;

use aviary_pg::Pool;
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;

use crate::app::Magpie;
use crate::ccfs::extract::hex;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;
use store::{Bucket, BucketConfig};

pub use locks::SharedLock;

/// A blob's SHA-256.
pub type Sha = [u8; 32];

/// The SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> Sha {
    Sha256::digest(bytes).into()
}

/// Lowercase hex (a blob's public id).
#[must_use]
pub fn to_hex(sha: &Sha) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in sha {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A blob id from its hex form (64 lowercase or uppercase hex digits).
#[must_use]
pub fn from_hex(s: &str) -> Option<Sha> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The classid of the blobs' advisory locks (the two-key space; the objid is the sha's first four bytes).
const LOCK_CLASS: i32 = 0x6d62_6c62; // "mblb"

fn lock_obj(sha: &Sha) -> i32 {
    i32::from_be_bytes([sha[0], sha[1], sha[2], sha[3]])
}

/// A blob's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRow {
    /// The sha.
    pub sha: Sha,
    /// Stored bytes.
    pub size: i64,
    /// `media`, `text` or `file`.
    pub kind: String,
    /// `base64-json`, `json-string` or `raw`.
    pub encoding: String,
    /// MIME type.
    pub mime: Option<String>,
    /// Image width.
    pub width: Option<i32>,
    /// Image height.
    pub height: Option<i32>,
    /// The bytes are in the bucket.
    pub stored: bool,
}

/// The row of blob `sha`.
///
/// # Errors
/// When the statement fails.
pub async fn row(db: &impl Db, sha: &Sha) -> Result<Option<BlobRow>> {
    let r = db
        .query_opt(
            "select size, kind, encoding, mime, width, height, stored from blobs where sha256 = $1",
            &[&sha.as_slice()],
        )
        .await?;
    Ok(r.map(|r| BlobRow {
        sha: *sha,
        size: r.get(0),
        kind: r.get(1),
        encoding: r.get(2),
        mime: r.get(3),
        width: r.get(4),
        height: r.get(5),
        stored: r.get(6),
    }))
}

/// A new raw blob (an upload, a model copy, a sent file) to record.
#[derive(Debug, Clone)]
pub struct RawBlob {
    /// Its sha.
    pub sha: Sha,
    /// Its size.
    pub size: i64,
    /// `media` or `file`.
    pub kind: &'static str,
    /// MIME type.
    pub mime: Option<String>,
    /// Image width.
    pub width: Option<u32>,
    /// Image height.
    pub height: Option<u32>,
}

fn i32_of(n: Option<u32>) -> Option<i32> {
    n.and_then(|n| i32::try_from(n).ok())
}

/// Records `blob`, whose bytes are in the bucket (a writer holding [`Blobs::shared`] put them), in `db`: inserted as
/// stored, or an existing row's `last_ref_at` bumped (which takes the row lock the GC needs) and `stored` set (the
/// writer put the bytes whenever the row did not say stored, [`Blobs::put_bytes`]: this also repairs a row the GC
/// found without its object, [`gc`]). A staged row's staging stays: the uploader puts it again and removes it.
///
/// # Errors
/// When the statement fails.
pub async fn record_stored(db: &impl Db, blob: &RawBlob) -> Result<()> {
    db.execute(
        "insert into blobs (sha256, size, kind, encoding, mime, width, height, stored)
         values ($1, $2, $3, 'raw', $4, $5, $6, true)
         on conflict (sha256) do update set last_ref_at = now(), stored = true",
        &[
            &blob.sha.as_slice(),
            &blob.size,
            &blob.kind,
            &blob.mime,
            &i32_of(blob.width),
            &i32_of(blob.height),
        ],
    )
    .await?;
    Ok(())
}

/// References `blob` from `owner` (`conversation`, `share` or `upload`), bumping its `last_ref_at` (magpie 5.3).
///
/// # Errors
/// When a statement fails (the blob row must exist).
pub async fn reference(db: &impl Db, owner: uuid::Uuid, owner_kind: &str, blob: &Sha) -> Result<()> {
    db.execute(
        "update blobs set last_ref_at = now() where sha256 = $1",
        &[&blob.as_slice()],
    )
    .await?;
    db.execute(
        "insert into blob_refs (owner, owner_kind, blob) values ($1, $2, $3) on conflict do nothing",
        &[&owner, &owner_kind, &blob.as_slice()],
    )
    .await?;
    Ok(())
}

/// The blob store of one replica: the bucket, the read cache, the decode permits, the blob lock sessions.
#[derive(Debug)]
pub struct Blobs {
    /// The bucket.
    pub bucket: Bucket,
    cache: moka::future::Cache<Sha, axum::body::Bytes>,
    decodes: Arc<Semaphore>,
    decode_memory: Arc<Semaphore>,
    body_memory: Arc<Semaphore>,
    writer_locks: Arc<locks::LockSession>,
    gc_locks: Arc<locks::LockSession>,
}

/// `bytes` in permits of 1 MiB (at least one).
fn mib_permits(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(1 << 20)).unwrap_or(u32::MAX).max(1)
}

/// A memory budget of `bytes` as a semaphore of 1 MiB permits.
fn budget(bytes: u64) -> Arc<Semaphore> {
    Arc::new(Semaphore::new(usize::try_from(bytes >> 20).unwrap_or(usize::MAX)))
}

/// Memory held from [`Blobs::hold_body`]: given back when dropped.
#[derive(Debug)]
pub struct BodyMemory(tokio::sync::OwnedSemaphorePermit);

impl BodyMemory {
    /// Keeps `bytes` of the held memory (at least 1 MiB) and gives the rest back.
    pub fn keep(&mut self, bytes: u64) {
        let keep = usize::try_from(mib_permits(bytes)).unwrap_or(usize::MAX);
        let extra = self.0.num_permits().saturating_sub(keep);
        if extra > 0 {
            drop(self.0.split(extra));
        }
    }
}

impl Blobs {
    /// The store for `cfg`, its blob locks on the database at `database_url` (no request or connection is made: the
    /// bucket's reachability never decides readiness, magpie 16.5).
    ///
    /// # Errors
    /// When the database URL does not parse.
    pub fn new(cfg: &BucketConfig, database_url: &str) -> Result<Self> {
        Ok(Self {
            bucket: Bucket::new(cfg),
            cache: moka::future::Cache::builder()
                .max_capacity(limits::BLOB_CACHE_BYTES)
                .weigher(|_, v: &axum::body::Bytes| u32::try_from(v.len()).unwrap_or(u32::MAX))
                .build(),
            decodes: Arc::new(Semaphore::new(limits::DECODES)),
            decode_memory: budget(limits::DECODE_MEMORY_BYTES),
            body_memory: budget(limits::BODY_MEMORY_BYTES),
            writer_locks: Arc::new(locks::LockSession::new(database_url, "magpie-blob-locks")?),
            gc_locks: Arc::new(locks::LockSession::new(database_url, "magpie-gc-locks")?),
        })
    }

    /// Takes `bytes` of the buffered-body budget ([`limits::BODY_MEMORY_BYTES`]): a request body read whole (an
    /// upload, a sandbox blob `PUT`), a staged blob the uploader holds or a blob read the read cache cannot answer
    /// ([`Blobs::stored_bytes`]), waiting until that much is free. Taken
    /// before the bytes are read and held until they are dropped. A holder may then take decode memory
    /// ([`Blobs::decode`]), never the other way round, so the two budgets cannot deadlock.
    ///
    /// # Errors
    /// [`Error::Internal`] when the budget was closed (never).
    pub async fn hold_body(&self, bytes: u64) -> Result<BodyMemory> {
        let total = u32::try_from(limits::BODY_MEMORY_BYTES >> 20).unwrap_or(u32::MAX);
        self.body_memory
            .clone()
            .acquire_many_owned(mib_permits(bytes).min(total))
            .await
            .map(BodyMemory)
            .map_err(|_| Error::Internal("body memory closed".into()))
    }

    /// Takes `sha`'s shared lock (writers of its object; see the module docs), on the writers' lock session.
    ///
    /// # Errors
    /// 503 `unavailable` when it cannot be had within [`locks::SHARED_WAIT`].
    pub async fn shared(&self, sha: &Sha) -> Result<SharedLock> {
        self.writer_locks.shared(sha).await
    }

    /// The GC's lock session.
    pub(crate) fn gc_locks(&self) -> &Arc<locks::LockSession> {
        &self.gc_locks
    }

    /// Puts `bytes` (whose sha is `sha`) into the bucket under the blob's key, with `lock` held by the caller. The
    /// bytes are not kept in the read cache (an upload or a sent file is rarely read back at once; reads fill it).
    ///
    /// # Errors
    /// 503 `unavailable` when the bucket cannot be reached.
    pub async fn put(&self, _lock: &SharedLock, sha: &Sha, bytes: axum::body::Bytes) -> Result<()> {
        self.bucket
            .put(&store::blob_key(&to_hex(sha)), bytes, None)
            .await
            .map_err(|e| unavailable(&e))
    }

    /// Makes `bytes` a stored object: takes its shared lock and puts it in the bucket, unless its row says it is
    /// stored already. The caller records the row ([`record_stored`]) and its references in a transaction that also
    /// runs [`SharedLock::check`], then releases the lock. No connection of `pool` is held across the `PUT`.
    ///
    /// # Errors
    /// 503 `unavailable` when the bucket cannot be reached or the lock not had; a statement error.
    pub async fn put_bytes(&self, pool: &Pool, bytes: axum::body::Bytes) -> Result<(Sha, SharedLock)> {
        let sha = sha256(&bytes);
        let lock = self.shared(&sha).await?;
        let stored = match pool.get().await {
            Ok(conn) => row(&conn, &sha).await.map(|r| r.is_some_and(|r| r.stored)),
            Err(e) => Err(e.into()),
        };
        let stored = match stored {
            Ok(s) => s,
            Err(e) => {
                lock.release().await;
                return Err(e);
            }
        };
        if !stored && let Err(e) = self.put(&lock, &sha, bytes).await {
            lock.release().await;
            return Err(e);
        }
        Ok((sha, lock))
    }

    /// Runs `f` (CPU-bound image work) on the blocking pool under a decode permit and `memory` bytes of the decode
    /// memory budget ([`limits::DECODE_MEMORY_BYTES`]; [`images::decode_memory`] estimates it). A decode whose
    /// estimate is over the whole budget is not run: it answers [`images::ImageError::OverMemory`] (415).
    ///
    /// # Errors
    /// What `f` answers; [`Error::Internal`] when the task panicked.
    pub async fn decode<T: Send + 'static>(
        &self,
        memory: u64,
        f: impl FnOnce() -> Result<T, images::ImageError> + Send + 'static,
    ) -> Result<Result<T, images::ImageError>> {
        let closed = |_| Error::Internal("decode permits closed".into());
        if memory > limits::DECODE_MEMORY_BYTES {
            return Ok(Err(images::ImageError::OverMemory(memory)));
        }
        let _memory = self
            .decode_memory
            .clone()
            .acquire_many_owned(mib_permits(memory))
            .await
            .map_err(closed)?;
        let _permit = self.decodes.clone().acquire_owned().await.map_err(closed)?;
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| Error::Internal(format!("image task: {e}")))
    }
}

/// 503 `unavailable` for a bucket that cannot be reached.
#[must_use]
pub fn unavailable(e: &store::StoreError) -> Error {
    tracing::warn!(error = %e, "bucket call failed");
    Error::code(magpie_api::errors::ErrorCode::Unavailable, "the bucket is unreachable")
}

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

/// Fetches the bytes of blobs stored in the bucket, outside any transaction (the cc store's reads of blobs a
/// transaction needs, [`Error::NeedBlobs`], and the read path's batches): their rows in one statement on a connection
/// returned before the bucket is called, then [`limits::BLOB_FETCH_CONCURRENCY`] objects at a time (magpie 6.11).
///
/// # Errors
/// 404 for a blob the bucket lost, 503 when it cannot be reached; a statement error.
pub async fn fetch_stored(app: &Magpie, shas: &[Sha]) -> Result<HashMap<Sha, Vec<u8>>> {
    use futures_util::{StreamExt as _, TryStreamExt as _};
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let keys: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
    let rows = app
        .pool
        .get()
        .await?
        .query(
            "select sha256, size, kind, encoding, mime, width, height, stored from blobs where sha256 = any($1)",
            &[&keys],
        )
        .await?;
    let mut found: HashMap<Sha, BlobRow> = HashMap::with_capacity(rows.len());
    for r in &rows {
        let sha: Vec<u8> = r.get(0);
        let Ok(sha) = Sha::try_from(sha.as_slice()) else {
            continue;
        };
        found.insert(
            sha,
            BlobRow {
                sha,
                size: r.get(1),
                kind: r.get(2),
                encoding: r.get(3),
                mime: r.get(4),
                width: r.get(5),
                height: r.get(6),
                stored: r.get(7),
            },
        );
    }
    if let Some(sha) = shas.iter().find(|s| !found.contains_key(*s)) {
        return Err(Error::Internal(format!("blob {} has no row", hex(sha))));
    }
    futures_util::stream::iter(found.into_values())
        .map(|r| async move {
            let b = app.blobs.stored_bytes(&app.pool, &r).await?;
            Ok::<_, Error>((r.sha, b.to_vec()))
        })
        .buffer_unordered(limits::BLOB_FETCH_CONCURRENCY)
        .try_collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let s = sha256(b"abc");
        let h = to_hex(&s);
        assert_eq!(h, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(from_hex(&h), Some(s));
        assert_eq!(from_hex(&h.to_uppercase()), Some(s));
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex(&format!("{}g", &h[..63])), None);
    }
}
