//! Cancel (M8, magpie 8.6) and ask answers (M11).
//!
//! A cancel never fails for "nothing running" and never waits on the sandbox: a turn whose `start_turn` no
//! supervisor has received ends `canceled` at once (the person's message stays, sent and unanswered); a delivered
//! one gets `turn.stop_requested`, the `interrupt` command and a 30 s deadline, after which the sweeper forces it
//! (`stop.forced`, [`super::sweeper`]). A held conversation gets `stop_tasks`.

use aviary_pg::Pool;
use magpie_api::errors::ErrorCode;
use magpie_api::events::{EventBody, TurnStopRequested};
use magpie_api::sandbox::AnswerAsk;
use magpie_api::turns::{AgentPermissionResponse, Behavior, CancelResponse};
use serde_json::{Value, json};
use uuid::Uuid;

use super::end::{self, Ending};
use super::{State, active, command, lock_start_turn, secs};
use crate::changes;
use crate::conversations::{Person, lock};
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;

/// M8.
///
/// # Errors
/// 404, 409 `conversation_migrating`; a statement error.
pub async fn cancel(pool: &Pool, person: &Person, id: Uuid) -> Result<CancelResponse> {
    aviary_pg::tx!(pool, |t| -> Result<CancelResponse> {
        let c = lock(t, &person.email, id).await?;
        c.refuse_migrating()?;
        if let Some(v) = &person.viewer {
            t.execute(
                "insert into view_as_log (admin, person, session, event, conversation_id) values ($1, $2, $3, 'cancel', $4)",
                &[&v.email, &person.email, &v.session, &id],
            )
            .await?;
        }
        let Some(turn) = active(t, id).await?.filter(|t| !t.state.is_ended()) else {
            let state: String = t
                .query_one("select run_state from conversations where id = $1", &[&id])
                .await?
                .get(0);
            if state == "held" {
                command(t, id, "stop_tasks", None, &json!({})).await?;
            }
            return Ok(CancelResponse { turn_id: None });
        };
        if turn.state == State::Stopping {
            return Ok(CancelResponse {
                turn_id: Some(turn.id),
            });
        }
        if turn.state == State::Parked {
            // no sandbox runs it: its questions close and it ends at once (DECISIONS 17)
            super::parked::cancel(t, &person.email, &turn).await?;
            return Ok(CancelResponse {
                turn_id: Some(turn.id),
            });
        }
        // the start_turn row's lock orders this decision with a command poll that delivers it at the same moment
        let (delivered, _) = lock_start_turn(t, &turn).await?;
        if !delivered {
            end::end(t, &person.email, &turn, Ending::canceled()).await?;
            return Ok(CancelResponse {
                turn_id: Some(turn.id),
            });
        }
        let body = EventBody::TurnStopRequested(TurnStopRequested { turn_id: turn.id });
        events::append(t, id, NewEvent::of(&body)?.turn(Some(turn.id)).viewer(person.viewer_email())).await?;
        let by = person.viewer_email().unwrap_or_else(|| person.email.clone());
        t.execute(
            "update turns set state = 'stopping', stop_requested_at = now(), stop_by = $2,
                    deadline = now() + make_interval(secs => $3), driver = null, driver_until = null
             where id = $1",
            &[&turn.id, &by, &secs(limits::STOP_TIMEOUT)],
        )
        .await?;
        command(t, id, "interrupt", Some(turn.id), &json!({})).await?;
        t.execute("update conversations set run_state = 'stopping' where id = $1", &[&id])
            .await?;
        changes::upserted(t, &person.email, id).await?;
        Ok(CancelResponse {
            turn_id: Some(turn.id),
        })
    })
    .await
}

fn already_resolved() -> Error {
    Error::code(ErrorCode::AlreadyResolved, "the ask is already answered")
}

/// M11: queues `answer_ask` for the supervisor; `ask.resolved` follows its report. The ask of a parked turn
/// (DECISIONS 17) is answered here, and its last answer admits the turn again: its id is returned, for its driver.
///
/// # Errors
/// 403 `view_as_read_only`, 404 (no such ask), 409 `already_resolved` / `conversation_migrating`; a statement error.
pub async fn answer(
    pool: &Pool,
    person: &Person,
    id: Uuid,
    ask_id: &str,
    resp: AgentPermissionResponse,
) -> Result<Option<Uuid>> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<Option<Uuid>> {
        let c = lock(t, &person.email, id).await?;
        c.refuse_migrating()?;
        let Some(pending) = t
            .query_opt(
                "select turn_id from events where conversation_id = $1 and kind = 'ask.pending' and data->>'askId' = $2",
                &[&id, &ask_id],
            )
            .await?
        else {
            return Err(Error::not_found());
        };
        let ask_turn: Option<Uuid> = pending.get(0);
        let done = t
            .query_opt(
                "select 1 from events where conversation_id = $1 and kind = 'ask.resolved' and data->>'askId' = $2
                 union all
                 select 1 from commands where conversation_id = $1 and type = 'answer_ask'
                   and payload->>'askId' = $2 and canceled_at is null
                 limit 1",
                &[&id, &ask_id],
            )
            .await?;
        if done.is_some() {
            return Err(already_resolved());
        }
        if let Some(turn) = active(t, id)
            .await?
            .filter(|a| a.state == State::Parked && Some(a.id) == ask_turn)
        {
            let resumed = super::parked::answer(t, &person.email, &turn, ask_id, resp.clone()).await?;
            return Ok(resumed.then_some(turn.id));
        }
        let allow = resp.behavior == Behavior::Allow;
        let cmd = AnswerAsk {
            ask_id: ask_id.to_owned(),
            allow,
            updated_input: if allow { resp.updated_input.clone() } else { None },
            message: if allow { None } else { resp.message.clone() },
        };
        let mut payload = serde_json::to_value(&cmd)?;
        if let Some(o) = payload.as_object_mut() {
            o.insert("resolution".to_owned(), serde_json::to_value(&resp)?);
        }
        command(t, id, "answer_ask", None, &payload).await?;
        Ok(None)
    })
    .await
}

/// The person's answer stored with the `answer_ask` command for `ask_id`, if one was queued.
///
/// # Errors
/// When a statement fails.
pub async fn stored_answer(
    db: &impl crate::db::Db,
    conversation: Uuid,
    ask_id: &str,
) -> Result<Option<AgentPermissionResponse>> {
    let row = db
        .query_opt(
            "select payload->'resolution' from commands
             where conversation_id = $1 and type = 'answer_ask' and payload->>'askId' = $2 and canceled_at is null
             order by cmd_seq desc limit 1",
            &[&conversation, &ask_id],
        )
        .await?;
    Ok(row
        .and_then(|r| r.get::<_, Option<Value>>(0))
        .and_then(|v| serde_json::from_value(v).ok()))
}
