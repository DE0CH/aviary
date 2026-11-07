//! The sandbox behind a gateway call (magpie 3.3): the conversation the gateway names, checked to exist and to
//! belong to the person it names, and, on state-changing calls, the supervisor's lease (magpie 6.9: the current
//! `leaseId` presented by the sandbox that holds it, the same rule as the cc store's).

use magpie_api::errors::ErrorCode;
use magpie_api::server::Ctx;
use uuid::Uuid;

use crate::db::Db;
use crate::error::{Error, Result};

/// The sandbox's conversation, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConversation {
    /// The conversation.
    pub id: Uuid,
    /// Its owner.
    pub person: String,
}

/// The conversation of a gateway call: 404 when the identity headers are missing, the conversation does not exist
/// or is not the named person's. With `lease`, the call's `Aviary-Lease` must be the conversation's current lease
/// from the sandbox that holds it (409 `lease_lost` otherwise).
///
/// # Errors
/// Those refusals; a statement error.
pub async fn conversation(db: &impl Db, ctx: &Ctx, lease: bool) -> Result<SandboxConversation> {
    let sb = ctx.sandbox.as_ref().ok_or_else(Error::not_found)?;
    let id: Uuid = sb.conversation.parse().map_err(|_| Error::not_found())?;
    let row = db
        .query_opt(
            "select person, lease_id, lease_sandbox from conversations where id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let person: String = row.get(0);
    if person != sb.person {
        return Err(Error::not_found());
    }
    if lease {
        let current: Option<Uuid> = row.get(1);
        let holder: Option<Uuid> = row.get(2);
        let given = ctx.lease.as_deref().and_then(|l| l.trim().parse::<Uuid>().ok());
        let sandbox = sb.id.trim().parse::<Uuid>().ok();
        if current.is_none() || current != given || holder.is_none() || holder != sandbox {
            return Err(Error::code(
                ErrorCode::LeaseLost,
                "this sandbox does not hold the lease",
            ));
        }
    }
    Ok(SandboxConversation { id, person })
}
