//! A person's message sent while a turn runs (M7 `midTurn`; Deyao, 2026-11-12).
//!
//! It starts no turn: it is kept (`pending_messages`) and written into the running turn at once as a `send_message`
//! command, as a schedule's fire is (`src/schedules/fire.rs`). Claude Code takes it in at its next tool result (a
//! `queued_command` attachment in its transcript), or answers it in a run of its own right after the turn's reply (a
//! `user` line). That prompt line, known by its text, is where the message shows on the timeline, as a `user` row
//! with the app's id of the message ([`taken`]; the app shows the message from the moment it was sent, and this row
//! takes its place). By its text, not by an id on the line: a supervisor of any version writes the message the same
//! way, so a sandbox started before this keeps working.
//!
//! A message its turn never took is sent again ([`settle`], [`redeliver_one`], on the job leader with the
//! schedules' pass): the supervisor said it did not write it (the turn was being stopped, or had ended), the turn
//! ended with the command unacknowledged, or it was written and then lost with its Claude Code (the conversation
//! has been idle for [`LOST_AFTER`] and no prompt line with its text came). Sent again it joins the turn that runs by then,
//! or, in an idle conversation, is admitted as a turn of its own exactly as a typed message is.
//!
//! Only a text message joins a running turn. One with attachments is refused as before (409 `turn_running`), and so
//! is one sent by an admin viewing as the person (the view-as log records a send by the event it made, and a
//! mid-turn message makes none until Claude Code takes it in).

use std::time::Duration;

use aviary_pg::{Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::SendMessage;
use magpie_api::turns::{SendRequest, SendResponse, TurnStarted};
use uuid::Uuid;

use super::{active, admission, command, secs};
use crate::conversations::{Person, ensure_person, lock};
use crate::error::Result;

/// A written message no transcript line has shown this long after the conversation went idle was lost with its
/// Claude Code (it would have started the message within a moment of its reply): it is sent again.
pub const LOST_AFTER: Duration = Duration::from_secs(30);

/// The states in which a message still waits to be taken in.
const OPEN: &str = "('pending', 'written', 'redeliver')";

/// The answer to a repeated send of a mid-turn message (a message that became a turn of its own is found by the
/// turn's `client_message_id` before this).
///
/// # Errors
/// When a statement fails.
pub async fn repeat(tx: &Tx<'_>, conversation: Uuid, client_message_id: &str) -> Result<Option<SendResponse>> {
    let row = tx
        .query_opt(
            "select p.turn_id, t.created_at from pending_messages p join turns t on t.id = p.turn_id
             where p.conversation_id = $1 and p.client_message_id = $2",
            &[&conversation, &client_message_id],
        )
        .await?;
    Ok(row.map(|r| SendResponse {
        turn: TurnStarted {
            id: r.get(0),
            started_at: Timestamp(r.get(1)),
        },
        mid_turn: true,
    }))
}

/// Joins `m` to the conversation's running turn (the caller holds the conversation's row lock and has found it
/// busy). `None` when it cannot join: it has attachments, an admin sends it viewing as the person, or no turn row is
/// active (the caller answers 409 `turn_running`).
///
/// # Errors
/// When a statement fails.
pub async fn join(tx: &Tx<'_>, conversation: Uuid, person: &Person, m: &SendRequest) -> Result<Option<SendResponse>> {
    if !m.images.is_empty() || !m.files.is_empty() || person.viewer.is_some() {
        return Ok(None);
    }
    let Some(turn) = active(tx, conversation).await?.filter(|t| !t.state.is_ended()) else {
        return Ok(None);
    };
    let seq = write(tx, conversation, turn.id, &m.text).await?;
    tx.execute(
        "insert into pending_messages (conversation_id, client_message_id, text, state, turn_id, cmd_seq)
         values ($1, $2, $3, 'pending', $4, $5)",
        &[&conversation, &m.client_message_id, &m.text, &turn.id, &seq],
    )
    .await?;
    Ok(Some(SendResponse {
        turn: TurnStarted {
            id: turn.id,
            started_at: Timestamp(turn.created_at),
        },
        mid_turn: true,
    }))
}

/// Queues the `send_message` that writes the message into `turn`.
async fn write(tx: &Tx<'_>, conversation: Uuid, turn: Uuid, text: &str) -> Result<i64> {
    let payload = SendMessage { text: text.to_owned() };
    command(
        tx,
        conversation,
        "send_message",
        Some(turn),
        &serde_json::to_value(&payload)?,
    )
    .await
}

/// A prompt line of the transcript with the text of a waiting message is that message taken in (the oldest such
/// one): it waits no more. The app's id of the message (`clientMessageId`), which the line's `user` row then
/// carries; `None` when the line is no waiting message's (a schedule's fire, a prompt Claude Code queued itself).
///
/// # Errors
/// When a statement fails.
pub async fn taken(tx: &Tx<'_>, conversation: Uuid, text: &str) -> Result<Option<String>> {
    let row = tx
        .query_opt(
            &format!(
                "update pending_messages set state = 'taken', at = now()
                 where conversation_id = $1 and client_message_id = (
                   select client_message_id from pending_messages
                   where conversation_id = $1 and text = $2 and state in {OPEN}
                   order by created_at, client_message_id limit 1)
                 returning client_message_id"
            ),
            &[&conversation, &text],
        )
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Settles the waiting messages: an acknowledged `send_message` makes its message `written` (delivered) or
/// `redeliver` (the supervisor did not write it), a canceled one `redeliver`; one never acknowledged whose turn has
/// ended is canceled and sent again; a written one whose conversation has been idle for [`LOST_AFTER`] is sent again.
///
/// # Errors
/// When a statement fails.
pub async fn settle(pool: &Pool) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        t.execute(
            "update pending_messages p
             set state = case when m.acked_at is not null and m.result->>'delivered' = 'true' then 'written'
                              else 'redeliver' end,
                 at = now()
             from commands m
             where p.state = 'pending' and m.conversation_id = p.conversation_id and m.cmd_seq = p.cmd_seq
               and (m.acked_at is not null or m.canceled_at is not null)",
            &[],
        )
        .await?;
        t.execute(
            "with gone as (
               update commands m set canceled_at = now()
               from pending_messages p join turns tu on tu.id = p.turn_id
               where p.state = 'pending' and m.conversation_id = p.conversation_id and m.cmd_seq = p.cmd_seq
                 and m.acked_at is null and m.canceled_at is null
                 and tu.state in ('completed', 'failed', 'canceled')
               returning m.conversation_id, m.cmd_seq)
             update pending_messages p set state = 'redeliver', at = now()
             from gone where p.conversation_id = gone.conversation_id and p.cmd_seq = gone.cmd_seq
               and p.state = 'pending'",
            &[],
        )
        .await?;
        t.execute(
            "update pending_messages p set state = 'redeliver', at = now()
             from conversations c
             where p.state = 'written' and c.id = p.conversation_id
               and c.run_state = 'idle' and c.active_turn is null
               and not exists (select 1 from turns l where l.id = c.last_turn
                                and l.ended_at > now() - make_interval(secs => $1::float8))",
            &[&secs(LOST_AFTER)],
        )
        .await?;
        Ok(())
    })
    .await
}

/// Sends again the oldest message that must be: into the turn that runs now, or as a turn of its own in an idle
/// conversation (`Some(Some(turn))`: the caller starts its driver). `None`: nothing to send again in this pass.
///
/// # Errors
/// When a statement fails.
pub async fn redeliver_one(pool: &Pool) -> Result<Option<Option<Uuid>>> {
    aviary_pg::tx!(pool, |t| -> Result<Option<Option<Uuid>>> {
        let Some(r) = t
            .query_opt(
                "select p.conversation_id, p.client_message_id, p.text, c.person
                 from pending_messages p join conversations c on c.id = p.conversation_id
                 where p.state = 'redeliver'
                 order by p.created_at limit 1 for update of p skip locked",
                &[],
            )
            .await?
        else {
            return Ok(None);
        };
        let conversation: Uuid = r.get(0);
        let client_message_id: String = r.get(1);
        let text: String = r.get(2);
        let person: String = r.get(3);
        let set = |state: &'static str, turn: Option<Uuid>, cmd: Option<i64>| {
            let client_message_id = client_message_id.clone();
            async move {
                t.execute(
                    "update pending_messages
                     set state = $3, turn_id = coalesce($4, turn_id), cmd_seq = $5, at = now()
                     where conversation_id = $1 and client_message_id = $2",
                    &[&conversation, &client_message_id, &state, &turn, &cmd],
                )
                .await
            }
        };
        let locked = match lock(t, &person, conversation).await {
            Ok(c) => c,
            Err(e) if e.is(&ErrorCode::NotFound) => {
                set("dropped", None, None).await?;
                return Ok(Some(None));
            }
            Err(e) => return Err(e),
        };
        if locked.deleted_at.is_some() {
            set("dropped", None, None).await?;
            return Ok(Some(None));
        }
        if let Some(turn) = active(t, conversation).await?.filter(|a| !a.state.is_ended()) {
            // a turn runs by now: the message joins it
            let seq = write(t, conversation, turn.id, &text).await?;
            set("pending", Some(turn.id), Some(seq)).await?;
            return Ok(Some(None));
        }
        let run_state: String = t
            .query_one("select run_state from conversations where id = $1", &[&conversation])
            .await?
            .get(0);
        if matches!(run_state.as_str(), "admitted" | "preparing" | "running" | "stopping") {
            // busy without an active turn row: the next pass tries again
            return Ok(None);
        }
        ensure_person(t, &person).await?;
        let sender = Person {
            email: person.clone(),
            viewer: None,
        };
        let req = SendRequest {
            client_message_id: client_message_id.clone(),
            text,
            images: Vec::new(),
            files: Vec::new(),
        };
        let started = admission::admit(t, conversation, &sender, &req).await?;
        set("turn", Some(started.id), None).await?;
        tracing::info!(conversation = %conversation, turn = %started.id, "a mid-turn message became a turn of its own");
        Ok(Some(Some(started.id)))
    })
    .await
}
