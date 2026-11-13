//! The one way a turn ends (magpie 8.1-8.8): every end (a supervisor report, a heartbeat's reconciliation, a cancel,
//! a forced stop, the driver's failure, the watchdog, hermit's feed) goes through [`end`], in the caller's
//! transaction under the conversation row lock.
//!
//! It appends `turn.ended` (whose projection makes the end rows, magpie 7.3 rule 8), records the outcome on the turn,
//! cancels the turn's unacknowledged commands (a later supervisor of the conversation must not run an ended turn's
//! `start_turn` or `interrupt`; a `kill` this end queues is kept, and served only to the supervisor of the turn's
//! sandbox, [`super::commands`]), moves the conversation to `idle` (or `held`),
//! and writes the `turn` and `conversation_upserted` changes (with `sandbox {ready}` when the preparing chip was
//! showing).

use magpie_api::changes::ChangeBody;
use magpie_api::conversations::{BackgroundTask, SandboxPhase};
use magpie_api::errors::ErrorInfo;
use magpie_api::events::{EventBody, TurnEnded};
use magpie_api::sandbox::{KillCommand, KillReason};
use magpie_api::turns::{TurnOutcome, TurnPhase};
use serde_json::Value;

use super::{State, Turn, command, turn_change};
use crate::changes;
use crate::error::Result;
use crate::events::{self, NewEvent};
use aviary_pg::Tx;

/// How a turn ends.
#[derive(Debug, Clone)]
pub struct Ending {
    /// The outcome.
    pub outcome: TurnOutcome,
    /// The error of a failed turn (and of a forced stop).
    pub error: Option<ErrorInfo>,
    /// Background tasks that outlive the turn (`held`); empty or `None`: `idle`.
    pub held: Option<Vec<BackgroundTask>>,
    /// The supervisor's token counts (informational).
    pub usage: Option<Value>,
    /// The supervisor's duration (informational).
    pub duration_ms: Option<i64>,
    /// A `kill` to queue for the supervisor (a forced stop, the watchdog).
    pub kill: Option<KillReason>,
}

impl Ending {
    /// A failure with `error`.
    #[must_use]
    pub fn failed(error: ErrorInfo) -> Self {
        Self {
            outcome: TurnOutcome::Failed,
            error: Some(error),
            held: None,
            usage: None,
            duration_ms: None,
            kill: None,
        }
    }

    /// A cancel (no error).
    #[must_use]
    pub fn canceled() -> Self {
        Self {
            outcome: TurnOutcome::Canceled,
            error: None,
            held: None,
            usage: None,
            duration_ms: None,
            kill: None,
        }
    }

    /// With a `kill` command.
    #[must_use]
    pub fn kill(mut self, reason: KillReason) -> Self {
        self.kill = Some(reason);
        self
    }
}

fn state_of(o: TurnOutcome) -> State {
    match o {
        TurnOutcome::Completed => State::Completed,
        TurnOutcome::Failed => State::Failed,
        TurnOutcome::Canceled => State::Canceled,
    }
}

fn phase_of(o: TurnOutcome) -> TurnPhase {
    match o {
        TurnOutcome::Completed => TurnPhase::Completed,
        TurnOutcome::Failed => TurnPhase::Failed,
        TurnOutcome::Canceled => TurnPhase::Canceled,
    }
}

/// Ends `turn` (not ended yet; its conversation is locked by the caller, owned by `person`).
///
/// # Errors
/// When a statement fails.
pub async fn end(tx: &Tx<'_>, person: &str, turn: &Turn, ending: Ending) -> Result<()> {
    let state = state_of(ending.outcome);
    let body = EventBody::TurnEnded(TurnEnded {
        turn_id: turn.id,
        outcome: ending.outcome,
        error: ending.error.clone(),
    });
    let seq = events::append(tx, turn.conversation, NewEvent::of(&body)?.turn(Some(turn.id))).await?;
    let code = ending.error.as_ref().map(|e| e.code.as_str().to_owned());
    let detail = ending
        .error
        .as_ref()
        .map(|e| e.detail.clone().unwrap_or_else(|| e.message.clone()));
    tx.execute(
        "update turns set state = $2, ended_at = now(), end_seq = $3, error_code = $4, error_detail = $5,
                usage = coalesce($6, usage), duration_ms = coalesce(coalesce(parked_ms, 0) + $7, duration_ms, parked_ms), deadline = null,
                driver = null, driver_until = null
         where id = $1",
        &[
            &turn.id,
            &state.as_str(),
            &seq,
            &code,
            &detail,
            &ending.usage,
            &ending.duration_ms,
        ],
    )
    .await?;
    // the window Claude Code ran the conversation's model at (the supervisor's usage names it): the app's meter
    if let Some(window) = ending
        .usage
        .as_ref()
        .and_then(|u| u.get("contextWindow"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|w| i32::try_from(w).ok())
        .filter(|w| *w > 0)
    {
        tx.execute(
            "update conversations set context_window = $2 where id = $1 and context_window is distinct from $2",
            &[&turn.conversation, &window],
        )
        .await?;
    }
    tx.execute(
        "update commands set canceled_at = now()
         where conversation_id = $1 and turn_id = $2 and acked_at is null and canceled_at is null and type <> 'kill'",
        &[&turn.conversation, &turn.id],
    )
    .await?;
    if let Some(reason) = ending.kill {
        command(
            tx,
            turn.conversation,
            "kill",
            Some(turn.id),
            &serde_json::to_value(KillCommand { reason })?,
        )
        .await?;
    }
    let held = ending.held.filter(|t| !t.is_empty());
    let run_state = if held.is_some() { "held" } else { "idle" };
    // the tasks the supervisor no longer holds at the end end their launch rows (rule 7)
    untracked_at_end(tx, turn, held.as_deref().unwrap_or_default()).await?;
    let background = serde_json::to_value(held.unwrap_or_default())?;
    let row = tx
        .query_one(
            "with old as (select sandbox_phase from conversations where id = $1)
             update conversations c set
                active_turn = case when c.active_turn = $2 then null else c.active_turn end,
                run_state = case when c.active_turn = $2 then $3 else c.run_state end,
                background = case when c.active_turn = $2 then $4 else c.background end,
                sandbox_phase = case when c.active_turn = $2 then null else c.sandbox_phase end,
                last_turn = $2,
                activity_at = now()
             from old where c.id = $1
             returning old.sandbox_phase",
            &[&turn.conversation, &turn.id, &run_state, &background],
        )
        .await?;
    if row.get::<_, Option<String>>(0).as_deref() == Some("preparing") {
        changes::emit(
            tx,
            person,
            turn.conversation,
            &ChangeBody::Sandbox {
                state: SandboxPhase::Ready,
            },
        )
        .await?;
    }
    turn_change(
        tx,
        person,
        turn.conversation,
        turn.id,
        phase_of(ending.outcome),
        turn.origin,
        ending.error,
    )
    .await?;
    changes::upserted(tx, person, turn.conversation).await?;
    Ok(())
}

/// [`super::untracked`] for the end of `turn`, while it is its conversation's active turn: the conversation's
/// `background` tasks that `held` no longer lists.
async fn untracked_at_end(tx: &Tx<'_>, turn: &Turn, held: &[BackgroundTask]) -> Result<()> {
    if let Some(r) = tx
        .query_opt(
            "select background from conversations where id = $1 and active_turn = $2",
            &[&turn.conversation, &turn.id],
        )
        .await?
    {
        super::untracked(tx, turn.conversation, Some(turn.id), &r.get(0), held).await?;
    }
    Ok(())
}
