//! The switch's one-off commands that outlive A6 (hermit 15.4, 22.3): `hermit jfs-owner` ([`fix_owner`]: everything
//! under `ws/` to uid/gid 1000) and `hermit legacy-cleanup` ([`cleanup_plan`], [`cleanup_apply`]: the old folders off
//! the volume, 30 days after the switch). None of them serves; each runs once per run of its `Job` and exits.
//! `hermit import-jarvis` (the old stack's sandbox history and share workspaces) went in the A6 contraction (cutover
//! 10.5 step 7); what it recorded of the old shares (`legacy_shares`) is what `legacy-cleanup` reads.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use aviary_pg::Pool;
use aviary_pg::reexports::tokio_postgres::types::FromSqlOwned;
use hermit_api::workspaces::SHARE_NS;
use serde::Serialize;
use uuid::Uuid;

use crate::fabric::Fut;
use crate::workspaces::JfsCli;

/// The sandbox user (15.4).
pub const SANDBOX_UID: u32 = 1000;

/// Why a command stopped.
#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    #[error("database: {0}")]
    Db(#[from] aviary_pg::Error),
    #[error("{0}")]
    Other(String),
}

/// The workspace id of an old share (magpie 12's convention; the share id is the old folder name).
#[must_use]
pub fn share_workspace(share_id: &str) -> Uuid {
    Uuid::new_v5(&SHARE_NS, format!("share:{share_id}").as_bytes())
}

/// A folder name the import accepts as an old user key or share id (base64url and hex only: nothing that could
/// name another path).
fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Every old share folder under `root` (the mount): `(share id, path within the volume)`, sorted.
///
/// # Errors
/// `users/` exists but cannot be read.
pub fn old_shares(root: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let users = root.join("users");
    let mut out = Vec::new();
    let keys = match std::fs::read_dir(&users) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for key in keys {
        let key = key?;
        let Some(k) = key.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !plain_name(&k) {
            continue;
        }
        let Ok(shares) = std::fs::read_dir(key.path().join("shares")) else {
            continue;
        };
        for share in shares {
            let share = share?;
            let Some(id) = share.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !plain_name(&id) {
                continue;
            }
            let rel = PathBuf::from("users").join(&k).join("shares").join(&id).join("ws");
            if std::fs::symlink_metadata(root.join(&rel)).is_ok_and(|m| m.is_dir()) {
                out.push((id, rel));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn get<T: FromSqlOwned>(r: &aviary_pg::Row, c: &str) -> Result<T, LegacyError> {
    r.try_get::<_, T>(c).map_err(|e| LegacyError::Db(e.into()))
}

// ---- jfs-owner (15.4) ----

/// What one `jfs-owner` run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OwnerReport {
    /// Entries looked at (`ws/` itself included).
    pub scanned: u64,
    /// Entries whose owner was changed.
    pub changed: u64,
    /// Entries that could not be read or changed (at most the first 20, as `<path>: <error>`), and how many.
    pub errors: Vec<String>,
    pub error_count: u64,
}

struct Walk {
    dev: u64,
    uid: u32,
    gid: u32,
    scanned: AtomicU64,
    changed: AtomicU64,
    error_count: AtomicU64,
    errors: Mutex<Vec<String>>,
}

impl Walk {
    fn fail(&self, p: &Path, e: &std::io::Error) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
        let mut v = self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if v.len() < 20 {
            v.push(format!("{}: {e}", p.display()));
        }
    }

    /// Owns `p` (never following a symlink) when it is not owned already; returns its metadata.
    fn own(&self, p: &Path) -> Option<std::fs::Metadata> {
        self.scanned.fetch_add(1, Ordering::Relaxed);
        let m = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) => {
                self.fail(p, &e);
                return None;
            }
        };
        if m.uid() != self.uid || m.gid() != self.gid {
            match std::os::unix::fs::lchown(p, Some(self.uid), Some(self.gid)) {
                Ok(()) => {
                    self.changed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => self.fail(p, &e),
            }
        }
        Some(m)
    }

    /// The tree under `p`, depth first, on one filesystem.
    fn tree(&self, p: &Path) {
        let Some(m) = self.own(p) else { return };
        if !m.is_dir() || m.dev() != self.dev {
            return;
        }
        let mut stack = vec![p.to_owned()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    self.fail(&dir, &e);
                    continue;
                }
            };
            for e in entries {
                let e = match e {
                    Ok(e) => e,
                    Err(err) => {
                        self.fail(&dir, &err);
                        continue;
                    }
                };
                let path = e.path();
                if let Some(m) = self.own(&path)
                    && m.is_dir()
                    && m.dev() == self.dev
                {
                    stack.push(path);
                }
            }
        }
    }
}

/// `chown 1000:1000 <root>/ws && chown -R 1000:1000 <root>/ws/*` (15.4), done as one walk that never follows a
/// symlink (`lchown`), stays on the volume, changes only entries not owned already, and runs the top-level folders
/// on `threads` threads. Needs root and a mount without squashing (Job `hermit-jfs-owner`).
///
/// # Errors
/// `<root>/ws` cannot be read.
pub fn fix_owner(root: &Path, uid: u32, gid: u32, threads: usize) -> std::io::Result<OwnerReport> {
    let ws = root.join("ws");
    let dev = std::fs::symlink_metadata(&ws)?.dev();
    let walk = Walk {
        dev,
        uid,
        gid,
        scanned: AtomicU64::new(0),
        changed: AtomicU64::new(0),
        error_count: AtomicU64::new(0),
        errors: Mutex::new(Vec::new()),
    };
    walk.own(&ws);
    let mut tops: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(&ws)? {
        match e {
            Ok(e) => tops.push(e.path()),
            Err(e) => walk.fail(&ws, &e),
        }
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = tops.get(i) else { break };
                    walk.tree(p);
                }
            });
        }
    });
    Ok(OwnerReport {
        scanned: walk.scanned.into_inner(),
        changed: walk.changed.into_inner(),
        errors: walk
            .errors
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        error_count: walk.error_count.into_inner(),
    })
}

// ---- legacy-cleanup (22.3 step 6) ----

/// What `legacy-cleanup` asks magpie: which workspace ids it still references (`POST /v1/hermit/workspaces/exists`,
/// magpie 9.7) and whether its import is complete (`GET /v1/hermit/legacy-import`).
pub trait Referenced: Send + Sync {
    /// The ones among `ids` (at most [`EXISTS_BATCH`]) magpie names.
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>>;
    /// `None` when magpie's import is recorded complete (a clean `magpie import verify --all` with every conversation
    /// owned, and nothing imported or changed since), else why not.
    fn import_incomplete(&self) -> Fut<'_, Result<Option<String>, String>>;
}

/// The ids of one `workspaces/exists` call.
pub const EXISTS_BATCH: usize = 1000;
/// Share clones with content removed per batch (one `exists` call, then one `juicefs rmr` each).
pub const ORPHAN_BATCH: usize = 20;
/// A workspace folder modified, or whose `workspaces` row was written, more recently than this is never removed: it
/// may belong to something being made right now (a clone whose share magpie has not committed yet).
pub const RECENT: Duration = Duration::from_hours(24);

impl Referenced for magpie_api::client::Client {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            let req = magpie_api::ops::WorkspacesExistsRequest { ids: ids.to_vec() };
            self.service(None)
                .workspaces_exists(&req)
                .await
                .map(|r| r.exists)
                .map_err(|e| format!("magpie workspaces/exists: {e}"))
        })
    }

    fn import_incomplete(&self) -> Fut<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            let s = self
                .service(None)
                .legacy_import()
                .await
                .map_err(|e| format!("magpie legacy-import: {e}"))?;
            Ok(if s.complete {
                None
            } else {
                Some(
                    s.why
                        .unwrap_or_else(|| "magpie does not record its import complete".to_owned()),
                )
            })
        })
    }
}

/// What `legacy-cleanup` removes, and what it keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlan {
    /// `users` and `sent` (those present), relative to the mount: magpie's import holds their contents.
    pub trees: Vec<String>,
    /// Why the trees stay, when magpie does not record its import complete ([`Referenced::import_incomplete`]): the
    /// trees hold the only other copy of what it imported. The folder removals go on; the run is not complete.
    pub trees_held: Option<String>,
    /// Empty `ws/<id>` folders that magpie does not reference and no live sandbox uses.
    pub empty_workspaces: Vec<Uuid>,
    /// Old shares' clones (`legacy_shares`) with content that magpie does not reference, of shares revoked in the old
    /// UI or deleted on purpose through hermit (`legacy_share_deletes`). A live share's clone magpie does not name blocks the
    /// cleanup instead ([`CleanupPlan::shares_not_carried`]): magpie's import may have missed it.
    pub orphan_shares: Vec<Uuid>,
    /// Old shares cloned once whose clone is gone now and that magpie does not reference (deleted on purpose before
    /// the delete was recorded): carried over, and removed since, as `<share id>: ws/<id>`.
    pub shares_gone: Vec<String>,
    /// Old share folders the operator let go (`--drop-share <id>`: no committed share row, and not modified for
    /// [`RECENT`]), as `<share id>: <why it would block>`.
    pub dropped: Vec<String>,
    /// `workspaces` rows older than [`RECENT`] whose folder is gone (a removal that stopped before its row went: a
    /// crash between the two), with no live sandbox: their row and quota go.
    pub stale_rows: Vec<Uuid>,
    /// Folders kept because magpie references them.
    pub kept_referenced: u64,
    /// Folders kept because a sandbox that is not gone has that workspace.
    pub kept_live: u64,
    /// Folders kept because they, or their `workspaces` row, are younger than [`RECENT`].
    pub kept_recent: u64,
    /// Workspace folders with content that are not old shares' clones (never removed here).
    pub kept_with_content: u64,
    /// Old shares whose data the switch has not carried over, as `<share id>: <why>`. Any one blocks the whole
    /// cleanup: `users/` (or the clone magpie does not name) holds their only copy.
    pub shares_not_carried: Vec<String>,
}

/// One `ws/<uuid>` folder: whether it is empty and when it was last modified.
struct Folder {
    id: Uuid,
    empty: bool,
    modified: SystemTime,
}

/// The `ws/<uuid>` folders under `root` (other names, such as a clone's temporary tree, are not workspaces and are
/// left alone).
fn workspace_folders(root: &Path) -> std::io::Result<Vec<Folder>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join("ws"))? {
        let e = e?;
        let Some(id) = e.file_name().to_str().and_then(|n| Uuid::try_parse(n).ok()) else {
            continue;
        };
        if id.to_string() != e.file_name().to_string_lossy() {
            continue;
        }
        let m = std::fs::symlink_metadata(e.path())?;
        if !m.is_dir() {
            continue;
        }
        let empty = std::fs::read_dir(e.path())?.next().is_none();
        out.push(Folder {
            id,
            empty,
            modified: m.modified().unwrap_or(SystemTime::now()),
        });
    }
    out.sort_by_key(|f| f.id);
    Ok(out)
}

fn recent(t: SystemTime) -> bool {
    t.elapsed().map_or(true, |age| age < RECENT)
}

/// What hermit's import recorded of one old share.
struct LegacyShare {
    workspace: Uuid,
    revoked: bool,
    /// Cloned (or found cloned) once.
    cloned: bool,
    /// Deleted on purpose through hermit, or by an earlier cleanup.
    deleted: bool,
}

impl LegacyShare {
    /// Revoked in the old UI or deleted on purpose since: its clone may go, and it needs no clone.
    fn ended(&self) -> bool {
        self.revoked || self.deleted
    }
}

async fn legacy_shares(hermit: &Pool) -> Result<BTreeMap<String, LegacyShare>, LegacyError> {
    hermit
        .get()
        .await?
        .query(
            "select s.id, s.workspace, s.revoked, s.cloned_at is not null as cloned,
                    exists (select 1 from legacy_share_deletes d where d.workspace = s.workspace) as deleted
               from legacy_shares s",
            &[],
        )
        .await?
        .iter()
        .map(|r| {
            Ok((
                get::<String>(r, "id")?,
                LegacyShare {
                    workspace: get(r, "workspace")?,
                    revoked: get(r, "revoked")?,
                    cloned: get(r, "cloned")?,
                    deleted: get(r, "deleted")?,
                },
            ))
        })
        .collect()
}

/// When an old share folder (`users/<key>/shares/<id>` and its `ws/`) was last modified, the later of the two.
fn share_modified(root: &Path, ws: &Path) -> SystemTime {
    let at = |p: &Path| {
        std::fs::symlink_metadata(root.join(p))
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| SystemTime::now())
    };
    let own = ws.parent().map_or_else(SystemTime::now, at);
    own.max(at(ws))
}

fn hours_ago(t: SystemTime) -> String {
    t.elapsed()
        .map_or_else(|_| "just now".to_owned(), |d| format!("{} h ago", d.as_secs() / 3600))
}

/// The plan of one `legacy-cleanup`: reads the volume, hermit's live sandboxes, recent workspace rows and the import's
/// `legacy_shares`, checks that every old share is carried over, and asks magpie about every folder it could remove
/// and every old share's clone it needs to know about (at least one call: a magpie that does not answer stops the
/// cleanup). An old share is carried over when it was revoked in the old UI, deleted on purpose through hermit, has
/// its clone, or was cloned once and magpie does not reference the clone that is gone now; `drop` (`--drop-share`)
/// lets a folder with no committed share row go once it is [`RECENT`] old. Removes nothing.
///
/// # Errors
/// The volume or the database cannot be read, or magpie does not answer: then nothing is to be removed.
#[allow(clippy::too_many_lines)] // one pass over every kind of old share and folder
pub async fn cleanup_plan(
    root: &Path,
    hermit: &Pool,
    magpie: &dyn Referenced,
    drop: &[String],
) -> Result<CleanupPlan, LegacyError> {
    let r = root.to_owned();
    let (folders, trees, shares) = tokio::task::spawn_blocking(move || {
        let trees: Vec<String> = ["users", "sent"]
            .into_iter()
            .filter(|t| std::fs::symlink_metadata(r.join(t)).is_ok_and(|m| m.is_dir()))
            .map(str::to_owned)
            .collect();
        let folders = workspace_folders(&r).map_err(|e| format!("ws/: {e}"))?;
        let shares: Vec<(String, SystemTime, bool)> = old_shares(&r)
            .map_err(|e| format!("users/: {e}"))?
            .into_iter()
            .map(|(id, rel)| {
                let at = share_modified(&r, &rel);
                // an unreadable folder counts as having content (never taken for an empty share)
                let empty = std::fs::read_dir(r.join(&rel)).is_ok_and(|mut d| d.next().is_none());
                (id, at, empty)
            })
            .collect();
        Ok::<_, String>((folders, trees, shares))
    })
    .await
    .map_err(|e| LegacyError::Other(e.to_string()))?
    .map_err(LegacyError::Other)?;
    let c = hermit.get().await?;
    let ids = |rows: Vec<aviary_pg::Row>| -> Result<BTreeSet<Uuid>, LegacyError> {
        rows.iter()
            .map(|r| r.try_get::<_, Uuid>(0).map_err(|e| LegacyError::Db(e.into())))
            .collect()
    };
    let live = ids(c
        .query("select conversation_id from sandboxes where status <> 'gone'", &[])
        .await?)?;
    let recent_rows = ids(c
        .query(
            "select id from workspaces where created_at > now() - $1::bigint * interval '1 second'",
            &[&i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX)],
        )
        .await?)?;
    let old_rows = ids(c
        .query(
            "select id from workspaces where created_at <= now() - $1::bigint * interval '1 second'",
            &[&i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX)],
        )
        .await?)?;
    let legacy = legacy_shares(hermit).await?;
    let by_workspace: BTreeMap<Uuid, (&str, &LegacyShare)> =
        legacy.iter().map(|(id, s)| (s.workspace, (id.as_str(), s))).collect();
    let present: BTreeSet<Uuid> = folders.iter().map(|f| f.id).collect();
    let empty_folders: BTreeSet<Uuid> = folders.iter().filter(|f| f.empty).map(|f| f.id).collect();
    let trees_held = if trees.is_empty() {
        None
    } else {
        magpie
            .import_incomplete()
            .await
            .map_err(LegacyError::Other)?
            .map(|why| format!("magpie's import is not recorded complete: {why}"))
    };
    let mut plan = CleanupPlan {
        trees,
        trees_held,
        stale_rows: old_rows
            .iter()
            .filter(|w| !present.contains(w) && !live.contains(w))
            .copied()
            .collect(),
        ..CleanupPlan::default()
    };
    // clones made once and gone now: carried over only if magpie no longer references them
    let mut gone_clones: Vec<(String, Uuid)> = Vec::new();
    for (id, modified, old_empty) in &shares {
        let why = match legacy.get(id) {
            None => format!(
                "{id}: no committed share row was imported (being made, a failed make, or the import did not run); \
                 its folder was modified {}",
                hours_ago(*modified)
            ),
            Some(s) if s.ended() => continue,
            // a folder hermit never cloned into, empty while the share's is not: something else made it
            Some(s) if !s.cloned && empty_folders.contains(&s.workspace) && !old_empty => {
                format!("{id}: ws/{} exists empty, not the share's clone", s.workspace)
            }
            Some(s) if present.contains(&s.workspace) => continue,
            Some(s) if s.cloned => {
                gone_clones.push((id.clone(), s.workspace));
                continue;
            }
            Some(s) => format!("{id}: a live share never cloned into ws/{}", s.workspace),
        };
        if !drop.contains(id) {
            plan.shares_not_carried.push(why);
        } else if legacy.contains_key(id) {
            plan.shares_not_carried.push(format!(
                "{why} (--drop-share takes only a folder with no committed share row)"
            ));
        } else if recent(*modified) {
            plan.shares_not_carried
                .push(format!("{why} (--drop-share refused: modified in the last 24 h)"));
        } else {
            tracing::warn!(share = %id, why = %why, "legacy-cleanup: old share folder dropped by the operator (--drop-share)");
            plan.dropped.push(why);
        }
    }
    let mut empty = Vec::new();
    let mut clones = Vec::new();
    for f in &folders {
        if live.contains(&f.id) {
            plan.kept_live += 1;
        } else if recent(f.modified) || recent_rows.contains(&f.id) {
            plan.kept_recent += 1;
        } else if f.empty {
            empty.push(f.id);
        } else if by_workspace.contains_key(&f.id) {
            clones.push(f.id);
        } else {
            plan.kept_with_content += 1;
        }
    }
    let gone_ids: Vec<Uuid> = gone_clones.iter().map(|(_, w)| *w).collect();
    let named = referenced(magpie, &empty, EXISTS_BATCH).await?;
    let named_clones = referenced(magpie, &clones, EXISTS_BATCH).await?;
    let named_gone = referenced(magpie, &gone_ids, EXISTS_BATCH).await?;
    if empty.is_empty() && clones.is_empty() && gone_ids.is_empty() {
        // nothing to ask about: magpie still has to answer before its import's trees go
        magpie.exists(&[]).await.map_err(LegacyError::Other)?;
    }
    for id in &empty {
        if named.contains(id) {
            plan.kept_referenced += 1;
        } else {
            plan.empty_workspaces.push(*id);
        }
    }
    for id in &clones {
        if named_clones.contains(id) {
            plan.kept_referenced += 1;
            continue;
        }
        match by_workspace.get(id) {
            Some((_, s)) if s.ended() => plan.orphan_shares.push(*id),
            Some((sid, _)) => plan.shares_not_carried.push(format!(
                "{sid}: magpie does not reference ws/{id}, the clone of a live share (its import may have missed it)"
            )),
            None => plan.kept_with_content += 1,
        }
    }
    for (sid, w) in &gone_clones {
        if named_gone.contains(w) {
            plan.shares_not_carried
                .push(format!("{sid}: its clone ws/{w} is missing and magpie references it"));
        } else {
            plan.shares_gone.push(format!("{sid}: ws/{w}"));
        }
    }
    Ok(plan)
}

/// The ids among `ids` magpie names, asked in batches of `batch`.
async fn referenced(magpie: &dyn Referenced, ids: &[Uuid], batch: usize) -> Result<BTreeSet<Uuid>, LegacyError> {
    let mut named = BTreeSet::new();
    for chunk in ids.chunks(batch) {
        named.extend(magpie.exists(chunk).await.map_err(LegacyError::Other)?);
    }
    Ok(named)
}

/// What [`cleanup_apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupDone {
    pub trees_removed: Vec<String>,
    pub workspaces_removed: u64,
    /// Stale `workspaces` rows removed ([`CleanupPlan::stale_rows`]).
    pub rows_removed: u64,
    /// Folders kept on the re-check right before their removal (`ws/<id>: <why>`): magpie or a sandbox took them,
    /// a delete of hermit's own is in flight, or an empty one got content.
    pub kept: Vec<String>,
    /// Removals that failed, as `<path>: <error>`.
    pub failed: Vec<String>,
}

impl CleanupDone {
    /// Whether everything planned was removed or kept on purpose (no failure).
    #[must_use]
    pub fn complete(&self, plan: &CleanupPlan) -> bool {
        self.failed.is_empty() && self.trees_removed.len() == plan.trees.len()
    }
}

async fn unmark(hermit: &Pool, id: Uuid) -> Result<(), LegacyError> {
    hermit
        .get()
        .await?
        .execute("delete from workspace_deletes where id = $1", &[&id])
        .await?;
    Ok(())
}

/// Carries out `plan`: `juicefs rmr` of each tree, then the workspace folders batch by batch, then the stale rows.
/// magpie is asked about a batch first; the folders it does not name are marked as being deleted (`workspace_deletes`:
/// a start or an ensure refuses them from then on), then magpie is asked again about those (it may have taken one in
/// between), then, right before each removal, its mark is renewed (a mark lapses after [`crate::db::DELETE_GUARD`],
/// and is renewed while a long `juicefs rmr` runs) and hermit's sandboxes and the folder's age are checked again; a
/// folder that fails any of these is kept and unmarked. A crash leaves at most the folders of one batch that magpie
/// did not name a moment before marked, until their marks lapse. An empty folder goes with `rmdir` (which keeps one
/// that got content since the plan), an orphan share clone with `juicefs rmr`; then its quota, its `workspaces` row
/// and its mark. A stale row ([`CleanupPlan::stale_rows`]) goes, with its quota, once its folder is still absent.
///
/// # Errors
/// A database failure.
pub async fn cleanup_apply(
    root: &Path,
    cli: &dyn JfsCli,
    hermit: &Pool,
    magpie: &dyn Referenced,
    plan: &CleanupPlan,
) -> Result<CleanupDone, LegacyError> {
    let mut done = CleanupDone::default();
    let trees: &[String] = if plan.trees_held.is_some() { &[] } else { &plan.trees };
    for t in trees {
        match cli.rmr(&root.join(t)).await {
            Ok(()) => done.trees_removed.push(t.clone()),
            Err(e) => done.failed.push(format!("{t}: {e}")),
        }
    }
    let batches = plan
        .empty_workspaces
        .chunks(EXISTS_BATCH)
        .map(|b| (b, false))
        .chain(plan.orphan_shares.chunks(ORPHAN_BATCH).map(|b| (b, true)));
    for (batch, orphan) in batches {
        // asked before anything is marked: a folder magpie names is never refused to its conversation
        let before = match referenced(magpie, batch, EXISTS_BATCH).await {
            Ok(n) => n,
            Err(e) => {
                for id in batch {
                    done.failed.push(format!("ws/{id}: not removed: {e}"));
                }
                continue;
            }
        };
        let mut marked = Vec::with_capacity(batch.len());
        for id in batch {
            if before.contains(id) {
                done.kept.push(format!("ws/{id}: magpie references it now"));
                continue;
            }
            if crate::db::workspace_deleting(hermit, *id).await? {
                done.kept
                    .push(format!("ws/{id}: a delete of hermit's own is in flight"));
                continue;
            }
            crate::db::mark_workspace_deleting(hermit, *id).await?;
            marked.push(*id);
        }
        let named = match referenced(magpie, &marked, EXISTS_BATCH).await {
            Ok(n) => n,
            Err(e) => {
                for id in &marked {
                    unmark(hermit, *id).await?;
                    done.failed.push(format!("ws/{id}: not removed: {e}"));
                }
                continue;
            }
        };
        for id in marked {
            // the mark lapses after DELETE_GUARD; a batch can take longer, so each folder's mark is fresh when it goes
            crate::db::mark_workspace_deleting(hermit, id).await?;
            if let Some(why) = keep_now(root, hermit, id, &named).await? {
                unmark(hermit, id).await?;
                done.kept.push(format!("ws/{id}: {why}"));
                continue;
            }
            let dir = root.join("ws").join(id.to_string());
            let removed = if orphan {
                crate::db::record_share_deleted(hermit, id).await?;
                rmr_marked(cli, hermit, id, &dir).await
            } else {
                let d = dir.clone();
                match tokio::task::spawn_blocking(move || std::fs::remove_dir(&d)).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) if e.raw_os_error() == Some(rustix::io::Errno::NOTEMPTY.raw_os_error()) => {
                        unmark(hermit, id).await?;
                        done.kept.push(format!("ws/{id}: it has content now"));
                        continue;
                    }
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            };
            if let Err(e) = removed {
                unmark(hermit, id).await?;
                done.failed.push(format!("ws/{id}: {e}"));
                continue;
            }
            if let Err(e) = cli.quota_delete(&format!("/ws/{id}")).await {
                tracing::info!(workspace = %id, error = %e, "quota delete after the cleanup's removal");
            }
            crate::db::forget_workspace(hermit, id).await;
            done.workspaces_removed += 1;
        }
    }
    remove_stale_rows(root, cli, hermit, &plan.stale_rows, &mut done).await;
    Ok(done)
}

/// The rows and quotas of [`CleanupPlan::stale_rows`] whose folder is still absent.
async fn remove_stale_rows(root: &Path, cli: &dyn JfsCli, hermit: &Pool, ids: &[Uuid], done: &mut CleanupDone) {
    for id in ids {
        let dir = root.join("ws").join(id.to_string());
        let gone = tokio::task::spawn_blocking(
            move || matches!(std::fs::symlink_metadata(dir), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        )
        .await
        .unwrap_or(false);
        if !gone {
            done.kept.push(format!("ws/{id}: its folder is back"));
            continue;
        }
        if let Err(e) = cli.quota_delete(&format!("/ws/{id}")).await {
            tracing::info!(workspace = %id, error = %e, "quota delete of a stale workspace row");
        }
        crate::db::forget_workspace(hermit, *id).await;
        done.rows_removed += 1;
    }
}

/// `juicefs rmr` of a marked folder, its mark renewed every third of [`crate::db::DELETE_GUARD`] while it runs (a
/// large tree can take longer than the guard).
async fn rmr_marked(cli: &dyn JfsCli, hermit: &Pool, id: Uuid, dir: &Path) -> Result<(), String> {
    let rmr = cli.rmr(dir);
    tokio::pin!(rmr);
    let mut renew = tokio::time::interval(crate::db::DELETE_GUARD / 3);
    renew.tick().await;
    loop {
        tokio::select! {
            r = &mut rmr => return r.map_err(|e| e.to_string()),
            _ = renew.tick() => {
                if let Err(e) = crate::db::mark_workspace_deleting(hermit, id).await {
                    tracing::warn!(workspace = %id, error = %e, "legacy-cleanup: delete mark not renewed");
                }
            }
        }
    }
}

/// Why folder `id` must be kept now (the re-check right before its removal), if it must.
async fn keep_now(
    root: &Path,
    hermit: &Pool,
    id: Uuid,
    named: &BTreeSet<Uuid>,
) -> Result<Option<&'static str>, LegacyError> {
    if named.contains(&id) {
        return Ok(Some("magpie references it now"));
    }
    let c = hermit.get().await?;
    let busy = c
        .query_opt(
            "select 1 from sandboxes where conversation_id = $1 and status <> 'gone'
             union all
             select 1 from workspaces where id = $1 and created_at > now() - $2::bigint * interval '1 second'
             limit 1",
            &[&id, &i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX)],
        )
        .await?;
    if busy.is_some() {
        return Ok(Some("a sandbox has it now, or it was made recently"));
    }
    let dir = root.join("ws").join(id.to_string());
    let m = tokio::task::spawn_blocking(move || std::fs::symlink_metadata(dir))
        .await
        .map_err(|e| LegacyError::Other(e.to_string()))?;
    Ok(match m {
        Ok(m) if !m.is_dir() => Some("it is not a folder any more"),
        Ok(m) if recent(m.modified().unwrap_or(SystemTime::now())) => Some("it was modified recently"),
        Ok(_) => None,
        Err(_) => Some("it is gone already"),
    })
}

/// What one `legacy-cleanup` run decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Cleanup {
    /// Without `--confirm`: the plan, and nothing removed.
    Refused {
        plan: CleanupPlan,
    },
    /// With `--confirm`, but an old share folder is not carried over ([`CleanupPlan::shares_not_carried`]): nothing
    /// removed.
    Blocked {
        plan: CleanupPlan,
    },
    Done {
        plan: CleanupPlan,
        done: CleanupDone,
    },
}

/// `hermit legacy-cleanup [--confirm] [--drop-share <id>]…`: the plan ([`cleanup_plan`]), then, only with `confirm`
/// and every old share carried over, [`cleanup_apply`]. `users/` and `sent/` hold the only other copy of what magpie
/// imported, so they go only when magpie records its import complete (`GET /v1/hermit/legacy-import`: the last
/// `magpie import verify --all` was clean with every conversation owned, and nothing changed since); otherwise
/// [`CleanupPlan::trees_held`] says why, the folders still go, and the run is not complete.
///
/// # Errors
/// As [`cleanup_plan`] (nothing removed) and [`cleanup_apply`].
pub async fn legacy_cleanup(
    root: &Path,
    hermit: &Pool,
    magpie: &dyn Referenced,
    cli: &dyn JfsCli,
    confirm: bool,
    drop: &[String],
) -> Result<Cleanup, LegacyError> {
    let plan = cleanup_plan(root, hermit, magpie, drop).await?;
    if !confirm {
        return Ok(Cleanup::Refused { plan });
    }
    if !plan.shares_not_carried.is_empty() {
        return Ok(Cleanup::Blocked { plan });
    }
    let done = cleanup_apply(root, cli, hermit, magpie, &plan).await?;
    Ok(Cleanup::Done { plan, done })
}
