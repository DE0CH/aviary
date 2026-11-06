//! Firing (Deyao, 2026-11-06): on the job leader, every [`EVERY`], each schedule whose time has come is posted into
//! its conversation as the exact prompt wrapped in `<scheduled-message rule="…">…</scheduled-message>` ([`message`]).
//!
//! - **Idle conversation:** the message is admitted as a turn exactly as a typed one is (magpie 8.2, the person's
//!   turn, the sandbox started by the driver as for any send), with the client message id
//!   `schedule:<id>:<due, Unix microseconds>` (run outcome `sent`).
//! - **A turn is running:** the message goes to that turn's supervisor at once as a `send_message` command, which
//!   writes it to Claude Code's stdin; Claude Code takes it in at its next tool result or when the model stops, as
//!   it does a message typed mid-turn (outcome `pending`, then `sent_mid_turn` once the supervisor acks it
//!   delivered). If that turn ended before the supervisor wrote it (a negative ack, or the turn ended with the
//!   command still unacked), the run is delivered again (`redeliver`), now as a new turn.
//! - **Late:** a fire found more than [`LATE_LIMIT`] after its time (the service was down or restarting) is not
//!   sent: the run is `skipped_late`, and a repeating rule goes on from its next time after now. Within the limit it
//!   is sent once on recovery.
//! - The pause-all switch records each due time as `skipped_paused`; a conversation in Recently deleted as
//!   `skipped_deleted`. A conversation still owned by the old stack (the switch window) is left to the old stack.
//!
//! **Exactly once:** each fire is one transaction that takes the schedule's row (`for update skip locked`), records
//! the run under the key `(schedule, due time)`, admits the turn or queues the command, and moves `next_at` on; a
//! second leader (a hand-over) skips the locked row and then finds it moved on. The run's outcome changes only
//! under its own row lock.

use std::sync::Arc;
use std::time::Duration;

use aviary_pg::{Leader, Pool, Tx};
use magpie_api::errors::ErrorCode;
use magpie_api::turns::SendRequest;
use serde_json::json;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{COLUMNS, LATE_LIMIT, Row, cron};
use crate::conversations::Person;
use crate::error::{Error, Result};
use crate::turns::driver::{self, Driver};

/// How often due schedules are looked for.
pub const EVERY: Duration = Duration::from_secs(5);
/// Fires per pass at most (the next pass takes the rest).
const BATCH: usize = 100;

/// The message a fire posts.
#[must_use]
pub fn message(describe: &str, prompt: &str) -> String {
    let rule = describe.replace('&', "&amp;").replace('"', "&quot;");
    format!("<scheduled-message rule=\"{rule}\">{prompt}</scheduled-message>")
}

/// Starts the firing loop: a pass every `every` ([`EVERY`] in production) while this replica leads.
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
                Ok(n) => tracing::info!(job = "schedules", fired = n, "job pass"),
                Err(e) => tracing::warn!(job = "schedules", error = %e, "job pass failed"),
            }
        }
    })
}

/// One pass: settle mid-turn deliveries, fire what is due, deliver again what must be. How many runs it handled.
///
/// # Errors
/// A statement error (the next pass goes on).
pub async fn pass(driver: &Driver) -> Result<usize> {
    settle(&driver.pool).await?;
    let mut n = 0;
    for _ in 0..BATCH {
        match due_one(&driver.pool).await? {
            Some(turn) => {
                n += 1;
                if let Some(t) = turn {
                    driver::spawn(driver.clone(), t);
                }
            }
            None => break,
        }
    }
    for _ in 0..BATCH {
        match redeliver_one(&driver.pool).await? {
            Some(turn) => {
                n += 1;
                if let Some(t) = turn {
                    driver::spawn(driver.clone(), t);
                }
            }
            None => break,
        }
    }
    Ok(n)
}

/// What a delivery did.
#[derive(Debug, Default)]
struct Delivery {
    outcome: &'static str,
    turn: Option<Uuid>,
    cmd: Option<i64>,
    /// A turn admitted now (its driver starts after the commit).
    new_turn: Option<Uuid>,
}

impl Delivery {
    fn just(outcome: &'static str) -> Self {
        Self {
            outcome,
            ..Self::default()
        }
    }
}

/// Posts `row`'s message into its conversation (inside the fire's transaction).
async fn deliver(t: &Tx<'_>, row: &Row, due: OffsetDateTime) -> Result<Delivery> {
    let c = match crate::conversations::lock(t, &row.person, row.conversation).await {
        Ok(c) => c,
        Err(e) if e.is(&ErrorCode::NotFound) => return Ok(Delivery::just("skipped_deleted")),
        Err(e) => return Err(e),
    };
    if c.deleted_at.is_some() {
        return Ok(Delivery::just("skipped_deleted"));
    }
    let text = message(&row.describe, &row.prompt);
    let run_state: String = t
        .query_one(
            "select run_state from conversations where id = $1",
            &[&row.conversation],
        )
        .await?
        .get(0);
    let active = crate::turns::active(t, row.conversation)
        .await?
        .filter(|a| !a.state.is_ended());
    if let Some(a) = active {
        // a turn runs (or waits for its sandbox, or for the person's answer): the message joins it
        let seq =
            crate::turns::command(t, row.conversation, "send_message", Some(a.id), &json!({"text": text})).await?;
        return Ok(Delivery {
            outcome: "pending",
            turn: Some(a.id),
            cmd: Some(seq),
            new_turn: None,
        });
    }
    if matches!(run_state.as_str(), "admitted" | "preparing" | "running" | "stopping") {
        // busy without an active turn row: the next pass tries again
        return Ok(Delivery::just("redeliver"));
    }
    crate::conversations::ensure_person(t, &row.person).await?;
    let person = Person {
        email: row.person.clone(),
        viewer: None,
    };
    let req = SendRequest {
        client_message_id: format!("schedule:{}:{}", row.id, due.unix_timestamp_nanos() / 1000),
        text,
        images: Vec::new(),
        files: Vec::new(),
    };
    let started = crate::turns::admission::admit(t, row.conversation, &person, &req).await?;
    Ok(Delivery {
        outcome: "sent",
        turn: Some(started.id),
        cmd: None,
        new_turn: Some(started.id),
    })
}

/// A schedule's state and next fire after a due time handled at `now`.
fn advance(row: &Row, due: OffsetDateTime, now: OffsetDateTime) -> (&'static str, Option<OffsetDateTime>) {
    let Some(expr) = row.cron.as_deref() else {
        return ("done", None);
    };
    let Ok(rule) = cron::parse(expr) else {
        return ("done", None);
    };
    let from = now.max(due).unix_timestamp();
    let next = rule
        .next_after(row.zone(), from)
        .filter(|n| row.until.is_none_or(|u| *n <= u.unix_timestamp()))
        .and_then(|n| OffsetDateTime::from_unix_timestamp(n).ok());
    match next {
        Some(n) => ("active", Some(n)),
        None => ("done", None),
    }
}

/// Fires the earliest due schedule, if any: `Some(turn to drive)`.
async fn due_one(pool: &Pool) -> Result<Option<Option<Uuid>>> {
    aviary_pg::tx!(pool, |t| -> Result<Option<Option<Uuid>>> {
        let Some(r) = t
            .query_opt(
                &format!(
                    "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                     where s.state = 'active' and s.next_at <= now() and not c.migrating
                       and not {}
                     order by s.next_at limit 1 for update of s skip locked",
                    crate::legacy::fence::REVERSE_FENCED_SQL
                ),
                &[],
            )
            .await?
        else {
            return Ok(None);
        };
        let row = Row::of(&r);
        let now = OffsetDateTime::now_utc();
        let due = row.next_at.unwrap_or(now);
        let paused_all: bool = t
            .query_one("select paused_all from schedule_settings where id = 1", &[])
            .await?
            .get(0);
        let late = (now - due).whole_seconds() > i64::try_from(LATE_LIMIT.as_secs()).unwrap_or(600);
        let d = if paused_all {
            Delivery::just("skipped_paused")
        } else if late {
            Delivery::just("skipped_late")
        } else {
            deliver(t, &row, due).await?
        };
        t.execute(
            "insert into schedule_runs (schedule_id, due_at, outcome, turn_id, cmd_seq, conversation_id)
             values ($1, $2, $3, $4, $5, $6) on conflict (schedule_id, due_at) do nothing",
            &[&row.id, &due, &d.outcome, &d.turn, &d.cmd, &row.conversation],
        )
        .await?;
        let (state, next) = advance(&row, due, now);
        t.execute(
            "update schedules set state = $2, next_at = $3, updated_at = now() where id = $1",
            &[&row.id, &state, &next],
        )
        .await?;
        tracing::info!(schedule = %row.id, conversation = %row.conversation, outcome = d.outcome, "schedule fired");
        Ok(Some(d.new_turn))
    })
    .await
}

/// Delivers again the earliest run that must be (its turn ended before it took the message).
async fn redeliver_one(pool: &Pool) -> Result<Option<Option<Uuid>>> {
    aviary_pg::tx!(pool, |t| -> Result<Option<Option<Uuid>>> {
        let Some(r) = t
            .query_opt(
                &format!(
                    "select r.schedule_id, r.due_at from schedule_runs r join conversations c on c.id = r.conversation_id
                     where r.outcome = 'redeliver' and not c.migrating and not {}
                     order by r.due_at limit 1 for update of r skip locked",
                    crate::legacy::fence::REVERSE_FENCED_SQL
                ),
                &[],
            )
            .await?
        else {
            return Ok(None);
        };
        let id: String = r.get(0);
        let due: OffsetDateTime = r.get(1);
        let row = t
            .query_opt(
                &format!(
                    "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                     where s.id = $1"
                ),
                &[&id],
            )
            .await?
            .as_ref()
            .map(Row::of)
            .ok_or_else(Error::not_found)?;
        let d = deliver(t, &row, due).await?;
        t.execute(
            "update schedule_runs set outcome = $3, turn_id = coalesce($4, turn_id), cmd_seq = $5, at = now()
             where schedule_id = $1 and due_at = $2",
            &[&id, &due, &d.outcome, &d.turn, &d.cmd],
        )
        .await?;
        tracing::info!(schedule = %id, outcome = d.outcome, "schedule delivered again");
        // a `redeliver` again (busy without a turn row) waits for the next pass rather than spinning in this one
        Ok(if d.outcome == "redeliver" { None } else { Some(d.new_turn) })
    })
    .await
}

/// Settles mid-turn deliveries: an acked `send_message` makes its run `sent_mid_turn` (delivered) or `redeliver`
/// (the turn had ended), a canceled one `redeliver`; one never acked whose turn has ended is canceled and delivered
/// again.
async fn settle(pool: &Pool) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        t.execute(
            "update schedule_runs r
             set outcome = case when m.acked_at is not null and m.result->>'delivered' = 'true' then 'sent_mid_turn'
                                else 'redeliver' end,
                 at = now()
             from commands m
             where r.outcome = 'pending' and m.conversation_id = r.conversation_id and m.cmd_seq = r.cmd_seq
               and (m.acked_at is not null or m.canceled_at is not null)",
            &[],
        )
        .await?;
        t.execute(
            "with gone as (
               update commands m set canceled_at = now()
               from schedule_runs r join turns tu on tu.id = r.turn_id
               where r.outcome = 'pending' and m.conversation_id = r.conversation_id and m.cmd_seq = r.cmd_seq
                 and m.acked_at is null and m.canceled_at is null
                 and tu.state in ('completed', 'failed', 'canceled')
               returning m.conversation_id, m.cmd_seq)
             update schedule_runs r set outcome = 'redeliver', at = now()
             from gone where r.conversation_id = gone.conversation_id and r.cmd_seq = gone.cmd_seq
               and r.outcome = 'pending'",
            &[],
        )
        .await?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::message;

    #[test]
    fn the_message_carries_the_rule() {
        assert_eq!(
            message("每個工作日 08:30", "整理新聞"),
            "<scheduled-message rule=\"每個工作日 08:30\">整理新聞</scheduled-message>"
        );
        assert_eq!(
            message("a\"b&c", "x"),
            "<scheduled-message rule=\"a&quot;b&amp;c\">x</scheduled-message>"
        );
    }
}
