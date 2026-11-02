//! Retention (magpie 15, 17): what magpie keeps only for a while.

use aviary_pg::Pool;

use crate::error::Result;
use crate::limits;

fn secs(d: std::time::Duration) -> f64 {
    d.as_secs_f64()
}

/// One pass; how many rows were deleted.
///
/// # Errors
/// When a statement fails.
pub async fn pass(pool: &Pool) -> Result<u64> {
    let conn = pool.get().await?;
    let mut n = 0;
    // unattached uploads after 24 h (their blob refs first; the blob GC frees the bytes)
    n += conn
        .execute(
            "delete from blob_refs where owner_kind = 'upload' and owner in
               (select id from uploads where conversation_id is null
                  and created_at < now() - make_interval(secs => $1))",
            &[&secs(limits::UNATTACHED_UPLOAD)],
        )
        .await?;
    n += conn
        .execute(
            "delete from uploads where conversation_id is null and created_at < now() - make_interval(secs => $1)",
            &[&secs(limits::UNATTACHED_UPLOAD)],
        )
        .await?;
    let day = secs(limits::IDEMPOTENCY_RETENTION);
    n += conn
        .execute(
            "delete from commands where acked_at < now() - make_interval(secs => $1)",
            &[&day],
        )
        .await?;
    n += conn
        .execute(
            "delete from cc_ops where at < now() - make_interval(secs => $1)",
            &[&day],
        )
        .await?;
    n += conn
        .execute(
            "delete from report_ids where at < now() - make_interval(secs => $1)",
            &[&day],
        )
        .await?;
    n += conn
        .execute(
            "delete from changes where at < now() - make_interval(secs => $1)",
            &[&secs(limits::CHANGES_RETENTION)],
        )
        .await?;
    n += conn
        .execute(
            "delete from sent_staging where created_at < now() - make_interval(secs => $1)",
            &[&secs(limits::SENT_STAGING_RETENTION)],
        )
        .await?;
    n += conn
        .execute(
            "delete from cc_limbo where reason = 'moved' and removed_at < now() - make_interval(secs => $1)",
            &[&secs(limits::MOVED_LIMBO)],
        )
        .await?;
    Ok(n)
}
