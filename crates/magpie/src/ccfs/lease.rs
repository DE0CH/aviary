//! The lease (magpie 6.9): one writer per conversation. `POST /v1/sandbox/lease` gives the calling sandbox a fresh
//! `leaseId` and bumps `lease_epoch`, but only when the caller is the conversation's live sandbox as hermit sees it:
//! the sandbox hermit's feed recorded (`hermit_sandbox`, `starting` or `ready`), else hermit's own answer to
//! `GET /v1/conversations/{id}/sandbox` (no wait, 2 s), which is then recorded. hermit naming another sandbox, or
//! none, answers 409 `not_current_sandbox`; hermit unreachable answers 503 (the supervisor retries and never takes a
//! lease on missing evidence). Granting makes every earlier lease dead at once; the answer carries `resync` and
//! clears `cc_resync` (6.12).

use hermit_api::sandbox::SandboxQuery;
use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::LeaseResponse;
use uuid::Uuid;

use super::{Caller, lock};
use crate::app::Magpie;
use crate::error::{Error, Result};
use crate::limits;

fn live(state: Option<&str>) -> bool {
    matches!(state, Some("starting" | "ready"))
}

fn not_current() -> Error {
    Error::code(
        ErrorCode::NotCurrentSandbox,
        "hermit does not name this sandbox as the conversation's live one",
    )
}

/// What hermit says about the conversation's sandbox: `(sandbox, state, version)`, `None` for no sandbox.
async fn ask_hermit(app: &Magpie, conversation: Uuid) -> Result<Option<(Uuid, String, i64)>> {
    let call = app.hermit.get_sandbox(conversation, SandboxQuery::default());
    match tokio::time::timeout(limits::LEASE_HERMIT_TIMEOUT, call).await {
        Ok(Ok(s)) => Ok(Some((
            s.id,
            s.state.as_str().to_owned(),
            i64::try_from(s.version).unwrap_or(i64::MAX),
        ))),
        Ok(Err(e)) if e.status() == Some(404) => Ok(None),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, conversation = %conversation, "lease: hermit did not answer");
            Err(Error::code(ErrorCode::Unavailable, "hermit cannot be reached"))
        }
        Err(_) => {
            tracing::warn!(conversation = %conversation, "lease: hermit timed out");
            Err(Error::code(ErrorCode::Unavailable, "hermit cannot be reached"))
        }
    }
}

/// `POST /v1/sandbox/lease`.
///
/// # Errors
/// 404, 409 `not_current_sandbox` / `conversation_migrating`, 503 `unavailable`; a statement error.
pub async fn lease(app: &Magpie, caller: &Caller) -> Result<LeaseResponse> {
    let row = app
        .pool
        .get()
        .await?
        .query_opt(
            "select person, migrating, hermit_sandbox, hermit_state from conversations where id = $1",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let person: String = row.get(0);
    if person != caller.person {
        return Err(Error::not_found());
    }
    if row.get::<_, bool>(1) {
        return Err(magpie_api::errors::Refusal::migrating(limits::MIGRATING_RETRY_AFTER_MS).into());
    }
    let recorded: Option<Uuid> = row.get(2);
    let state: Option<String> = row.get(3);
    // the feed's record first; hermit's own answer (outside any transaction) only when it does not name the caller
    let hermit = if recorded == Some(caller.sandbox) && live(state.as_deref()) {
        None
    } else {
        match ask_hermit(app, caller.conversation).await? {
            Some((id, state, version)) if id == caller.sandbox && live(Some(&state)) => Some((id, state, version)),
            other => {
                tracing::info!(
                    conversation = %caller.conversation,
                    caller = %caller.sandbox,
                    hermit = ?other.map(|o| o.0),
                    "lease refused: not hermit's current sandbox"
                );
                return Err(not_current());
            }
        }
    };
    let grant = aviary_pg::tx!(&app.pool, |t| -> Result<LeaseResponse> {
        let conv = lock(t, caller).await?;
        conv.refuse_migrating()?;
        if let Some((id, state, version)) = &hermit {
            t.execute(
                "update conversations set hermit_sandbox = $2, hermit_state = $3, hermit_version = $4
                      where id = $1 and (hermit_sandbox is distinct from $2 or hermit_version is null
                                         or hermit_version < $4)",
                &[&caller.conversation, id, state, version],
            )
            .await?;
        } else {
            // re-checked under the lock: the feed may have moved on since the read above
            let row = t
                .query_one(
                    "select hermit_sandbox, hermit_state from conversations where id = $1",
                    &[&caller.conversation],
                )
                .await?;
            let id: Option<Uuid> = row.get(0);
            let state: Option<String> = row.get(1);
            if id != Some(caller.sandbox) || !live(state.as_deref()) {
                return Err(not_current());
            }
        }
        let lease = Uuid::new_v4();
        let row = t
            .query_one(
                "update conversations c set lease_id = $2, lease_sandbox = $3, lease_epoch = lease_epoch + 1,
                        cc_resync = false, lease_granted_at = now()
                   from (select cc_resync from conversations where id = $1) old
                  where c.id = $1
                  returning c.lease_epoch, c.provider_session_id, c.main_transcript, old.cc_resync",
                &[&caller.conversation, &lease, &caller.sandbox],
            )
            .await?;
        Ok(LeaseResponse {
            lease_id: lease,
            epoch: u64::try_from(row.get::<_, i64>(0)).unwrap_or(0),
            conversation_id: caller.conversation,
            provider_session_id: row.get(1),
            main_transcript: row.get(2),
            resync: row.get(3),
        })
    })
    .await?;
    tracing::info!(conversation = %caller.conversation, sandbox = %caller.sandbox, epoch = grant.epoch, "lease granted");
    Ok(grant)
}
