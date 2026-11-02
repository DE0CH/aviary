//! The uploader (magpie 5.2, 15): every replica, every second, moves staged blobs to the bucket, up to
//! [`limits::UPLOAD_IN_FLIGHT`] at a time.
//!
//! An item is claimed by pushing its `next_at` out by the claim time in one statement (`for update skip locked`), so
//! two replicas never upload the same blob at once and an item whose replica died is taken again after the claim
//! lapses. The `PUT` runs outside any transaction under the blob's shared lock; then one transaction sets `stored`
//! and deletes the staging row. A failed `PUT` backs off (1 s doubling to 5 min) and logs `blob upload failing` at
//! most every 10 min per replica.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aviary_pg::Pool;
use axum::body::Bytes;
use futures_util::future::join_all;
use tokio::sync::Mutex;

use super::{Blobs, Sha};
use crate::error::Result;
use crate::limits;

/// The backoff after `attempts` failed uploads: 1 s doubling to 5 min.
#[must_use]
pub fn backoff(attempts: i32) -> Duration {
    let n = u32::try_from(attempts.saturating_sub(1).clamp(0, 20)).unwrap_or(0);
    limits::UPLOAD_BACKOFF_FIRST
        .saturating_mul(1u32 << n)
        .min(limits::UPLOAD_BACKOFF_MAX)
}

/// The uploader of one replica (remembers when it last logged a failure).
#[derive(Debug, Default)]
pub struct Uploader {
    last_failing_log: Mutex<Option<Instant>>,
}

impl Uploader {
    /// One pass: claims due items and uploads them; how many were stored.
    ///
    /// # Errors
    /// When a statement fails.
    pub async fn pass(&self, pool: &Pool, blobs: &Arc<Blobs>) -> Result<usize> {
        let claim = time::Duration::try_from(limits::UPLOAD_CLAIM).unwrap_or(time::Duration::MINUTE);
        let rows = pool
            .get()
            .await?
            .query(
                "update blob_staging s set next_at = now() + make_interval(secs => $2)
                 where s.sha256 in (select sha256 from blob_staging where next_at <= now()
                                    order by next_at limit $1 for update skip locked)
                 returning s.sha256, s.bytes, s.attempts",
                &[&limits::UPLOAD_IN_FLIGHT_I64, &claim.as_seconds_f64()],
            )
            .await?;
        let jobs = rows.into_iter().filter_map(|r| {
            let sha: Vec<u8> = r.get(0);
            let sha = Sha::try_from(sha.as_slice()).ok()?;
            let bytes = Bytes::from(r.get::<_, Vec<u8>>(1));
            let attempts: i32 = r.get(2);
            Some(self.upload(pool, blobs, sha, bytes, attempts))
        });
        let done = join_all(jobs).await;
        let mut n = 0;
        for r in done {
            if r? {
                n += 1;
            }
        }
        Ok(n)
    }

    async fn upload(&self, pool: &Pool, blobs: &Arc<Blobs>, sha: Sha, bytes: Bytes, attempts: i32) -> Result<bool> {
        let lock = blobs.shared(pool, &sha).await?;
        let put = blobs.put(&lock, &sha, bytes).await;
        let outcome = match put {
            Ok(()) => aviary_pg::tx!(pool, |t| -> Result<()> {
                t.execute("update blobs set stored = true where sha256 = $1", &[&sha.as_slice()])
                    .await?;
                t.execute("delete from blob_staging where sha256 = $1", &[&sha.as_slice()])
                    .await?;
                Ok(())
            })
            .await
            .map(|()| true),
            Err(e) => {
                let attempts = attempts.saturating_add(1);
                let wait = time::Duration::try_from(backoff(attempts)).unwrap_or(time::Duration::MINUTE);
                pool.get()
                    .await?
                    .execute(
                        "update blob_staging set attempts = $2, next_at = now() + make_interval(secs => $3)
                         where sha256 = $1",
                        &[&sha.as_slice(), &attempts, &wait.as_seconds_f64()],
                    )
                    .await?;
                let mut last = self.last_failing_log.lock().await;
                if last.is_none_or(|t| t.elapsed() >= limits::UPLOAD_FAILING_LOG_EVERY) {
                    *last = Some(Instant::now());
                    tracing::error!(blob = %super::to_hex(&sha), attempts, error = %e, "blob upload failing");
                }
                Ok(false)
            }
        };
        lock.release().await;
        outcome
    }
}
