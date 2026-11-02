//! H3 `POST /v1/warm` (hermit 3.5): a demand hint for the scaler. Never creates, touches or extends a sandbox.

use hermit_api::error::{self, ApiError};
use hermit_api::people::{WarmReply, WarmRequest, normalise_email};

use crate::Core;
use crate::db;
use crate::sandboxes::OrUnavailable;

/// 202 `{hinted}`.
///
/// # Errors
/// 403 `not_allowed`, 503.
pub async fn warm(core: &Core, body: &WarmRequest) -> Result<WarmReply, ApiError> {
    let person = normalise_email(&body.person).ok_or_else(error::not_allowed)?;
    if !core.catalogue.allowed(&person) {
        return Err(error::not_allowed());
    }
    if core.is_draining() {
        return Ok(WarmReply { hinted: false });
    }
    if let Some(cid) = body.conversation_id
        && db::latest_of(&core.pool, cid).await.or_503()?.is_some_and(|r| {
            matches!(
                r.status,
                hermit_api::sandbox::SandboxState::Starting | hermit_api::sandbox::SandboxState::Ready
            )
        })
    {
        return Ok(WarmReply { hinted: false });
    }
    core.pool
        .get()
        .await
        .or_503()?
        .execute(
            "insert into demand_hints (person, at) values ($1, now()) on conflict (person) do update set at = now()",
            &[&person],
        )
        .await
        .or_503()?;
    Ok(WarmReply { hinted: true })
}
