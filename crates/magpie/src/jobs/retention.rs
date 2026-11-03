//! Retention (magpie 15, 17): what magpie keeps only for a while.
//!
//! Each table is deleted in batches of [`limits::RETENTION_BATCH`] rows, one short statement each, until nothing is
//! due: however far behind a pass starts (a leader that never ran, the import's old changes and operation ids), no
//! statement comes near the statement timeout, so retention always catches up.

use aviary_pg::Pool;

use crate::error::Result;
use crate::limits;

fn secs(d: std::time::Duration) -> f64 {
    d.as_secs_f64()
}

/// Deletes the rows of `table` matching `due` (which uses `$1`, a number of seconds), in batches; how many.
async fn delete_due(pool: &Pool, table: &str, due: &str, age: std::time::Duration) -> Result<u64> {
    let sql = format!("delete from {table} where ctid in (select ctid from {table} where {due} limit $2)");
    let conn = pool.get().await?;
    let mut n = 0;
    loop {
        let deleted = conn
            .execute(sql.as_str(), &[&secs(age), &limits::RETENTION_BATCH])
            .await?;
        n += deleted;
        if deleted < u64::try_from(limits::RETENTION_BATCH).unwrap_or(u64::MAX) {
            return Ok(n);
        }
    }
}

/// The tables, what is due in each, and after how long. Unattached uploads' blob refs go before the uploads (the
/// blob GC frees the bytes). The legacy-path set (an upload with `legacy_path` or in `import_uploads`) is kept until
/// A7 (DESIGN 9.5: drafts keep old asset paths for weeks); each send attaches a fresh copy of it.
const DUE: &[(&str, &str, std::time::Duration)] = &[
    (
        "blob_refs",
        "owner_kind = 'upload' and owner in (select id from uploads u where conversation_id is null
           and created_at < now() - make_interval(secs => $1) and legacy_path is null
           and not exists (select 1 from import_uploads i where i.upload_id = u.id))",
        limits::UNATTACHED_UPLOAD,
    ),
    (
        "uploads",
        "conversation_id is null and created_at < now() - make_interval(secs => $1) and legacy_path is null
           and not exists (select 1 from import_uploads i where i.upload_id = uploads.id)",
        limits::UNATTACHED_UPLOAD,
    ),
    (
        "commands",
        "acked_at < now() - make_interval(secs => $1)",
        limits::IDEMPOTENCY_RETENTION,
    ),
    (
        "cc_ops",
        "at < now() - make_interval(secs => $1)",
        limits::IDEMPOTENCY_RETENTION,
    ),
    (
        "report_ids",
        "at < now() - make_interval(secs => $1)",
        limits::IDEMPOTENCY_RETENTION,
    ),
    (
        "changes",
        "at < now() - make_interval(secs => $1)",
        limits::CHANGES_RETENTION,
    ),
    (
        "sent_staging",
        "created_at < now() - make_interval(secs => $1)",
        limits::SENT_STAGING_RETENTION,
    ),
    (
        "cc_limbo",
        "reason = 'moved' and removed_at < now() - make_interval(secs => $1)",
        limits::MOVED_LIMBO,
    ),
];

/// One pass; how many rows were deleted.
///
/// # Errors
/// When a statement fails.
pub async fn pass(pool: &Pool) -> Result<u64> {
    let mut n = 0;
    for (table, due, age) in DUE {
        n += delete_due(pool, table, due, *age).await?;
    }
    n += crate::shares::expire_pending(pool, limits::PENDING_WORKSPACE_EXPIRY).await?;
    Ok(n)
}
