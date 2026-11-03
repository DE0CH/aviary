//! The reverse export (cutover 14.3, `magpie import export [--since <time>] [<id>…]`): magpie's copy of each owned
//! conversation with events after `since` (or created after it) written back to the old stack, so that a fresh old
//! child reads what magpie holds. The caller has fenced the conversation (the reverse hand-over, 14.2, is WP-52's);
//! the export writes nothing to magpie.
//!
//! - Transcript files: every cc file reconstructed through magpie's read path and written to
//!   `users/<key>/claude/<cc path>` (a temporary file in the same folder, then a rename; mode 0600, uid and gid 1000
//!   as Claude Code writes them, when the export runs as root).
//! - Auto-memory (decision 6): the conversation's `projects/-workspace/memory/**` goes to
//!   `users/<key>/claude/aviary-memory/<conversation>/`, which the old stack does not read.
//! - The sessions row (`cc_u<key>.sessions`): upserted with the conversation's id, Claude Code session id, title,
//!   `name_source`, Recently deleted, `updated_at` and `jsonl_path`. A person with no schema yet gets no row.
//! - Failed turns after `since` that never reached Claude Code (no Claude Code event in the turn) become
//!   `jarvis_failed_turns` rows with the old sentence of their error.
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
    /// Its shares written to the old side, and old share links revoked.
    pub shares: crate::export_more::Shares,
}

fn ts(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// The conversations to export: `ids` when given, else every owned conversation with an event after `since` or
/// created after it (every owned one without `since`).
///
/// # Errors
/// A statement error.
pub async fn targets(imp: &Importer, since: Option<OffsetDateTime>, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    let conn = imp.magpie.pool.get().await?;
    let rows = if ids.is_empty() {
        conn.query(
            "select c.id from conversations c where not c.migrating and
                    ($1::timestamptz is null or c.created_at > $1
                     or exists (select 1 from events e where e.conversation_id = c.id and e.at > $1))
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

/// Runs the export.
///
/// # Errors
/// The read of the targets; per-conversation errors are in the result.
pub async fn export(imp: &Importer, since: Option<OffsetDateTime>, ids: &[Uuid]) -> Result<Exported> {
    let mut out = Exported::default();
    for id in targets(imp, since, ids).await? {
        match conversation(imp, id, since).await {
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

pub(crate) fn own(p: &Path) {
    // only root can give the file to Claude Code's uid (the export's writer sidecar runs as root)
    match std::os::unix::fs::chown(p, Some(OWNER_ID), Some(OWNER_ID)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(e) => tracing::warn!(error = %e, "chown failed"),
    }
}

/// A temporary file beside `dest`, written by `fill`, then renamed over `dest` (folders made and owned).
async fn replace<F, Fut>(dest: &Path, write: F) -> Result<u64>
where
    F: FnOnce(std::fs::File) -> Fut,
    Fut: std::future::Future<Output = Result<(std::fs::File, u64)>>,
{
    let dir = dest
        .parent()
        .ok_or_else(|| Error::Source(format!("no folder for {}", dest.display())))?;
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        own(dir);
    }
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
    let r = write(opened).await;
    let (done, n) = match r {
        Ok(x) => x,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    done.sync_all()?;
    drop(done);
    own(&tmp);
    std::fs::rename(&tmp, dest)?;
    Ok(n)
}

/// Writes magpie's reconstruction of `path` of cc scope `scope` (a conversation or a share snapshot) to `dest`.
pub(crate) async fn write_scope(imp: &Importer, scope: Uuid, path: &str, dest: &Path) -> Result<u64> {
    let q = CcFileQuery {
        path: path.to_owned(),
        offset: None,
        length: None,
    };
    let res = reconstruct::scope_file(imp.magpie.clone(), scope, &q).await?;
    replace(dest, |mut f| async move {
        let mut body = res.into_body();
        let mut n = 0u64;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| Error::Source(format!("the read of {path} ended early: {e}")))?;
            if let Ok(data) = frame.into_data() {
                f.write_all(&data)?;
                n += data.len() as u64;
            }
        }
        Ok((f, n))
    })
    .await
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

async fn conversation(imp: &Importer, id: Uuid, since: Option<OffsetDateTime>) -> Result<Conversation> {
    let m = meta(imp, id).await?;
    let key = aviary_core::log::person_key(&m.person);
    let claude = crate::tree::claude_dir(&imp.jfs, &key);
    let mut out = Conversation {
        conversation: id,
        person_key: key.clone(),
        ..Conversation::default()
    };
    for (path, _) in crate::verify::magpie_files(imp, id).await? {
        let dest = destination(&claude, id, &path)?;
        out.bytes += write_scope(imp, id, &path, &dest).await?;
        out.files += 1;
        if path.starts_with(crate::tree::MEMORY) {
            out.memory += 1;
        }
    }
    let base = imp.jfs.join("sent").join(id.to_string());
    out.sent_files = write_sent(imp, id, since, None, &base).await?.len();
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
                                               \"isArchived\", deleted_at, created_at, updated_at, jsonl_path, model)
                 values ($1, 'claude', $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 on conflict (session_id) do update set provider_session_id = excluded.provider_session_id,
                     custom_name = excluded.custom_name, name_source = excluded.name_source,
                     \"isArchived\" = excluded.\"isArchived\", deleted_at = excluded.deleted_at,
                     updated_at = excluded.updated_at, jsonl_path = excluded.jsonl_path,
                     model = coalesce(excluded.model, {schema}.sessions.model)"
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
            ],
        )
        .await?;
        out.row = true;
        out.failed_turns = failed(imp, &conn, &schema, id, since).await?;
    }
    out.shares = crate::export_more::shares(imp, id, &key).await?;
    tracing::info!(conversation = %id, files = out.files, row = out.row, "conversation exported");
    Ok(out)
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
        let ended: Option<OffsetDateTime> = r.get(2);
        let text: Option<String> = r.get(3);
        let user_at: Option<OffsetDateTime> = r.get(4);
        let error_at = ts(ended.unwrap_or_else(OffsetDateTime::now_utc));
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
        replace(&dest, |mut f| async move {
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
