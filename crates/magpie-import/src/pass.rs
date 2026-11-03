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
}

/// Every `import_sources` row.
///
/// # Errors
/// A statement error.
pub async fn known(imp: &Importer) -> Result<HashMap<Uuid, Known>> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select conversation_id, row_fp, files, failed_turns_fp, released_at is not null, sent_fp
             from import_sources",
            &[],
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
            },
        );
    }
    Ok(out)
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
/// The `cloudcli` read or the magpie read of `import_sources` failing; per-conversation errors are in the report.
pub async fn pass(imp: &Importer, only: Option<&BTreeSet<Uuid>>) -> Result<Report> {
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
    for p in &snap.persons {
        for s in &p.sessions {
            let Ok(id) = s.id() else {
                report
                    .errors
                    .push((s.session_id.clone(), "the sessions row's id is not a uuid".to_owned()));
                continue;
            };
            seen.insert(id);
            if only.is_some_and(|o| !o.contains(&id)) {
                continue;
            }
            let failed = p.failed.get(&s.session_id).cloned().unwrap_or_default();
            let (imp, known, gate) = (imp.clone(), known.clone(), gate.clone());
            let (person, schema, key, s) = (p.person.clone(), p.schema.clone(), p.key.clone(), s.clone());
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                let r = sync_conversation(&imp, &person, &schema, &key, &s, &failed, known.get(&id)).await;
                (id, r)
            });
        }
    }
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
            Ok(Outcome::Unchanged | Outcome::Released) | Err(Error::Released) => {}
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(conversation = %id, error = %msg, "conversation not synced");
                let _ = conv::record_error(imp, id, &msg).await;
                failed_ids.insert(id);
                report.errors.push((id.to_string(), msg));
            }
        }
    }
    if only.is_none() {
        match crate::viewas::pass(imp, &imp.jarvis).await {
            Ok(c) => report.view_as = c,
            Err(e) => report.errors.push(("view-as".into(), e.to_string())),
        }
    }
    if only.is_none() && snap.problems.is_empty() {
        for (id, k) in known.iter() {
            if !k.released && !seen.contains(id) && conv::purge(imp, *id).await? {
                report.purged += 1;
            }
        }
    }
    // shares after the conversations (and the purge of vanished rows, so a share of a purged one is revoked)
    if only.is_none() {
        match crate::share::pass(imp, &snap.shares, &failed_ids).await {
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
    let sources = tree::sources(&imp.jfs, key, &psid)?;
    let main = tree::main_path(&psid);
    let mut outcome = Outcome::Unchanged;
    let empty = Known {
        row_fp: String::new(),
        files: Files::new(),
        failed_fp: String::new(),
        released: false,
        sent_fp: String::new(),
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
            Created::New => outcome = Outcome::Created,
            Created::Imported => {}
        }
        empty
    };
    let imported = failed::ids_of(&k.failed_fp);
    let new_failed: Vec<FailedTurn> = failed.iter().filter(|f| !imported.contains(&f.id)).cloned().collect();
    let row_changed = k.row_fp != s.fingerprint();
    let (snapshots, sent_fp) = sent::read(&imp.jfs, id)?;
    let sent_changed = sent_fp != k.sent_fp;
    if !stats_changed(&sources, &k.files) && !row_changed && new_failed.is_empty() && !sent_changed {
        return Ok(outcome);
    }
    let workspace = tree::workspace_uploads(&imp.jfs, id)?;
    let mut w = Writer::new(imp, id, person, workspace);
    let mut placement = Placement::new(person, id, new_failed.clone());
    let place = !new_failed.is_empty() && sources.contains_key(&main) && w.file(&main).await?.is_none();
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
    if sent_changed {
        let done = sent::imported(imp, id).await?;
        for snap in snapshots.iter().filter(|s| !done.contains(&s.tool_use_id)) {
            sent::import(imp, person, id, snap).await?;
            changed = true;
        }
    }
    if row_changed {
        changed |= conv::apply_row(imp, person, s).await?;
    }
    conv::record(imp, id, &files, &failed::fingerprint(&all), &sent_fp, changed).await?;
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
