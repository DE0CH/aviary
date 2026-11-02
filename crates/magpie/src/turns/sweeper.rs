//! The turn sweeper and the availability tick (magpie 8.8, 15), on every replica every 5 s.
//!
//! - The availability tick: `last_tick = now()`, and when the last tick is older than 20 s (no replica could reach
//!   the database, or none ran) `available_since = now()` first. Silence is measured only over time magpie could
//!   accept heartbeats, so after an outage of any length a running turn gets a full 90 s for its supervisor's
//!   retried heartbeat to land.
//! - Lapsed driver claims: a waiting turn whose claim lapsed (its replica died) or that no driver claimed gets one.
//! - Deadlines (magpie 8.8): `admitted`/`preparing` after 15 min → `sandbox.timeout`; `starting` with `start_turn`
//!   unacknowledged 120 s after ready → `sandbox.unreachable` and `kill {watchdog}` (acknowledged: 30 s more, while
//!   the heartbeat reconciles it, [`super::reports::heartbeat`], until 5 min of magpie-available time after the
//!   acknowledgement; then `sandbox.unreachable` and `kill {watchdog}`, so no starting turn waits for ever);
//!   `running` silent past its deadline → hermit is asked (`gone`, `releasing`, another sandbox or none →
//!   `sandbox.lost`; alive per hermit → 90 s more, until 5 min of silence → `sandbox.unreachable`; hermit unreachable
//!   → 90 s more, no turn is failed on missing evidence); `stopping` 30 s after the stop → `stop.forced` and
//!   `kill {stop}`.
//!
//! Each turn is handled in its own transaction under its conversation's lock, re-checked there; a sweep on another
//! replica that got there first leaves nothing to do. Migrating conversations and `import` turns are skipped.

use hermit_api::sandbox::{SandboxQuery, SandboxState};
use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::sandbox::KillReason;
use magpie_api::turns::TurnOutcome;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::driver::{self, Driver, lock_id};
use super::end::{self, Ending};
use super::{State, Turn, load, secs, start_turn_status};
use crate::error::Result;
use crate::limits;

/// Starts the sweeper loop of this replica.
#[must_use]
pub fn start(d: Driver) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(limits::SWEEP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if d.draining.is_draining() {
                continue;
            }
            if let Err(e) = availability_tick(&d.pool).await {
                tracing::warn!(error = %e, "availability tick failed");
                continue;
            }
            match pass(&d).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(turns = n, "turn sweep"),
                Err(e) => tracing::warn!(error = %e, "turn sweep failed"),
            }
        }
    })
}

/// One availability tick (magpie 8.8).
///
/// # Errors
/// When the statement fails (Postgres unreachable: the next tick after the outage starts a new period).
pub async fn availability_tick(pool: &aviary_pg::Pool) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "insert into availability (id, available_since, last_tick) values (true, now(), now())
             on conflict (id) do update set
                available_since = case when availability.last_tick < now() - make_interval(secs => $1) then now()
                                       else availability.available_since end,
                last_tick = now()",
            &[&secs(limits::AVAILABILITY_GAP)],
        )
        .await?;
    Ok(())
}

/// One sweep: lapsed claims get a driver, expired deadlines are handled; how many turns it acted on.
///
/// # Errors
/// When a statement fails.
pub async fn pass(d: &Driver) -> Result<usize> {
    let conn = d.pool.get().await?;
    let unclaimed = conn
        .query(
            "select t.id from turns t join conversations c on c.id = t.conversation_id
             where t.state in ('admitted', 'preparing') and (t.driver_until is null or t.driver_until < now())
               and not c.migrating and t.origin <> 'import'
             order by t.created_at limit $1",
            &[&limits::SWEEP_BATCH],
        )
        .await?;
    let expired = conn
        .query(
            "select t.id, t.conversation_id from turns t join conversations c on c.id = t.conversation_id
             where t.state not in ('completed', 'failed', 'canceled') and t.deadline < now()
               and not c.migrating and t.origin <> 'import'
             order by t.deadline limit $1",
            &[&limits::SWEEP_BATCH],
        )
        .await?;
    drop(conn);
    let mut n = 0;
    for r in unclaimed {
        driver::spawn(d.clone(), r.get(0));
        n += 1;
    }
    for r in expired {
        let (turn, conversation): (Uuid, Uuid) = (r.get(0), r.get(1));
        match expire(d, conversation, turn).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(turn = %turn, error = %e, "turn deadline not handled"),
        }
    }
    Ok(n)
}

fn err(code: DataCode, message: &str) -> ErrorInfo {
    ErrorInfo::new(code, message)
}

/// The turn's moment of last sign of life for the running deadline: the later of its start, its conversation's last
/// heartbeat and magpie's availability.
async fn last_life(tx: &aviary_pg::Tx<'_>, turn: Uuid) -> Result<OffsetDateTime> {
    let row = tx
        .query_one(
            "select greatest(coalesce(t.started_at, t.created_at), c.heartbeat_at,
                             (select available_since from availability where id))
             from turns t join conversations c on c.id = t.conversation_id where t.id = $1",
            &[&turn],
        )
        .await?;
    Ok(row.get(0))
}

/// What hermit said about a running turn's sandbox.
enum Hermit {
    /// Gone, releasing, another sandbox, or none.
    Lost,
    /// Live, last active at.
    Alive(OffsetDateTime),
    /// No answer.
    Unreachable,
}

async fn ask_hermit(d: &Driver, conversation: Uuid, sandbox: Option<Uuid>) -> Hermit {
    let call = d.hermit.get_sandbox(conversation, SandboxQuery::default());
    match tokio::time::timeout(limits::HERMIT_READ_TIMEOUT, call).await {
        Ok(Ok(s)) if matches!(s.state, SandboxState::Gone | SandboxState::Releasing) => Hermit::Lost,
        Ok(Ok(s)) if sandbox.is_some_and(|id| id != s.id) => Hermit::Lost,
        Ok(Ok(s)) => Hermit::Alive(s.last_active_at.datetime()),
        Ok(Err(e)) if e.status() == Some(404) => Hermit::Lost,
        Ok(Err(e)) => {
            tracing::warn!(conversation = %conversation, error = %e, "watchdog: hermit did not answer");
            Hermit::Unreachable
        }
        Err(_) => {
            tracing::warn!(conversation = %conversation, "watchdog: hermit timed out");
            Hermit::Unreachable
        }
    }
}

/// Handles one expired deadline; whether it changed the turn.
#[allow(clippy::too_many_lines)] // one match over the states of magpie 8.8, then the running check
async fn expire(d: &Driver, conversation: Uuid, turn_id: Uuid) -> Result<bool> {
    // the running check asks hermit, outside any transaction: first see whether it is needed
    let first = aviary_pg::tx!(&d.pool, |t| -> Result<Option<Turn>> {
        let Some(c) = lock_id(t, conversation).await? else {
            return Ok(None);
        };
        let Some(turn) = load(t, conversation, turn_id).await? else {
            return Ok(None);
        };
        if c.migrating || turn.state.is_ended() || turn.deadline.is_none_or(|dl| dl > OffsetDateTime::now_utc()) {
            return Ok(None);
        }
        match turn.state {
            State::Admitted | State::Preparing => {
                let e = err(DataCode::SandboxTimeout, "the sandbox was not ready within 15 minutes");
                end::end(t, &c.person, &turn, Ending::failed(e)).await?;
                Ok(None)
            }
            State::Starting => {
                let (_, acked) = start_turn_status(t, &turn).await?;
                // an acknowledged start whose running report and heartbeats never showed it: 5 min, then the watchdog
                let gave_up = acked && {
                    let row = t
                        .query_one(
                            "select greatest((select acked_at from commands where conversation_id = $1 and cmd_seq = $2),
                                             (select available_since from availability where id))
                                    + make_interval(secs => $3) <= now()",
                            &[&turn.conversation, &turn.start_cmd_seq, &secs(limits::SILENCE_MAX)],
                        )
                        .await?;
                    row.get::<_, Option<bool>>(0).unwrap_or(false)
                };
                if acked && !gave_up {
                    t.execute(
                        "update turns set deadline = now() + make_interval(secs => $2) where id = $1",
                        &[&turn.id, &secs(limits::START_ACK_GRACE)],
                    )
                    .await?;
                } else if acked {
                    let e = err(
                        DataCode::SandboxUnreachable,
                        "the supervisor took the turn but never reported it running",
                    );
                    end::end(t, &c.person, &turn, Ending::failed(e).kill(KillReason::Watchdog)).await?;
                } else {
                    let e = err(DataCode::SandboxUnreachable, "the supervisor never took the turn");
                    end::end(t, &c.person, &turn, Ending::failed(e).kill(KillReason::Watchdog)).await?;
                }
                Ok(None)
            }
            State::Stopping => {
                let row = t
                    .query_one(
                        "select greatest(coalesce(stop_requested_at, now()),
                                         (select available_since from availability where id))
                                + make_interval(secs => $2)
                         from turns where id = $1",
                        &[&turn.id, &secs(limits::STOP_TIMEOUT)],
                    )
                    .await?;
                let due: OffsetDateTime = row.get(0);
                if due > OffsetDateTime::now_utc() {
                    t.execute("update turns set deadline = $2 where id = $1", &[&turn.id, &due])
                        .await?;
                } else {
                    let ending = Ending {
                        outcome: TurnOutcome::Canceled,
                        error: Some(err(DataCode::StopForced, "the stop was not confirmed in 30 s")),
                        ..Ending::canceled()
                    };
                    end::end(t, &c.person, &turn, ending.kill(KillReason::Stop)).await?;
                }
                Ok(None)
            }
            State::Running => {
                let due = last_life(t, turn.id).await? + limits::HEARTBEAT_DEADLINE;
                if due > OffsetDateTime::now_utc() {
                    t.execute("update turns set deadline = $2 where id = $1", &[&turn.id, &due])
                        .await?;
                    return Ok(None);
                }
                Ok(Some(turn))
            }
            State::Completed | State::Failed | State::Canceled => Ok(None),
        }
    })
    .await?;
    let Some(turn) = first else {
        return Ok(true);
    };
    let verdict = ask_hermit(d, conversation, turn.sandbox).await;
    aviary_pg::tx!(&d.pool, |t| -> Result<bool> {
        let Some(c) = lock_id(t, conversation).await? else {
            return Ok(false);
        };
        let Some(turn) = load(t, conversation, turn_id).await? else {
            return Ok(false);
        };
        let now = OffsetDateTime::now_utc();
        if turn.state != State::Running || turn.deadline.is_none_or(|dl| dl > now) {
            return Ok(false);
        }
        let silent_since = last_life(t, turn.id).await?;
        let more = || async {
            t.execute(
                "update turns set deadline = now() + make_interval(secs => $2) where id = $1",
                &[&turn.id, &secs(limits::HEARTBEAT_DEADLINE)],
            )
            .await
        };
        match verdict {
            Hermit::Lost => {
                let e = err(DataCode::SandboxLost, "the sandbox is gone");
                end::end(t, &c.person, &turn, Ending::failed(e)).await?;
            }
            Hermit::Alive(active) if now - active <= limits::HERMIT_ALIVE_WITHIN => {
                if now - silent_since > limits::SILENCE_MAX {
                    let e = err(
                        DataCode::SandboxUnreachable,
                        "the supervisor went silent while hermit sees it",
                    );
                    end::end(t, &c.person, &turn, Ending::failed(e).kill(KillReason::Watchdog)).await?;
                } else {
                    more().await?;
                }
            }
            Hermit::Alive(_) => {
                let e = err(
                    DataCode::SandboxUnreachable,
                    "the supervisor went silent and hermit sees no activity",
                );
                end::end(t, &c.person, &turn, Ending::failed(e).kill(KillReason::Watchdog)).await?;
            }
            Hermit::Unreachable => {
                more().await?;
            }
        }
        Ok(true)
    })
    .await
}
