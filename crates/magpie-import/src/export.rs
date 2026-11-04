//! The reverse export (cutover 14.3, `magpie import export [--since <time>] [<id>…]`): magpie's copy of each owned
//! conversation with events after `since` (or created after it) written back to the old stack, so that a fresh old
//! child reads what magpie holds. It runs in the fenced state (14.3): a conversation without a live reverse fence
//! (`legacy_fence_at` set on the owned conversation by the reverse hand-over, 14.2, WP-52's, and younger than its
//! [`magpie::limits::FENCE_LEASE`]) is the export's error and is not written, unless `--unfenced` says the operator
//! has stopped writes some other way. The export renews the fence's lease before each file and before the
//! `cloudcli` writes, so a conversation larger than one lease still exports; a lease that lapsed meanwhile is the
//! conversation's error. Whatever the fence does, the export reads the conversation's head seq, cc file versions, row
//! and shares before writing and again after: a conversation that changed in magpie while it was written (a native write the
//! fence did not hold) is the conversation's error, to be exported again. Its writes to magpie are that lease and,
//! for an imported conversation, the release record (`import_sources.files`) set to the files it wrote and deleted.
//! It checks the mount first and stops when the leader lock is lost.
//!
//! One Claude Code session id, one old-side transcript: a person who continued a share twice (or their own share)
//! has two conversations with the same `provider_session_id` (magpie does not rewrite it on continue), which the old
//! side would read from the same `projects/-workspace/<psid>.jsonl`. Only the person's earliest conversation with
//! that session id (migrating ones included: a legacy one owns the old file) is exported under it; each other one is
//! the export's error. A permanent deletion leaves a session id that another conversation of the person still uses.
//!
//! Each exported conversation's result lists the files written with their stats and sha256 (`recorded`, in
//! `import_sources.files`' shape), which the reverse hand-over's reopen (14.2 step 5) stores so the importer's next
//! pass sees them as already synced.
//!
//! - Transcript files: every cc file reconstructed through magpie's read path and written to
//!   `users/<key>/claude/<cc path>` (a temporary file in the same folder, then a rename; mode 0600, uid and gid 1000
//!   as Claude Code writes them: a file that cannot be given to that uid fails the export, since the old sandbox could
//!   not read it). A file the old side has that magpie's scope no longer has (removed or relocated on the new side)
//!   is deleted: any file under the conversation's session folder's `tool-results/` and `subagents/` (the parts the
//!   importer imports; other files there are never in magpie's scope and stay) or its
//!   `aviary-memory/<conversation>/` folder, and any path the importer recorded for it (`import_sources.files`); the
//!   person's shared memory folder is never touched.
//! - Auto-memory (decision 6): the conversation's `projects/-workspace/memory/**` goes to
//!   `users/<key>/claude/aviary-memory/<conversation>/`, which the old stack does not read.
//! - The sessions row (`cc_u<key>.sessions`): upserted with the conversation's id, Claude Code session id, title,
//!   `name_source`, Recently deleted, `updated_at`, `jsonl_path` and `project_path` (`/workspace`, with the
//!   schema's `projects` row when it lacks one, as the old rows have). A person with no schema yet gets no row.
//! - Failed turns after `since` that never reached Claude Code (no Claude Code event in the turn) become
//!   `jarvis_failed_turns` rows with the old sentence of their error (once each, by their end time; a turn with no
//!   end time is not exported).
//! - Sent files after `since`: `sent/<conversation>/<toolUseId>/<path>` from their blobs.
//! - Shares made on the new side, shares revoked there, and conversations deleted for good there: [`crate::export_more`].
//! - Not exported: view-as rows (they stay in magpie's audit); uploads (already in the shared workspace).

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use http_body_util::BodyExt as _;
use magpie::ccfs::reconstruct;
use magpie_api::sandbox::CcFileQuery;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::{Error, Importer, Result};

/// Claude Code's uid and gid in the old sandboxes.
pub const OWNER_ID: u32 = 1000;

/// The project every old conversation ran in (`sessions.project_path`, `projects.project_path`).
pub const PROJECT_PATH: &str = "/workspace";

/// What the export did.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exported {
    /// Per conversation.
    pub conversations: Vec<Conversation>,
    /// Conversations deleted for good on the new side, deleted on the old side.
    pub purged: Vec<crate::export_more::Purged>,
    /// Per conversation errors.
    pub errors: Vec<(String, String)>,
}

/// One exported conversation.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Its id.
    pub conversation: Uuid,
    /// The person's key.
    pub person_key: String,
    /// cc files written (memory included).
    pub files: usize,
    /// Their bytes.
    pub bytes: u64,
    /// Of those, auto-memory files (written under `aviary-memory/`).
    pub memory: usize,
    /// The sessions row was written (false: the person has no schema yet).
    pub row: bool,
    /// Failed turns written.
    pub failed_turns: usize,
    /// Sent files written.
    pub sent_files: usize,
    /// Old-side files deleted because magpie's scope no longer has them.
    pub removed: usize,
    /// Its shares written to the old side, and old share links revoked.
    pub shares: crate::export_more::Shares,
    /// Every cc file written, as `import_sources.files` records a synced source (the old-side path, its stat after
    /// the write, its sha256): the reopen's record (14.2 step 5).
    pub recorded: crate::sync::Files,
}

fn ts(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// The conversations to export: `ids` when given, else every owned conversation created after `since`, or with an
/// event, a change (magpie's `changes` feed: a move to Recently deleted and a restore append no event) or a share
/// created or revoked after it (every owned one without `since`).
///
/// # Errors
/// A statement error.
pub async fn targets(imp: &Importer, since: Option<OffsetDateTime>, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    let conn = imp.magpie.pool.get().await?;
    let rows = if ids.is_empty() {
        conn.query(
            "select c.id from conversations c where not c.migrating and
                    ($1::timestamptz is null or c.created_at > $1
                     or exists (select 1 from events e where e.conversation_id = c.id and e.at > $1)
                     or exists (select 1 from changes x where x.person = c.person and x.conversation_id = c.id
                                and x.at > $1)
                     or exists (select 1 from shares s where s.conversation_id = c.id
                                and (s.created_at > $1 or s.revoked_at > $1)))
             order by c.id",
            &[&since],
        )
        .await?
    } else {
        conn.query(
            "select id from conversations where id = any($1) and not migrating order by id",
            &[&ids],
        )
        .await?
    };
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Renews the reverse fence of owned conversation `id` when it is live ([`magpie::legacy::fence::renew_reverse`]):
/// whether it was. A lapsed fence is not renewed: a native write may have gone through it.
async fn renew(imp: &Importer, id: Uuid) -> Result<bool> {
    Ok(magpie::legacy::fence::renew_reverse(&imp.magpie.pool, id).await?)
}

/// Keeps the reverse fence of `id` for the next write (nothing when exporting `--unfenced`).
async fn keep(imp: &Importer, id: Uuid, fenced: bool) -> Result<()> {
    if fenced && !renew(imp, id).await? {
        return Err(Error::Source(
            "its reverse fence lapsed during the export (a native write may have gone through): export it again"
                .to_owned(),
        ));
    }
    Ok(())
}

/// What the export reads of a conversation before writing it and again after: its head seq, every cc file's
/// version, the columns the sessions row takes, and its shares.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    head_seq: i64,
    files: Vec<(String, i64)>,
    row: SeenRow,
    shares: Vec<(String, Option<OffsetDateTime>, i64)>,
}

/// `title, name_source, deleted_at, provider_session_id, model, activity_at`.
type SeenRow = (
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
    Option<String>,
    Option<String>,
    OffsetDateTime,
);

async fn seen(imp: &Importer, id: Uuid) -> Result<Seen> {
    let conn = imp.magpie.pool.get().await?;
    let r = conn
        .query_one(
            "select head_seq, title, name_source, deleted_at, provider_session_id, model, activity_at
             from conversations where id = $1",
            &[&id],
        )
        .await?;
    let files = conn
        .query(
            "select path, version from cc_files where scope = $1 and kind <> 'dir' order by path",
            &[&id],
        )
        .await?
        .iter()
        .map(|f| (f.get(0), f.get(1)))
        .collect();
    let shares = conn
        .query(
            "select id, revoked_at, upto_seq from shares where conversation_id = $1 order by id",
            &[&id],
        )
        .await?
        .iter()
        .map(|s| (s.get(0), s.get(1), s.get(2)))
        .collect();
    Ok(Seen {
        head_seq: r.get(0),
        files,
        row: (r.get(1), r.get(2), r.get(3), r.get(4), r.get(5), r.get(6)),
        shares,
    })
}

/// The person's conversations whose Claude Code session id is `psid`, earliest first (migrating ones included).
pub(crate) async fn psid_users(imp: &Importer, person: &str, psid: &str) -> Result<Vec<Uuid>> {
    Ok(imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select id from conversations where person = $1 and coalesce(provider_session_id, id::text) = $2
             order by created_at, id",
            &[&person, &psid],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

/// Runs the export; `unfenced` exports conversations that are not reverse-fenced too.
///
/// # Errors
/// The mount not there, the read of the targets, the leader lock lost; per-conversation errors are in the result.
pub async fn export(imp: &Importer, since: Option<OffsetDateTime>, ids: &[Uuid], unfenced: bool) -> Result<Exported> {
    crate::tree::check_mount(&imp.jfs)?;
    let mut out = Exported::default();
    for id in targets(imp, since, ids).await? {
        imp.check_lock().await?;
        if !unfenced && !renew(imp, id).await? {
            out.errors.push((
                id.to_string(),
                "not reverse-fenced (or its fence lapsed): not exported (a native turn may be writing it; --unfenced \
                 overrides)"
                    .to_owned(),
            ));
            continue;
        }
        match conversation(imp, id, since, !unfenced).await {
            Ok(c) => out.conversations.push(c),
            Err(e) => {
                tracing::warn!(conversation = %id, error = %e, "conversation not exported");
                out.errors.push((id.to_string(), e.to_string()));
            }
        }
    }
    out.purged = crate::export_more::purged(imp, since, ids, &mut out.errors).await?;
    Ok(out)
}

/// A relative path with only normal components (no `..`, no root), joined under `base`.
pub(crate) fn under(base: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Error::Source(format!("not a relative path: {rel}")));
    }
    Ok(base.join(p))
}

/// Where cc path `path` of conversation `id` goes under the person's `claude/` folder.
fn destination(claude: &Path, id: Uuid, path: &str) -> Result<PathBuf> {
    match path.strip_prefix(crate::tree::MEMORY).and_then(|r| r.strip_prefix('/')) {
        Some(rest) => under(&claude.join("aviary-memory").join(id.to_string()), rest),
        None => under(claude, path),
    }
}

/// Gives `p` to `owner` (uid and gid; the writer sidecar runs as root). A file Claude Code's uid cannot own is one
/// the old sandbox cannot read (0600): an error, never a quiet success.
pub(crate) fn own(owner: Option<u32>, p: &Path) -> Result<()> {
    let Some(id) = owner else { return Ok(()) };
    std::os::unix::fs::lchown(p, Some(id), Some(id))
        .map_err(|e| Error::Source(format!("{} cannot be given to uid {id}: {e}", p.display())))
}

/// Makes `dir` and its missing parents, each given to `owner`.
pub(crate) fn make_dirs(owner: Option<u32>, dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        make_dirs(owner, parent)?;
    }
    match std::fs::create_dir(dir) {
        Ok(()) => own(owner, dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// A temporary file beside `dest`, written by `fill`, then renamed over `dest` (folders made and owned).
async fn replace<F, Fut>(owner: Option<u32>, dest: &Path, write: F) -> Result<u64>
where
    F: FnOnce(std::fs::File) -> Fut,
    Fut: std::future::Future<Output = Result<(std::fs::File, u64)>>,
{
    let dir = dest
        .parent()
        .ok_or_else(|| Error::Source(format!("no folder for {}", dest.display())))?;
    make_dirs(owner, dir)?;
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.aviary-export-{}", Uuid::new_v4().simple()));
    let opened = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?
    };
    let r = async {
        let (done, n) = write(opened).await?;
        done.sync_all()?;
        drop(done);
        own(owner, &tmp)?;
        std::fs::rename(&tmp, dest)?;
        Ok(n)
    }
    .await;
    if r.is_err() {
        // every failure takes its temporary file with it (a crash's is swept by a later export)
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// How old a leftover temporary file or share folder of the export must be before an export sweeps it (younger, it
/// may be another export's still being written).
pub const TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(600);

/// Removes the export's leftover temporary files and folders ([`crate::tree::is_export_temp`]) older than
/// [`TEMP_AGE`] in `dir` (and its subfolders when `deep`): what a crash or a kill left. How many.
pub(crate) fn sweep_temps(dir: &Path, deep: bool) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(0);
        }
        Err(e) => return Err(e.into()),
    };
    let mut n = 0;
    for e in entries {
        let e = e?;
        let m = e.metadata()?;
        let temp = e.file_name().to_str().is_some_and(crate::tree::is_export_temp);
        if temp {
            let old = m
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|a| a >= TEMP_AGE);
            if old {
                if m.is_dir() {
                    std::fs::remove_dir_all(e.path())?;
                } else {
                    std::fs::remove_file(e.path())?;
                }
                n += 1;
            }
        } else if deep && m.is_dir() {
            n += sweep_temps(&e.path(), true)?;
        }
    }
    Ok(n)
}

/// Writes magpie's reconstruction of `path` of cc scope `scope` (a conversation or a share snapshot) to `dest`.
pub(crate) async fn write_scope(imp: &Importer, scope: Uuid, path: &str, dest: &Path) -> Result<u64> {
    Ok(write_scope_recorded(imp, scope, path, dest).await?.size)
}

/// [`write_scope`], answering what was written as the importer records a synced source.
async fn write_scope_recorded(imp: &Importer, scope: Uuid, path: &str, dest: &Path) -> Result<crate::sync::Recorded> {
    use sha2::Digest as _;
    let q = CcFileQuery {
        path: path.to_owned(),
        offset: None,
        length: None,
    };
    let res = reconstruct::scope_file(imp.magpie.clone(), scope, &q).await?;
    let mut h = sha2::Sha256::new();
    let hasher = &mut h;
    let n = replace(imp.owner, dest, |mut f| async move {
        let mut body = res.into_body();
        let mut n = 0u64;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| Error::Source(format!("the read of {path} ended early: {e}")))?;
            if let Ok(data) = frame.into_data() {
                f.write_all(&data)?;
                hasher.update(&data);
                n += data.len() as u64;
            }
        }
        Ok((f, n))
    })
    .await?;
    let stat = crate::tree::restat(dest)?
        .ok_or_else(|| Error::Source(format!("{} is gone right after its write", dest.display())))?;
    Ok(crate::sync::Recorded {
        src: dest.to_string_lossy().into_owned(),
        size: n,
        mtime_ns: stat.mtime_ns,
        ino: stat.ino,
        sha256: crate::hex(&h.finalize()),
    })
}

struct Meta {
    person: String,
    psid: String,
    title: Option<String>,
    name_source: Option<String>,
    created: OffsetDateTime,
    activity: OffsetDateTime,
    deleted: Option<OffsetDateTime>,
    model: Option<String>,
}

async fn meta(imp: &Importer, id: Uuid) -> Result<Meta> {
    let r = imp
        .magpie
        .pool
        .get()
        .await?
        .query_one(
            "select person, coalesce(provider_session_id, id::text), title, name_source, created_at, activity_at,
                    deleted_at, model
             from conversations where id = $1",
            &[&id],
        )
        .await?;
    Ok(Meta {
        person: r.get(0),
        psid: r.get(1),
        title: r.get(2),
        name_source: r.get(3),
        created: r.get(4),
        activity: r.get(5),
        deleted: r.get(6),
        model: r.get(7),
    })
}

#[allow(clippy::too_many_lines)] // files, sessions row, failed turns, shares, each fenced
async fn conversation(imp: &Importer, id: Uuid, since: Option<OffsetDateTime>, fenced: bool) -> Result<Conversation> {
    // the snapshot first, then what is written from it: a change between the two is in the snapshot taken after
    let before = seen(imp, id).await?;
    let m = meta(imp, id).await?;
    let key = aviary_core::log::person_key(&m.person);
    let claude = crate::tree::claude_dir(&imp.jfs, &key);
    let users = psid_users(imp, &m.person, &m.psid).await?;
    if let Some(first) = users.first().filter(|f| **f != id) {
        return Err(Error::Source(format!(
            "its Claude Code session {} is also conversation {first}'s (a continued share), which the old side keeps \
             under that session id: not exported",
            m.psid
        )));
    }
    let mut out = Conversation {
        conversation: id,
        person_key: key.clone(),
        ..Conversation::default()
    };
    // what an earlier export that crashed left in this conversation's folders (and the person's share folders)
    let project = claude.join(crate::tree::PROJECT);
    sweep_temps(&project, false)?;
    for part in crate::tree::SESSION_PARTS {
        sweep_temps(&project.join(&m.psid).join(part), true)?;
    }
    sweep_temps(&claude.join("aviary-memory").join(id.to_string()), true)?;
    sweep_temps(&imp.jfs.join("sent").join(id.to_string()), true)?;
    sweep_temps(&imp.jfs.join("users").join(&key).join("shares"), false)?;
    let scope = crate::verify::magpie_files(imp, id).await?;
    let mut removed = Vec::new();
    let wrote = async {
        for path in scope.keys() {
            keep(imp, id, fenced).await?;
            let dest = destination(&claude, id, path)?;
            let rec = write_scope_recorded(imp, id, path, &dest).await?;
            out.bytes += rec.size;
            out.files += 1;
            if path.starts_with(crate::tree::MEMORY) {
                out.memory += 1;
            }
            out.recorded.insert(path.clone(), rec);
            crate::pause(imp.pause_in_fence).await;
        }
        keep(imp, id, fenced).await?;
        remove_stale(imp, id, &claude, &m.psid, &scope, &mut removed).await
    }
    .await;
    out.removed = removed.len();
    // what was written and deleted is the release record now (also when the export stopped part-way), so a released
    // conversation that stays owned verifies against the export's files and still sees the old stack's own writes
    record_export(imp, id, &out.recorded, &removed).await?;
    wrote?;
    let base = imp.jfs.join("sent").join(id.to_string());
    out.sent_files = write_sent(imp, id, since, None, &base).await?.len();
    keep(imp, id, fenced).await?;
    let schema = format!("cc_{key}");
    let conn = imp.cloudcli.get().await?;
    let has: bool = conn
        .query_one(
            "select to_regclass(format('%I.sessions', $1::text)) is not null",
            &[&schema],
        )
        .await?
        .get(0);
    if has {
        let jsonl = format!(
            "/var/lib/cloudcli/home/{key}/.claude/{}",
            crate::tree::main_path(&m.psid)
        );
        conn.execute(
            &format!(
                "insert into {schema}.sessions (session_id, provider, provider_session_id, custom_name, name_source,
                                               \"isArchived\", deleted_at, created_at, updated_at, jsonl_path, model,
                                               project_path)
                 values ($1, 'claude', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 on conflict (session_id) do update set provider_session_id = excluded.provider_session_id,
                     custom_name = excluded.custom_name, name_source = excluded.name_source,
                     \"isArchived\" = excluded.\"isArchived\", deleted_at = excluded.deleted_at,
                     updated_at = excluded.updated_at, jsonl_path = excluded.jsonl_path,
                     model = coalesce(excluded.model, {schema}.sessions.model),
                     project_path = coalesce({schema}.sessions.project_path, excluded.project_path)"
            ),
            &[
                &id.to_string(),
                &m.psid,
                &m.title,
                &m.name_source,
                &i32::from(m.deleted.is_some()),
                &m.deleted.map(ts),
                &ts(m.created),
                &ts(m.activity),
                &jsonl,
                &m.model,
                &PROJECT_PATH,
            ],
        )
        .await?;
        // the project the row belongs to (CloudCLI groups and searches by it), when the schema has none yet
        let projects: bool = conn
            .query_one(
                "select to_regclass(format('%I.projects', $1::text)) is not null",
                &[&schema],
            )
            .await?
            .get(0);
        if projects {
            let project_id = Uuid::new_v5(&crate::NS, format!("project:{key}:{PROJECT_PATH}").as_bytes()).to_string();
            conn.execute(
                &format!(
                    "insert into {schema}.projects (project_id, project_path) values ($1, $2)
                     on conflict do nothing"
                ),
                &[&project_id, &PROJECT_PATH],
            )
            .await?;
        }
        out.row = true;
        out.failed_turns = failed(imp, &conn, &schema, id, since).await?;
    }
    out.shares = crate::export_more::shares(imp, id, &key).await?;
    // nothing moved in magpie while the old side was written, and the fence held to the end
    keep(imp, id, fenced).await?;
    if seen(imp, id).await? != before {
        return Err(Error::Source(
            "it changed in magpie while it was exported (a native write the fence did not hold): export it again"
                .to_owned(),
        ));
    }
    tracing::info!(conversation = %id, files = out.files, row = out.row, "conversation exported");
    Ok(out)
}

/// Records the files the export wrote and deleted in an imported conversation's release record
/// (`import_sources.files`, released conversations only; a magpie-born one has none until the reopen creates it):
/// [`crate::verify`]'s check of a released conversation then holds the old side to what the export wrote, and what
/// the old stack writes after it is still a mismatch.
async fn record_export(imp: &Importer, id: Uuid, recorded: &crate::sync::Files, removed: &[String]) -> Result<()> {
    if recorded.is_empty() && removed.is_empty() {
        return Ok(());
    }
    let files = serde_json::to_value(recorded)?;
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        imp.check_lock_in(t).await?;
        t.execute(
            "update import_sources set files = (files - $3::text[]) || $2::jsonb
              where conversation_id = $1 and released_at is not null",
            &[&id, &files, &removed],
        )
        .await?;
        Ok(())
    })
    .await
}

/// Failed turns after `since` that never reached Claude Code, as `jarvis_failed_turns` rows (once each: a row with
/// the same `error_at` is not written again).
async fn failed(
    imp: &Importer,
    conn: &aviary_pg::Conn,
    schema: &str,
    id: Uuid,
    since: Option<OffsetDateTime>,
) -> Result<usize> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select t.error_code, t.error_detail, t.ended_at, u.text, u.at from turns t
             left join events u on u.conversation_id = t.conversation_id and u.seq = t.user_seq
             where t.conversation_id = $1 and t.state = 'failed' and t.origin <> 'import'
               and ($2::timestamptz is null or t.ended_at > $2)
               and not exists (select 1 from events e where e.conversation_id = t.conversation_id
                               and e.turn_id = t.id and e.kind like 'cc.%')
             order by t.ended_at",
            &[&id, &since],
        )
        .await?;
    let mut n = 0;
    for r in rows {
        let code: Option<String> = r.get(0);
        let detail: Option<String> = r.get(1);
        let Some(ended) = r.get::<_, Option<OffsetDateTime>>(2) else {
            // no end time to key it on: each export would add it again
            continue;
        };
        let text: Option<String> = r.get(3);
        let user_at: Option<OffsetDateTime> = r.get(4);
        let error_at = ts(ended);
        let sentence = crate::failed::sentence_of(code.as_deref().unwrap_or(""), detail.as_deref());
        let wrote = conn
            .execute(
                &format!(
                    "insert into {schema}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
                     select $1, $2, $3, $4, $5 where not exists
                       (select 1 from {schema}.jarvis_failed_turns where session_id = $1 and error_at = $5)"
                ),
                &[&id.to_string(), &text, &user_at.map(ts), &sentence, &error_at],
            )
            .await?;
        n += usize::try_from(wrote).unwrap_or(0);
    }
    Ok(n)
}

/// Deletes the old side's files of conversation `id` that magpie's scope `scope` no longer has: everything under its
/// session folder's `tool-results/` and `subagents/` (what the importer imports of it), its `aviary-memory/<id>/`
/// folder, and every path the importer
/// recorded for it (a main transcript before a relocation, an earlier session folder's files); never the person's
/// shared memory folder. Each cc path deleted goes into `removed`.
async fn remove_stale(
    imp: &Importer,
    id: Uuid,
    claude: &Path,
    psid: &str,
    scope: &std::collections::BTreeMap<String, u64>,
    removed: &mut Vec<String>,
) -> Result<()> {
    let memory = format!("{}/", crate::tree::MEMORY);
    let mut candidates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(r) = crate::verify::row(imp, id).await? {
        candidates.extend(r.files.into_keys().filter(|p| !p.starts_with(&memory)));
    }
    // only the session folder's parts the importer imports (tree::sources): any other file Claude Code or the old
    // stack keeps there is never in magpie's scope, and stays
    let mut session = Vec::new();
    for part in crate::tree::SESSION_PARTS {
        let folder = format!("{}/{psid}/{part}", crate::tree::PROJECT);
        under(claude, &folder)?;
        files_under(claude, &folder, &mut session)?;
    }
    candidates.extend(session);
    for p in &candidates {
        if scope.contains_key(p) {
            continue;
        }
        let f = under(claude, p)?;
        if std::fs::symlink_metadata(&f).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(&f)?;
            removed.push(p.clone());
        }
    }
    let mem = claude.join("aviary-memory").join(id.to_string());
    let mut exported = Vec::new();
    files_under(&mem, "", &mut exported)?;
    for rel in exported {
        let cc = format!("{memory}{rel}");
        if !scope.contains_key(&cc) {
            std::fs::remove_file(under(&mem, &rel)?)?;
            removed.push(cc);
        }
    }
    Ok(())
}

/// The files under `base/rel` as `rel/…` paths (`rel` empty: relative to `base`); nothing for a folder that is not
/// there. A name that is not UTF-8 is an error.
fn files_under(base: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    let dir = if rel.is_empty() {
        base.to_path_buf()
    } else {
        base.join(rel)
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let e = e?;
        let name = crate::tree::utf8_name(&e, rel)?;
        if crate::tree::is_export_temp(&name) {
            continue;
        }
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let t = e.file_type()?;
        if t.is_dir() {
            files_under(base, &child, out)?;
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// The sends of `id` after `since` and up to event `upto` written under `base` as `<toolUseId>/<path>` from their
/// blobs (the conversation's `sent/<id>/`, or a share folder's `sent/`); their paths, in send order.
pub(crate) async fn write_sent(
    imp: &Importer,
    id: Uuid,
    since: Option<OffsetDateTime>,
    upto: Option<i64>,
    base: &Path,
) -> Result<Vec<String>> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select tool_use_id, path, blob from sent_files where conversation_id = $1
               and ($2::timestamptz is null or sent_at > $2) and ($3::int8 is null or seq <= $3)
             order by seq, path",
            &[&id, &since, &upto],
        )
        .await?;
    let mut paths = Vec::new();
    for r in rows {
        let tool: String = r.get(0);
        let path: String = r.get(1);
        let blob: Vec<u8> = r.get(2);
        let sha = magpie::blobs::Sha::try_from(blob.as_slice())
            .map_err(|_| Error::Source(format!("sent file {path}: not a sha256")))?;
        let bytes = magpie::blobs::fetch_stored(&imp.magpie, &[sha])
            .await?
            .remove(&sha)
            .ok_or_else(|| Error::Source(format!("sent file {path}: its blob is missing")))?;
        let dest = under(&under(base, &tool)?, &path)?;
        replace(imp.owner, &dest, |mut f| async move {
            f.write_all(&bytes)?;
            Ok((f, bytes.len() as u64))
        })
        .await?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_goes_to_its_own_folder_and_paths_stay_beneath() {
        let c = Path::new("/jfs/users/u1/claude");
        let id = Uuid::nil();
        assert_eq!(
            destination(c, id, "projects/-workspace/memory/MEMORY.md").unwrap(),
            c.join(format!("aviary-memory/{id}/MEMORY.md"))
        );
        assert_eq!(
            destination(c, id, "projects/-workspace/a.jsonl").unwrap(),
            c.join("projects/-workspace/a.jsonl")
        );
        assert!(destination(c, id, "projects/../../etc/passwd").is_err());
        assert!(destination(c, id, "/etc/passwd").is_err());
    }
}
