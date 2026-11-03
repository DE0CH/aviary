//! The fence and the release (cutover 7.8, DESIGN 9.4), and the fenced wait of native mutations (cutover 6.4).
//!
//! The importer fences a legacy conversation with no open ticket ([`fence`]), re-stats every fingerprint outside any
//! transaction, then releases it ([`release`], or [`release_in`] inside its own transaction) or lifts the fence
//! ([`unfence`]). A fence is a lease of [`limits::FENCE_LEASE`] (3 s): once older, it holds nothing (a dead importer
//! leader never keeps writes waiting), cannot be released, and the next writer that finds it clears it
//! ([`lapse`]). While a conversation is fenced, a native mutation waits for the release and then proceeds natively
//! ([`native`]); a ticket call waits the same way ([`crate::legacy::tickets`]).

use std::future::Future;
use std::time::Duration;

use aviary_pg::{Pool, Tx};
use magpie_api::errors::ErrorCode;
use magpie_api::events::NameSource;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::Magpie;
use crate::changes;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::fanout;
use crate::limits;

/// How often a draining replica re-reads a fenced conversation's owner ([`await_unfenced`]).
const DRAIN_POLL: Duration = Duration::from_millis(50);

fn lease_secs() -> f64 {
    limits::FENCE_LEASE.as_secs_f64()
}

/// Who may write a conversation now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// magpie (not migrating).
    Native,
    /// The old stack, through pigeon's legacy upstream (migrating, no fence or a lapsed one).
    Legacy,
    /// Nobody: fenced, for at most `remaining` more.
    Fenced {
        /// What is left of the fence's lease.
        remaining: Duration,
    },
}

/// A conversation's owner and its person's key, read without a lock; `None` when it does not exist.
///
/// # Errors
/// When the statement fails.
pub async fn owner_of(db: &impl Db, id: Uuid) -> Result<Option<(String, String, Owner)>> {
    let Some(r) = db
        .query_opt(
            "select c.person, p.person_key, c.migrating,
                    extract(epoch from c.legacy_fence_at + make_interval(secs => $2) - clock_timestamp())::float8
             from conversations c join persons p on p.person = c.person where c.id = $1",
            &[&id, &lease_secs()],
        )
        .await?
    else {
        return Ok(None);
    };
    let migrating: bool = r.get(2);
    let left: Option<f64> = r.get(3);
    let owner = match (migrating, left) {
        (false, _) => Owner::Native,
        (true, Some(s)) if s > 0.0 => Owner::Fenced {
            remaining: Duration::from_secs_f64(s),
        },
        (true, _) => Owner::Legacy,
    };
    Ok(Some((r.get(0), r.get(1), owner)))
}

/// Whether a native write of `person` to `id` can be admitted now or once a fence ends: the person's conversation,
/// native or fenced. `false` for an unfenced legacy conversation (its write answers 409 `route: "legacy"` at once),
/// and for one that is missing or another person's (the write's own 404).
///
/// # Errors
/// When the statement fails.
pub async fn may_go_native(db: &impl Db, person: &str, id: Uuid) -> Result<bool> {
    Ok(owner_of(db, id)
        .await?
        .is_some_and(|(p, _, o)| p == person && o != Owner::Legacy))
}

/// What [`fence`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fence {
    /// Fenced at this instant (database time).
    Fenced(OffsetDateTime),
    /// The conversation is not migrating (or does not exist): nothing to fence.
    NotMigrating,
    /// A fence that has not lapsed is already set.
    AlreadyFenced,
    /// A ticket is open: the old stack may be writing.
    OpenTicket,
}

/// The fence transaction (cutover 7.8): only a migrating conversation with no live fence and no open ticket. A
/// lapsed fence is replaced. Change `conversation_upserted`.
///
/// # Errors
/// When a statement fails.
pub async fn fence(pool: &Pool, id: Uuid) -> Result<Fence> {
    aviary_pg::tx!(pool, |t| -> Result<Fence> {
        let Some(r) = t
            .query_opt(
                "select person, migrating, legacy_fence_at > clock_timestamp() - make_interval(secs => $2)
                 from conversations where id = $1 for update",
                &[&id, &lease_secs()],
            )
            .await?
        else {
            return Ok(Fence::NotMigrating);
        };
        let person: String = r.get(0);
        if !r.get::<_, bool>(1) {
            return Ok(Fence::NotMigrating);
        }
        if r.get::<_, Option<bool>>(2) == Some(true) {
            return Ok(Fence::AlreadyFenced);
        }
        if open_ticket(t, id).await? {
            return Ok(Fence::OpenTicket);
        }
        let at: OffsetDateTime = t
            .query_one(
                "update conversations set legacy_fence_at = clock_timestamp() where id = $1 returning legacy_fence_at",
                &[&id],
            )
            .await?
            .get(0);
        changes::upserted(t, &person, id).await?;
        Ok(Fence::Fenced(at))
    })
    .await
}

async fn open_ticket(t: &Tx<'_>, id: Uuid) -> Result<bool> {
    Ok(t.query_opt(
        "select 1 from legacy_tickets where conversation_id = $1 and done_at is null limit 1",
        &[&id],
    )
    .await?
    .is_some())
}

/// What the release stores on the conversation (DESIGN 9.4): the old child's current model, and the old title's
/// `name_source` (mapped by the importer so that a title the person sees is never retitled).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Release {
    /// The old child's model at release; `None` keeps the imported one.
    pub model: Option<String>,
    /// `None` keeps a provisional title overwritable by the title model.
    pub name_source: Option<NameSource>,
}

/// Why a release did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReleased {
    /// Not migrating (already released, or no such conversation).
    NotMigrating,
    /// No fence, or a lapsed one: a writer may have taken a ticket since.
    NotFenced,
    /// A ticket is open.
    OpenTicket,
}

/// A release that happened inside a transaction: how long the fence was held. Recorded on `magpie_fence_seconds`
/// with [`Released::record`] once that transaction has committed (a rolled-back release is no release).
#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use = "record it on magpie_fence_seconds once the release transaction has committed"]
pub struct Released {
    /// Fence to release, in seconds (by the clock at the release).
    pub held: f64,
}

impl Released {
    /// Observes the held time on `metrics`' `magpie_fence_seconds` (the gate of DESIGN 9.9 and cutover 10.3's A5
    /// checklist).
    pub fn record(self, metrics: &super::Metrics) {
        metrics.fence_released(self.held);
    }
}

/// The release, inside the caller's transaction (the importer adds its upload-ingestion rows and its
/// `import_sources` update): locks the row, requires the conversation migrating with a live fence and no open
/// ticket, then makes it native (`migrating = false`, the fence cleared, `run_state = 'idle'`, the release-time model
/// and `name_source`) with change `conversation_upserted`. Answers how long the fence was held, which the caller
/// records with [`Released::record`] after its commit, on the `magpie::legacy::Metrics` its process registers (the
/// import Deployment's own `/metrics`, where every release runs).
///
/// The lease and the held time are judged by the clock when this runs (`clock_timestamp()`), not the transaction's
/// start, so work done earlier in the caller's transaction counts against the 3 s. The importer calls this **last**
/// in its release transaction, after the upload-ingestion pass has written its rows (they need no lock on the
/// conversation): the row lock, which writers queued in the fence wait on, is then held only for the release itself.
///
/// # Errors
/// When a statement fails.
pub async fn release_in(t: &Tx<'_>, id: Uuid, r: &Release) -> Result<Result<Released, NotReleased>> {
    let Some(row) = t
        .query_opt(
            "select person, migrating, legacy_fence_at > clock_timestamp() - make_interval(secs => $2),
                    extract(epoch from clock_timestamp() - legacy_fence_at)::float8
             from conversations where id = $1 for update",
            &[&id, &lease_secs()],
        )
        .await?
    else {
        return Ok(Err(NotReleased::NotMigrating));
    };
    let person: String = row.get(0);
    if !row.get::<_, bool>(1) {
        return Ok(Err(NotReleased::NotMigrating));
    }
    if row.get::<_, Option<bool>>(2) != Some(true) {
        return Ok(Err(NotReleased::NotFenced));
    }
    if open_ticket(t, id).await? {
        return Ok(Err(NotReleased::OpenTicket));
    }
    let held: f64 = row.get::<_, Option<f64>>(3).unwrap_or(0.0);
    let name_source = r.name_source.map(|n| match n {
        NameSource::Title => "title",
        NameSource::User => "user",
    });
    t.execute(
        "update conversations set migrating = false, legacy_fence_at = null, run_state = 'idle',
                model = coalesce($2, model), name_source = coalesce($3, name_source)
         where id = $1",
        &[&id, &r.model, &name_source],
    )
    .await?;
    changes::upserted(t, &person, id).await?;
    Ok(Ok(Released { held }))
}

/// The release transaction of cutover 7.8 on its own: [`release_in`] plus `import_sources.released_at` and
/// `release_reason` (`idle` or `forced`). The fence time goes to `magpie_fence_seconds` of `metrics` after the commit;
/// answers it.
///
/// # Errors
/// When a statement fails.
pub async fn release(
    pool: &Pool,
    id: Uuid,
    r: &Release,
    reason: &str,
    metrics: &super::Metrics,
) -> Result<Result<f64, NotReleased>> {
    let out = aviary_pg::tx!(pool, |t| -> Result<Result<Released, NotReleased>> {
        let out = release_in(t, id, r).await?;
        if out.is_ok() {
            t.execute(
                "update import_sources set released_at = now(), release_reason = $2 where conversation_id = $1",
                &[&id, &reason],
            )
            .await?;
        }
        Ok(out)
    })
    .await?;
    Ok(out.map(|r| {
        r.record(metrics);
        r.held
    }))
}

/// Lifts a fence (a fingerprint changed inside it, cutover 7.8): writes waiting in it then go through the legacy
/// upstream. Whether a fence was lifted.
///
/// # Errors
/// When a statement fails.
pub async fn unfence(pool: &Pool, id: Uuid) -> Result<bool> {
    aviary_pg::tx!(pool, |t| -> Result<bool> {
        let Some(r) = t
            .query_opt(
                "update conversations set legacy_fence_at = null
                 where id = $1 and migrating and legacy_fence_at is not null returning person",
                &[&id],
            )
            .await?
        else {
            return Ok(false);
        };
        changes::upserted(t, &r.get::<_, String>(0), id).await?;
        Ok(true)
    })
    .await
}

/// Clears a lapsed fence (one older than its lease); whether one was cleared.
///
/// # Errors
/// When a statement fails.
pub async fn lapse(pool: &Pool, id: Uuid) -> Result<bool> {
    aviary_pg::tx!(pool, |t| -> Result<bool> { lapse_in(t, id).await }).await
}

/// [`lapse`] inside the caller's transaction (after its row lock).
///
/// # Errors
/// When a statement fails.
pub async fn lapse_in(t: &Tx<'_>, id: Uuid) -> Result<bool> {
    let Some(r) = t
        .query_opt(
            "update conversations set legacy_fence_at = null
             where id = $1 and migrating and legacy_fence_at <= clock_timestamp() - make_interval(secs => $2)
             returning person",
            &[&id, &lease_secs()],
        )
        .await?
    else {
        return Ok(false);
    };
    tracing::info!(conversation = %id, "a lapsed fence was cleared");
    changes::upserted(t, &r.get::<_, String>(0), id).await?;
    Ok(true)
}

/// Waits while `id` is fenced, at most what is left of the fence's lease (so never more than 3 s), woken by the
/// person's change feed (the release, an unfence and a lapse each write a change; a draining replica polls instead). The owner then; a fence that
/// lapsed meanwhile is cleared and answers [`Owner::Legacy`]. `None` when the conversation does not exist.
///
/// # Errors
/// When a statement fails.
pub async fn await_unfenced(app: &Magpie, id: Uuid) -> Result<Option<Owner>> {
    let pool = &app.pool;
    let read = || async move { Ok::<_, Error>(owner_of(&pool.get().await?, id).await?.map(|(_, _, o)| o)) };
    let Some((_, key, first)) = owner_of(&pool.get().await?, id).await? else {
        return Ok(None);
    };
    let Owner::Fenced { remaining } = first else {
        return Ok(Some(first));
    };
    let deadline = tokio::time::Instant::now() + remaining + Duration::from_millis(20);
    let mut last = app
        .fanout
        .hold_until(
            &[(fanout::CHANGES, key.as_str())],
            remaining + Duration::from_millis(20),
            &app.draining,
            || async move {
                let o = read().await?;
                Ok((!matches!(o, Some(Owner::Fenced { .. })), o))
            },
        )
        .await?;
    // a draining replica's hold answers at once: wait out the rest of the lease anyway (at most 3 s, well inside the
    // drain grace), so a fenced write still becomes native at the release instead of 409 `route: "legacy"`
    while matches!(last, Some(Owner::Fenced { .. })) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep((deadline - now).min(DRAIN_POLL)).await;
        last = read().await?;
    }
    match last {
        Some(Owner::Native) | None => Ok(last),
        // lapsed while waiting (or about to): cleared, so the conversation is plainly legacy again
        Some(_) => {
            lapse(pool, id).await?;
            read().await
        }
    }
}

/// A native mutation of `id` (cutover 6.4): runs `op`; when it answers 409 `conversation_migrating` and the
/// conversation is fenced, waits for the release ([`await_unfenced`]) and runs `op` once more, so a send during a
/// fence becomes the first native turn. An unfenced legacy conversation's 409 (`route: "legacy"`) is answered as it
/// is and counted.
///
/// # Errors
/// `op`'s, or a statement error of the wait.
pub async fn native<T, F, Fut>(app: &Magpie, id: Uuid, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let first = op().await;
    let out = match first {
        Err(e) if e.is(&ErrorCode::ConversationMigrating) => match await_unfenced(app, id).await? {
            Some(Owner::Native) => op().await,
            _ => Err(e),
        },
        other => other,
    };
    if let Err(e) = &out
        && e.is(&ErrorCode::ConversationMigrating)
    {
        app.legacy.migrating();
    }
    out
}

/// [`Error`] for a write that found the conversation fenced past every wait (never expected: a fence lapses in 3 s).
#[must_use]
pub fn still_fenced() -> Error {
    Error::code(ErrorCode::Unavailable, "the conversation stayed fenced")
}
