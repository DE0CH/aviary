//! One pass (cutover 7.4): one read of `cloudcli`, then, for every conversation not released, a stat walk of its
//! sources and a sync of whatever changed (its sessions row, its files, its failed turns), at most
//! [`PARALLEL`] conversations at a time; then the purge of migrating conversations whose sessions row vanished.
//! `run` (the bulk import) and `catch-up` are each one pass; the hand-over loop (WP-52) repeats it. A conversation's
//! error is recorded on its `import_sources` row and the pass goes on.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::conv::{self, Created};
use crate::failed::{self, Placement};
use crate::sent;
use crate::source::{self, FailedTurn, PersonSource, SessionRow};
use crate::sync::{Files, Writer};
use crate::tree;
use crate::{Error, Importer, Result};

/// Conversations synced at once (cutover 7.6).
pub const PARALLEL: usize = 8;

/// The most conversations one pass purges on its own: more than this many (and more than half of the open ones)
/// missing from one read looks like a wrong or partial `cloudcli` read, not people deleting conversations, and the
/// pass purges none of them (each is an error until the read has them again, or an operator removes them).
pub const PURGE_MAX: usize = 3;

/// What the importer knows of an imported conversation.
#[derive(Debug, Clone)]
pub struct Known {
    /// `row_fp`.
    pub row_fp: String,
    /// `files`.
    pub files: Files,
    /// `failed_turns_fp`.
    pub failed_fp: String,
    /// `sent_fp`.
    pub sent_fp: String,
    /// Released (the importer never writes it again).
    pub released: bool,
    /// The person's schema it was imported from (`source_schema`).
    pub schema: String,
}

/// Every `import_sources` row.
///
/// # Errors
/// A statement error.
pub async fn known(imp: &Importer) -> Result<HashMap<Uuid, Known>> {
    known_where(imp, None).await
}

async fn known_where(imp: &Importer, id: Option<Uuid>) -> Result<HashMap<Uuid, Known>> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select conversation_id, row_fp, files, failed_turns_fp, released_at is not null, sent_fp, source_schema
             from import_sources where $1::uuid is null or conversation_id = $1",
            &[&id],
        )
        .await?;
    let mut out = HashMap::new();
    for r in rows {
        out.insert(
            r.get(0),
            Known {
                row_fp: r.get(1),
                files: serde_json::from_value(r.get(2))?,
                failed_fp: r.get(3),
                released: r.get(4),
                sent_fp: r.get(5),
                schema: r.get(6),
            },
        );
    }
    Ok(out)
}

/// The `import_sources` row of one conversation, as [`known`] has it.
///
/// # Errors
/// A statement error.
pub async fn known_of(imp: &Importer, id: Uuid) -> Result<Option<Known>> {
    Ok(known_where(imp, Some(id)).await?.remove(&id))
}

/// What one conversation's sync did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Nothing changed.
    Unchanged,
    /// Created now.
    Created,
    /// Synced a change.
    Changed,
    /// Released: not the importer's any more.
    Released,
    /// The id is a native conversation's.
    Conflict,
    /// Deleted for good on the new side (an `import_purged` tombstone): never imported again, so the deletion
    /// stays and reaches the old side with the reverse export.
    Purged,
}

/// A pass's summary.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Conversations looked at.
    pub conversations: usize,
    /// Created.
    pub created: usize,
    /// Changed.
    pub changed: usize,
    /// Purged because their sessions row vanished.
    pub purged: usize,
    /// Native conversations with an imported row's id.
    pub conflicts: Vec<Uuid>,
    /// Per conversation (or schema) errors.
    pub errors: Vec<(String, String)>,
    /// The view-as pass (a full pass only).
    pub view_as: crate::viewas::Copied,
    /// The share pass (a full pass only).
    pub shares: crate::share::Copied,
}

/// One pass over every person's conversations (`only`: just these).
///
/// # Errors
/// The mount not there ([`crate::tree::check_mount`]), the `cloudcli` read or the magpie read of `import_sources`
/// failing, the leader lock lost; per-conversation errors are in the report.
#[allow(clippy::too_many_lines)] // the conversations, then the purge, then the shares, in 7.4's order
pub async fn pass(imp: &Importer, only: Option<&BTreeSet<Uuid>>) -> Result<Report> {
    tree::check_mount(&imp.jfs)?;
    imp.check_lock().await?;
    let snap = source::read(&imp.cloudcli).await?;
    let known = Arc::new(known(imp).await?);
    let mut report = Report::default();
    for (schema, why) in &snap.problems {
        report.errors.push((schema.clone(), why.clone()));
    }
    let gate = Arc::new(Semaphore::new(PARALLEL));
    let mut set = tokio::task::JoinSet::new();
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    let mut failed_ids: BTreeSet<Uuid> = BTreeSet::new();
    let mut purged_ids: BTreeSet<Uuid> = BTreeSet::new();
    let duplicates = snap.duplicate_ids();
    for (id, schemas) in &duplicates {
        // neither row is synced: two persons' trees would go into one conversation
        let msg = format!("its sessions row is in more than one schema ({})", schemas.join(", "));
        tracing::warn!(conversation = %id, error = %msg, "conversation not synced");
        let _ = conv::record_error(imp, *id, &msg).await;
        failed_ids.insert(*id);
        report.errors.push((id.to_string(), msg));
    }
    for p in &snap.persons {
        for s in &p.sessions {
            let Ok(id) = s.id() else {
                report
                    .errors
                    .push((s.session_id.clone(), "the sessions row's id is not a uuid".to_owned()));
                continue;
            };
            seen.insert(id);
            if only.is_some_and(|o| !o.contains(&id)) || duplicates.contains_key(&id) {
                continue;
            }
            let failed = p.failed.get(&s.session_id).cloned().unwrap_or_default();
            let (imp, known, gate) = (imp.clone(), known.clone(), gate.clone());
            let (person, schema, key, s) = (p.person.clone(), p.schema.clone(), p.key.clone(), s.clone());
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                let r = match imp.check_lock().await {
                    Ok(()) => sync_conversation(&imp, &person, &schema, &key, &s, &failed, known.get(&id)).await,
                    Err(e) => Err(e),
                };
                (id, r)
            });
        }
    }
    let mut lost = false;
    while let Some(j) = set.join_next().await {
        let Ok((id, r)) = j else {
            report
                .errors
                .push(("pass".into(), "a sync task ended unexpectedly".into()));
            continue;
        };
        report.conversations += 1;
        match r {
            Ok(Outcome::Created) => report.created += 1,
            Ok(Outcome::Changed) => report.changed += 1,
            Ok(Outcome::Conflict) => report.conflicts.push(id),
            Ok(Outcome::Purged) => {
                purged_ids.insert(id);
            }
            Ok(Outcome::Unchanged | Outcome::Released) | Err(Error::Released) => {}
            Err(Error::LockLost) => {
                // stop at once: another writer may hold the lock now
                lost = true;
                set.abort_all();
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(conversation = %id, error = %msg, "conversation not synced");
                let _ = conv::record_error(imp, id, &msg).await;
                failed_ids.insert(id);
                report.errors.push((id.to_string(), msg));
            }
        }
    }
    if lost {
        return Err(Error::LockLost);
    }
    if only.is_none() {
        match crate::viewas::pass(imp, &imp.jarvis).await {
            Ok(c) => report.view_as = c,
            Err(e) => report.errors.push(("view-as".into(), e.to_string())),
        }
    }
    if only.is_none() && snap.problems.is_empty() {
        for (id, why) in purge_refusals(&snap, &known, &seen) {
            report.errors.push((id, why));
        }
        for id in purgeable(&snap, &known, &seen) {
            imp.check_lock().await?;
            match conv::purge(imp, id).await {
                Ok(true) => report.purged += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(conversation = %id, error = %e, "conversation not purged");
                    report.errors.push((id.to_string(), format!("purge: {e}")));
                }
            }
        }
    }
    // shares after the conversations (and the purge of vanished rows, so a share of a purged one is revoked)
    if only.is_none() {
        imp.check_lock().await?;
        let presence = crate::share::Presence {
            skip: failed_ids,
            in_snapshot: seen,
            unreadable: snap.problems.iter().map(|(s, _)| s.clone()).collect(),
            purged: purged_ids,
        };
        match crate::share::pass(imp, &snap.shares, &presence).await {
            Ok(c) => {
                for (id, e) in &c.errors {
                    report.errors.push((format!("share {id}"), e.clone()));
                }
                report.shares = c;
            }
            Err(e) => report.errors.push(("shares".into(), e.to_string())),
        }
    }
    Ok(report)
}

/// The open conversations missing from the read whose purge the read can be trusted for: their schema was read,
/// the read has persons, and not too many go at once ([`PURGE_MAX`]).
fn purgeable(snap: &source::Snapshot, known: &HashMap<Uuid, Known>, seen: &BTreeSet<Uuid>) -> Vec<Uuid> {
    if purge_refusals(snap, known, seen).iter().any(|(id, _)| id == "purge") {
        return Vec::new();
    }
    let read: BTreeSet<&str> = snap.persons.iter().map(|p| p.schema.as_str()).collect();
    let mut out: Vec<Uuid> = known
        .iter()
        .filter(|(id, k)| !k.released && !seen.contains(*id) && read.contains(k.schema.as_str()))
        .map(|(id, _)| *id)
        .collect();
    out.sort_unstable();
    out
}

/// Why open conversations missing from the read are not purged: `("purge", why)` when the read as a whole is not
/// trusted (no persons at all while conversations are open, or more missing than [`PURGE_MAX`] and half the open
/// ones), else one error per conversation whose schema is not in the read (a schema gone or renamed is an
/// operator's call, never a purge).
fn purge_refusals(
    snap: &source::Snapshot,
    known: &HashMap<Uuid, Known>,
    seen: &BTreeSet<Uuid>,
) -> Vec<(String, String)> {
    let open = known.values().filter(|k| !k.released).count();
    let read: BTreeSet<&str> = snap.persons.iter().map(|p| p.schema.as_str()).collect();
    let missing: Vec<(&Uuid, &Known)> = known
        .iter()
        .filter(|(id, k)| !k.released && !seen.contains(*id))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    if snap.persons.is_empty() {
        return vec![(
            "purge".to_owned(),
            format!(
                "the cloudcli read has no person schemas while {open} imported conversations are open: nothing \
                 purged (an empty or wrong database?)"
            ),
        )];
    }
    let mut out = Vec::new();
    let mut gone = 0usize;
    for (id, k) in &missing {
        if read.contains(k.schema.as_str()) {
            gone += 1;
        } else {
            out.push((
                id.to_string(),
                format!(
                    "its schema {} is not in the cloudcli read: not purged (a schema gone or renamed is an \
                     operator's call)",
                    k.schema
                ),
            ));
        }
    }
    if gone > PURGE_MAX && gone * 2 > open {
        out.push((
            "purge".to_owned(),
            format!(
                "{gone} of {open} open conversations are missing from the cloudcli read: nothing purged (a wrong or \
                 partial read?)"
            ),
        ));
    }
    out
}

fn stats_changed(sources: &BTreeMap<String, tree::Source>, files: &Files) -> bool {
    sources.len() != files.len()
        || sources.iter().any(|(p, s)| {
            files
                .get(p)
                .is_none_or(|r| r.size != s.stat.size || r.mtime_ns != s.stat.mtime_ns || r.ino != s.stat.ino)
        })
}

/// Syncs one conversation (cutover 7.4.1-7.4.3).
///
/// # Errors
/// What the sync failed with; [`Error::Released`] when the conversation was released meanwhile.
#[allow(clippy::too_many_lines)] // files, failed turns, sent folders and the row, in that order
pub async fn sync_conversation(
    imp: &Importer,
    person: &str,
    schema: &str,
    key: &str,
    s: &SessionRow,
    failed: &[FailedTurn],
    known: Option<&Known>,
) -> Result<Outcome> {
    let id = s.id()?;
    if known.is_some_and(|k| k.released) {
        return Ok(Outcome::Released);
    }
    let psid = s.psid().to_owned();
    let sources = tree::sources(&imp.jfs, key, &psid, Some(id))?;
    let main = tree::main_path(&psid);
    let mut outcome = Outcome::Unchanged;
    let empty = Known {
        row_fp: String::new(),
        files: Files::new(),
        failed_fp: String::new(),
        released: false,
        sent_fp: String::new(),
        schema: schema.to_owned(),
    };
    let k = if let Some(k) = known {
        k.clone()
    } else {
        let first = match sources.get(&main) {
            Some(m) => failed::line_times(&m.abs, m.stat.size)?
                .into_iter()
                .find_map(|(_, t)| t),
            None => None,
        };
        match conv::create(imp, person, schema, s, first).await? {
            Created::Conflict => return Ok(Outcome::Conflict),
            Created::Purged => return Ok(Outcome::Purged),
            Created::New => outcome = Outcome::Created,
            Created::Imported => {}
        }
        empty
    };
    // the main transcript of a sessions row that still exists does not just vanish: a mount that is not there, or a
    // folder gone for a moment, must not unlink the conversation's transcript (and retract its events)
    if k.files.contains_key(&main) && !sources.contains_key(&main) {
        return Err(Error::Source(format!(
            "{main} was synced before and is not on the mount now (its sessions row is still there)"
        )));
    }
    let imported = failed::ids_of(&k.failed_fp);
    let new_failed: Vec<FailedTurn> = failed.iter().filter(|f| !imported.contains(&f.id)).cloned().collect();
    let row_changed = k.row_fp != s.fingerprint();
    let (snapshots, mut sent_fp) = sent::read(&imp.jfs, id)?;
    let sent_changed = sent_fp != k.sent_fp;
    if !stats_changed(&sources, &k.files) && !row_changed && new_failed.is_empty() && !sent_changed {
        return Ok(outcome);
    }
    let workspace = tree::workspace_uploads(&imp.jfs, id)?;
    let mut w = Writer::new(imp, id, person, workspace);
    let mut placement = Placement::new(person, id, new_failed.clone());
    // failed turns are placed by time while the main transcript's first import has not been recorded (also when an
    // earlier pass wrote part of it and stopped: the placement resumes where it ended)
    let place = !new_failed.is_empty() && sources.contains_key(&main) && !k.files.contains_key(&main);
    let (files, wrote) = if place {
        w.sync_all(&sources, &k.files, Some(main.as_str()), Some(&mut placement))
            .await?
    } else {
        w.sync_all(&sources, &k.files, None, None).await?
    };
    if wrote {
        conv::flush_limbo(imp, id).await?;
    }
    let mut all = imported;
    for f in &new_failed {
        if !placement.placed.contains(&f.id) {
            failed::insert(imp, person, id, f).await?;
        }
        all.insert(f.id);
    }
    let mut changed = wrote || !new_failed.is_empty();
    let mut late: Vec<String> = Vec::new();
    if sent_changed {
        let done = sent::imported(imp, id).await?;
        let mut waiting = false;
        for snap in &snapshots {
            if let Some(paths) = done.get(&snap.tool_use_id) {
                // imported before: a file it did not have then is never imported on its own
                late.extend(
                    snap.files
                        .keys()
                        .filter(|p| !paths.contains(*p))
                        .map(|p| format!("{}/{p}", snap.tool_use_id)),
                );
                continue;
            }
            // a folder the old hook may still be writing waits for a later pass (all of its files at once)
            if !snap.settled(imp.sent_quiet) {
                waiting = true;
                continue;
            }
            // no files (yet): nothing was sent; a send the hook refused left its folder behind
            if snap.files.is_empty() || sent::failed_call(imp, id, &snap.tool_use_id).await? {
                continue;
            }
            sent::import(imp, person, id, snap).await?;
            changed = true;
        }
        if !late.is_empty() {
            sent_fp = format!("{}{sent_fp}", sent::LATE);
        } else if waiting {
            // never equal to a read's fingerprint: the next pass looks again, and verification waits
            sent_fp = format!("{}{sent_fp}", sent::WAITING);
        }
    }
    if row_changed {
        changed |= conv::apply_row(imp, person, s).await?;
    }
    conv::record(imp, id, &files, &failed::fingerprint(&all), &sent_fp, changed).await?;
    if !late.is_empty() {
        return Err(Error::Source(format!(
            "files appeared in sent folders after they were imported (not imported): {}",
            late.join(", ")
        )));
    }
    if changed && outcome == Outcome::Unchanged {
        outcome = Outcome::Changed;
    }
    Ok(outcome)
}

/// The persons of a snapshot by person email (for commands that act on one conversation).
#[must_use]
pub fn find(snap: &source::Snapshot, id: Uuid) -> Option<(&PersonSource, &SessionRow)> {
    snap.persons
        .iter()
        .find_map(|p| p.sessions.iter().find(|s| s.id().ok() == Some(id)).map(|s| (p, s)))
}
