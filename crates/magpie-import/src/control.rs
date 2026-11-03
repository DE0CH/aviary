//! The importer's one-shot commands (cutover 7.9): `status`, `hold on|off`, `reset --yes`, `unfence <id>` and
//! `release <id> --force`.

use magpie::legacy::{fence, tickets};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Error, Importer, Result};

/// One open conversation in the status document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Open {
    /// The conversation.
    pub conversation: Uuid,
    /// Its person's key (never the email, cutover 7.9).
    pub person_key: String,
    /// What it waits for (`gate`, `verify`, …).
    pub waiting_for: &'static str,
    /// Since when (its last change).
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// A conversation's error in the status document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEntry {
    /// The conversation.
    pub conversation: Uuid,
    /// What failed.
    pub error: String,
    /// When it was last synced.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// The counts of the status document.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    /// Every imported conversation.
    pub imported: i64,
    /// Still the old stack's.
    pub legacy: i64,
    /// Fenced now.
    pub fenced: i64,
    /// Handed over.
    pub released: i64,
    /// Shown running by the old stack (the mirror).
    pub running: i64,
    /// Not verified, with an error.
    pub stuck: i64,
}

/// The status document (cutover 7.9), as far as a one-shot command knows it (the loop's own fields, sockets and
/// fence times, are the hand-over Deployment's).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Whether this process leads (a one-shot command never does).
    pub leader: bool,
    /// `sync` (bulk import and catch-up), `handover` (the gate is set) or `done` (every conversation released).
    pub phase: &'static str,
    /// The gate.
    #[serde(with = "time::serde::rfc3339::option")]
    pub gate: Option<OffsetDateTime>,
    /// Hold (sync only, no release).
    pub hold: bool,
    /// Counts.
    pub conversations: Counts,
    /// The open ones.
    pub open: Vec<Open>,
    /// Errors.
    pub errors: Vec<ErrorEntry>,
}

/// Reads the status.
///
/// # Errors
/// A statement error.
pub async fn status(imp: &Importer) -> Result<Status> {
    let conn = imp.magpie.pool.get().await?;
    let c = conn
        .query_one(
            "select handover_allowed_at, hold, all_owned_at from import_control",
            &[],
        )
        .await?;
    let gate: Option<OffsetDateTime> = c.get(0);
    let hold: bool = c.get(1);
    let done: Option<OffsetDateTime> = c.get(2);
    let n = conn
        .query_one(
            "select count(*),
                    count(*) filter (where s.released_at is null),
                    count(*) filter (where s.released_at is null and c.legacy_fence_at is not null),
                    count(*) filter (where s.released_at is not null),
                    count(*) filter (where s.released_at is null and c.run_state <> 'idle'),
                    count(*) filter (where s.released_at is null and s.last_error is not null)
             from import_sources s join conversations c on c.id = s.conversation_id",
            &[],
        )
        .await?;
    let rows = conn
        .query(
            "select conversation_id, user_key, verified_at is not null, last_change_at, last_error, last_synced_at
             from import_sources where released_at is null order by last_change_at desc",
            &[],
        )
        .await?;
    let mut open = Vec::new();
    let mut errors = Vec::new();
    for r in &rows {
        let verified: bool = r.get(2);
        let err: Option<String> = r.get(4);
        open.push(Open {
            conversation: r.get(0),
            person_key: r.get(1),
            waiting_for: if err.is_some() || !verified { "verify" } else { "gate" },
            since: r.get(3),
        });
        if let Some(e) = err {
            errors.push(ErrorEntry {
                conversation: r.get(0),
                error: e,
                at: r.get(5),
            });
        }
    }
    Ok(Status {
        leader: false,
        phase: if done.is_some() {
            "done"
        } else if gate.is_some() {
            "handover"
        } else {
            "sync"
        },
        gate,
        hold,
        conversations: Counts {
            imported: n.get(0),
            legacy: n.get(1),
            fenced: n.get(2),
            released: n.get(3),
            running: n.get(4),
            stuck: n.get(5),
        },
        open,
        errors,
    })
}

/// `hold on|off`.
///
/// # Errors
/// A statement error.
pub async fn hold(imp: &Importer, on: bool) -> Result<()> {
    imp.magpie
        .pool
        .get()
        .await?
        .execute("update import_control set hold = $1", &[&on])
        .await?;
    Ok(())
}

/// `reset --yes` (A2 only, before the gate): removes every conversation that has an `import_sources` row and is
/// migrating (rows and refs; blobs go to the GC; no hermit call, the workspaces are the old stack's). How many.
///
/// # Errors
/// [`Error::Source`] once the gate is set; a statement error.
pub async fn reset(imp: &Importer) -> Result<u64> {
    let pool = &imp.magpie.pool;
    let gate: Option<OffsetDateTime> = pool
        .get()
        .await?
        .query_one("select handover_allowed_at from import_control", &[])
        .await?
        .get(0);
    if gate.is_some() {
        return Err(Error::Source("the hand-over gate is set: reset is for A2 only".into()));
    }
    let ids: Vec<Uuid> = pool
        .get()
        .await?
        .query(
            "select s.conversation_id from import_sources s join conversations c on c.id = s.conversation_id
             where c.migrating",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let mut n = 0;
    for id in ids {
        if crate::conv::purge(imp, id).await? {
            n += 1;
        }
    }
    Ok(n)
}

/// `unfence <id>`: lifts a fence by hand (normally automatic). Whether one was lifted.
///
/// # Errors
/// A statement error.
pub async fn unfence(imp: &Importer, id: Uuid) -> Result<bool> {
    Ok(fence::unfence(&imp.magpie.pool, id).await?)
}

/// What `release --force` did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Forced {
    /// The conversation.
    pub conversation: Uuid,
    /// Tickets expired first.
    pub tickets_expired: u64,
    /// Fence to release, seconds.
    pub fence_seconds: f64,
}

/// `release <id> --force` (cutover 7.7 step 3, A2 rehearsal 7): closes the conversation's open tickets (an open
/// ticket otherwise refuses the fence and the release for ever), fences it, releases it with `release_reason =
/// forced`; the fence time goes on `magpie_fence_seconds` of this process's registry. The old title's `name_source`
/// is kept as imported.
///
/// # Errors
/// [`Error::Source`] when it cannot be fenced or released (not migrating, a ticket taken meanwhile); a statement
/// error.
pub async fn release_forced(imp: &Importer, id: Uuid) -> Result<Forced> {
    let pool = &imp.magpie.pool;
    let at = tickets::now(pool).await?;
    let expired = tickets::expire(pool, id, at).await?;
    // verified before the fence: the fence is a 3 s lease
    let verdict = crate::verify::verify(imp, id).await;
    match &verdict {
        Ok(v) if !v.ok() => {
            tracing::warn!(conversation = %id, mismatches = ?v.mismatches, "released by force with a mismatch");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(conversation = %id, error = %e, "released by force without a verification"),
    }
    match fence::fence(pool, id).await? {
        fence::Fence::Fenced(_) | fence::Fence::AlreadyFenced => {}
        other => return Err(Error::Source(format!("{id} cannot be fenced: {other:?}"))),
    }
    let held = fence::release(pool, id, &fence::Release::default(), "forced", &imp.magpie.legacy).await?;
    match held {
        Ok(s) => Ok(Forced {
            conversation: id,
            tickets_expired: expired,
            fence_seconds: s,
        }),
        Err(why) => {
            let _ = fence::unfence(pool, id).await;
            Err(Error::Source(format!("{id} was not released: {why:?}")))
        }
    }
}
