//! Sandbox console tails (platform 15.4): AWS hosts are terminated after their sandboxes leave, and their logs with
//! them, so hermit keeps the last 64 KiB of each sandbox's console in `sandbox_logs` when it releases or loses it.
//! For a sandbox whose host is lost nothing can be fetched; the row says so (`bytes` null, the reason). Kept 14 days
//! (the janitor's retention); read by `GET /v1/admin/sandboxes/{id}/log`.

use hermit_api::Timestamp;
use serde::Serialize;
use uuid::Uuid;

use crate::Core;
use crate::db::SandboxRow;

/// At most this many bytes of a console are kept (sandboxd's `?tail=65536`).
pub const TAIL_BYTES: usize = 64 * 1024;

/// Why a tail was taken (`sandbox_logs.reason`).
pub const RELEASE: &str = "release";
/// The host was lost: nothing could be fetched.
pub const HOST_LOST: &str = "host_lost";
/// The host did not answer for the tail.
pub const UNAVAILABLE: &str = "tail_unavailable";

/// Fetches the console tail of `row` from its host (best effort, 5 s in the fabric) and stores it; when the host
/// does not answer the row records that instead. Once per sandbox: a destroy the stuck rule retries, or one of a
/// sandbox whose host was lost (its row written by [`record_lost`]), fetches nothing. Errors are logged: a tail
/// never holds up a destroy.
pub async fn capture(core: &Core, row: &SandboxRow) {
    let Some(host) = row.host.as_deref() else {
        return;
    };
    match stored(core, row.id).await {
        Ok(false) => {}
        Ok(true) => return,
        Err(e) => tracing::info!(sandbox = %row.id, error = %e, "console tail: no row read (fetching)"),
    }
    let tail = core.providers.fabric.console_tail(host, &row.name).await;
    let reason = if tail.is_some() { RELEASE } else { UNAVAILABLE };
    store(core, row.id, tail, reason).await;
}

/// Records that sandbox `id`'s console could not be fetched because its host is gone (13.7).
pub async fn record_lost(core: &Core, id: Uuid) {
    store(core, id, None, HOST_LOST).await;
}

async fn stored(core: &Core, id: Uuid) -> Result<bool, aviary_pg::Error> {
    Ok(core
        .pool
        .get()
        .await?
        .query_opt("select 1 from sandbox_logs where sandbox_id = $1", &[&id])
        .await?
        .is_some())
}

async fn store(core: &Core, id: Uuid, bytes: Option<Vec<u8>>, reason: &str) {
    let bytes = bytes.map(|mut b| {
        if b.len() > TAIL_BYTES {
            b.drain(..b.len() - TAIL_BYTES);
        }
        b
    });
    let res = async {
        core.pool
            .get()
            .await?
            .execute(
                // a tail already fetched is never replaced by a later "nothing could be fetched"
                "insert into sandbox_logs (sandbox_id, bytes, reason) values ($1, $2, $3)
                 on conflict (sandbox_id) do update set bytes = excluded.bytes, captured_at = now(),
                   reason = excluded.reason
                 where excluded.bytes is not null or sandbox_logs.bytes is null",
                &[&id, &bytes, &reason],
            )
            .await
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(sandbox = %id, error = %e, "console tail not stored");
    }
}

/// A stored tail as `GET /v1/admin/sandboxes/{id}/log` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tail {
    pub sandbox_id: Uuid,
    pub captured_at: Timestamp,
    pub reason: Option<String>,
    /// Bytes kept (`null` when nothing could be fetched).
    pub size: Option<u64>,
    /// The bytes as text (the console is JSON lines; invalid UTF-8 replaced).
    pub text: Option<String>,
}

/// The stored tail of sandbox `id`.
///
/// # Errors
/// A database error.
pub async fn read(core: &Core, id: Uuid) -> Result<Option<Tail>, aviary_pg::Error> {
    let row = core
        .pool
        .get()
        .await?
        .query_opt(
            "select captured_at, reason, bytes from sandbox_logs where sandbox_id = $1",
            &[&id],
        )
        .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    let bytes: Option<Vec<u8>> = r.try_get(2)?;
    Ok(Some(Tail {
        sandbox_id: id,
        captured_at: Timestamp::from(r.try_get::<_, time::OffsetDateTime>(0)?),
        reason: r.try_get(1)?,
        size: bytes.as_ref().map(|b| b.len() as u64),
        text: bytes.map(|b| String::from_utf8_lossy(&b).into_owned()),
    }))
}
