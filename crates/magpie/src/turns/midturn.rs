//! A person's message sent while a turn runs (M7 `midTurn`; Deyao, 2026-11-12): the queue is magpie's.
//!
//! It starts no turn: it is kept (`pending_messages`) and shows in the conversation's `pendingMessages` until it
//! reaches Claude Code's conversation.
//!
//! - **A text message** is written into the running turn at once as a `send_message` command, as a schedule's fire
//!   is (`src/schedules/fire.rs`). Claude Code takes it in at its next tool result (a `queued_command` attachment in
//!   its transcript), or answers it in a run of its own right after the turn's reply (a `user` line). That prompt
//!   line, known by its text, is where the message shows on the timeline, as a `user` row with the app's id of the
//!   message ([`taken`]). By its text, not by an id on the line: a supervisor of any version writes the message the
//!   same way, so a sandbox started before this keeps working.
//!   Behind a held message (below) it waits too, so messages arrive in the order they were sent.
//! - **A message with attachments** cannot be written into a running turn (its files go into the workspace when a
//!   turn starts): it is held (`redeliver`, its request kept) and starts a turn of its own once the conversation is
//!   idle. While it is held the person can take it back ([`withdraw`]).
//!
//! A text message its turn never took is sent again ([`settle`], [`redeliver`], on the job leader every
//! [`EVERY`]): the supervisor said it did not write it (the turn was being stopped, or had ended), the turn
//! ended with the command unacknowledged, or it was written and then lost with its Claude Code (the conversation
//! has been idle for [`LOST_AFTER`] and no prompt line with its text came). Sent again it joins the turn that runs
//! by then, or, in an idle conversation, is admitted as a turn of its own exactly as a typed message is. Held
//! messages start in the order they were sent, one turn each.
//!
//! An admin's view-as send is refused as before (409 `turn_running`): the view-as log records a send by the event
//! it made, and a waiting message makes none until it is taken in.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aviary_pg::{Leader, Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::conversations::{Conversation, PendingMessage};
use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::SendMessage;
use magpie_api::turns::{SendRequest, SendResponse, TurnStarted};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::driver::{self, Driver};
use super::{active, admission, command, secs};
use crate::changes;
use crate::conversations::{Person, ensure_person, load_owned, lock};
use crate::db::Db;
use crate::error::{Error, Result};

/// How often the job leader settles and sends again (a held message starts within this of its turn's end).
pub const EVERY: Duration = Duration::from_secs(1);

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

/// Queues `m` behind the conversation's running turn (the caller holds the conversation's row lock and has found
/// it busy): a text message is written into the turn, one with attachments is held until the turn has ended.
/// `None` when it cannot wait: an admin sends it viewing as the person, or no turn row is active (the caller
/// answers 409 `turn_running`).
///
/// # Errors
/// When a statement fails.
pub async fn join(tx: &Tx<'_>, conversation: Uuid, person: &Person, m: &SendRequest) -> Result<Option<SendResponse>> {
    if person.viewer.is_some() {
        return Ok(None);
    }
    let Some(turn) = active(tx, conversation).await?.filter(|t| !t.state.is_ended()) else {
        return Ok(None);
    };
    // the order they were sent in is the order they arrive in: behind a held message (one with attachments, which
    // waits for the turn to end) a text message waits too, and joins the turn that message starts (`pass` sends
    // each conversation's oldest waiting message first)
    let behind_held: bool = tx
        .query_one(
            "select exists (select 1 from pending_messages
               where conversation_id = $1 and state = 'redeliver' and request is not null)",
            &[&conversation],
        )
        .await?
        .get(0);
    if m.images.is_empty() && m.files.is_empty() && behind_held {
        tx.execute(
            "insert into pending_messages (conversation_id, client_message_id, text, state, turn_id)
             values ($1, $2, $3, 'redeliver', $4)",
            &[&conversation, &m.client_message_id, &m.text, &turn.id],
        )
        .await?;
    } else if m.images.is_empty() && m.files.is_empty() {
        let seq = write(tx, conversation, turn.id, &m.text).await?;
        tx.execute(
            "insert into pending_messages (conversation_id, client_message_id, text, state, turn_id, cmd_seq)
             values ($1, $2, $3, 'pending', $4, $5)",
            &[&conversation, &m.client_message_id, &m.text, &turn.id, &seq],
        )
        .await?;
    } else {
        tx.execute(
            "insert into pending_messages (conversation_id, client_message_id, text, state, turn_id, request)
             values ($1, $2, $3, 'redeliver', $4, $5)",
            &[
                &conversation,
                &m.client_message_id,
                &m.text,
                &turn.id,
                &serde_json::to_value(m)?,
            ],
        )
        .await?;
    }
    changes::upserted(tx, &person.email, conversation).await?;
    Ok(Some(SendResponse {
        turn: TurnStarted {
            id: turn.id,
            started_at: Timestamp(turn.created_at),
        },
        mid_turn: true,
    }))
}

/// Fills `pending_messages` of `convs`: the messages still waiting, oldest first.
///
/// # Errors
/// When a statement fails.
pub async fn attach(db: &impl Db, convs: &mut [Conversation]) -> Result<()> {
    let ids: Vec<Uuid> = convs.iter().map(|c| c.id).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = db
        .query(
            &format!(
                "select conversation_id, client_message_id, text, created_at, state, request
                 from pending_messages
                 where conversation_id = any($1) and state in {OPEN}
                 order by conversation_id, created_at, client_message_id"
            ),
            &[&ids],
        )
        .await?;
    let mut by_conv: HashMap<Uuid, Vec<PendingMessage>> = HashMap::new();
    for r in rows {
        let request: Option<SendRequest> = r
            .get::<_, Option<serde_json::Value>>(5)
            .and_then(|v| serde_json::from_value(v).ok());
        let attachments = request.as_ref().map_or(0, |m| m.images.len() + m.files.len());
        by_conv.entry(r.get(0)).or_default().push(PendingMessage {
            client_message_id: r.get(1),
            text: r.get(2),
            attachments: u32::try_from(attachments).unwrap_or(u32::MAX),
            sent_at: Timestamp(r.get::<_, OffsetDateTime>(3)),
            held: r.get::<_, String>(4) == "redeliver",
        });
    }
    for c in convs {
        if let Some(p) = by_conv.remove(&c.id) {
            c.pending_messages = p;
        }
    }
    Ok(())
}

/// Takes back the person's held message `client_message_id` (it has not been written into a turn and has started
/// none): it is gone, as if never sent.
///
/// # Errors
/// 403 `view_as_read_only`, 404 `not_found` (no such waiting message), 409 `bad_transition` (it is on its way into
/// the running turn); a statement error.
pub async fn withdraw(pool: &Pool, person: &Person, id: Uuid, client_message_id: &str) -> Result<Conversation> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        lock(t, &person.email, id).await?;
        let state: Option<String> = t
            .query_opt(
                &format!(
                    "select state from pending_messages
                     where conversation_id = $1 and client_message_id = $2 and state in {OPEN}"
                ),
                &[&id, &client_message_id],
            )
            .await?
            .map(|r| r.get(0));
        match state.as_deref() {
            None => return Err(Error::not_found()),
            Some("redeliver") => {}
            Some(_) => {
                return Err(Error::code(
                    ErrorCode::BadTransition,
                    "the message is on its way into the running turn",
                ));
            }
        }
        t.execute(
            "delete from pending_messages where conversation_id = $1 and client_message_id = $2",
            &[&id, &client_message_id],
        )
        .await?;
        changes::upserted(t, &person.email, id).await?;
        Ok(())
    })
    .await?;
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
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
    // only a message written into a turn: a held one has reached no Claude Code
    let row = tx
        .query_opt(
            &format!(
                "update pending_messages set state = 'taken', at = now()
                 where conversation_id = $1 and client_message_id = (
                   select client_message_id from pending_messages
                   where conversation_id = $1 and text = $2 and state in {OPEN} and request is null
                   order by created_at, client_message_id limit 1)
                 returning client_message_id"
            ),
            &[&conversation, &text],
        )
        .await?;
    if let Some(r) = row {
        return Ok(Some(r.get(0)));
    }
    taken_together(tx, conversation, text).await
}

/// Claude Code takes every message that waits when it next looks as ONE prompt, their texts joined by a newline in
/// the order they were sent. `text` read as such a line: the waiting messages, oldest first, whose texts make it up
/// are all taken in; the id is the first one's (the line's one row shows under it). `None` when the line is not made
/// of waiting messages.
async fn taken_together(tx: &Tx<'_>, conversation: Uuid, text: &str) -> Result<Option<String>> {
    if !text.contains('\n') {
        return Ok(None);
    }
    let waiting = tx
        .query(
            &format!(
                "select client_message_id, text from pending_messages
                 where conversation_id = $1 and state in {OPEN} and request is null
                 order by created_at, client_message_id"
            ),
            &[&conversation],
        )
        .await?;
    let mut rest = text;
    let mut ids: Vec<String> = Vec::new();
    for r in &waiting {
        let t: String = r.get(1);
        if t.is_empty() {
            continue;
        }
        if rest == t {
            ids.push(r.get(0));
            rest = "";
            break;
        }
        // (a message that is not this line's next part still waits: an older one never written, say)
        if let Some(after) = rest.strip_prefix(t.as_str()).and_then(|a| a.strip_prefix('\n')) {
            ids.push(r.get(0));
            rest = after;
        }
    }
    if !rest.is_empty() || ids.len() < 2 {
        return Ok(None);
    }
    tx.execute(
        "update pending_messages set state = 'taken', at = now()
         where conversation_id = $1 and client_message_id = any($2)",
        &[&conversation, &ids],
    )
    .await?;
    Ok(ids.into_iter().next())
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

/// Starts the leader's loop: a pass every `every` ([`EVERY`] in production) while this replica leads.
#[must_use]
pub fn spawn(leader: Arc<Leader>, driver: Driver, every: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !leader.confirm().await {
                continue;
            }
            match pass(&driver).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(job = "midturn", sent = n, "job pass"),
                Err(e) => tracing::warn!(job = "midturn", error = %e, "job pass failed"),
            }
        }
    })
}

/// One pass: settle, then send again what must be. How many messages it sent again.
///
/// # Errors
/// A statement error (the next pass goes on).
pub async fn pass(driver: &Driver) -> Result<usize> {
    settle(&driver.pool).await?;
    // each conversation's oldest message that must be sent again (the later ones wait for it: one turn each)
    let due: Vec<(Uuid, String)> = driver
        .pool
        .get()
        .await?
        .query(
            "select distinct on (conversation_id) conversation_id, client_message_id from pending_messages
             where state = 'redeliver' order by conversation_id, created_at, client_message_id",
            &[],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let mut n = 0;
    for (conversation, client_message_id) in due {
        if let Some(turn) = redeliver(&driver.pool, conversation, &client_message_id).await? {
            n += 1;
            if let Some(t) = turn {
                driver::spawn(driver.clone(), t);
            }
        }
    }
    Ok(n)
}

/// Sends the waiting message again: a text message into the turn that runs now, any message as a turn of its own
/// in an idle conversation (`Some(Some(turn))`: the caller starts its driver). `None`: it goes on waiting (a
/// message with attachments while a turn runs; a conversation busy without a turn row).
///
/// # Errors
/// When a statement fails.
async fn redeliver(pool: &Pool, conversation: Uuid, client_message_id: &str) -> Result<Option<Option<Uuid>>> {
    aviary_pg::tx!(pool, |t| -> Result<Option<Option<Uuid>>> {
        let Some(r) = t
            .query_opt(
                "select p.text, p.request, c.person
                 from pending_messages p join conversations c on c.id = p.conversation_id
                 where p.conversation_id = $1 and p.client_message_id = $2 and p.state = 'redeliver'
                 for update of p skip locked",
                &[&conversation, &client_message_id],
            )
            .await?
        else {
            return Ok(None);
        };
        let text: String = r.get(0);
        let request: Option<SendRequest> = r
            .get::<_, Option<serde_json::Value>>(1)
            .and_then(|v| serde_json::from_value(v).ok());
        let person: String = r.get(2);
        let set = |state: &'static str, turn: Option<Uuid>, cmd: Option<i64>| async move {
            t.execute(
                "update pending_messages
                 set state = $3, turn_id = coalesce($4, turn_id), cmd_seq = $5, at = now()
                 where conversation_id = $1 and client_message_id = $2",
                &[&conversation, &client_message_id, &state, &turn, &cmd],
            )
            .await
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
            changes::upserted(t, &person, conversation).await?;
            return Ok(Some(None));
        }
        if let Some(turn) = active(t, conversation).await?.filter(|a| !a.state.is_ended()) {
            if request.is_some() {
                // attachments: it waits for the turn to end
                return Ok(None);
            }
            // a turn runs by now: the text joins it
            let seq = write(t, conversation, turn.id, &text).await?;
            set("pending", Some(turn.id), Some(seq)).await?;
            changes::upserted(t, &person, conversation).await?;
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
        let req = request.unwrap_or_else(|| SendRequest {
            client_message_id: client_message_id.to_owned(),
            text,
            images: Vec::new(),
            files: Vec::new(),
        });
        // admit() tells the change feed (the conversation without this message in its queue, with its turn)
        let started = match admission::admit(t, conversation, &sender, &req).await {
            Ok(s) => s,
            Err(e) if e.is(&ErrorCode::UnknownUpload) => {
                // its uploads are gone (retention): nothing is left to send
                set("dropped", None, None).await?;
                changes::upserted(t, &person, conversation).await?;
                return Ok(Some(None));
            }
            Err(e) => return Err(e),
        };
        set("turn", Some(started.id), None).await?;
        // once more, now that it has left the queue
        changes::upserted(t, &person, conversation).await?;
        tracing::info!(conversation = %conversation, turn = %started.id, "a waiting message became a turn of its own");
        Ok(Some(Some(started.id)))
    })
    .await
}
