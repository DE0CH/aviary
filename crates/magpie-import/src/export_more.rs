//! The reverse export's shares and permanent deletions (cutover 14.3; switch-final C14.2's completeness).
//!
//! - **Shares made on the new side**: each live magpie share of an exported conversation that `cc_shared.shares`
//!   lacks gets the old stack's layout under `users/<key>/shares/<id>/`: `transcript.jsonl` (the snapshot's main
//!   transcript, byte for byte through magpie's read path), `session/` (the snapshot's session folder: sub-agent
//!   transcripts, saved tool output), `sent/<toolUseId>/<path>` (the sends within the share, from their blobs) and
//!   `ws/` (the share's frozen workspace, `ws/<uuidv5(NS, "share:" + id)>`, cloned with `juicefs clone`, metadata
//!   only), then the row with the same id. No `messages.json`: the old share
//!   page builds a snapshot's history from its transcript and session folder when that file is absent
//!   (DE0CH/claudecodeui `jarvis-shares.ts` `frozenHistory`). A share revoked on the new side whose old row is live is
//!   revoked there (the row marked, the folder removed), as the old stack's own revoke does.
//! - **Deletions for good on the new side** (`import_purged`, written by magpie's purge): the old sessions row, the
//!   conversation's transcript and session folder, its exported memory folder, its failed turns, its sent files and its
//!   share links are deleted; the person's shared memory (`projects/-workspace/memory/`) is never touched. The
//!   workspace `ws/<id>` is hermit's, deleted by the purge's own outbox call. A tombstone whose id is a conversation in
//!   magpie again (re-imported or restored meanwhile) is skipped.

use std::path::{Path, PathBuf};

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::export::{OWNER_ID, own, under, write_scope};
use crate::{Error, Importer, Result};

/// The old title of a conversation that has none (`jarvisUntitledName()` in the staff's locale, zh-HK).
pub const UNTITLED: &str = "新對話";

/// Where the old pods mount the volume (the old rows' `dir` is a path there).
pub const OLD_JFS: &str = "/jfs";

/// What the share part did for one conversation.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shares {
    /// Shares written to the old side.
    pub written: Vec<String>,
    /// Old rows revoked because the share was revoked on the new side.
    pub revoked: Vec<String>,
}

/// One permanent deletion carried to the old side.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Purged {
    /// The conversation.
    pub conversation: Uuid,
    /// The person's key.
    pub person_key: String,
    /// Files and folders removed.
    pub removed: usize,
    /// The sessions row was deleted.
    pub row: bool,
    /// Share links revoked.
    pub shares_revoked: usize,
}

const SHARES_DDL: &str = "create schema if not exists cc_shared;
    create table if not exists cc_shared.shares (
      id text primary key,
      owner text not null,
      owner_name text not null,
      session_id text not null,
      title text not null,
      dir text not null,
      files jsonb not null default '[]',
      created_at timestamptz not null default now(),
      revoked_at timestamptz);
    create index if not exists shares_owner_session on cc_shared.shares (owner, session_id)";

/// A share's folder on the mount and as the old pods see it.
fn share_dirs(jfs: &Path, key: &str, id: &str) -> Result<(PathBuf, String)> {
    let rel = format!("users/{key}/shares/{id}");
    Ok((under(jfs, &rel)?, format!("{OLD_JFS}/{rel}")))
}

fn remove_tree(p: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).map(|()| true).map_err(Into::into),
        Ok(_) => std::fs::remove_file(p).map(|()| true).map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Revokes the old rows `ids` (those still live) and removes their folders.
async fn revoke_old(imp: &Importer, conn: &aviary_pg::Conn, rows: &[(String, String)]) -> Result<usize> {
    let mut n = 0;
    for (id, key) in rows {
        let done = conn
            .execute(
                "update cc_shared.shares set revoked_at = now() where id = $1 and revoked_at is null",
                &[id],
            )
            .await?;
        let (dir, _) = share_dirs(&imp.jfs, key, id)?;
        remove_tree(&dir)?;
        n += usize::try_from(done).unwrap_or(0);
    }
    Ok(n)
}

struct Share {
    id: String,
    owner: String,
    owner_name: String,
    title: Option<String>,
    upto: i64,
    snapshot: Uuid,
    psid: String,
    workspace: Uuid,
    created: OffsetDateTime,
    revoked: bool,
}

/// The share part of exporting `conversation` (owned by `person`, key `key`).
///
/// # Errors
/// A statement or file error (the conversation's export reports it).
pub async fn shares(imp: &Importer, conversation: Uuid, key: &str) -> Result<Shares> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select s.id, s.owner, s.owner_name, coalesce(s.title, c.title), s.upto_seq, s.snapshot,
                    coalesce(s.provider_session_id, c.provider_session_id, c.id::text), s.workspace_id, s.created_at,
                    s.revoked_at is not null
             from shares s join conversations c on c.id = s.conversation_id
             where s.conversation_id = $1 order by s.created_at, s.id",
            &[&conversation],
        )
        .await?;
    let mine: Vec<Share> = rows
        .iter()
        .map(|r| Share {
            id: r.get(0),
            owner: r.get(1),
            owner_name: r.get(2),
            title: r.get(3),
            upto: r.get(4),
            snapshot: r.get(5),
            psid: r.get(6),
            workspace: r.get(7),
            created: r.get(8),
            revoked: r.get(9),
        })
        .collect();
    let mut out = Shares::default();
    if mine.is_empty() {
        return Ok(out);
    }
    let conn = imp.cloudcli.get().await?;
    conn.batch_execute(SHARES_DDL).await?;
    let old: std::collections::HashMap<String, bool> = conn
        .query(
            "select id, revoked_at is not null from cc_shared.shares where session_id = $1",
            &[&conversation.to_string()],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let mut revoke = Vec::new();
    for s in &mine {
        match (old.get(&s.id), s.revoked) {
            (Some(false), true) => revoke.push((s.id.clone(), key.to_owned())),
            (None, false) => {
                write_share(imp, &conn, conversation, key, s).await?;
                out.written.push(s.id.clone());
            }
            _ => {}
        }
    }
    revoke_old(imp, &conn, &revoke).await?;
    out.revoked = revoke.into_iter().map(|(id, _)| id).collect();
    Ok(out)
}

async fn write_share(imp: &Importer, conn: &aviary_pg::Conn, conversation: Uuid, key: &str, s: &Share) -> Result<()> {
    let (dir, old_dir) = share_dirs(&imp.jfs, key, &s.id)?;
    let parent = dir
        .parent()
        .ok_or_else(|| Error::Source(format!("no folder for {}", dir.display())))?;
    crate::export::make_dirs(imp.owner, parent)?;
    // built under a temporary name beside it, then renamed into place (a folder left by a failed attempt goes first)
    let tmp = parent.join(format!(".{}.aviary-export-{}", s.id, Uuid::new_v4().simple()));
    let r = fill(imp, conversation, s, &tmp).await;
    let files = match r {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    remove_tree(&dir)?;
    std::fs::rename(&tmp, &dir)?;
    let files = serde_json::to_string(&files).map_err(|e| Error::Source(e.to_string()))?;
    let title = s
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(UNTITLED);
    conn.execute(
        "insert into cc_shared.shares (id, owner, owner_name, session_id, title, dir, files, created_at)
         values ($1, $2, $3, $4, $5, $6, $7::text::jsonb, $8) on conflict (id) do nothing",
        &[
            &s.id,
            &s.owner,
            &s.owner_name,
            &conversation.to_string(),
            &title,
            &old_dir,
            &files,
            &s.created,
        ],
    )
    .await?;
    tracing::info!(conversation = %conversation, "share exported");
    Ok(())
}

/// Writes the share's folder at `dir`; the files a viewer may download (the sends' paths, then the uploads).
async fn fill(imp: &Importer, conversation: Uuid, s: &Share, dir: &Path) -> Result<Vec<String>> {
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    own(imp.owner, dir)?;
    let main = crate::tree::main_path(&s.psid);
    let session = format!("{}/{}/", crate::tree::PROJECT, s.psid);
    let files = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select path from cc_files where scope = $1 and kind <> 'dir' order by path",
            &[&s.snapshot],
        )
        .await?;
    let mut wrote_main = false;
    for r in files {
        let path: String = r.get(0);
        let dest = if path == main {
            wrote_main = true;
            dir.join("transcript.jsonl")
        } else if let Some(rest) = path.strip_prefix(&session) {
            under(&dir.join("session"), rest)?
        } else {
            continue;
        };
        write_scope(imp, s.snapshot, &path, &dest).await?;
    }
    if !wrote_main {
        return Err(Error::Source(format!(
            "share {}: its snapshot has no main transcript",
            s.id
        )));
    }
    let sent = crate::export::write_sent(imp, conversation, None, Some(s.upto), &dir.join("sent")).await?;
    let ws = dir.join("ws");
    let src = under(&imp.jfs.join("ws"), &s.workspace.to_string())?;
    if src.is_dir() {
        clone_tree(imp.juicefs.as_deref(), &src, &ws).await?;
    } else {
        std::fs::create_dir_all(&ws)?;
        own(imp.owner, &ws)?;
    }
    let mut out: Vec<String> = Vec::new();
    for p in sent {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    let mut uploads = Vec::new();
    list_files(&ws, "uploads", &mut uploads)?;
    uploads.sort();
    for p in uploads {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

/// The regular files under `root/rel`, as `rel/…` paths (symlinks are not followed and not listed).
fn list_files(root: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(root.join(rel)) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let e = e?;
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let p = format!("{rel}/{name}");
        let t = e.file_type()?;
        if t.is_dir() {
            list_files(root, &p, out)?;
        } else if t.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

/// `src` to `dst` (which must not exist): `juicefs clone` with the importer's client, or (no client: tests) a
/// recursive copy that keeps symlinks as links.
async fn clone_tree(juicefs: Option<&Path>, src: &Path, dst: &Path) -> Result<()> {
    if let Some(bin) = juicefs {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::process::Command::new(bin)
                .arg("clone")
                .arg(src)
                .arg(dst)
                .output(),
        )
        .await
        .map_err(|_| Error::Source("juicefs clone timed out".into()))??;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Source(format!("juicefs clone failed: {}", err.trim())));
        }
        return Ok(());
    }
    copy_tree(src, dst)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let m = std::fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)?;
    } else if m.is_dir() {
        std::fs::create_dir(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(m.mode() & 0o7777))?;
    } else if m.is_file() {
        std::fs::copy(src, dst)?;
    }
    if !m.file_type().is_symlink() {
        let uid = if m.uid() == 0 { OWNER_ID } else { m.uid() };
        let _ = std::os::unix::fs::lchown(dst, Some(uid), Some(m.gid()));
    }
    Ok(())
}

struct Tomb {
    conversation: Uuid,
    person: String,
    psid: Option<String>,
    paths: Vec<String>,
}

/// The permanent deletions purged after `since` (all without it; only `ids` when given), carried to the old side.
///
/// # Errors
/// The read of the tombstones; per-conversation errors are in `errors`.
pub async fn purged(
    imp: &Importer,
    since: Option<OffsetDateTime>,
    ids: &[Uuid],
    errors: &mut Vec<(String, String)>,
) -> Result<Vec<Purged>> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select p.conversation_id, p.person, p.provider_session_id, p.cc_paths from import_purged p
             where ($1::timestamptz is null or p.purged_at > $1)
               and (cardinality($2::uuid[]) = 0 or p.conversation_id = any($2))
               and not exists (select 1 from conversations c where c.id = p.conversation_id)
             order by p.purged_at, p.conversation_id",
            &[&since, &ids],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let t = Tomb {
            conversation: r.get(0),
            person: r.get(1),
            psid: r.get(2),
            paths: r.get(3),
        };
        imp.check_lock().await?;
        match purge_one(imp, &t).await {
            Ok(p) => out.push(p),
            Err(e) => {
                tracing::warn!(conversation = %t.conversation, error = %e, "permanent deletion not exported");
                errors.push((t.conversation.to_string(), e.to_string()));
            }
        }
    }
    Ok(out)
}

async fn purge_one(imp: &Importer, t: &Tomb) -> Result<Purged> {
    let id = t.conversation;
    let key = aviary_core::log::person_key(&t.person);
    let claude = crate::tree::claude_dir(&imp.jfs, &key);
    let mut out = Purged {
        conversation: id,
        person_key: key.clone(),
        ..Purged::default()
    };
    // the conversation's own cc files (never the person's shared memory folder), then its session folder
    let memory = format!("{}/", crate::tree::MEMORY);
    for p in &t.paths {
        if p == crate::tree::MEMORY || p.starts_with(&memory) {
            continue;
        }
        let f = under(&claude, p)?;
        if std::fs::symlink_metadata(&f).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(&f)?;
            out.removed += 1;
        }
    }
    let psid = t.psid.clone().unwrap_or_else(|| id.to_string());
    if !psid.is_empty() && !psid.contains('/') && psid != "." && psid != ".." {
        let main = under(&claude, &crate::tree::main_path(&psid))?;
        out.removed += usize::from(remove_tree(&main)?);
        let session = under(&claude, &format!("{}/{psid}", crate::tree::PROJECT))?;
        out.removed += usize::from(remove_tree(&session)?);
    }
    out.removed += usize::from(remove_tree(&claude.join("aviary-memory").join(id.to_string()))?);
    out.removed += usize::from(remove_tree(&imp.jfs.join("sent").join(id.to_string()))?);
    // the sessions row and its failed turns, then its share links
    let conn = imp.cloudcli.get().await?;
    let schema = format!("cc_{key}");
    let tables = conn
        .query_one(
            "select to_regclass(format('%I.sessions', $1::text)) is not null,
                    to_regclass(format('%I.jarvis_failed_turns', $1::text)) is not null,
                    to_regclass('cc_shared.shares') is not null",
            &[&schema],
        )
        .await?;
    let sid = id.to_string();
    if tables.get::<_, bool>(0) {
        let n = conn
            .execute(&format!("delete from {schema}.sessions where session_id = $1"), &[&sid])
            .await?;
        out.row = n > 0;
    }
    if tables.get::<_, bool>(1) {
        conn.execute(
            &format!("delete from {schema}.jarvis_failed_turns where session_id = $1"),
            &[&sid],
        )
        .await?;
    }
    if tables.get::<_, bool>(2) {
        let live: Vec<(String, String)> = conn
            .query(
                "select id from cc_shared.shares where session_id = $1 and revoked_at is null",
                &[&sid],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), key.clone()))
            .collect();
        out.shares_revoked = revoke_old(imp, &conn, &live).await?;
    }
    tracing::info!(conversation = %id, removed = out.removed, row = out.row, "permanent deletion exported");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_keeps_links_as_links_and_the_listing_skips_them() {
        let d = std::env::temp_dir().join(format!("wp-export-more-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("src");
        std::fs::create_dir_all(src.join("uploads/a")).unwrap();
        std::fs::write(src.join("uploads/a/x.png"), b"x").unwrap();
        std::fs::write(d.join("secret"), b"s").unwrap();
        std::os::unix::fs::symlink(d.join("secret"), src.join("uploads/link")).unwrap();
        let dst = d.join("dst");
        copy_tree(&src, &dst).unwrap();
        assert!(
            std::fs::symlink_metadata(dst.join("uploads/link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(dst.join("uploads/a/x.png")).unwrap(), b"x");
        let mut files = Vec::new();
        list_files(&dst, "uploads", &mut files).unwrap();
        assert_eq!(files, vec!["uploads/a/x.png".to_owned()]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
