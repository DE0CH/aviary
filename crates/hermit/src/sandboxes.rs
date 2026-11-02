//! Sandboxes as magpie sees them (hermit 3.6): get-or-start, release, and the release task with its three kinds
//! (13.12: soft, forced, lost) and the destroy.

use std::sync::Arc;
use std::time::Duration;

use hermit_api::Timestamp;
use hermit_api::error::{self, ApiError, codes};
use hermit_api::people::normalise_email;
use hermit_api::sandbox::{
    ConversationReleaseReason, PutSandbox, PutSandboxRequest, ReleaseCode, ReleaseReason, ReleaseSandboxReply,
    ReleaseSandboxRequest, SandboxState,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{self, Pending, SandboxRow, bump, txt};
use crate::supervisor_client::{self, Released};
use crate::tasks::{self, Claim, Kind};
use crate::{Core, ms};

/// A database failure as the contract's 503.
#[must_use]
pub fn unavailable(e: &aviary_pg::Error) -> ApiError {
    tracing::warn!(error = %e, "database unavailable");
    ApiError::unavailable("the database is unavailable", 1)
}

/// `aviary_pg::Error` to `ApiError` for `?`.
pub trait OrUnavailable<T> {
    /// # Errors
    /// 503 `unavailable` for a database error.
    fn or_503(self) -> Result<T, ApiError>;
}

impl<T> OrUnavailable<T> for Result<T, aviary_pg::Error> {
    fn or_503(self) -> Result<T, ApiError> {
        self.map_err(|e| unavailable(&e))
    }
}

impl<T> OrUnavailable<T> for Result<T, aviary_pg::reexports::tokio_postgres::Error> {
    fn or_503(self) -> Result<T, ApiError> {
        self.map_err(|e| unavailable(&aviary_pg::Error::from(e)))
    }
}

/// The kind of a release (13.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseKind {
    Soft,
    Forced,
    /// No supervisor call: destroy at once.
    Lost,
}

impl ReleaseKind {
    #[must_use]
    pub fn of(p: &Pending) -> Self {
        if p.code() == ReleaseCode::SandboxLost {
            Self::Lost
        } else if p.force {
            Self::Forced
        } else {
            Self::Soft
        }
    }

    /// Lost > Forced > Soft: a release is never replaced by a weaker kind.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Soft => 0,
            Self::Forced => 1,
            Self::Lost => 2,
        }
    }
}

/// Whether a pending release `old` stays when `new` is asked (13.12): a stronger kind always stays (a forced one is
/// never replaced by a soft one, a lost one by a forced one), and so does the same ask again (the janitor's retry
/// keeps what became of it). Otherwise `new` replaces it.
#[must_use]
pub fn keeps(old: &Pending, new: &Pending) -> bool {
    let (ko, kn) = (ReleaseKind::of(old), ReleaseKind::of(new));
    ko.rank() > kn.rank() || (ko == kn && old.reason == new.reason)
}

enum PutStep {
    Answer(PutSandbox),
    /// A releasing sandbox (or one a forced release is about to end): wait for it to be `gone`, then try again.
    WaitGone(Uuid),
    /// A ready sandbox whose soft release a task is asking (or may have asked) its supervisor about: the `PUT`
    /// marked it cancelled on the row; wait for the task to settle (refused: the sandbox stays and is answered 200;
    /// agreed: it goes and a new one starts), then try again. `true`: this `PUT` took the release claim (a sent
    /// release no task runs) and spawns the task.
    WaitRelease(Uuid, bool),
    /// A new row with this replica's start claim (spawn the start).
    Started(PutSandbox, Uuid, bool),
}

/// `PUT /v1/conversations/{cid}/sandbox` (3.6.2): never waits for a start.
///
/// # Errors
/// 400 `bad_email`, 403 `not_allowed`, 409 `person_mismatch`, 503 `sandbox_busy` (disabled), 503 `unavailable`.
pub async fn put(core: &Arc<Core>, cid: Uuid, body: &PutSandboxRequest) -> Result<PutSandbox, ApiError> {
    let person =
        normalise_email(&body.person).ok_or_else(|| error::api(400, codes::BAD_EMAIL, "not an email address"))?;
    if !core.catalogue.allowed(&person) {
        return Err(error::not_allowed());
    }
    let enabled: bool = core
        .pool
        .get()
        .await
        .or_503()?
        .query_one("select sandboxes_enabled from controls where id = 1", &[])
        .await
        .or_503()?
        .try_get(0)
        .or_503()?;
    if !enabled {
        return Err(error::sandboxes_disabled());
    }
    let waited_since = tokio::time::Instant::now();
    loop {
        match put_once(core, cid, &person).await? {
            PutStep::Answer(a) => return Ok(a),
            PutStep::Started(a, id, owned) => {
                if let Some(row) = db::load(&core.pool, id).await.ok().flatten() {
                    core.live.apply(&row);
                }
                if owned {
                    tracing::info!(sandbox = %id, conversation = %cid, person_key = %aviary_core::log::person_key(&person), "sandbox start requested");
                    tasks::run(core, Claim::adopt(core, id, Kind::Start));
                }
                return Ok(a);
            }
            PutStep::WaitGone(id) => {
                let left = still_releasing(core, waited_since)?;
                wait_row(core, cid, id, left, |r| r.status == SandboxState::Gone).await;
            }
            PutStep::WaitRelease(id, took) => {
                if took {
                    tasks::run(core, Claim::adopt(core, id, Kind::Release));
                }
                let left = still_releasing(core, waited_since)?;
                wait_row(core, cid, id, left, |r| {
                    r.task.as_deref() != Some(Kind::Release.as_str()) || r.status != SandboxState::Ready
                })
                .await;
            }
        }
    }
}

/// What is left of the `PUT`'s 5 s wait for a release to end, or the retryable answer once it is spent.
///
/// The unique index allows one live row per conversation (10.2), so no new row can be queued behind a releasing
/// one (3.6.2): the `PUT` answers 503 `unavailable` with `retry-after: 1`, which magpie's driver retries with its
/// backoff (magpie 8.3 step 1, "other 5xx"), and a later `PUT` finds the old sandbox gone and starts a new one.
/// A releasing sandbox is never handed to magpie: its step 2 would fail the turn on `gone`.
fn still_releasing(core: &Core, since: tokio::time::Instant) -> Result<Duration, ApiError> {
    let left = core.timings.put_release_wait.saturating_sub(since.elapsed());
    if left.is_zero() {
        return Err(ApiError::unavailable(
            "the conversation's sandbox is still being released",
            1,
        ));
    }
    Ok(left)
}

async fn put_once(core: &Arc<Core>, cid: Uuid, person: &str) -> Result<PutStep, ApiError> {
    let cfg = &core.cfg;
    let s = cfg.sandbox();
    let reserve = ms(crate::config::Config::secs(s.reserve_seconds));
    let pod = core.pod.clone();
    let draining = core.is_draining();
    let claim_ms = if draining { 0 } else { core.claim_ms() };
    let (start_deadline, expires, grace) = (
        ms(crate::config::Config::secs(s.start_deadline_seconds)),
        i64::from(s.max_lifetime_hours) * 3_600_000,
        i64::from(s.lifetime_grace_minutes) * 60_000,
    );
    let (cpu, memory) = (s.cpu, i64::try_from(cfg.memory_bytes).unwrap_or(i64::MAX));
    let kid = i16::try_from(core.keys.newest()).unwrap_or(i16::MAX);
    let owner = person.to_owned();
    let c = core.clone();
    let res = aviary_pg::tx!(&core.pool, |t| -> Result<PutStep, PutError> {
        let live = t
            .query_opt("select * from sandboxes where conversation_id = $1 and status <> 'gone' for update", &[&cid])
            .await?;
        if let Some(r) = live {
            let row = SandboxRow::from_row(&r)?;
            if row.person != owner {
                return Err(PutError::Mismatch);
            }
            let pending = row.release_pending.as_ref().map(ReleaseKind::of);
            match row.status {
                SandboxState::Releasing => return Ok(PutStep::WaitGone(row.id)),
                // a forced or lost release is ending this sandbox: never hand it out
                _ if pending.is_some_and(|k| k != ReleaseKind::Soft) => return Ok(PutStep::WaitGone(row.id)),
                SandboxState::Ready | SandboxState::Starting | SandboxState::Paused | SandboxState::Unknown => {
                    // a pending soft release not yet answered is cancelled by the use (3.6.2, 13.12). One no task
                    // runs and none sent is cleared. One a task runs, or one already sent, may be out at the
                    // supervisor: it is marked cancelled, never cleared, so the task (or the takeover) stays if the
                    // supervisor refuses and goes if it agreed, and this PUT waits for that answer
                    let soft = pending == Some(ReleaseKind::Soft);
                    let release_task = row.task.as_deref() == Some(Kind::Release.as_str());
                    let sent = row.release_pending.as_ref().is_some_and(|p| p.sent);
                    let mark = soft && (release_task || sent);
                    let clear = soft && !mark;
                    let r = t
                        .query_one(
                            "update sandboxes set last_active_at = greatest(last_active_at, now()),
                               reserved_until = now() + $2::bigint * interval '1 millisecond',
                               release_pending = case when $3 then null
                                 when $4 then jsonb_set(release_pending, '{cancelled}', 'true') else release_pending end
                             where id = $1 returning *",
                            &[&row.id, &reserve, &clear, &mark],
                        )
                        .await?;
                    let row = SandboxRow::from_row(&r)?;
                    if mark && row.status == SandboxState::Ready {
                        // a sent release no task runs: this PUT runs it (the supervisor is asked again)
                        let took = !release_task && tasks::take_in(t, &c, row.id, Kind::Release).await?;
                        return Ok(PutStep::WaitRelease(row.id, took));
                    }
                    let sbx = row.to_sandbox();
                    return Ok(PutStep::Answer(if row.status == SandboxState::Ready {
                        PutSandbox::Ready(sbx)
                    } else {
                        PutSandbox::Starting(sbx)
                    }));
                }
                SandboxState::Gone => {}
            }
        }
        let id = Uuid::now_v7();
        let name = format!("sbx-{}", &id.simple().to_string()[..16]);
        t.execute(
            "insert into sandboxes (id, conversation_id, person, status, phase, name, token_kid, cpu, memory_bytes,
               task, task_owner, task_until, start_deadline, expires_at, hard_expires_at, reserved_until)
             values ($1, $2, $3, 'starting', 'capacity', $4, $5, $6, $7,
               'start', $8, now() + $9::bigint * interval '1 millisecond', now() + $10::bigint * interval '1 millisecond',
               now() + $11::bigint * interval '1 millisecond', now() + ($11::bigint + $12::bigint) * interval '1 millisecond',
               now() + $13::bigint * interval '1 millisecond')",
            &[&id, &cid, &owner, &name, &kid, &cpu, &memory, &pod, &claim_ms, &start_deadline, &expires, &grace, &reserve],
        )
        .await?;
        let row = bump(t, id).await?;
        Ok(PutStep::Started(PutSandbox::Starting(row.to_sandbox()), id, !draining))
    })
    .await;
    match res {
        Ok(step) => {
            if let PutStep::Answer(PutSandbox::Ready(s)) = &step {
                silence_check_if_quiet(core, s.id);
            }
            Ok(step)
        }
        Err(PutError::Mismatch) => {
            tracing::error!(conversation = %cid, "PUT sandbox: the live sandbox belongs to another person");
            Err(error::api(
                409,
                codes::PERSON_MISMATCH,
                "the conversation's sandbox belongs to another person",
            ))
        }
        // another replica inserted the conversation's row first: answer with it
        Err(PutError::Db(e))
            if e.code() == Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::UNIQUE_VIOLATION) =>
        {
            Box::pin(put_once(core, cid, person)).await
        }
        Err(PutError::Db(e)) => Err(unavailable(&e)),
    }
}

#[derive(Debug)]
enum PutError {
    Mismatch,
    Db(aviary_pg::Error),
}

impl From<aviary_pg::Error> for PutError {
    fn from(e: aviary_pg::Error) -> Self {
        Self::Db(e)
    }
}

/// A ready sandbox whose last report is older than 45 s gets the silence check of hermit 12 step 3 in the
/// background (the `PUT` never waits for it).
fn silence_check_if_quiet(core: &Arc<Core>, id: Uuid) {
    let c = core.clone();
    tokio::spawn(async move {
        let Ok(Some(row)) = db::load(&c.pool, id).await else {
            return;
        };
        let quiet = row
            .last_heartbeat_at
            .or(row.ready_at)
            .is_some_and(|at| OffsetDateTime::now_utc() - at > c.timings.put_silence);
        if quiet {
            crate::janitor::silence_check(&c, &row).await;
        }
    });
}

/// Waits up to `left` for row `id` to satisfy `done` (woken by its conversation's notifications, re-read every
/// `recheck`).
async fn wait_row(core: &Core, cid: Uuid, id: Uuid, left: Duration, done: impl Fn(&SandboxRow) -> bool) {
    let mut w = core.listener.waiter(db::CHANNEL, &cid.to_string());
    let end = tokio::time::Instant::now() + left;
    loop {
        if let Ok(Some(r)) = db::load(&core.pool, id).await
            && (r.status == SandboxState::Gone || done(&r))
        {
            return;
        }
        if tokio::time::Instant::now() >= end {
            return;
        }
        let _ = tokio::time::timeout_at(end.min(tokio::time::Instant::now() + core.timings.recheck), w.changed()).await;
    }
}

/// `POST /v1/conversations/{cid}/sandbox/release` (3.6.4): answers at once; the release runs in the background.
///
/// # Errors
/// 503 `unavailable`.
pub async fn release_conversation(
    core: &Arc<Core>,
    cid: Uuid,
    body: ReleaseSandboxRequest,
) -> Result<ReleaseSandboxReply, ApiError> {
    let force = !matches!(body.reason, ConversationReleaseReason::Requested);
    let Some(row) = db::latest_of(&core.pool, cid)
        .await
        .or_503()?
        .filter(SandboxRow::is_live)
    else {
        return Ok(ReleaseSandboxReply {
            released: 0,
            sandbox: None,
        });
    };
    request(core, row.id, body.reason.into(), force, None).await.or_503()?;
    let now = db::load(&core.pool, row.id).await.or_503()?.unwrap_or(row);
    Ok(ReleaseSandboxReply {
        released: 1,
        sandbox: Some(now.to_sandbox()),
    })
}

/// Asks for a release of sandbox `id` (13.12): records `release_pending` and, unless another task holds the row,
/// takes the `release` claim and runs the release. A start or migration that holds the row performs it when it
/// ends. A stronger kind replaces a weaker one and is never replaced by it (Lost > Forced > Soft, [`keeps`]).
/// Idempotent.
///
/// # Errors
/// A database error.
pub async fn request(
    core: &Arc<Core>,
    id: Uuid,
    reason: ReleaseReason,
    force: bool,
    note: Option<String>,
) -> Result<(), aviary_pg::Error> {
    let c = core.clone();
    let took = aviary_pg::tx!(&core.pool, |t| -> Result<bool, aviary_pg::Error> {
        request_in(t, &c, id, reason, force, note.clone()).await
    })
    .await?;
    if took {
        run_requested(core, id);
    }
    Ok(())
}

/// [`request`]'s change inside the caller's transaction `t` (an admin route writes its audit row in the same one).
/// `true` when it took the `release` claim: once `t` commits, the caller runs the release with [`run_requested`].
///
/// # Errors
/// A database error.
pub async fn request_in(
    t: &aviary_pg::Tx<'_>,
    core: &Arc<Core>,
    id: Uuid,
    reason: ReleaseReason,
    force: bool,
    note: Option<String>,
) -> Result<bool, aviary_pg::Error> {
    // provisional: a forced release's deadline is written again when the row enters `releasing` (set_releasing)
    let deadline = Timestamp::from_unix_millis(Timestamp::now().unix_millis() + ms(core.timings.release_deadline))
        .unwrap_or_else(Timestamp::now);
    let pending = Pending {
        reason,
        note: note.map(|n| db::clip(&n, 200)),
        force,
        deadline_at: deadline,
        asked_at: Timestamp::now(),
        refusals: 0,
        sent: false,
        cancelled: false,
    };
    let Some(r) = t
        .query_opt(
            "select * from sandboxes where id = $1 and status <> 'gone' for update",
            &[&id],
        )
        .await?
    else {
        return Ok(false);
    };
    let row = SandboxRow::from_row(&r)?;
    let keep_old = row.release_pending.as_ref().is_some_and(|old| keeps(old, &pending));
    if !keep_old {
        let mut pending = pending.clone();
        // a soft release that replaces a soft one already sent keeps that fact: the supervisor may have agreed
        if let Some(old) = &row.release_pending
            && ReleaseKind::of(old) == ReleaseKind::Soft
            && ReleaseKind::of(&pending) == ReleaseKind::Soft
        {
            pending.sent = old.sent;
        }
        let v = serde_json::to_value(&pending).unwrap_or_default();
        t.execute("update sandboxes set release_pending = $2 where id = $1", &[&id, &v])
            .await?;
    }
    // a starting row's start task honours the pending release (11.2); it is never cut over to a release claim
    if row.status == SandboxState::Starting {
        return Ok(false);
    }
    tasks::take_in(t, core, id, Kind::Release).await
}

/// Runs the release whose claim [`request_in`] took, once its transaction committed.
pub fn run_requested(core: &Arc<Core>, id: Uuid) {
    tasks::run(core, Claim::adopt(core, id, Kind::Release));
}

/// The release task (13.12), run under the row's `release` claim; a replica that takes over resumes it from
/// `release_pending` and the row's state.
pub async fn run_release(core: Arc<Core>, claim: Claim) {
    if let Err(e) = release_steps(&core, &claim).await {
        tracing::warn!(sandbox = %claim.id, error = %e, "release interrupted (resumed by the takeover sweep)");
    }
}

/// What one pass of the release task came to.
enum Pass {
    Done,
    /// The row's pending release changed under the task (a forced one replaced the soft one it ran, or a `PUT`
    /// cancelled it): run the task again from the row, still under the claim.
    Again,
}

async fn release_steps(core: &Arc<Core>, claim: &Claim) -> Result<(), aviary_pg::Error> {
    // a pass ends Done or changes the row (Again); a few passes cover any sequence of replacements
    for _ in 0..4 {
        if !claim.held() {
            return Ok(());
        }
        if let Pass::Done = release_pass(core, claim).await? {
            return Ok(());
        }
    }
    tasks::clear(core, claim).await?;
    Ok(())
}

async fn release_pass(core: &Arc<Core>, claim: &Claim) -> Result<Pass, aviary_pg::Error> {
    let Some(row) = db::load(&core.pool, claim.id).await? else {
        return Ok(Pass::Done);
    };
    if !row.is_live() {
        return Ok(Pass::Done);
    }
    let Some(p) = row.release_pending.clone() else {
        if row.status == SandboxState::Releasing {
            // a release cut before it recorded what it was: finish it as stuck would, keeping its code
            destroy_and_gone(
                core,
                claim,
                &row,
                ReleaseReason::Stuck,
                None,
                ReleaseCode::SandboxReleased,
                "release resumed without its record",
            )
            .await?;
            return Ok(Pass::Done);
        }
        // nothing to do (a PUT cancelled a soft release before it was sent)
        tasks::clear(core, claim).await?;
        return Ok(Pass::Done);
    };
    let reason = p.reason;
    if ReleaseKind::of(&p) == ReleaseKind::Soft && p.cancelled && !p.sent && row.status != SandboxState::Releasing {
        // a PUT cancelled it before it was sent: nothing to undo
        return drop_cancelled(core, claim, &p).await;
    }
    match ReleaseKind::of(&p) {
        ReleaseKind::Lost => {
            destroy_and_gone(
                core,
                claim,
                &row,
                reason,
                p.note.clone(),
                ReleaseCode::SandboxLost,
                &format!("sandbox lost: {reason}"),
            )
            .await?;
        }
        ReleaseKind::Forced => {
            // the deadline runs from the moment the row enters `releasing` (set_releasing writes it); a row that was
            // already `releasing` when this task resumed keeps its own and skips the call once it passed (13.12)
            let row = if row.status == SandboxState::Releasing {
                row
            } else {
                set_releasing(core, claim, &row).await?
            };
            let deadline = row.release_pending.as_ref().map_or(p.deadline_at, |q| q.deadline_at);
            let left = Duration::from_millis(
                u64::try_from(deadline.unix_millis() - Timestamp::now().unix_millis()).unwrap_or(0),
            );
            if !left.is_zero() && row.status == SandboxState::Releasing && claim.held() {
                match supervisor_client::release(core, &row, reason, true, left).await {
                    Released::Done => {}
                    other => {
                        tracing::warn!(sandbox = %row.id, outcome = ?other, "forced release: the supervisor did not confirm (destroying at the deadline)");
                    }
                }
            }
            destroy_and_gone(
                core,
                claim,
                &row,
                reason,
                p.note.clone(),
                p.code(),
                &format!("released: {reason}"),
            )
            .await?;
        }
        ReleaseKind::Soft => return soft(core, claim, row, &p).await,
    }
    Ok(Pass::Done)
}

/// The row's pending release, read under this replica's claim (`None`: the claim is not ours any more).
async fn owned_pending(core: &Core, claim: &Claim) -> Result<Option<Option<Pending>>, aviary_pg::Error> {
    let r = core
        .pool
        .get()
        .await?
        .query_opt(
            "select release_pending from sandboxes where id = $1 and task = $2 and task_owner = $3",
            &[&claim.id, &claim.kind.as_str(), &claim.owner],
        )
        .await?;
    let Some(r) = r else {
        claim.mark_lost();
        return Ok(None);
    };
    let v: Option<serde_json::Value> = r.try_get(0)?;
    Ok(Some(v.and_then(|v| serde_json::from_value(v).ok())))
}

/// Before the soft release `p` is sent: under the claim, the row must still carry the same ask, not cancelled
/// before it was ever sent; it is then marked `sent`, so a takeover knows the supervisor may have agreed. `None`:
/// do not send (the claim is gone, or the row changed: [`Pass::Again`] runs what it carries now).
async fn mark_sent(core: &Core, claim: &Claim, p: &Pending) -> Result<Option<Pending>, aviary_pg::Error> {
    if !claim.held() {
        return Ok(None);
    }
    let (id, owner, kind, mine) = (claim.id, claim.owner.clone(), claim.kind.as_str(), p.clone());
    let now = aviary_pg::tx!(&core.pool, |t| -> Result<Option<Option<Pending>>, aviary_pg::Error> {
        let Some(r) = t
            .query_opt(
                "select release_pending from sandboxes where id = $1 and task = $2 and task_owner = $3 for update",
                &[&id, &kind, &owner],
            )
            .await?
        else {
            return Ok(None);
        };
        let v: Option<serde_json::Value> = r.try_get(0)?;
        let now: Option<Pending> = v.and_then(|v| serde_json::from_value(v).ok());
        let Some(mut q) = now.filter(|q| q.same_ask(&mine) && (q.sent || !q.cancelled)) else {
            return Ok(Some(None));
        };
        if !q.sent {
            q.sent = true;
            t.execute(
                "update sandboxes set release_pending = jsonb_set(release_pending, '{sent}', 'true') where id = $1",
                &[&id],
            )
            .await?;
        }
        Ok(Some(Some(q)))
    })
    .await?;
    match now {
        None => {
            claim.mark_lost();
            Ok(None)
        }
        Some(q) => Ok(q),
    }
}

/// A soft release a `PUT` cancelled before it was sent: cleared with the claim.
async fn drop_cancelled(core: &Core, claim: &Claim, p: &Pending) -> Result<Pass, aviary_pg::Error> {
    let (id, owner, kind, mine) = (claim.id, claim.owner.clone(), claim.kind.as_str(), p.clone());
    let done = aviary_pg::tx!(&core.pool, |t| -> Result<Option<bool>, aviary_pg::Error> {
        let Some(r) = t
            .query_opt(
                "select release_pending from sandboxes where id = $1 and task = $2 and task_owner = $3 for update",
                &[&id, &kind, &owner],
            )
            .await?
        else {
            return Ok(None);
        };
        let v: Option<serde_json::Value> = r.try_get(0)?;
        let now: Option<Pending> = v.and_then(|v| serde_json::from_value(v).ok());
        if !now
            .as_ref()
            .is_some_and(|q| q.same_ask(&mine) && q.cancelled && !q.sent)
        {
            // it changed meanwhile: run what the row carries now
            return Ok(Some(false));
        }
        t.execute(
            "update sandboxes set release_pending = null, task = null, task_owner = null, task_until = null
             where id = $1",
            &[&id],
        )
        .await?;
        Ok(Some(true))
    })
    .await?;
    match done {
        None => {
            claim.mark_lost();
            Ok(Pass::Done)
        }
        Some(true) => Ok(Pass::Done),
        Some(false) => Ok(Pass::Again),
    }
}

async fn soft(core: &Arc<Core>, claim: &Claim, row: SandboxRow, p: &Pending) -> Result<Pass, aviary_pg::Error> {
    if row.status == SandboxState::Releasing {
        // the supervisor already agreed before a takeover: finish the destroy
        destroy_and_gone(
            core,
            claim,
            &row,
            p.reason,
            p.note.clone(),
            p.code(),
            &format!("released: {}", p.reason),
        )
        .await?;
        return Ok(Pass::Done);
    }
    let deadline = core.timings.release_deadline;
    let mut outcome = Released::Failed(String::new());
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(core.timings.release_retry).await;
            if !claim.held() {
                return Ok(Pass::Done);
            }
        }
        // cancelled by a PUT before it was sent, or replaced meanwhile: never send a stale soft release
        if mark_sent(core, claim, p).await?.is_none() {
            return Ok(if claim.held() { Pass::Again } else { Pass::Done });
        }
        outcome = supervisor_client::release(core, &row, p.reason, false, deadline).await;
        if !matches!(outcome, Released::Failed(_)) {
            break;
        }
    }
    match outcome {
        Released::Done => {
            // the supervisor agreed: the sandbox goes, whatever was asked meanwhile. A PUT that cancelled the release
            // after it was sent waits for this and then starts a new sandbox; a forced release that replaced it
            // names the reason it ends with.
            let row = set_releasing(core, claim, &row).await?;
            let now = row
                .release_pending
                .clone()
                .filter(|q| ReleaseKind::of(q) != ReleaseKind::Soft);
            let q = now.as_ref().unwrap_or(p);
            destroy_and_gone(
                core,
                claim,
                &row,
                q.reason,
                q.note.clone(),
                q.code(),
                &format!("released: {}", q.reason),
            )
            .await?;
            Ok(Pass::Done)
        }
        Released::Busy(state) => refused(core, claim, &row, p, &state).await,
        Released::Failed(why) => {
            // a forced or lost release that replaced this one while it was out runs now (nobody else will)
            let Some(now) = owned_pending(core, claim).await? else {
                return Ok(Pass::Done);
            };
            if now.as_ref().is_some_and(|q| ReleaseKind::of(q) != ReleaseKind::Soft) {
                return Ok(Pass::Again);
            }
            if core.providers.fabric.gateway_settled()
                && !supervisor_client::alive_twice(core, &row, core.timings.health_gap).await
            {
                tracing::info!(sandbox = %row.id, error = %why, "release: the supervisor is gone");
                // the release the row carries now names how it ends (one asked during the health checks too)
                let Some(now) = owned_pending(core, claim).await? else {
                    return Ok(Pass::Done);
                };
                let q = now.as_ref().unwrap_or(p);
                destroy_and_gone(
                    core,
                    claim,
                    &row,
                    q.reason,
                    q.note.clone(),
                    q.code(),
                    &format!("released ({}); the supervisor did not answer", q.reason),
                )
                .await?;
                return Ok(Pass::Done);
            }
            if owned_pending(core, claim)
                .await?
                .is_some_and(|q| q.is_some_and(|q| ReleaseKind::of(&q) != ReleaseKind::Soft))
            {
                return Ok(Pass::Again);
            }
            // nothing decided: the pending release stays and is retried by the janitor
            tracing::info!(sandbox = %row.id, error = %why, "release: no answer; retried later");
            tasks::clear(core, claim).await?;
            Ok(Pass::Done)
        }
    }
}

/// The supervisor refused the soft release `p` (409). Under the claim, the row's pending release is re-read: if it
/// is still `p`, it is cleared (kept for `requested`, unless a `PUT` cancelled it); a soft one that replaced it is
/// left as it is; a forced one that replaced it is run now ([`Pass::Again`]), since nobody else will.
async fn refused(
    core: &Arc<Core>,
    claim: &Claim,
    row: &SandboxRow,
    p: &Pending,
    state: &str,
) -> Result<Pass, aviary_pg::Error> {
    let mut kept = p.clone();
    kept.refusals += 1;
    // refused: nothing is out at the supervisor any more
    kept.sent = false;
    let kept = (p.reason == ReleaseReason::Requested).then(|| serde_json::to_value(&kept).unwrap_or_default());
    let data = serde_json::json!({"reason": p.reason.as_str(), "state": state});
    let (id, owner, host, mine, reason) = (row.id, claim.owner.clone(), row.host.clone(), p.clone(), p.reason);
    let pass = aviary_pg::tx!(&core.pool, |t| -> Result<Pass, aviary_pg::Error> {
        let Some(r) = t
            .query_opt(
                "select release_pending from sandboxes where id = $1 and task = 'release' and task_owner = $2 for update",
                &[&id, &owner],
            )
            .await?
        else {
            return Ok(Pass::Done);
        };
        let now: Option<serde_json::Value> = r.try_get(0)?;
        let now: Option<Pending> = now.and_then(|v| serde_json::from_value(v).ok());
        if now.as_ref().is_some_and(|q| ReleaseKind::of(q) != ReleaseKind::Soft) {
            return Ok(Pass::Again);
        }
        let next = if now.as_ref().is_some_and(|q| q.same_ask(&mine)) {
            if now.as_ref().is_some_and(|q| q.cancelled) {
                None
            } else {
                kept.clone()
            }
        } else {
            now.map(|q| serde_json::to_value(&q).unwrap_or_default())
        };
        t.execute(
            "update sandboxes set release_pending = $2, last_active_at = now(), task = null, task_owner = null, task_until = null
             where id = $1",
            &[&id, &next],
        )
        .await?;
        t.execute("insert into host_events (kind, host, sandbox_id, data) values ('release-refused', $1, $2, $3)", &[&host, &id, &data])
            .await?;
        Ok(Pass::Done)
    })
    .await?;
    if let Pass::Done = pass {
        tracing::info!(sandbox = %row.id, reason = %reason, state, "release refused");
    }
    Ok(pass)
}

/// Moves the row to `releasing` under the claim and starts its forced deadline now (`deadlineAt`, 13.12: "destroy
/// after the 200 or at `deadlineAt` (30 s)" counts from here, not from when the release was asked).
async fn set_releasing(core: &Core, claim: &Claim, row: &SandboxRow) -> Result<SandboxRow, aviary_pg::Error> {
    let (id, owner, kind) = (row.id, claim.owner.clone(), claim.kind.as_str());
    let deadline = Timestamp::from_unix_millis(Timestamp::now().unix_millis() + ms(core.timings.release_deadline))
        .unwrap_or_else(Timestamp::now);
    let deadline = serde_json::to_value(deadline).unwrap_or_default();
    let updated = aviary_pg::tx!(&core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
        let n = t
            .execute(
                "update sandboxes set status = 'releasing', phase = null,
                   release_pending = case when release_pending is null then null
                     else jsonb_set(release_pending, '{deadlineAt}', $4) end
                 where id = $1 and status <> 'gone' and task = $2 and task_owner = $3",
                &[&id, &kind, &owner, &deadline],
            )
            .await?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(bump(t, id).await?))
    })
    .await?;
    if let Some(r) = updated {
        core.live.apply(&r);
        Ok(r)
    } else {
        claim.mark_lost();
        Ok(row.clone())
    }
}

/// Destroys a sandbox and records it `gone` (13.12 "Destroy"): the console tail, sandboxd `DELETE` on its host
/// (and on a migration target), then one transaction under the claim.
///
/// # Errors
/// A database error.
pub async fn destroy_and_gone(
    core: &Core,
    claim: &Claim,
    row: &SandboxRow,
    reason: ReleaseReason,
    note: Option<String>,
    code: ReleaseCode,
    message: &str,
) -> Result<(), aviary_pg::Error> {
    destroy_and_record(
        core,
        claim,
        row,
        Ending {
            reason,
            note: note.as_deref(),
            code,
            message,
        },
        None,
    )
    .await
}

/// How a sandbox ends ([`destroy_and_record`]).
#[derive(Clone, Copy, Debug)]
pub struct Ending<'a> {
    pub reason: ReleaseReason,
    pub note: Option<&'a str>,
    pub code: ReleaseCode,
    pub message: &'a str,
}

/// A failed start's `sandbox_starts` row, written in the same transaction as the `gone`.
#[derive(Clone, Debug)]
pub struct FailedStart {
    pub ms: i32,
}

/// [`destroy_and_gone`], also recording a failed start. The destroy runs only under [`Claim::guard`] (10.3: a
/// destroy runs only under the claim; 20.2 step 5: a replica whose claims were handed back makes no effect), and
/// the failed start is written only with the guarded `gone`.
///
/// # Errors
/// A database error.
pub async fn destroy_and_record(
    core: &Core,
    claim: &Claim,
    row: &SandboxRow,
    end: Ending<'_>,
    start: Option<FailedStart>,
) -> Result<(), aviary_pg::Error> {
    let Some(_effect) = claim.guard(core).await? else {
        return Ok(());
    };
    destroy(core, row).await;
    let (id, owner, kind) = (row.id, claim.owner.clone(), claim.kind.as_str());
    let (reason, code) = (end.reason, end.code);
    let (reason_s, code_s, msg) = (txt(reason), txt(code), db::message(end.message));
    let note = end.note.map(|n| db::clip(n, 200));
    let (cid, person, host) = (row.conversation_id, row.person.clone(), row.host.clone());
    let gone = aviary_pg::tx!(&core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
        let n = t
            .execute(
                "update sandboxes set status = 'gone', phase = null, released_at = now(), release_reason = $4,
                   release_note = $5, release_code = $6, release_message = $7, release_pending = null, busy = false,
                   task = null, task_owner = null, task_until = null
                 where id = $1 and status <> 'gone' and task = $2 and task_owner = $3",
                &[&id, &kind, &owner, &reason_s, &note, &code_s, &msg],
            )
            .await?;
        if n == 0 {
            return Ok(None);
        }
        if let Some(f) = &start {
            t.execute(
                "insert into sandbox_starts (ok, ms, sandbox_id, conversation_id, person, host, reason, error)
                 values (false, $1, $2, $3, $4, $5, $6, $7)",
                &[&f.ms, &id, &cid, &person, &host, &reason_s, &msg],
            )
            .await?;
        }
        Ok(Some(bump(t, id).await?))
    })
    .await?;
    match gone {
        Some(r) => {
            core.live.apply(&r);
            let lost = code == ReleaseCode::SandboxLost;
            tracing::info!(sandbox = %r.id, conversation = %r.conversation_id, reason = %reason, code = %code,
                "{}", if lost { "sandbox lost" } else { "sandbox released" });
        }
        None => claim.mark_lost(),
    }
    Ok(())
}

/// The console tail and sandboxd `DELETE` (no row change). A missing sandbox is fine; errors are logged.
pub async fn destroy(core: &Core, row: &SandboxRow) {
    let fabric = &core.providers.fabric;
    crate::console::capture(core, row).await;
    if (row.host.is_some() || row.ip.is_some())
        && let Err(e) = fabric
            .destroy(row.host.as_deref(), row.migrating_to.as_deref(), &row.name)
            .await
    {
        tracing::warn!(sandbox = %row.id, error = %e, "destroy failed (the stuck rule retries)");
    }
}

/// The idle clock after a report: the later of the last `PUT` and the end of the work, never in the future.
#[must_use]
pub fn idle_from(last_active: OffsetDateTime, idle_since: Option<Timestamp>, now: OffsetDateTime) -> OffsetDateTime {
    let ended = idle_since.map_or(now, |t| t.datetime().min(now));
    last_active.max(ended)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn idle_clock() {
        let put = datetime!(2026-11-02 04:00:00 UTC);
        let now = datetime!(2026-11-02 04:10:00 UTC);
        // the work ended after the last PUT: the clock starts when it ended
        let ended = Timestamp::from(datetime!(2026-11-02 04:05:00 UTC));
        assert_eq!(idle_from(put, Some(ended), now), datetime!(2026-11-02 04:05:00 UTC));
        // never earlier than the last PUT
        let early = Timestamp::from(datetime!(2026-11-02 03:00:00 UTC));
        assert_eq!(idle_from(put, Some(early), now), put);
        // never in the future
        let future = Timestamp::from(datetime!(2026-11-02 05:00:00 UTC));
        assert_eq!(idle_from(put, Some(future), now), now);
    }

    #[test]
    fn kinds() {
        let p = |reason, force| Pending {
            reason,
            note: None,
            force,
            deadline_at: Timestamp::now(),
            asked_at: Timestamp::now(),
            refusals: 0,
            sent: false,
            cancelled: false,
        };
        assert_eq!(ReleaseKind::of(&p(ReleaseReason::Idle, false)), ReleaseKind::Soft);
        assert_eq!(ReleaseKind::of(&p(ReleaseReason::Deleted, true)), ReleaseKind::Forced);
        assert_eq!(ReleaseKind::of(&p(ReleaseReason::HostLost, false)), ReleaseKind::Lost);
        assert_eq!(ReleaseKind::of(&p(ReleaseReason::Unhealthy, true)), ReleaseKind::Lost);
    }

    #[test]
    fn a_release_is_never_replaced_by_a_weaker_kind() {
        let p = |reason, force| Pending {
            reason,
            note: None,
            force,
            deadline_at: Timestamp::now(),
            asked_at: Timestamp::now(),
            refusals: 0,
            sent: false,
            cancelled: false,
        };
        let soft = p(ReleaseReason::Idle, false);
        let forced = p(ReleaseReason::Lifetime, true);
        let deleted = p(ReleaseReason::Deleted, true);
        let lost = p(ReleaseReason::HostLost, false);
        // Lost > Forced > Soft
        assert!(keeps(&lost, &forced), "a forced release replaced a lost one");
        assert!(keeps(&lost, &deleted));
        assert!(keeps(&lost, &soft));
        assert!(keeps(&forced, &soft));
        assert!(!keeps(&forced, &lost), "a lost release may replace a forced one");
        assert!(!keeps(&soft, &forced));
        assert!(!keeps(&soft, &lost));
        // the same ask again keeps what became of it; another reason of the same kind replaces it
        assert!(keeps(&forced, &p(ReleaseReason::Lifetime, true)));
        assert!(!keeps(&forced, &deleted));
        assert!(keeps(&soft, &p(ReleaseReason::Idle, false)));
        assert!(!keeps(&soft, &p(ReleaseReason::Pool, false)));
    }
}
