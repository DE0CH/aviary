//! The supervisor behind a gateway call (magpie 3.3): the identity the gateway sets (`Aviary-Sandbox-*`), and the
//! lease every state change and the command poll must carry (`Aviary-Lease`, magpie 6.9).

use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::server::Ctx;
use uuid::Uuid;

use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// The sandbox behind a gateway call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// hermit's sandbox id.
    pub sandbox: Uuid,
    /// Its conversation.
    pub conversation: Uuid,
    /// The conversation's owner.
    pub person: String,
    /// `Aviary-Lease`, when sent and a uuid.
    pub lease: Option<Uuid>,
}

impl Sandbox {
    /// From the request's headers.
    ///
    /// # Errors
    /// 400 `bad_request` without the sandbox identity (the gateway always sets it).
    pub fn from_ctx(ctx: &Ctx) -> Result<Self> {
        let s = ctx
            .sandbox
            .as_ref()
            .ok_or_else(|| Error::bad_request("the sandbox identity is missing"))?;
        let parse = |v: &str, what: &str| {
            Uuid::parse_str(v.trim()).map_err(|_| Error::bad_request(format!("{what} is not a uuid")))
        };
        Ok(Self {
            sandbox: parse(&s.id, "Aviary-Sandbox-Id")?,
            conversation: parse(&s.conversation, "Aviary-Sandbox-Conversation")?,
            person: s.person.trim().to_lowercase(),
            lease: ctx.lease.as_deref().and_then(|l| Uuid::parse_str(l.trim()).ok()),
        })
    }
}

/// What a sandbox call learns from the conversation row it locked.
#[derive(Debug, Clone)]
pub struct Locked {
    /// The conversation.
    pub id: Uuid,
    /// Its owner.
    pub person: String,
    /// Its active turn.
    pub active_turn: Option<Uuid>,
    /// `run_state`.
    pub run_state: String,
}

/// Locks the caller's conversation (`select … for update`) and checks it: 404 when it does not exist or is not the
/// caller's person's, 409 `conversation_migrating` while the importer holds it, 409 `lease_lost` when `Aviary-Lease`
/// is not the conversation's current lease.
///
/// # Errors
/// Those refusals, or a statement error.
pub async fn lock(db: &impl Db, caller: &Sandbox) -> Result<Locked> {
    let row = db
        .query_opt(
            "select person, migrating, lease_id, lease_sandbox, active_turn, run_state from conversations
             where id = $1 for update",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    check(&row, caller)?;
    Ok(Locked {
        id: caller.conversation,
        person: row.get(0),
        active_turn: row.get(4),
        run_state: row.get(5),
    })
}

/// The checks of [`lock`] without the lock (the command poll's look), on a row of `person, migrating, lease_id,
/// lease_sandbox`: the lease must be the current one, held by the calling sandbox (as the cc routes check it).
///
/// # Errors
/// As [`lock`].
pub fn check(row: &aviary_pg::Row, caller: &Sandbox) -> Result<()> {
    let person: String = row.get(0);
    if person != caller.person {
        return Err(Error::not_found());
    }
    if row.get::<_, bool>(1) {
        return Err(Refusal::migrating(limits::MIGRATING_RETRY_AFTER_MS).into());
    }
    let lease: Option<Uuid> = row.get(2);
    let holder: Option<Uuid> = row.get(3);
    if lease.is_none() || lease != caller.lease || holder != Some(caller.sandbox) {
        return Err(Error::code(
            ErrorCode::LeaseLost,
            "the call's lease is not the conversation's current lease",
        ));
    }
    Ok(())
}
