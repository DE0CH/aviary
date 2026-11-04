//! The importer's one-shot commands (cutover 7.9): `status`, `hold on|off`, `reset --yes`, `unfence <id>`,
//! `release <id> --force` and `purge <id>…`.

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
    /// The per-person gates (`allow-handover --person`), by person key (never the email).
    pub person_gates: Vec<String>,
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
    let person_gates: Vec<String> = conn
        .query("select person from import_gate_persons order by person", &[])
        .await?
        .iter()
        .map(|r| aviary_core::log::person_key(r.get::<_, &str>(0)))
        .collect();
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
        } else if gate.is_some() || !person_gates.is_empty() {
            "handover"
        } else {
            "sync"
        },
        gate,
        person_gates,
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
    let gate: bool = pool
        .get()
        .await?
        .query_one(
            "select handover_allowed_at is not null or exists (select 1 from import_gate_persons) from import_control",
            &[],
        )
        .await?
        .get(0);
    if gate {
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
        imp.check_lock().await?;
        if crate::conv::purge(imp, id).await? {
            n += 1;
        }
    }
    // the imported shares with them: the purge revoked them and left them without a conversation; a later run
    // imports them again
    pool.get()
        .await?
        .batch_execute(
            "delete from shares where conversation_id is null and id in (select share_id from import_shares);
             delete from import_shares where share_id not in (select id from shares);",
        )
        .await?;
    Ok(n)
}

/// What `purge <id>…` did with one conversation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorPurge {
    /// The conversation.
    pub conversation: Uuid,
    /// Purged.
    pub purged: bool,
    /// Why not.
    pub error: Option<String>,
}

/// `purge <id>…`: the operator's purge of migrating conversations a pass would not purge on its own (a read missing
/// too many at once, [`crate::pass::PURGE_MAX`]). Each is purged only when a fresh, clean read of its own schema has
/// no sessions row for it, as a pass's purge (no hermit call; the old stack's workspace stays).
///
/// # Errors
/// The lock lost; a statement error.
pub async fn purge(imp: &Importer, ids: &[Uuid]) -> Result<Vec<OperatorPurge>> {
    let mut out = Vec::new();
    for &id in ids {
        imp.check_lock().await?;
        let why = match crate::verify::row(imp, id).await? {
            None => Some("not imported".to_owned()),
            Some(r) if r.released => Some("released: not the importer's".to_owned()),
            Some(r) => match crate::source::read_schema(&imp.cloudcli, &r.schema).await {
                Err(e) => Some(format!("the read of schema {}: {e}", r.schema)),
                Ok(snap) => {
                    if let Some((_, w)) = snap.problems.first() {
                        Some(format!("schema {}: {w}", r.schema))
                    } else if !snap.persons.iter().any(|p| p.schema == r.schema) {
                        Some(format!("schema {} is not in the read", r.schema))
                    } else if crate::pass::find(&snap, id).is_some() {
                        Some("its sessions row is there".to_owned())
                    } else {
                        None
                    }
                }
            },
        };
        let purged = why.is_none() && crate::conv::purge(imp, id).await?;
        out.push(OperatorPurge {
            conversation: id,
            purged,
            error: why.or_else(|| (!purged).then(|| "not migrating".to_owned())),
        });
    }
    Ok(out)
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
    /// Tickets expired first (an old turn still running under one is cut off: the operator forces the release
    /// knowing that).
    pub tickets_expired: u64,
    /// Verification before the fence passed.
    pub verified: bool,
    /// What verification found different (released anyway, as forced).
    pub mismatches: Vec<crate::verify::Mismatch>,
    /// What verification found not synced yet (the syncs before and inside the fence sync it).
    pub pending: Vec<String>,
    /// Files verification re-ingested after a first mismatch.
    pub reingested: Vec<String>,
    /// Verification's own error, when it could not run.
    pub verify_error: Option<String>,
    /// The conversation's own sync error (the last of the syncs before and inside the fence), released past as
    /// forced: what that sync could not bring over (a file that appeared in a sent folder after its import, a main
    /// transcript gone, a name that is not UTF-8, a write magpie refused) stays on the old side only.
    pub sync_error: Option<String>,
    /// Fence to release by the database's clock, seconds (what `magpie_fence_seconds` of this process observed:
    /// a one-shot process's registry, which nothing scrapes, so forced releases are counted here and kept out of the
    /// A5 gate's histogram on purpose).
    pub fence_seconds: f64,
    /// The importer's own time from the fence to the release's commit, seconds (the final sync and re-stat inside
    /// it).
    pub in_fence_seconds: f64,
}

/// The final sync of one conversation: one read of its person's schema, then the same sync a pass makes (files,
/// sessions row, failed turns, sent files). The outer error stops a forced release (the mount, the read, a
/// conversation not imported or whose sessions row is gone, the lock lost, the conversation released meanwhile);
/// the inner one is the conversation's own sync error (a late sent file, a vanished main transcript, a name that is
/// not UTF-8, a write magpie refused), which a forced release reports and releases past.
async fn final_sync(imp: &Importer, id: Uuid) -> Result<Result<()>> {
    crate::tree::check_mount(&imp.jfs)?;
    let Some(row) = crate::verify::row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    let snap = crate::source::read_schema(&imp.cloudcli, &row.schema).await?;
    if let Some((_, why)) = snap.problems.first() {
        return Err(Error::Source(format!("schema {}: {why}", row.schema)));
    }
    let Some((p, s)) = crate::pass::find(&snap, id) else {
        return Err(Error::Source(format!("{id}: its sessions row is gone")));
    };
    let known = crate::pass::known_of(imp, id).await?;
    let failed = p.failed.get(&s.session_id).cloned().unwrap_or_default();
    match crate::pass::sync_conversation(imp, &p.person, &p.schema, &p.key, s, &failed, known.as_ref()).await {
        Ok(_) => Ok(Ok(())),
        Err(e @ (Error::LockLost | Error::Released)) => Err(e),
        Err(e) => Ok(Err(e)),
    }
}

/// The conversation's sources now, as fingerprints (the forced release's re-stat when its sync failed).
async fn fingerprints(imp: &Importer, id: Uuid) -> Result<crate::verify::Fingerprints> {
    let Some(r) = crate::verify::row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    Ok(crate::verify::current(imp, id, &r).await?.1)
}

/// `release <id> --force` (cutover 7.7 step 3, A2 rehearsal 7): verifies the conversation (a mismatch is reported,
/// not a refusal), syncs it once before the fence (so the sync inside it usually has nothing left to do), closes its
/// open tickets (an open ticket otherwise refuses the fence and the release for ever; an old turn still running is
/// cut off, which the operator accepts by forcing), fences it, syncs it once more inside the fence and re-stats every
/// source and fingerprint (cutover 7.8): anything that changed meanwhile lifts the fence and fails the release. A
/// sync error of the conversation's own (a late sent file, a main transcript gone, a name that is not UTF-8) is
/// reported in [`Forced::sync_error`] and released past, as a verification mismatch is: forcing is the way out for a
/// conversation stuck on one; the re-stat is then against the sources as they were when the fence was taken. Then it
/// releases with `release_reason = forced`, in one transaction that also checks the one-shot lock. The fence is a
/// lease of [`magpie::limits::FENCE_LEASE`]: a final sync that outlasts it fails the release with that said (nothing
/// is released; run it again). The old title's `name_source` is kept as imported.
///
/// Not done here: cutover 7.8's upload-ingestion pass inside the release transaction (WP-52's; until it exists a
/// native send naming an old asset waits for the synchronous ingest of the import Deployment, A3 on).
///
/// # Errors
/// [`Error::Source`] when it cannot be fenced or released (not migrating, its sessions row gone, a ticket taken
/// meanwhile, a source that changed inside the fence, the fence's lease over before the release); the mount not
/// there; the lock lost; a statement error.
#[allow(clippy::too_many_lines)] // verify, sync, fence, sync, re-stat, release, each with its own failure
pub async fn release_forced(imp: &Importer, id: Uuid) -> Result<Forced> {
    imp.check_lock().await?;
    let pool = &imp.magpie.pool;
    // verified before the fence: the fence is a 3 s lease
    let verdict = crate::verify::verify(imp, id).await;
    let mut out = Forced {
        conversation: id,
        tickets_expired: 0,
        verified: false,
        mismatches: Vec::new(),
        pending: Vec::new(),
        reingested: Vec::new(),
        verify_error: None,
        sync_error: None,
        fence_seconds: 0.0,
        in_fence_seconds: 0.0,
    };
    match verdict {
        Ok(v) => {
            if !v.ok() {
                tracing::warn!(
                    conversation = %id, mismatches = ?v.mismatches, pending = ?v.pending,
                    "released by force without a clean verification"
                );
            }
            out.verified = v.ok();
            out.mismatches = v.mismatches;
            out.pending = v.pending;
            out.reingested = v.reingested;
        }
        Err(e @ Error::LockLost) => return Err(e),
        Err(e) => {
            tracing::warn!(conversation = %id, error = %e, "released by force without a verification");
            out.verify_error = Some(e.to_string());
        }
    }
    // what the old stack wrote since the verification, synced before the fence; a release that cannot happen (the
    // mount, the read, the row gone) fails here, before any open ticket is cut off
    if let Err(e) = final_sync(imp, id).await? {
        tracing::warn!(conversation = %id, error = %e, "the sync before a forced release failed");
        out.sync_error = Some(e.to_string());
    }
    imp.check_lock().await?;
    let at = tickets::now(pool).await?;
    out.tickets_expired = tickets::expire(pool, id, at).await?;
    match fence::fence(pool, id).await? {
        fence::Fence::Fenced(_) | fence::Fence::AlreadyFenced => {}
        other => return Err(Error::Source(format!("{id} cannot be fenced: {other:?}"))),
    }
    let fenced = std::time::Instant::now();
    crate::pause(imp.pause_in_fence).await;
    // inside the fence: whatever the old stack wrote since is synced now, then nothing may move
    let changes = async {
        let at_fence = fingerprints(imp, id).await;
        match final_sync(imp, id).await? {
            Ok(()) => {
                out.sync_error = None;
                crate::verify::changed_since_sync(imp, id).await
            }
            Err(e) => {
                tracing::warn!(conversation = %id, error = %e, "the sync inside a forced release's fence failed");
                match (at_fence, fingerprints(imp, id).await) {
                    (Ok(a), Ok(b)) => {
                        out.sync_error = Some(e.to_string());
                        Ok(a.changes(&b))
                    }
                    // sources that cannot even be stated (a name that is not UTF-8): no re-stat; what the old side
                    // writes from here on is lost as after any forced release (`forcedWrites`)
                    (Err(x), _) | (_, Err(x)) => {
                        out.sync_error = Some(format!("{e} (no re-stat inside the fence: {x})"));
                        Ok(Vec::new())
                    }
                }
            }
        }
    }
    .await;
    let changes = match changes {
        Ok(c) => c,
        Err(e @ Error::LockLost) => {
            let _ = fence::unfence(pool, id).await;
            return Err(e);
        }
        Err(e) => vec![format!("the final sync failed: {e}")],
    };
    if !changes.is_empty() {
        let _ = fence::unfence(pool, id).await;
        return Err(Error::Source(format!(
            "{id} changed inside the fence and was not released: {}",
            changes.join(", ")
        )));
    }
    let held = aviary_pg::tx!(pool, |t| -> Result<Result<fence::Released, fence::NotReleased>> {
        imp.check_lock_in(t).await?;
        let r = fence::release_in(t, id, &fence::Release::default()).await?;
        if r.is_ok() {
            t.execute(
                "update import_sources set released_at = now(), release_reason = 'forced' where conversation_id = $1",
                &[&id],
            )
            .await?;
        }
        Ok(r)
    })
    .await;
    out.in_fence_seconds = fenced.elapsed().as_secs_f64();
    match held {
        Ok(Ok(r)) => {
            out.fence_seconds = r.held;
            r.record(&imp.magpie.legacy);
            Ok(out)
        }
        Ok(Err(fence::NotReleased::NotFenced)) => {
            let _ = fence::unfence(pool, id).await;
            Err(Error::Source(format!(
                "{id} was not released: the fence's {} s lease ran out after {:.1} s inside it (the final sync and \
                 re-stat took too long); nothing was released, run it again",
                magpie::limits::FENCE_LEASE.as_secs(),
                out.in_fence_seconds
            )))
        }
        Ok(Err(why)) => {
            let _ = fence::unfence(pool, id).await;
            Err(Error::Source(format!(
                "{id} was not released: {why:?} ({:.1} s inside the fence)",
                out.in_fence_seconds
            )))
        }
        Err(e) => {
            let _ = fence::unfence(pool, id).await;
            Err(e)
        }
    }
}
