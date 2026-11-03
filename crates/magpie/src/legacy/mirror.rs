//! The running mirror (DESIGN 9.3 Lists, cutover 6.3): the importer writes each legacy conversation's old running
//! state into `run_state` every 5 s, from the running sets it reads in the old pods, so native lists (M1) and the
//! change feed (M12) show an old turn as running.

use aviary_pg::Pool;
use magpie_api::conversations::RunState;
use uuid::Uuid;

use crate::changes;
use crate::error::{Error, Result};

/// Sets a migrating conversation's `run_state` to `state` (`idle`, `running` or `held`), with change
/// `conversation_upserted` when it changed. A conversation that is not migrating is never written (cutover 6.2
/// invariant 2): `Ok(false)`; `Ok(true)` otherwise, changed or not.
///
/// # Errors
/// 400 `bad_request` for another state; a statement error.
pub async fn run_state(pool: &Pool, id: Uuid, state: RunState) -> Result<bool> {
    let s = match state {
        RunState::Idle => "idle",
        RunState::Running => "running",
        RunState::Held => "held",
        _ => return Err(Error::bad_request("the running mirror writes idle, running or held")),
    };
    aviary_pg::tx!(pool, |t| -> Result<bool> {
        let Some(r) = t
            .query_opt(
                "select person, migrating, run_state from conversations where id = $1 for update",
                &[&id],
            )
            .await?
        else {
            return Ok(false);
        };
        if !r.get::<_, bool>(1) {
            return Ok(false);
        }
        if r.get::<_, String>(2) == s {
            return Ok(true);
        }
        t.execute("update conversations set run_state = $2 where id = $1", &[&id, &s])
            .await?;
        changes::upserted(t, &r.get::<_, String>(0), id).await?;
        Ok(true)
    })
    .await
}
