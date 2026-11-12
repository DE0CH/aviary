//! Parked turns (DECISIONS 17): a question nobody answers never holds a sandbox.
//!
//! When hermit releases a sandbox whose run waits only on its open questions (`AskUserQuestion`, `ExitPlanMode`; no
//! tool call or background task running), the supervisor reports the turn `parked` ([`park`]) instead of ending it:
//! the turn keeps its place as the conversation's active turn, has no sandbox and no deadline, and its asks stay
//! open (`Conversation.asks`), so the person still sees the question and nothing timed out. The person's answer
//! ([`answer`], M11) is recorded at once (`ask.resolved`); once every question the turn waits on is answered, the
//! turn is admitted again ([`resume`]): a `start_turn` that resumes the conversation and carries the answers, which
//! the new sandbox's supervisor delivers as the turn's next message (a resumed Claude Code no longer takes an answer
//! to its predecessor's question, supervisor 7.6). The driver then gets the turn a sandbox as for any admitted
//! turn. A cancel of a parked turn ends it at once ([`cancel`]): there is nothing to interrupt.

use aviary_pg::Tx;
use magpie_api::Timestamp;
use magpie_api::changes::{AskResolvedChange, ChangeBody};
use magpie_api::events::{AskPending, AskResolved, EventBody};
use magpie_api::sandbox::{ParkedAnswer, Resume, StartTurn, TurnStateReport};
use magpie_api::turns::{AgentPermissionResponse, Behavior};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::end::{self, Ending};
use super::{State, Turn, command, secs};
use crate::changes;
use crate::error::Result;
use crate::events::{self, NewEvent};
use crate::limits;

/// The open asks of `turn`: `(askId, its ask.pending)`, in the order they were asked.
async fn open_asks(tx: &Tx<'_>, conversation: Uuid, turn: Uuid) -> Result<Vec<(String, AskPending)>> {
    let rows = tx
        .query(
            "select e.data from events e
             where e.conversation_id = $1 and e.kind = 'ask.pending' and e.turn_id = $2
               and not exists (select 1 from events r
                               where r.conversation_id = e.conversation_id and r.kind = 'ask.resolved'
                                 and r.data->>'askId' = e.data->>'askId')
             order by e.seq",
            &[&conversation, &turn],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| serde_json::from_value::<AskPending>(r.get(0)).ok())
        .map(|p| (p.ask_id.clone(), p))
        .collect())
}

/// Records `ask_id`'s answer: `ask.resolved` with the person's response and the `ask_resolved` change.
async fn resolve(
    tx: &Tx<'_>,
    person: &str,
    conversation: Uuid,
    turn: Uuid,
    ask_id: &str,
    resolution: AgentPermissionResponse,
) -> Result<()> {
    let body = EventBody::AskResolved(AskResolved {
        ask_id: ask_id.to_owned(),
        resolution: resolution.clone(),
    });
    events::append(tx, conversation, NewEvent::of(&body)?.turn(Some(turn))).await?;
    changes::emit(
        tx,
        person,
        conversation,
        &ChangeBody::AskResolved(AskResolvedChange {
            ask_id: ask_id.to_owned(),
            resolution,
        }),
    )
    .await?;
    Ok(())
}

/// The supervisor's `parked` report for `turn` (running): no sandbox, no deadline, the asks it waits on kept. A
/// question the person answered while the report was on its way (its `answer_ask` reached a supervisor that had
/// already let the question go) is taken from that command, and once every question is answered the turn resumes at
/// once. A replay is a no-op.
///
/// # Errors
/// 409 `bad_transition` from a state other than `running` (a cancel came first); a statement error.
pub async fn park(tx: &Tx<'_>, person: &str, turn: &Turn, r: &TurnStateReport) -> Result<()> {
    match turn.state {
        State::Parked => return Ok(()),
        State::Running => {}
        s => return Err(magpie_api::errors::Refusal::bad_transition(s.as_str()).into()),
    }
    let asks = open_asks(tx, turn.conversation, turn.id).await?;
    if asks.is_empty() {
        // nothing left to wait on (never expected: the ask's report precedes this one in the turn's lane): the
        // sandbox went with the turn, as before decision 17
        let e = magpie_api::errors::ErrorInfo::new(
            magpie_api::errors::DataCode::SandboxLost,
            "released while waiting on a question magpie does not have",
        );
        return end::end(tx, person, turn, Ending::failed(e)).await;
    }
    let ids: Vec<String> = asks.iter().map(|(id, _)| id.clone()).collect();
    let worked = r.duration_ms.and_then(|d| i64::try_from(d).ok());
    tx.execute(
        "update turns set state = 'parked', sandbox_id = null, ready_at = null, deadline = null, driver = null,
                driver_until = null, parked_asks = $2, parked_ms = coalesce(parked_ms, 0) + coalesce($3::bigint, 0),
                usage = coalesce($4, usage)
         where id = $1",
        &[&turn.id, &ids, &worked, &r.usage.map(serde_json::to_value).transpose()?],
    )
    .await?;
    tracing::info!(turn = %turn.id, asks = ?ids, "turn parked: its sandbox went while it waited only on its questions");
    // an answer queued for a supervisor that no longer holds the question is the person's answer all the same
    for (id, _) in &asks {
        if let Some(a) = super::cancel::stored_answer(tx, turn.conversation, id).await? {
            tx.execute(
                "update commands set canceled_at = now()
                 where conversation_id = $1 and type = 'answer_ask' and payload->>'askId' = $2 and acked_at is null
                   and canceled_at is null",
                &[&turn.conversation, id],
            )
            .await?;
            resolve(tx, person, turn.conversation, turn.id, id, a).await?;
        }
    }
    let parked = Turn {
        state: State::Parked,
        ..turn.clone()
    };
    if open_asks(tx, turn.conversation, turn.id).await?.is_empty() {
        resume(tx, &parked).await?;
    }
    changes::upserted(tx, person, turn.conversation).await?;
    Ok(())
}

/// M11 for a parked turn's ask: the answer is recorded now; the last one resumes the turn (or a deny that asks for
/// an interrupt ends it canceled). `true` when the turn was admitted again (its driver may start).
///
/// # Errors
/// A statement error.
pub async fn answer(
    tx: &Tx<'_>,
    person: &str,
    turn: &Turn,
    ask_id: &str,
    resp: AgentPermissionResponse,
) -> Result<bool> {
    let interrupt = resp.behavior == Behavior::Deny && resp.interrupt == Some(true);
    resolve(tx, person, turn.conversation, turn.id, ask_id, resp).await?;
    if interrupt {
        cancel(tx, person, turn).await?;
        return Ok(false);
    }
    if !open_asks(tx, turn.conversation, turn.id).await?.is_empty() {
        changes::upserted(tx, person, turn.conversation).await?;
        return Ok(false);
    }
    resume(tx, turn).await?;
    changes::upserted(tx, person, turn.conversation).await?;
    Ok(true)
}

/// A cancel of a parked turn: its questions close (canceled) and it ends canceled at once.
///
/// # Errors
/// A statement error.
pub async fn cancel(tx: &Tx<'_>, person: &str, turn: &Turn) -> Result<()> {
    for (id, _) in open_asks(tx, turn.conversation, turn.id).await? {
        resolve(
            tx,
            person,
            turn.conversation,
            turn.id,
            &id,
            AgentPermissionResponse::canceled(),
        )
        .await?;
    }
    // its working time is the part before it was parked (`parked_ms`, end.rs)
    end::end(tx, person, turn, Ending::canceled()).await
}

/// Admits the parked `turn` again with the answers to the questions it waited on: `start_turn` (resume, the
/// answers), a fresh admission deadline counted from now, the answer line's tool calls for the projection (6.5).
async fn resume(tx: &Tx<'_>, turn: &Turn) -> Result<()> {
    let row = tx
        .query_one(
            "select coalesce(t.parked_asks, '{}'), c.provider_session_id, c.picked_model from turns t
             join conversations c on c.id = t.conversation_id where t.id = $1",
            &[&turn.id],
        )
        .await?;
    let ids: Vec<String> = row.get(0);
    let provider_session_id: Option<String> = row.get(1);
    let mut answers = Vec::new();
    for id in &ids {
        let asked = tx
            .query_opt(
                "select data from events where conversation_id = $1 and kind = 'ask.pending' and data->>'askId' = $2",
                &[&turn.conversation, id],
            )
            .await?
            .and_then(|r| serde_json::from_value::<AskPending>(r.get(0)).ok());
        let given = tx
            .query_opt(
                "select data->'resolution' from events
                 where conversation_id = $1 and kind = 'ask.resolved' and data->>'askId' = $2",
                &[&turn.conversation, id],
            )
            .await?
            .and_then(|r| serde_json::from_value::<AgentPermissionResponse>(r.get::<_, Value>(0)).ok());
        let (Some(asked), Some(given)) = (asked, given) else {
            continue;
        };
        let allow = given.behavior == Behavior::Allow;
        answers.push(ParkedAnswer {
            ask_id: id.clone(),
            tool_name: asked.tool_name,
            tool_use_id: asked.tool_use_id,
            input: asked.input,
            allow,
            updated_input: if allow { given.updated_input } else { None },
            message: if allow { None } else { given.message },
        });
    }
    let tool_uses: Vec<String> = answers.iter().filter_map(|a| a.tool_use_id.clone()).collect();
    let now = OffsetDateTime::now_utc();
    let payload = StartTurn {
        text: String::new(),
        files: Vec::new(),
        images: Vec::new(),
        resume: Resume { provider_session_id },
        sent_at: Timestamp(now),
        answers,
        model: row.get(2),
    };
    let cmd = command(
        tx,
        turn.conversation,
        "start_turn",
        Some(turn.id),
        &serde_json::to_value(&payload)?,
    )
    .await?;
    tx.execute(
        "update turns set state = 'admitted', start_cmd_seq = $2, resumed_at = $3, sandbox_id = null, ready_at = null,
                deadline = $3::timestamptz + make_interval(secs => $4::float8), driver = null, driver_until = null,
                parked_asks = null, answer_tool_uses = $5
         where id = $1",
        &[&turn.id, &cmd, &now, &secs(limits::ADMIT_TIMEOUT), &tool_uses],
    )
    .await?;
    tx.execute(
        "update conversations set run_state = 'admitted', sandbox_phase = null where id = $1 and active_turn = $2",
        &[&turn.conversation, &turn.id],
    )
    .await?;
    tracing::info!(turn = %turn.id, "parked turn answered: admitted again");
    Ok(())
}

/// How long the answered questions of each of `turns` (of `conversations`) waited (`ask.pending` to its
/// `ask.resolved`), in ms.
///
/// # Errors
/// A statement error.
pub async fn ask_waits(
    db: &impl crate::db::Db,
    conversations: &[Uuid],
    turns: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, u64>> {
    if turns.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = db
        .query(
            "select e.turn_id, sum(extract(epoch from (r.at - e.at)) * 1000)::bigint from events e
             join events r on r.conversation_id = e.conversation_id and r.kind = 'ask.resolved'
                          and r.data->>'askId' = e.data->>'askId'
             where e.conversation_id = any($1) and e.kind = 'ask.pending' and e.turn_id = any($2)
             group by e.turn_id",
            &[&conversations, &turns],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let ms: Option<i64> = r.get(1);
            (r.get(0), u64::try_from(ms.unwrap_or(0)).unwrap_or(0))
        })
        .collect())
}
