//! An operator has a conversation go on from where its last turn was cut off (`POST
//! /v1/admin/conversations/{id}/continue`).
//!
//! A model keeps no state: what a turn has done is the harness's transcript, and a turn that failed after its tool
//! results were written (the model's provider refused the request that carried them, the sandbox was lost) can be
//! taken up there. The harness does that itself, with no message from anyone: Qwen Code's `continue_last_turn`
//! sends the tool results again (or closes the tool calls nothing answered), and its reply continues the same
//! work. What was missing is the way to ask for it from outside the sandbox, and a sandbox to ask.
//!
//! [`continue_last`] is that way:
//! 1. **The turn to go on from** is the newest failed turn that wrote something (an assistant or a tool result
//!    line). The failed person turns after it that wrote nothing are messages that got no reply ("continue", sent
//!    into a conversation that failed every message): they are **rolled back**. With no failed turn that wrote
//!    anything, the oldest of those unanswered messages is the one to go on from (the harness sends it again) and
//!    only the ones after it are rolled back; a message that never reached the harness (its turn failed before
//!    the supervisor had it) is nothing to go on from.
//! 2. **Rolled back** means gone from the timeline, as a transcript line that was rewritten is: one `retract`
//!    event names every event of those turns (their `user.sent`, `turn.started`, `turn.ended`, the sandbox's), and
//!    the failure of the turn that goes on (its `turn.ended`), unless that end closed a tool row that was still
//!    running. The events themselves stay (the admin's events, the audit).
//! 3. **The continuation** is a turn of origin `agent` (the harness goes on by itself: there is no message and no
//!    `user.sent`), admitted as a person's turn is: `start_turn` with `continueLast`, in the harness and session the
//!    conversation last ran, and the driver gets it a sandbox. The supervisor hands the harness its
//!    `continue_last_turn` instead of a message; a harness with nothing to go on from fails the turn.
//!
//! Refused (409 `nothing_to_continue`): the last turn did not fail, or the conversation last ran in a harness that
//! cannot do this (Claude Code). 409 `turn_running` while a turn runs or background tasks are held.

use aviary_pg::{Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::errors::ErrorCode;
use magpie_api::events::{EventBody, Retract, TurnStartedData};
use magpie_api::ops::ContinuedTurn;
use magpie_api::sandbox::{Resume, StartTurn};
use magpie_api::turns::{TurnOrigin, TurnPhase, TurnStarted};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{command, secs, turn_change};
use crate::changes;
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;

/// How many of the newest turns are looked at: more unanswered messages in a row than this are not rolled back.
const LOOK_BACK: i64 = 50;

/// A trailing failed turn.
struct Failed {
    id: Uuid,
    end_seq: Option<i64>,
    /// Whether its message reached the harness (the supervisor reported it running).
    started: bool,
}

fn nothing(why: &str) -> Error {
    Error::code(ErrorCode::NothingToContinue, why)
}

/// The turn to go on from and the turns to roll back (oldest first), from the conversation's newest turns.
async fn plan(tx: &Tx<'_>, conversation: Uuid) -> Result<(Failed, Vec<Failed>)> {
    let rows = tx
        .query(
            "select t.id, t.state, t.origin, t.end_seq, t.started_at is not null,
                    exists (select 1 from events e
                            where e.conversation_id = t.conversation_id and e.turn_id = t.id
                              and e.kind in ('cc.assistant', 'cc.user')
                              and not exists (select 1 from retracted r
                                              where r.conversation_id = e.conversation_id and r.seq = e.seq))
             from turns t where t.conversation_id = $1
             order by t.created_at desc, t.id desc limit $2",
            &[&conversation, &LOOK_BACK],
        )
        .await?;
    let mut unanswered = Vec::new();
    let mut from = None;
    for r in &rows {
        if r.get::<_, String>(1) != "failed" {
            break;
        }
        let turn = Failed {
            id: r.get(0),
            end_seq: r.get(3),
            started: r.get(4),
        };
        let wrote: bool = r.get(5);
        if wrote || r.get::<_, String>(2) != "person" {
            from = Some(turn);
            break;
        }
        unanswered.push(turn);
    }
    // newest first so far
    unanswered.reverse();
    let from = match from {
        Some(t) => t,
        None if unanswered.is_empty() => return Err(nothing("the conversation's last turn did not fail")),
        // no failed turn got anywhere: the first unanswered message is what the harness sends again, when it has it
        None if unanswered[0].started => unanswered.remove(0),
        None => return Err(nothing("the message that failed never reached the harness")),
    };
    Ok((from, unanswered))
}

/// The events the rollback takes out of the timeline.
async fn rolled_back_seqs(tx: &Tx<'_>, conversation: Uuid, from: &Failed, back: &[Failed]) -> Result<Vec<i64>> {
    let ids: Vec<Uuid> = back.iter().map(|t| t.id).collect();
    let mut seqs: Vec<i64> = tx
        .query(
            "select seq from events where conversation_id = $1 and turn_id = any($2) order by seq",
            &[&conversation, &ids],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    if let Some(end) = from.end_seq {
        // its failure goes too, unless that end closed a tool row that was running: taking it back would leave the
        // row running for good (the continuation's own end closes only its own rows)
        let closed_a_tool = tx
            .query_opt(
                "select 1 from rows where conversation_id = $1 and kind = 'tool' and source_seqs @> array[$2]::bigint[]
                 limit 1",
                &[&conversation, &end],
            )
            .await?
            .is_some();
        if !closed_a_tool {
            seqs.push(end);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

/// What the continuation runs in: the harness and session that hold the turn that was cut off, and the
/// conversation's picked model and thinking level.
struct Run {
    harness: magpie_api::Harness,
    session: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
}

/// Admits the continuation: a turn of origin `agent` with no message, as [`super::admission::admit`] admits a
/// person's (its `turn.started`, the row, `start_turn` with `continueLast`, the conversation's run state, the
/// changes).
async fn admit(tx: &Tx<'_>, id: Uuid, person: &str, run: Run) -> Result<TurnStarted> {
    let now = OffsetDateTime::now_utc();
    let turn_id = Uuid::now_v7();
    let started = EventBody::TurnStarted(TurnStartedData {
        turn_id,
        origin: TurnOrigin::Agent,
    });
    let mut ev = NewEvent::of(&started)?.turn(Some(turn_id));
    ev.at = now;
    let start_seq = events::append(tx, id, ev).await?;
    tx.execute(
        "insert into turns (id, conversation_id, origin, state, start_seq, created_at, deadline, model)
         values ($1, $2, 'agent', 'admitted', $3, $4, $4::timestamptz + make_interval(secs => $5::float8), $6)",
        &[
            &turn_id,
            &id,
            &start_seq,
            &now,
            &secs(limits::ADMIT_TIMEOUT),
            &run.model,
        ],
    )
    .await?;
    let payload = StartTurn {
        text: String::new(),
        files: Vec::new(),
        images: Vec::new(),
        // the harness and the session that hold the turn that was cut off
        resume: Resume {
            provider_session_id: run.session,
        },
        sent_at: Timestamp(now),
        answers: Vec::new(),
        model: run.model.clone(),
        thinking: run.thinking,
        harness: run.harness,
        carry_over: None,
        continue_last: true,
    };
    let cmd = command(tx, id, "start_turn", Some(turn_id), &serde_json::to_value(&payload)?).await?;
    tx.execute("update turns set start_cmd_seq = $2 where id = $1", &[&turn_id, &cmd])
        .await?;
    tx.execute(
        "update conversations set run_state = 'admitted', active_turn = $2, activity_at = $3, sandbox_phase = null
         where id = $1",
        &[&id, &turn_id, &now],
    )
    .await?;
    turn_change(tx, person, id, turn_id, TurnPhase::Started, TurnOrigin::Agent, None).await?;
    changes::upserted(tx, person, id).await?;
    Ok(TurnStarted {
        id: turn_id,
        started_at: Timestamp(now),
    })
}

async fn continue_in(tx: &Tx<'_>, id: Uuid, admin: &str) -> Result<ContinuedTurn> {
    let c = tx
        .query_opt(
            "select person, deleted_at is not null, closed_reason, run_state, harness, provider_session_id,
                    picked_model, picked_thinking
             from conversations where id = $1 for update",
            &[&id],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let person: String = c.get(0);
    if c.get::<_, bool>(1) {
        return Err(Error::code(
            ErrorCode::ConversationDeleted,
            "the conversation is in Recently deleted",
        ));
    }
    if let Some(why) = c.get::<_, Option<String>>(2) {
        return Err(Error::code(ErrorCode::ConversationClosed, why));
    }
    if c.get::<_, String>(3) != "idle" {
        return Err(Error::code(
            ErrorCode::TurnRunning,
            "a turn is running or background tasks are held",
        ));
    }
    let harness = magpie_api::Harness::parse(c.get(4)).unwrap_or_default();
    if c.get::<_, Option<String>>(5).is_none() {
        return Err(nothing("no harness has run in the conversation yet"));
    }
    if harness != magpie_api::Harness::Qwen {
        return Err(nothing(
            "the conversation last ran in a harness that cannot go on from a turn that was cut off",
        ));
    }
    let (from, back) = plan(tx, id).await?;
    let seqs = rolled_back_seqs(tx, id, &from, &back).await?;
    if !seqs.is_empty() {
        let body = EventBody::Retract(Retract {
            seqs: seqs.iter().filter_map(|s| u64::try_from(*s).ok()).collect(),
        });
        events::append(tx, id, NewEvent::of(&body)?).await?;
    }

    let turn = admit(
        tx,
        id,
        &person,
        Run {
            harness,
            session: c.get(5),
            model: c.get(6),
            thinking: c.get(7),
        },
    )
    .await?;
    tracing::info!(
        conversation = %id,
        turn = %turn.id,
        from = %from.id,
        rolled_back = back.len(),
        admin = %admin,
        "an operator's continue: the conversation goes on from the turn that was cut off"
    );
    Ok(ContinuedTurn {
        turn,
        rolled_back: back.iter().map(|t| t.id).collect(),
    })
}

/// The admin's continue of conversation `id` (see the module). The caller starts the turn's driver.
///
/// # Errors
/// 404, 409 `conversation_deleted` / `conversation_closed` / `turn_running` / `nothing_to_continue`; a statement
/// error.
pub async fn continue_last(pool: &Pool, id: Uuid, admin: &str) -> Result<ContinuedTurn> {
    aviary_pg::tx!(pool, |t| -> Result<ContinuedTurn> { continue_in(t, id, admin).await }).await
}
