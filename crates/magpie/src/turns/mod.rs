//! Turns (magpie 8): admission ([`admission`]), the driver that gets the turn a sandbox from hermit ([`driver`]), the
//! supervisor's work queue ([`commands`]), its reports ([`reports`]), cancel and ask answers ([`cancel`]), the
//! deadlines and the watchdog ([`sweeper`]), titles ([`titles`]) and the one way a turn ends ([`end`]).
//!
//! The state machine (magpie 8.1): `admitted` → (`preparing`) → `starting` → `running` → (`stopping`) → `completed`
//! | `failed` | `canceled`. Every transition runs in one transaction under the conversation row lock
//! ([`crate::conversations::lock`] or [`caller::lock`]), which serialises the writers of a conversation; the turn's
//! row is only read and written after that lock. The conversation's `run_state` follows its active turn.

pub mod admission;
pub mod caller;
pub mod cancel;
pub mod commands;
pub mod driver;
pub mod end;
pub mod reports;
pub mod sweeper;
pub mod titles;

use std::time::Duration;

use aviary_pg::Tx;
use magpie_api::changes::{ChangeBody, TurnChange};
use magpie_api::errors::ErrorInfo;
use magpie_api::turns::{SendRequest, TurnOrigin, TurnPhase, TurnStarted};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations::Person;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::fanout;

/// The background tasks of `before` (the conversation's `background`) that `after` no longer lists: magpie stops
/// tracking them without the supervisor's terminal report (the turn ended and the supervisor did not hold them, the
/// sandbox went away, the supervisor's heartbeat went idle). Each gets a `task.status` (event `updated`, status
/// `stopped`), so its launch's tool row ends (magpie 7.3 rule 7) instead of showing `running` for ever. A later
/// notification or report of the task still updates the row (it is only the last word so far).
///
/// # Errors
/// When a statement fails.
pub async fn untracked(
    tx: &Tx<'_>,
    conversation: Uuid,
    turn: Option<Uuid>,
    before: &serde_json::Value,
    after: &[magpie_api::conversations::BackgroundTask],
) -> Result<()> {
    use magpie_api::events::{EventBody, TaskEvent, TaskState, TaskStatus};
    let before: Vec<magpie_api::conversations::BackgroundTask> =
        serde_json::from_value(before.clone()).unwrap_or_default();
    for b in before {
        if after.iter().any(|a| a.task_id == b.task_id) {
            continue;
        }
        let body = EventBody::TaskStatus(TaskStatus {
            task_id: b.task_id,
            tool_use_id: b.tool_use_id,
            event: TaskEvent::Updated,
            status: TaskState::Stopped,
            description: b.description,
            summary: None,
            output_file: None,
        });
        crate::events::append(tx, conversation, crate::events::NewEvent::of(&body)?.turn(turn)).await?;
    }
    Ok(())
}

/// Whether a message carries anything to admit (text or attachments, magpie 9.2).
#[must_use]
pub fn has_content(message: &SendRequest) -> bool {
    !message.text.trim().is_empty() || !message.images.is_empty() || !message.files.is_empty()
}

/// Admits `message` as a person turn of `conversation` (locked by the caller, created in this transaction) exactly
/// as M7 does (magpie 8.2), and answers the turn. The replica that created the conversation starts the turn's
/// driver after the commit ([`driver::spawn`]); until it claims the turn, any replica's sweeper may.
///
/// # Errors
/// 400 `unknown_upload` / `too_many_attachments`; a statement error.
pub async fn admit_first(
    tx: &Tx<'_>,
    conversation: Uuid,
    person: &Person,
    message: &SendRequest,
) -> Result<TurnStarted> {
    admission::admit(tx, conversation, person, message).await
}

/// A turn's state (`turns.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Admitted; the driver asks hermit for a sandbox.
    Admitted,
    /// The sandbox was not ready within 1 s (the chip shows).
    Preparing,
    /// hermit said ready; `start_turn` waits for the supervisor.
    Starting,
    /// The supervisor said running.
    Running,
    /// A cancel was sent to the supervisor.
    Stopping,
    /// Ended: the reply finished.
    Completed,
    /// Ended: failed (with a code).
    Failed,
    /// Ended: canceled.
    Canceled,
}

impl State {
    /// From the column.
    #[must_use]
    pub fn of(s: &str) -> Self {
        match s {
            "admitted" => State::Admitted,
            "preparing" => State::Preparing,
            "starting" => State::Starting,
            "running" => State::Running,
            "stopping" => State::Stopping,
            "completed" => State::Completed,
            "canceled" => State::Canceled,
            _ => State::Failed,
        }
    }

    /// The column value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            State::Admitted => "admitted",
            State::Preparing => "preparing",
            State::Starting => "starting",
            State::Running => "running",
            State::Stopping => "stopping",
            State::Completed => "completed",
            State::Failed => "failed",
            State::Canceled => "canceled",
        }
    }

    /// `completed`, `failed` or `canceled`.
    #[must_use]
    pub fn is_ended(self) -> bool {
        matches!(self, State::Completed | State::Failed | State::Canceled)
    }

    /// Still waiting for a sandbox (the driver's states).
    #[must_use]
    pub fn is_waiting(self) -> bool {
        matches!(self, State::Admitted | State::Preparing)
    }
}

/// A turn row, as the transitions need it.
#[derive(Debug, Clone)]
pub struct Turn {
    /// The turn.
    pub id: Uuid,
    /// Its conversation.
    pub conversation: Uuid,
    /// Who started it.
    pub origin: TurnOrigin,
    /// Its state.
    pub state: State,
    /// Admitted at.
    pub created_at: OffsetDateTime,
    /// hermit said ready at.
    pub ready_at: Option<OffsetDateTime>,
    /// Its sandbox (recorded by the driver).
    pub sandbox: Option<Uuid>,
    /// Its `start_turn` command.
    pub start_cmd_seq: Option<i64>,
    /// The current state's deadline.
    pub deadline: Option<OffsetDateTime>,
}

/// The columns [`Turn::from_row`] reads.
pub const TURN_COLUMNS: &str =
    "id, conversation_id, origin, state, created_at, ready_at, sandbox_id, start_cmd_seq, deadline";

fn origin_of(s: &str) -> TurnOrigin {
    match s {
        "agent" => TurnOrigin::Agent,
        "import" => TurnOrigin::Import,
        _ => TurnOrigin::Person,
    }
}

/// The column value of an origin.
#[must_use]
pub fn origin_str(o: TurnOrigin) -> &'static str {
    match o {
        TurnOrigin::Person => "person",
        TurnOrigin::Agent => "agent",
        TurnOrigin::Import => "import",
    }
}

impl Turn {
    /// From a row with [`TURN_COLUMNS`].
    #[must_use]
    pub fn from_row(r: &aviary_pg::Row) -> Self {
        Self {
            id: r.get("id"),
            conversation: r.get("conversation_id"),
            origin: origin_of(r.get("origin")),
            state: State::of(r.get("state")),
            created_at: r.get("created_at"),
            ready_at: r.get("ready_at"),
            sandbox: r.get("sandbox_id"),
            start_cmd_seq: r.get("start_cmd_seq"),
            deadline: r.get("deadline"),
        }
    }
}

/// The turn `id` of `conversation`, if it exists.
///
/// # Errors
/// When a statement fails.
pub async fn load(db: &impl Db, conversation: Uuid, id: Uuid) -> Result<Option<Turn>> {
    let row = db
        .query_opt(
            &format!("select {TURN_COLUMNS} from turns where id = $1 and conversation_id = $2"),
            &[&id, &conversation],
        )
        .await?;
    Ok(row.as_ref().map(Turn::from_row))
}

/// The conversation's active turn (`conversations.active_turn`), if it has one.
///
/// # Errors
/// When a statement fails.
pub async fn active(db: &impl Db, conversation: Uuid) -> Result<Option<Turn>> {
    let row = db
        .query_opt(
            &format!(
                "select {TURN_COLUMNS} from turns
                 where id = (select active_turn from conversations where id = $1)"
            ),
            &[&conversation],
        )
        .await?;
    Ok(row.as_ref().map(Turn::from_row))
}

/// Seconds, for `make_interval(secs => …)`.
#[must_use]
pub fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Sets a running turn's deadline from its last sign of life: the later of `heartbeat_at` (or now, for a fresh
/// report) and `available_since`, plus 90 s.
///
/// # Errors
/// When a statement fails.
pub async fn push_running_deadline(tx: &Tx<'_>, turn: Uuid) -> Result<()> {
    tx.execute(
        "update turns set deadline = greatest(now(), (select available_since from availability where id))
                                      + make_interval(secs => $2)
         where id = $1",
        &[&turn, &secs(crate::limits::HEARTBEAT_DEADLINE)],
    )
    .await?;
    Ok(())
}

/// The conversation's `row_head` now (a `turn` change's `timelineSeq`, magpie 7.6).
///
/// # Errors
/// When a statement fails.
pub async fn row_head(tx: &Tx<'_>, conversation: Uuid) -> Result<u64> {
    let row = tx
        .query_one("select row_head from conversations where id = $1", &[&conversation])
        .await?;
    Ok(u64::try_from(row.get::<_, i64>(0)).unwrap_or(0))
}

/// A `turn` change (`timelineSeq` = `row_head` at this point of the transaction).
///
/// # Errors
/// When a statement fails.
pub async fn turn_change(
    tx: &Tx<'_>,
    person: &str,
    conversation: Uuid,
    turn: Uuid,
    phase: TurnPhase,
    origin: TurnOrigin,
    error: Option<ErrorInfo>,
) -> Result<()> {
    let timeline_seq = row_head(tx, conversation).await?;
    crate::changes::emit(
        tx,
        person,
        conversation,
        &ChangeBody::Turn(TurnChange {
            turn_id: turn,
            phase,
            origin,
            error,
            timeline_seq,
        }),
    )
    .await?;
    Ok(())
}

/// Queues a command for the conversation's supervisor (its seq: `cmd_head + 1`) and wakes the command polls.
///
/// # Errors
/// When a statement fails.
pub async fn command(
    tx: &Tx<'_>,
    conversation: Uuid,
    kind: &str,
    turn: Option<Uuid>,
    payload: &serde_json::Value,
) -> Result<i64> {
    let row = tx
        .query_opt(
            "update conversations set cmd_head = cmd_head + 1 where id = $1 returning cmd_head",
            &[&conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let seq: i64 = row.get(0);
    tx.execute(
        "insert into commands (conversation_id, cmd_seq, type, turn_id, payload) values ($1, $2, $3, $4, $5)",
        &[&conversation, &seq, &kind, &turn, payload],
    )
    .await?;
    fanout::notify(tx, fanout::COMMANDS, &conversation.to_string(), seq).await?;
    Ok(seq)
}

/// Whether the turn's `start_turn` was delivered to a supervisor (and, with `acked`, acknowledged).
///
/// # Errors
/// When a statement fails.
pub async fn start_turn_status(db: &impl Db, turn: &Turn) -> Result<(bool, bool)> {
    start_turn_row(db, turn, "").await
}

/// [`start_turn_status`] with the `start_turn` row locked (`for update`) until the transaction ends. The command
/// poll marks a delivery without the conversation lock, so a writer that decides by the delivery (cancel, 8.6) takes
/// this lock: the poll's update then waits for it and finds the command canceled, or the writer waits for the poll's
/// and sees the delivery.
///
/// # Errors
/// When a statement fails.
pub async fn lock_start_turn(tx: &Tx<'_>, turn: &Turn) -> Result<(bool, bool)> {
    start_turn_row(tx, turn, " for update").await
}

async fn start_turn_row(db: &impl Db, turn: &Turn, lock: &str) -> Result<(bool, bool)> {
    let Some(seq) = turn.start_cmd_seq else {
        // an agent-started turn has no start_turn: the supervisor runs it
        return Ok((true, true));
    };
    let row = db
        .query_opt(
            &format!(
                "select delivered_at is not null, acked_at is not null from commands
                 where conversation_id = $1 and cmd_seq = $2{lock}"
            ),
            &[&turn.conversation, &seq],
        )
        .await?;
    // a start_turn deleted by retention was acked long ago
    Ok(row.map_or((true, true), |r| (r.get(0), r.get(1))))
}

/// A sha-256 as lower-case hex (a blob id).
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
