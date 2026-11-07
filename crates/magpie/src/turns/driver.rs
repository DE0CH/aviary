//! The driver (magpie 8.3): gets an admitted turn its sandbox from hermit (hermit 3.6).
//!
//! The replica that admitted the turn claims it (`driver = pod`, `driver_until = now + 30 s`, renewed every 10 s) and
//! calls hermit's get-or-start `PUT` (idempotent per conversation), then, while the sandbox starts, hermit's
//! long-poll `GET …?waitMs=25000&afterVersion=<v>`, so the turn moves the moment hermit's state changes. No answer
//! that says ready within 1 s of admission shows the preparing chip (`sandbox.preparing`). Errors and timeouts retry
//! with backoff (0.5 s doubling to 10 s) until the turn's deadline (`turns.deadline`: admission + 15 min, and a fresh
//! 15 min from a re-acquire, below, capped at admission + 45 min).
//!
//! A replica that dies stops renewing; the sweeper of any replica then claims the turn ([`super::sweeper`]) and
//! continues it from its state (hermit's `PUT` is idempotent). A drain hands its claims back at once. Every write of
//! the driver checks that the turn is still waiting, so a cancel, a report or another driver that moved it first
//! wins and this one stops.
//!
//! A sandbox that goes away before the supervisor acknowledged `start_turn` (released, or lost) is replaced: the
//! turn goes back to preparing and the driver asks hermit again (hermit spec 28, magpie item 1). Only a start that
//! failed (`sandbox_busy`, `sandbox_failed`) or a release of a conversation in Recently deleted fails it
//! (`sandbox.failed` for the latter too: before running, 8.3 step 2).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use aviary_pg::{Pool, Tx};
use hermit_api::client::ClientError;
use hermit_api::error::codes;
use hermit_api::sandbox::{PutSandbox, PutSandboxRequest, Release, ReleaseCode, Sandbox, SandboxQuery, SandboxState};
use magpie_api::Empty;
use magpie_api::changes::ChangeBody;
use magpie_api::conversations::SandboxPhase;
use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::EventBody;
use time::OffsetDateTime;
use uuid::Uuid;

use super::end::{self, Ending};
use super::{State, Turn, load, secs, start_turn_status};
use crate::changes;
use crate::error::Result;
use crate::events::{self, NewEvent};
use crate::limits;

/// What a driver needs from its replica.
#[derive(Debug, Clone)]
pub struct Driver {
    /// The pool.
    pub pool: Pool,
    /// hermit (`MAGPIE_TOKEN_HERMIT`).
    pub hermit: hermit_api::client::Client,
    /// This replica's name (the claim's owner).
    pub pod: String,
    /// The replica's drain: drivers stop (their claims are handed back by the drain).
    pub draining: aviary_core::drain::Draining,
}

impl Driver {
    /// The driver of `app`'s replica.
    #[must_use]
    pub fn of(app: &crate::app::Magpie) -> Self {
        Self {
            pool: app.pool.clone(),
            hermit: app.hermit.clone(),
            pod: app.pod.clone(),
            draining: app.draining.clone(),
        }
    }
}

/// The turns each replica (by pod name) drives now: one driver task per turn and replica.
fn driving() -> &'static Mutex<HashSet<(String, Uuid)>> {
    static DRIVING: OnceLock<Mutex<HashSet<(String, Uuid)>>> = OnceLock::new();
    DRIVING.get_or_init(Mutex::default)
}

/// Starts driving `turn` in the background (after the admitting transaction committed), unless this replica already
/// drives it.
pub fn spawn(d: Driver, turn: Uuid) {
    let key = (d.pod.clone(), turn);
    if !driving().lock().is_ok_and(|mut s| s.insert(key.clone())) {
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = drive(&d, turn).await {
            tracing::warn!(turn = %turn, error = %e, "driver stopped (the sweeper continues the turn)");
        }
        if let Ok(mut s) = driving().lock() {
            s.remove(&key);
        }
    });
}

/// The conversation of a sandbox call or a driver step, locked: its owner and whether it is in Recently deleted.
#[derive(Debug, Clone)]
pub struct Conv {
    /// Its owner.
    pub person: String,
    /// In Recently deleted.
    pub deleted: bool,
    /// `sandbox_phase`.
    pub sandbox_phase: Option<String>,
}

/// Locks conversation `id` (`select … for update`); `None` when it is gone.
///
/// # Errors
/// When a statement fails.
pub async fn lock_id(tx: &Tx<'_>, id: Uuid) -> Result<Option<Conv>> {
    let row = tx
        .query_opt(
            "select person, deleted_at is not null, sandbox_phase from conversations where id = $1 for update",
            &[&id],
        )
        .await?;
    Ok(row.map(|r| Conv {
        person: r.get(0),
        deleted: r.get(1),
        sandbox_phase: r.get(2),
    }))
}

/// Who saw the sandbox state [`record_sandbox`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// hermit's feed (or its recovery's re-read), applied in `seq` order: it may name another sandbox.
    Feed,
    /// A driver's answer from hermit (`PUT`, long-poll). hermit's `version` counts per sandbox row, so it orders only
    /// answers about one sandbox: an answer naming another sandbox than the one recorded may be older than the
    /// feed's record of a replacement, and is recorded only when nothing is.
    Answer,
}

/// Records hermit's view of the conversation's sandbox when it is newer than the one recorded (magpie 8.7): the same
/// sandbox at a higher version; another sandbox that is live, only from the feed ([`Seen`]) or when nothing is
/// recorded. A `gone` of a sandbox other than the recorded one changes nothing (a replacement may already be
/// recorded).
///
/// # Errors
/// When a statement fails.
pub async fn record_sandbox(
    tx: &Tx<'_>,
    conversation: Uuid,
    sandbox: Uuid,
    state: SandboxState,
    version: u64,
    seen: Seen,
) -> Result<()> {
    let version = i64::try_from(version).unwrap_or(i64::MAX);
    tx.execute(
        "update conversations set hermit_sandbox = $2, hermit_state = $3, hermit_version = $4
         where id = $1
           and ((hermit_sandbox is distinct from $2 and $3 <> 'gone' and ($5 or hermit_sandbox is null))
                or (hermit_sandbox = $2 and coalesce(hermit_version, -1) < $4))",
        &[
            &conversation,
            &sandbox,
            &state.as_str(),
            &version,
            &(seen == Seen::Feed),
        ],
    )
    .await?;
    Ok(())
}

/// What a sandbox that went away means for a turn that has not ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Gone {
    /// The turn fails with this error.
    Fail(ErrorInfo),
    /// The turn goes back to preparing and gets a new sandbox.
    Reacquire,
}

/// The verdict for `turn` whose sandbox ended with `release` (magpie 8.3, 8.7; hermit spec 28).
#[must_use]
pub fn gone_verdict(turn: &Turn, start_acked: bool, deleted: bool, release: Option<&Release>) -> Gone {
    let code = release.map(|r| r.code);
    let detail = release.map(|r| r.reason.as_str().to_owned());
    let message = release.map_or_else(|| "the sandbox ended".to_owned(), |r| r.message.clone());
    let with = |c: DataCode| {
        let mut e = ErrorInfo::new(c, message.clone());
        e.detail.clone_from(&detail);
        e
    };
    match code {
        Some(ReleaseCode::SandboxBusy) if turn.state.is_waiting() || turn.state == State::Starting => {
            Gone::Fail(with(DataCode::SandboxBusy))
        }
        Some(ReleaseCode::SandboxFailed) if turn.state.is_waiting() || turn.state == State::Starting => {
            Gone::Fail(with(DataCode::SandboxFailed))
        }
        _ if !deleted && !start_acked && (turn.state.is_waiting() || turn.state == State::Starting) => Gone::Reacquire,
        // a conversation in Recently deleted gets no new sandbox: a turn that never ran fails as a start that failed
        _ if !start_acked && (turn.state.is_waiting() || turn.state == State::Starting) => {
            Gone::Fail(with(DataCode::SandboxFailed))
        }
        _ => Gone::Fail(with(DataCode::SandboxLost)),
    }
}

/// Moves a turn whose sandbox went away before its `start_turn` was acknowledged back to preparing (the chip shows
/// again); `driver` keeps the claim (the driver itself), `None` (the feed) leaves a live driver's claim alone and
/// otherwise hands the turn to the sweeper. The replacement gets a fresh
/// 15 min deadline, which the driver reads from the row, but never past admission + 45 min
/// ([`limits::ADMIT_TOTAL_MAX`]), so sandboxes that keep dying before the acknowledgement end the turn
/// (`sandbox.timeout`). `start_turn` (unacknowledged) loses the delivery the lost
/// sandbox's supervisor took, so a cancel while the replacement starts ends the turn at once (8.6), and the next
/// supervisor still receives it.
///
/// # Errors
/// When a statement fails.
pub async fn reacquire(tx: &Tx<'_>, person: &str, c: &Conv, turn: &Turn, driver: Option<&str>) -> Result<()> {
    if let Some(seq) = turn.start_cmd_seq {
        tx.execute(
            "update commands set delivered_at = null
             where conversation_id = $1 and cmd_seq = $2 and acked_at is null and canceled_at is null",
            &[&turn.conversation, &seq],
        )
        .await?;
    }
    tx.execute(
        "update turns set state = 'preparing', sandbox_id = null, ready_at = null,
                deadline = least(now() + make_interval(secs => $2),
                               coalesce(resumed_at, created_at) + make_interval(secs => $5)),
                driver = case when $3::text is not null then $3 when driver_until > now() then driver end,
                driver_until = case when $3::text is not null then now() + make_interval(secs => $4)
                                    when driver_until > now() then driver_until end
         where id = $1",
        &[
            &turn.id,
            &secs(limits::ADMIT_TIMEOUT),
            &driver,
            &secs(limits::DRIVER_CLAIM),
            &secs(limits::ADMIT_TOTAL_MAX),
        ],
    )
    .await?;
    show_preparing(tx, person, c, turn.conversation, turn.id).await?;
    tracing::info!(turn = %turn.id, "sandbox went away before start_turn was acknowledged: getting a new one");
    Ok(())
}

async fn show_preparing(tx: &Tx<'_>, person: &str, c: &Conv, conversation: Uuid, turn: Uuid) -> Result<()> {
    if c.sandbox_phase.as_deref() != Some("preparing") {
        events::append(
            tx,
            conversation,
            NewEvent::of(&EventBody::SandboxPreparing(Empty {}))?.turn(Some(turn)),
        )
        .await?;
        changes::emit(
            tx,
            person,
            conversation,
            &ChangeBody::Sandbox {
                state: SandboxPhase::Preparing,
            },
        )
        .await?;
    }
    tx.execute(
        "update conversations set run_state = 'preparing', sandbox_phase = 'preparing' where id = $1 and active_turn = $2",
        &[&conversation, &turn],
    )
    .await?;
    changes::upserted(tx, person, conversation).await?;
    Ok(())
}

/// The claimed turn.
struct Claimed {
    conversation: Uuid,
    person: String,
    /// Admitted at (a parked turn: when its answers admitted it again).
    created_at: OffsetDateTime,
    /// `turns.deadline` at the claim (a re-acquire moves it; [`deadline_passes`] follows the row).
    deadline: OffsetDateTime,
}

async fn claim(d: &Driver, turn: Uuid) -> Result<Option<Claimed>> {
    let row = d
        .pool
        .get()
        .await?
        .query_opt(
            "update turns t set driver = $2, driver_until = now() + make_interval(secs => $3)
             from conversations c
             where t.id = $1 and c.id = t.conversation_id and t.state in ('admitted', 'preparing')
               and t.origin <> 'import'
               and (t.driver is null or t.driver_until is null or t.driver_until < now())
             returning t.conversation_id, c.person, coalesce(t.resumed_at, t.created_at),
                       coalesce(t.deadline, coalesce(t.resumed_at, t.created_at) + make_interval(secs => $4))",
            &[&turn, &d.pod, &secs(limits::DRIVER_CLAIM), &secs(limits::ADMIT_TIMEOUT)],
        )
        .await?;
    Ok(row.map(|r| Claimed {
        conversation: r.get(0),
        person: r.get(1),
        created_at: r.get(2),
        deadline: r.get(3),
    }))
}

/// Renews the claim every 10 s; returns when it is lost (the turn moved on, or another replica took it).
async fn renew(d: &Driver, turn: Uuid) {
    loop {
        tokio::time::sleep(limits::DRIVER_RENEW).await;
        let renewed = async {
            d.pool
                .get()
                .await?
                .execute(
                    "update turns set driver_until = now() + make_interval(secs => $3)
                     where id = $1 and driver = $2 and state in ('admitted', 'preparing')",
                    &[&turn, &d.pod, &secs(limits::DRIVER_CLAIM)],
                )
                .await
        }
        .await;
        match renewed {
            Ok(0) => return,
            Ok(_) => {}
            // Postgres unreachable: the claim lapses unless the next renewal lands
            Err(e) => tracing::warn!(turn = %turn, error = %e, "driver claim not renewed"),
        }
    }
}

/// The time left until `at` (zero when it has passed).
fn until(at: OffsetDateTime) -> Duration {
    Duration::try_from(at - OffsetDateTime::now_utc()).unwrap_or(Duration::ZERO)
}

/// Resolves once the waiting turn's deadline has passed by the row (`turns.deadline`, re-read each time the last one
/// read is reached, so a re-acquire's fresh deadline counts), or once the turn no longer waits.
async fn deadline_passes(d: &Driver, turn: Uuid, mut at: OffsetDateTime) {
    loop {
        tokio::time::sleep(until(at)).await;
        let row = async {
            d.pool
                .get()
                .await?
                .query_opt(
                    "select deadline, deadline <= now() from turns
                     where id = $1 and state in ('admitted', 'preparing')",
                    &[&turn],
                )
                .await
        }
        .await;
        match row {
            Ok(Some(r)) => match (r.get::<_, Option<OffsetDateTime>>(0), r.get::<_, Option<bool>>(1)) {
                (Some(dl), Some(false)) => at = dl,
                _ => return,
            },
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(turn = %turn, error = %e, "driver: deadline not read");
                at = OffsetDateTime::now_utc() + limits::HERMIT_BACKOFF_MAX;
            }
        }
    }
}

/// Drives `turn` until it is ready, has failed, or is no longer this replica's.
///
/// # Errors
/// A statement error (the claim then lapses and another replica continues).
pub async fn drive(d: &Driver, turn: Uuid) -> Result<()> {
    let Some(c) = claim(d, turn).await? else {
        return Ok(());
    };
    let preparing_at = c.created_at + limits::PREPARING_AFTER;
    let preparing = async {
        tokio::time::sleep(until(preparing_at)).await;
        if let Err(e) = step_preparing(d, c.conversation, turn).await {
            tracing::warn!(turn = %turn, error = %e, "preparing not shown");
        }
        std::future::pending::<()>().await;
    };
    let mut draining = d.draining.clone();
    tokio::select! {
        () = draining.wait() => Ok(()),
        () = renew(d, turn) => {
            tracing::info!(turn = %turn, "driver claim lost");
            Ok(())
        }
        r = acquire(d, &c, turn) => r,
        () = deadline_passes(d, turn, c.deadline) => step_timeout(d, c.conversation, turn).await,
        () = preparing => Ok(()),
    }
}

fn backoff(n: u32) -> Duration {
    limits::HERMIT_BACKOFF_FIRST
        .saturating_mul(1u32 << n.min(10))
        .min(limits::HERMIT_BACKOFF_MAX)
}

fn refusal_field<'a>(e: &'a ClientError, name: &str) -> Option<&'a serde_json::Value> {
    match e {
        ClientError::Api(r) => r.body.fields.get(name),
        _ => None,
    }
}

/// What one step of the acquisition found.
enum Next {
    /// The turn left the driver's states (ready, failed, canceled, taken over): stop.
    Done,
    /// Ask hermit's `PUT` again (after a pause, the number of failures so far).
    Put(u32),
    /// Long-poll the starting sandbox from this version.
    Wait(Uuid, u64, u32),
}

async fn acquire(d: &Driver, c: &Claimed, turn: Uuid) -> Result<()> {
    let mut next = Next::Put(0);
    loop {
        next = match next {
            Next::Done => return Ok(()),
            Next::Put(failures) => {
                if failures > 0 {
                    tokio::time::sleep(backoff(failures - 1)).await;
                }
                // a conversation deleted while its turn waited gets no new sandbox (its release already ran)
                if deleted(d, c.conversation).await? {
                    let e = ErrorInfo::new(DataCode::SandboxFailed, "the conversation was deleted");
                    step_fail(d, c.conversation, turn, e).await?;
                    return Ok(());
                }
                let req = PutSandboxRequest {
                    person: c.person.clone(),
                };
                match tokio::time::timeout(limits::HERMIT_PUT_TIMEOUT, d.hermit.put_sandbox(c.conversation, &req)).await
                {
                    Ok(Ok(PutSandbox::Ready(s))) => step_ready(d, c.conversation, turn, &s).await?,
                    Ok(Ok(PutSandbox::Starting(s))) => {
                        if step_starting(d, c.conversation, turn, &s).await? {
                            Next::Wait(s.id, s.version, 0)
                        } else {
                            Next::Done
                        }
                    }
                    Ok(Err(e)) => put_refused(d, c, turn, &e, failures).await?,
                    Err(_) => {
                        tracing::info!(turn = %turn, "hermit PUT timed out, retrying");
                        Next::Put(failures + 1)
                    }
                }
            }
            Next::Wait(sandbox, version, failures) => {
                if failures > 0 {
                    tokio::time::sleep(backoff(failures - 1)).await;
                }
                let q = SandboxQuery {
                    wait_ms: Some(limits::HERMIT_WAIT_MS),
                    after_version: Some(version),
                };
                match tokio::time::timeout(limits::HERMIT_WAIT_TIMEOUT, d.hermit.get_sandbox(c.conversation, q)).await {
                    Ok(Ok(s)) if s.state == SandboxState::Ready => step_ready(d, c.conversation, turn, &s).await?,
                    Ok(Ok(s)) if s.state == SandboxState::Gone => {
                        if s.id == sandbox {
                            step_gone(d, c.conversation, turn, &s).await?
                        } else {
                            Next::Put(0)
                        }
                    }
                    Ok(Ok(s)) if s.id != sandbox => {
                        // a newer sandbox of the conversation: follow it
                        if step_starting(d, c.conversation, turn, &s).await? {
                            Next::Wait(s.id, s.version, 0)
                        } else {
                            Next::Done
                        }
                    }
                    Ok(Ok(s)) => {
                        if still_waiting(d, turn).await? {
                            Next::Wait(sandbox, s.version, 0)
                        } else {
                            Next::Done
                        }
                    }
                    Ok(Err(e)) if e.status() == Some(404) => Next::Put(0),
                    Ok(Err(e)) => {
                        tracing::info!(turn = %turn, error = %e, "hermit long-poll failed, retrying");
                        Next::Wait(sandbox, version, failures + 1)
                    }
                    Err(_) => Next::Wait(sandbox, version, failures + 1),
                }
            }
        };
    }
}

async fn put_refused(d: &Driver, c: &Claimed, turn: Uuid, e: &ClientError, failures: u32) -> Result<Next> {
    let code = e.code().unwrap_or_default().to_owned();
    match (e.status(), code.as_str()) {
        (Some(403), codes::NOT_ALLOWED) => {
            let e = ErrorInfo::new(DataCode::SandboxNotAllowed, "hermit does not allow the person");
            step_fail(d, c.conversation, turn, e).await?;
            Ok(Next::Done)
        }
        (Some(409), codes::PERSON_MISMATCH) => {
            tracing::error!(turn = %turn, conversation = %c.conversation, "hermit person mismatch");
            let e = ErrorInfo::new(DataCode::Internal, "hermit person mismatch");
            step_fail(d, c.conversation, turn, e).await?;
            Ok(Next::Done)
        }
        (Some(503), codes::SANDBOX_BUSY) => {
            let mut info = ErrorInfo::new(DataCode::SandboxBusy, "hermit has no sandbox for the turn");
            info.detail = refusal_field(e, "reason").and_then(|v| v.as_str()).map(str::to_owned);
            info.retry_after_s = refusal_field(e, "retryAfterS").and_then(serde_json::Value::as_u64);
            step_fail(d, c.conversation, turn, info).await?;
            Ok(Next::Done)
        }
        _ => {
            tracing::info!(turn = %turn, error = %e, "hermit PUT failed, retrying");
            Ok(Next::Put(failures + 1))
        }
    }
}

/// Runs `f` on the locked conversation and the turn when the turn is still waiting for its sandbox.
macro_rules! waiting_step {
    ($d:expr, $conversation:expr, $turn:expr, |$t:ident, $c:ident, $tr:ident| $body:block) => {{
        let (conversation, turn_id) = ($conversation, $turn);
        aviary_pg::tx!(&$d.pool, |$t| -> Result<bool> {
            let Some($c) = lock_id($t, conversation).await? else {
                return Ok(false);
            };
            let Some($tr) = load($t, conversation, turn_id).await? else {
                return Ok(false);
            };
            if !$tr.state.is_waiting() {
                return Ok(false);
            }
            $body
        })
        .await
    }};
}

async fn deleted(d: &Driver, conversation: Uuid) -> Result<bool> {
    let row = d
        .pool
        .get()
        .await?
        .query_opt(
            "select deleted_at is not null from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    Ok(row.is_none_or(|r| r.get(0)))
}

async fn still_waiting(d: &Driver, turn: Uuid) -> Result<bool> {
    let row = d
        .pool
        .get()
        .await?
        .query_opt(
            "select 1 from turns where id = $1 and state in ('admitted', 'preparing') and driver = $2",
            &[&turn, &d.pod],
        )
        .await?;
    Ok(row.is_some())
}

async fn step_preparing(d: &Driver, conversation: Uuid, turn: Uuid) -> Result<bool> {
    waiting_step!(d, conversation, turn, |t, c, tr| {
        if tr.state != State::Admitted {
            return Ok(true);
        }
        t.execute("update turns set state = 'preparing' where id = $1", &[&tr.id])
            .await?;
        show_preparing(t, &c.person, &c, conversation, tr.id).await?;
        Ok(true)
    })
}

/// hermit answered `starting`: the sandbox is recorded; true while the turn still waits.
async fn step_starting(d: &Driver, conversation: Uuid, turn: Uuid, s: &Sandbox) -> Result<bool> {
    waiting_step!(d, conversation, turn, |t, _c, tr| {
        t.execute("update turns set sandbox_id = $2 where id = $1", &[&tr.id, &s.id])
            .await?;
        record_sandbox(t, conversation, s.id, s.state, s.version, Seen::Answer).await?;
        Ok(true)
    })
}

async fn step_ready(d: &Driver, conversation: Uuid, turn: Uuid, s: &Sandbox) -> Result<Next> {
    waiting_step!(d, conversation, turn, |t, c, tr| {
        mark_ready(t, &c, &tr, s).await?;
        Ok(true)
    })?;
    Ok(Next::Done)
}

/// The turn's sandbox is ready: `starting`, with the `start_turn` deadline (magpie 8.8).
///
/// # Errors
/// When a statement fails.
pub async fn mark_ready(tx: &Tx<'_>, c: &Conv, turn: &Turn, s: &Sandbox) -> Result<()> {
    tx.execute(
        "update turns set state = 'starting', sandbox_id = $2, ready_at = now(),
                deadline = greatest(now(), (select available_since from availability where id))
                           + make_interval(secs => $3),
                driver = null, driver_until = null
         where id = $1",
        &[&turn.id, &s.id, &secs(limits::START_ACK_TIMEOUT)],
    )
    .await?;
    record_sandbox(tx, turn.conversation, s.id, s.state, s.version, Seen::Answer).await?;
    if c.sandbox_phase.as_deref() == Some("preparing") {
        events::append(
            tx,
            turn.conversation,
            NewEvent::of(&EventBody::SandboxReady(Empty {}))?.turn(Some(turn.id)),
        )
        .await?;
        changes::emit(
            tx,
            &c.person,
            turn.conversation,
            &ChangeBody::Sandbox {
                state: SandboxPhase::Ready,
            },
        )
        .await?;
    }
    tx.execute(
        "update conversations set run_state = 'running',
                sandbox_phase = case when sandbox_phase = 'preparing' then 'ready' else sandbox_phase end
         where id = $1 and active_turn = $2",
        &[&turn.conversation, &turn.id],
    )
    .await?;
    changes::upserted(tx, &c.person, turn.conversation).await?;
    Ok(())
}

async fn step_gone(d: &Driver, conversation: Uuid, turn: Uuid, s: &Sandbox) -> Result<Next> {
    let mut again = false;
    let again_ref = &mut again;
    waiting_step!(d, conversation, turn, |t, c, tr| {
        record_sandbox(t, conversation, s.id, s.state, s.version, Seen::Answer).await?;
        let (_, acked) = start_turn_status(t, &tr).await?;
        match gone_verdict(&tr, acked, c.deleted, s.release.as_ref()) {
            Gone::Fail(e) => end::end(t, &c.person, &tr, Ending::failed(e)).await?,
            Gone::Reacquire => {
                reacquire(t, &c.person, &c, &tr, Some(&d.pod)).await?;
                *again_ref = true;
            }
        }
        Ok(true)
    })?;
    Ok(if again { Next::Put(0) } else { Next::Done })
}

/// The deadline passed: `sandbox.timeout`, unless the row says otherwise (a re-acquire that moved it, or an end).
async fn step_timeout(d: &Driver, conversation: Uuid, turn: Uuid) -> Result<()> {
    waiting_step!(d, conversation, turn, |t, c, tr| {
        let due: bool = t
            .query_one(
                "select coalesce(deadline <= now(), false) from turns where id = $1",
                &[&tr.id],
            )
            .await?
            .get(0);
        if due {
            let e = ErrorInfo::new(DataCode::SandboxTimeout, "the sandbox was not ready in time");
            end::end(t, &c.person, &tr, Ending::failed(e)).await?;
        }
        Ok(true)
    })?;
    Ok(())
}

async fn step_fail(d: &Driver, conversation: Uuid, turn: Uuid, e: ErrorInfo) -> Result<()> {
    waiting_step!(d, conversation, turn, |t, c, tr| {
        end::end(t, &c.person, &tr, Ending::failed(e.clone())).await?;
        Ok(true)
    })?;
    Ok(())
}
