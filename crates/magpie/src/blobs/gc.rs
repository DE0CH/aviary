//! The blob GC (magpie 5.3, 15; job leader, hourly) and the weekly bucket reconcile.
//!
//! A blob is freed only when, under its row lock, it is stored, nothing references it (`blob_refs`, nor an upload or
//! a sent file row) and its `last_ref_at` is older than 24 h; inserting a reference bumps `last_ref_at` under the
//! same row lock, so a blob about to be referenced is never freed. The row goes first (committed), then its objects
//! (the stored bytes, the thumbnail, the display copy), all under the blob's exclusive advisory lock, which every
//! writer of the blob's object holds shared (see [`super`]). The GC only tries that lock, on its own lock session
//! ([`super::locks`]): a blob a writer is putting is left for the next pass, and no wait pins a connection.
//!
//! A lock session can end while the GC deletes (a database switchover): a writer may then take the shared lock on a
//! new session, put the same key and commit a stored row while the GC's `DELETE`, already sent, lands after the
//! `PUT`. So when the GC finds its lock lost after a delete, it waits to take the blob's lock again (every writer of
//! that moment has finished), and if a row says the blob is stored, it asks the bucket (`HEAD`): a missing object
//! clears `stored`, so the next writer of those bytes puts them again ([`super::record_stored`]), and logs
//! `blob object lost`. The blob is first recorded in `blob_reverify`, and the row is removed only once the check has
//! run, so a check that could not run (the lock not had again within [`limits::BLOB_REVERIFY_WAIT`], the bucket or the
//! database unreachable) is retried by every later GC pass until it has.
//!
//! The reconcile lists the bucket and deletes objects older than 24 h whose blob has no row (a `PUT` whose
//! transaction then failed, a derived copy made after its blob was freed), each re-checked under the same lock; a lock
//! lost while such a delete was in flight is followed by the same re-verification.

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
        reverify_queued(pool, blobs).await?;
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
    let mut lost = false;
    if deleted {
        blobs.cache.invalidate(sha).await;
        let hex = to_hex(sha);
        for key in [store::blob_key(&hex), store::thumb_key(&hex), store::display_key(&hex)] {
            if !lock.held().await {
                // the lock session ended: a writer may be putting the object again (the reconcile deletes leftovers)
                tracing::warn!(key = %key, "blob lock lost; freed blob's objects left for the reconcile");
                lost = true;
                break;
            }
            if let Err(e) = blobs.bucket.delete(&key).await {
                // the reconcile deletes it later
                tracing::warn!(error = %e, key = %key, "freed blob's object not deleted");
            }
            if !lock.held().await {
                // lost while the delete was in flight: it may have landed after a writer's put
                lost = true;
                break;
            }
        }
    }
    lock.release().await;
    if lost {
        reverify(pool, blobs, sha).await?;
    }
    Ok(deleted)
}

/// After the GC lost a blob's lock while deleting its objects: records the blob in `blob_reverify`, then, once the
/// lock is had again (no writer is mid-put), checks a row that says stored against the bucket and clears `stored`
/// when the object is gone. Waits at most [`limits::BLOB_REVERIFY_WAIT`]; a check not done by then stays recorded
/// for the next pass ([`reverify_queued`]).
///
/// # Errors
/// None today (failures are retried, then left recorded); kept for the callers' `?`.
pub async fn reverify(pool: &Pool, blobs: &Arc<Blobs>, sha: &Sha) -> Result<()> {
    reverify_within(pool, blobs, sha, limits::BLOB_REVERIFY_WAIT).await
}

/// [`reverify`], waiting at most `wait`.
///
/// # Errors
/// As [`reverify`].
pub async fn reverify_within(pool: &Pool, blobs: &Arc<Blobs>, sha: &Sha, wait: std::time::Duration) -> Result<()> {
    let hex = to_hex(sha);
    let start = Instant::now();
    let mut queued = false;
    loop {
        if !queued {
            queued = queue(pool, sha).await.is_ok();
        }
        if verify_once(pool, blobs, sha).await {
            return Ok(());
        }
        if start.elapsed() >= wait {
            if queued {
                tracing::warn!(blob = %hex, "blob lock lost by the GC; re-verification left for the next pass");
            } else {
                tracing::error!(blob = %hex, "blob lock lost by the GC; its object neither re-verified nor queued");
            }
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn queue(pool: &Pool, sha: &Sha) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "insert into blob_reverify (sha256) values ($1) on conflict do nothing",
            &[&sha.as_slice()],
        )
        .await?;
    Ok(())
}

/// Re-verifies every recorded blob it can now (one try each); how many are still recorded.
///
/// # Errors
/// When the recorded blobs cannot be listed.
pub async fn reverify_queued(pool: &Pool, blobs: &Arc<Blobs>) -> Result<u64> {
    let rows = pool
        .get()
        .await?
        .query("select sha256 from blob_reverify order by since", &[])
        .await?;
    let mut left = 0u64;
    for r in rows {
        let sha: Vec<u8> = r.get(0);
        let Ok(sha) = Sha::try_from(sha.as_slice()) else {
            continue;
        };
        if !verify_once(pool, blobs, &sha).await {
            left += 1;
        }
    }
    if left > 0 {
        tracing::warn!(
            blobs = left,
            "blobs still waiting for re-verification after a lost GC lock"
        );
    }
    Ok(left)
}

/// One try of the check under the blob's exclusive lock; whether it ran (and the record is gone).
async fn verify_once(pool: &Pool, blobs: &Arc<Blobs>, sha: &Sha) -> bool {
    let hex = to_hex(sha);
    let Ok(Some(lock)) = blobs.gc_locks().try_exclusive(sha).await else {
        return false;
    };
    let done = async {
        let stored = super::row(&pool.get().await?, sha).await?.is_some_and(|r| r.stored);
        if stored {
            match blobs.bucket.exists(&store::blob_key(&hex)).await {
                Ok(true) => {}
                Ok(false) => {
                    pool.get()
                        .await?
                        .execute("update blobs set stored = false where sha256 = $1", &[&sha.as_slice()])
                        .await?;
                    tracing::error!(blob = %hex, "blob object lost to a GC delete; stored cleared for the next writer");
                }
                Err(e) => {
                    tracing::warn!(blob = %hex, error = %e, "blob re-verification: the bucket did not answer");
                    return Ok::<_, crate::error::Error>(false);
                }
            }
        }
        if !lock.held().await {
            // lost again meanwhile: what was checked may be stale
            return Ok(false);
        }
        pool.get()
            .await?
            .execute("delete from blob_reverify where sha256 = $1", &[&sha.as_slice()])
            .await?;
        Ok(true)
    }
    .await;
    lock.release().await;
    match done {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(blob = %hex, error = %e, "blob re-verification failed");
            false
        }
    }
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
                let mut lost = false;
                if !exists && lock.held().await {
                    match blobs.bucket.delete(&key).await {
                        Ok(()) => n += 1,
                        Err(e) => tracing::warn!(error = %e, key = %key, "orphan object not deleted"),
                    }
                    // lost while the delete was in flight: a writer may have put the key and recorded it meanwhile
                    lost = !lock.held().await;
                }
                lock.release().await;
                if lost {
                    reverify(pool, blobs, &sha).await?;
                }
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
