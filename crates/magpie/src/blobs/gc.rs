//! The blob GC (magpie 5.3, 15; job leader, hourly) and the weekly bucket reconcile.
//!
//! A blob is freed only when, under its row lock, it is stored, nothing references it (`blob_refs`, nor an upload or
//! a sent file row) and its `last_ref_at` is older than 24 h; inserting a reference bumps `last_ref_at` under the
//! same row lock, so a blob about to be referenced is never freed. The row goes first (committed), then its objects
//! (the stored bytes, the thumbnail, the display copy), all under the blob's exclusive advisory lock, which every
//! writer of the blob's object holds shared (see [`super`]). The GC only tries that lock, on its own lock session
//! ([`super::locks`]): a blob a writer is putting is left for the next pass, and no wait pins a connection.
//!
//! The reconcile lists the bucket and deletes objects older than 24 h whose blob has no row (a `PUT` whose
//! transaction then failed, a derived copy made after its blob was freed), each re-checked under the same lock.

use std::sync::Arc;
use std::time::Instant;

use aviary_pg::Pool;
use tokio::sync::Mutex;

use super::store::{self, StoreError};
use super::{Blobs, Sha, from_hex, to_hex};
use crate::error::Result;
use crate::limits;

/// What makes a blob free (over `blobs b`; `$1` is the grace in seconds).
const FREE: &str = "b.stored and b.last_ref_at < now() - make_interval(secs => $1)
    and not exists (select 1 from blob_refs r where r.blob = b.sha256)
    and not exists (select 1 from blob_staging s where s.sha256 = b.sha256)
    and not exists (select 1 from uploads u where u.blob = b.sha256 or u.model_blob = b.sha256)
    and not exists (select 1 from sent_files f where f.blob = b.sha256)";

fn grace() -> f64 {
    limits::BLOB_GC_GRACE.as_secs_f64()
}

/// The GC of the job leader (remembers when it last reconciled).
#[derive(Debug, Default)]
pub struct Gc {
    last_reconcile: Mutex<Option<Instant>>,
}

impl Gc {
    /// One hourly pass: frees what is free, and reconciles the bucket when a week has passed since the last time
    /// (and on a leader's first pass). How many blobs and objects went.
    ///
    /// # Errors
    /// When a statement fails.
    pub async fn pass(&self, pool: &Pool, blobs: &Arc<Blobs>) -> Result<u64> {
        let mut n = free(pool, blobs).await?;
        let mut last = self.last_reconcile.lock().await;
        if last.is_none_or(|t| t.elapsed() >= limits::BUCKET_RECONCILE_EVERY) {
            match reconcile(pool, blobs).await {
                Ok(m) => {
                    *last = Some(Instant::now());
                    n += m;
                }
                Err(e) => tracing::warn!(error = %e, "bucket reconcile failed"),
            }
        }
        Ok(n)
    }
}

/// Frees every free blob, in batches; how many.
///
/// # Errors
/// When a statement fails.
pub async fn free(pool: &Pool, blobs: &Arc<Blobs>) -> Result<u64> {
    let mut n = 0;
    loop {
        let rows = pool
            .get()
            .await?
            .query(
                &format!("select b.sha256 from blobs b where {FREE} limit $2"),
                &[&grace(), &limits::BLOB_GC_BATCH],
            )
            .await?;
        let batch = rows.len();
        let before = n;
        for r in rows {
            let sha: Vec<u8> = r.get(0);
            let Ok(sha) = Sha::try_from(sha.as_slice()) else {
                continue;
            };
            if free_one(pool, blobs, &sha).await? {
                n += 1;
            }
        }
        // a full batch of which nothing went (writers held them all) is not selected again this pass
        if batch < usize::try_from(limits::BLOB_GC_BATCH).unwrap_or(usize::MAX) || n == before {
            return Ok(n);
        }
    }
}

async fn free_one(pool: &Pool, blobs: &Arc<Blobs>, sha: &Sha) -> Result<bool> {
    let Some(lock) = blobs.gc_locks().try_exclusive(sha).await? else {
        // a writer is putting it: it is about to be referenced
        return Ok(false);
    };
    let deleted = aviary_pg::tx!(pool, |t| -> Result<bool> {
        let Some(_) = t
            .query_opt(
                &format!("select 1 from blobs b where b.sha256 = $2 and {FREE} for update"),
                &[&grace(), &sha.as_slice()],
            )
            .await?
        else {
            return Ok(false);
        };
        t.execute("delete from blobs where sha256 = $1", &[&sha.as_slice()])
            .await?;
        Ok(true)
    })
    .await;
    let deleted = match deleted {
        Ok(d) => d,
        Err(e) => {
            lock.release().await;
            return Err(e);
        }
    };
    if deleted {
        blobs.cache.invalidate(sha).await;
        let hex = to_hex(sha);
        for key in [store::blob_key(&hex), store::thumb_key(&hex), store::display_key(&hex)] {
            if !lock.held().await {
                // the lock session ended: a writer may be putting the object again (the reconcile deletes leftovers)
                tracing::warn!(key = %key, "blob lock lost; freed blob's objects left for the reconcile");
                break;
            }
            if let Err(e) = blobs.bucket.delete(&key).await {
                // the reconcile deletes it later
                tracing::warn!(error = %e, key = %key, "freed blob's object not deleted");
            }
        }
    }
    lock.release().await;
    Ok(deleted)
}

/// The sha a bucket key belongs to.
#[must_use]
pub fn sha_of_key(key: &str) -> Option<Sha> {
    let name = key.rsplit('/').next()?;
    let hex = name.split(['-', '.']).next()?;
    from_hex(hex)
}

/// Deletes the bucket's objects older than 24 h whose blob has no row; how many.
///
/// # Errors
/// When the bucket cannot be listed or a statement fails.
pub async fn reconcile(pool: &Pool, blobs: &Arc<Blobs>) -> Result<u64> {
    let cutoff = time::OffsetDateTime::now_utc()
        - time::Duration::try_from(limits::BLOB_GC_GRACE).unwrap_or(time::Duration::DAY);
    reconcile_before(pool, blobs, cutoff).await
}

/// [`reconcile`] of the objects last modified before `cutoff`.
///
/// # Errors
/// As [`reconcile`].
pub async fn reconcile_before(pool: &Pool, blobs: &Arc<Blobs>, cutoff: time::OffsetDateTime) -> Result<u64> {
    let mut n = 0;
    for prefix in ["blobs/", "thumbs/", "display/"] {
        let mut token = None;
        loop {
            let (items, next) = blobs
                .bucket
                .list(prefix, token)
                .await
                .map_err(|e: StoreError| super::unavailable(&e))?;
            let old: Vec<(String, Sha)> = items
                .into_iter()
                .filter(|(_, at)| at.is_some_and(|a| a < cutoff))
                .filter_map(|(k, _)| sha_of_key(&k).map(|s| (k, s)))
                .collect();
            let keys: Vec<&[u8]> = old.iter().map(|(_, s)| s.as_slice()).collect();
            let known = pool
                .get()
                .await?
                .query("select sha256 from blobs where sha256 = any($1)", &[&keys])
                .await?;
            let known: std::collections::HashSet<Vec<u8>> = known.into_iter().map(|r| r.get(0)).collect();
            for (key, sha) in old {
                if known.contains(sha.as_slice()) {
                    continue;
                }
                let Some(lock) = blobs.gc_locks().try_exclusive(&sha).await? else {
                    continue;
                };
                let exists = pool
                    .get()
                    .await?
                    .query_opt("select 1 from blobs where sha256 = $1", &[&sha.as_slice()])
                    .await?
                    .is_some();
                if !exists && lock.held().await {
                    match blobs.bucket.delete(&key).await {
                        Ok(()) => n += 1,
                        Err(e) => tracing::warn!(error = %e, key = %key, "orphan object not deleted"),
                    }
                }
                lock.release().await;
            }
            match next {
                Some(t) => token = Some(t),
                None => break,
            }
        }
    }
    if n > 0 {
        tracing::info!(objects = n, "bucket reconciled");
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_their_blob() {
        let sha = super::super::sha256(b"x");
        let hex = to_hex(&sha);
        assert_eq!(sha_of_key(&store::blob_key(&hex)), Some(sha));
        assert_eq!(sha_of_key(&store::thumb_key(&hex)), Some(sha));
        assert_eq!(sha_of_key(&store::display_key(&hex)), Some(sha));
        assert_eq!(sha_of_key("blobs/ab/nothex"), None);
    }
}
