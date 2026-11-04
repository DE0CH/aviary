//! Sent files (cutover 7.4.3, magpie 19.1): each `sent/<conversation>/<toolUseId>/` folder on `JuiceFS` (the old
//! hook's snapshot of what a `SendUserFile` call sent, paths relative to `/workspace`) becomes blobs, `sent_files`
//! rows and one `files.sent` event, in the same transaction shape as the supervisor's commit (magpie 11.2), with the
//! caption from the call's input. The event is appended at the next seq; its `toolUseSeq` ties it to the call's tool
//! row (magpie 7.3 rule 6), which is where the timeline shows it. The fingerprint is the sorted (path, size, mtime)
//! list of the conversation's folder.
//!
//! A folder is imported once, when nothing in it (its files and every folder in it) changed for [`QUIET`]; a folder
//! with no files is not imported (no event without files). A folder whose call has an error result in the
//! transcript (the hook blocked the send, but its snapshot was left) is never imported. A file that appears in a
//! folder after it was imported is the conversation's error, and its `sent_fp` is recorded so verification never
//! passes on it ([`LATE`]).

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use magpie::blobs;
use magpie::events::{self, NewEvent};
use magpie_api::events::{EventBody, FilesSent};
use magpie_api::uploads::{BlobKind, BlobPointer, BlobRef, SentFile};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Error, Importer, Result};

/// One snapshot folder: its files (path relative to `/workspace` → absolute source) and its time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The `SendUserFile` call.
    pub tool_use_id: String,
    /// `rel path` → source file.
    pub files: BTreeMap<String, PathBuf>,
    /// The folder's modification time (when the hook wrote it).
    pub at: OffsetDateTime,
    /// The newest modification time of the folder, every folder in it and its files.
    pub latest: std::time::SystemTime,
}

/// How long a snapshot folder must be unchanged before it is imported: the old hook writes a send's files one after
/// another, and a folder is imported once (a pass every 5 s would otherwise catch it half written).
pub const QUIET: std::time::Duration = std::time::Duration::from_secs(15);

/// What a recorded `sent_fp` starts with while a folder waits for its quiet period: never equal to a read's
/// fingerprint, so the next pass looks again and verification does not pass meanwhile.
pub const WAITING: &str = "waiting:";

/// What a recorded `sent_fp` starts with when an imported folder has files it did not have at its import: never
/// equal to a read's fingerprint, so verification reports it until an operator decides.
pub const LATE: &str = "late:";

impl Snapshot {
    /// Whether nothing in the folder changed for `quiet`.
    #[must_use]
    pub fn settled(&self, quiet: std::time::Duration) -> bool {
        self.latest.elapsed().is_ok_and(|age| age >= quiet) || quiet.is_zero()
    }
}

fn tool_use_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The conversation's snapshot folders and the fingerprint of all of them.
///
/// # Errors
/// An I/O error other than "not found".
pub fn read(jfs: &Path, conversation: Uuid) -> Result<(Vec<Snapshot>, String)> {
    let base = jfs.join("sent").join(conversation.to_string());
    let mut out = Vec::new();
    let mut fp: Vec<(String, u64, i64)> = Vec::new();
    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((out, crate::sha_hex(b"[]"))),
        Err(e) => return Err(e.into()),
    };
    let mut dirs: Vec<(String, std::fs::Metadata)> = Vec::new();
    for e in entries {
        let e = e?;
        let m = e.metadata()?;
        if let (true, Some(name)) = (m.is_dir(), e.file_name().to_str().map(str::to_owned))
            && tool_use_id_ok(&name)
        {
            dirs.push((name, m));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, m) in dirs {
        let mut files = BTreeMap::new();
        let mut latest = m.modified()?;
        walk(&base.join(&id), "", &mut files, &mut latest)?;
        for (rel, abs) in &files {
            let fm = std::fs::metadata(abs)?;
            latest = latest.max(fm.modified()?);
            fp.push((
                format!("{id}/{rel}"),
                fm.len(),
                fm.mtime().saturating_mul(1_000_000_000) + fm.mtime_nsec(),
            ));
        }
        // the snapshot folder's own time, to the nanosecond (an export `--since` compares it)
        let at = OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        )
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
        out.push(Snapshot {
            tool_use_id: id,
            files,
            at,
            latest,
        });
    }
    Ok((out, crate::sha_hex(serde_json::to_string(&fp)?.as_bytes())))
}

fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>, latest: &mut std::time::SystemTime) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = crate::tree::utf8_name(&e, rel)?;
        if crate::tree::is_export_temp(&name) {
            continue;
        }
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let m = e.metadata()?;
        if m.is_dir() {
            // a file written into a subfolder changes that folder's time, not the snapshot folder's
            *latest = (*latest).max(m.modified()?);
            walk(&e.path(), &child, out, latest)?;
        } else if m.is_file() {
            out.insert(child, e.path());
        }
    }
    Ok(())
}

/// The `SendUserFile` calls of `id` already imported, with the paths each sent.
///
/// # Errors
/// A statement error.
pub async fn imported(imp: &Importer, id: Uuid) -> Result<BTreeMap<String, std::collections::BTreeSet<String>>> {
    let mut out: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for r in imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select tool_use_id, path from sent_files where conversation_id = $1",
            &[&id],
        )
        .await?
    {
        out.entry(r.get(0)).or_default().insert(r.get(1));
    }
    Ok(out)
}

/// Whether the transcript (as imported) has an error result for call `tool_use_id`: the send did not happen.
///
/// # Errors
/// A statement error.
pub async fn failed_call(imp: &Importer, id: Uuid, tool_use_id: &str) -> Result<bool> {
    Ok(imp
        .magpie
        .pool
        .get()
        .await?
        .query_one(
            "select exists (select 1 from tool_links l join events e
                              on e.conversation_id = l.conversation_id and e.seq = l.result_seq
                             where l.conversation_id = $1 and l.tool_use_id = $2
                               and jsonb_path_exists(e.data,
                                     '$.message.content[*] ? (@.type == \"tool_result\" && @.tool_use_id == $id
                                                              && @.is_error == true)',
                                     jsonb_build_object('id', $2::text)))",
            &[&id, &tool_use_id],
        )
        .await?
        .get(0))
}

/// Imports one snapshot into migrating conversation `id` of `person` (nothing when it is there already).
///
/// # Errors
/// [`Error::Released`] when the conversation is not migrating; a file over the sent-file cap; a bucket or statement
/// error.
#[allow(clippy::too_many_lines)] // the bucket puts, then one transaction in magpie 11.2's order
pub async fn import(imp: &Importer, person: &str, id: Uuid, s: &Snapshot) -> Result<()> {
    let app = &imp.magpie;
    let mut put: Vec<(String, blobs::RawBlob)> = Vec::new();
    for (rel, abs) in &s.files {
        let bytes = std::fs::read(abs)?;
        if bytes.len() as u64 > magpie_api::sandbox::SENT_FILE_MAX_BYTES {
            return Err(Error::Source(format!(
                "sent file {rel} of {} is over the cap",
                s.tool_use_id
            )));
        }
        let sha = blobs::sha256(&bytes);
        let facts = magpie::files::raw_facts(sha, &bytes, Some(magpie_api::mime::from_name(rel)));
        let had = app
            .pool
            .get()
            .await?
            .execute(
                "update blobs set last_ref_at = now() where sha256 = $1 and stored",
                &[&sha.as_slice()],
            )
            .await?;
        if had == 0 {
            let (_, lock) = app.blobs.put_bytes(&app.pool, axum::body::Bytes::from(bytes)).await?;
            let (held, f) = (&lock, &facts);
            let r = aviary_pg::tx!(&app.pool, |t| -> magpie::error::Result<()> {
                blobs::record_stored(t, f).await?;
                held.check(t).await
            })
            .await;
            lock.release().await;
            r?;
        }
        put.push((rel.clone(), facts));
    }
    let tool_use_id = s.tool_use_id.clone();
    let at = s.at;
    aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        imp.check_lock_in(t).await?;
        let Some(r) = t
            .query_opt(
                "select migrating, person from conversations where id = $1 for update",
                &[&id],
            )
            .await?
        else {
            return Err(Error::Released);
        };
        if r.get::<_, String>(1) != person {
            return Err(crate::conv::another_persons(id));
        }
        if !r.get::<_, bool>(0) {
            return Err(Error::Released);
        }
        if t.query_opt(
            "select 1 from sent_files where conversation_id = $1 and tool_use_id = $2 limit 1",
            &[&id, &tool_use_id],
        )
        .await?
        .is_some()
        {
            return Ok(());
        }
        let mut sent = Vec::with_capacity(put.len());
        for (rel, facts) in &put {
            let mime = facts
                .mime
                .clone()
                .unwrap_or_else(|| magpie_api::mime::from_name(rel).to_owned());
            let size = u64::try_from(facts.size).unwrap_or(0);
            sent.push(SentFile {
                path: rel.clone(),
                name: magpie::files::file_name(rel).to_owned(),
                size,
                mime_type: mime.clone(),
                is_image: magpie::files::shown_as_image(&mime),
                blob: BlobPointer {
                    blob: BlobRef {
                        blob_id: blobs::to_hex(&facts.sha),
                        bytes: size,
                        kind: if facts.kind == "media" {
                            BlobKind::Media
                        } else {
                            BlobKind::File
                        },
                        mime: Some(mime),
                        width: facts.width,
                        height: facts.height,
                        preview: None,
                        lines: None,
                    },
                },
            });
        }
        let call = t
            .query_opt(
                "select seq, jsonb_path_query_first(data,
                          '$.message.content[*] ? (@.type == \"tool_use\" && @.id == $id).input.caption',
                          jsonb_build_object('id', $2::text)) #>> '{}'
                 from events
                 where conversation_id = $1 and kind = 'cc.assistant'
                   and jsonb_path_exists(data, '$.message.content[*] ? (@.type == \"tool_use\" && @.id == $id)',
                                         jsonb_build_object('id', $2::text))
                 order by seq desc limit 1",
                &[&id, &tool_use_id],
            )
            .await?;
        let tool_use_seq = call.as_ref().and_then(|r| u64::try_from(r.get::<_, i64>(0)).ok());
        let caption = call
            .and_then(|r| r.get::<_, Option<String>>(1))
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        let body = EventBody::FilesSent(FilesSent {
            tool_use_id: tool_use_id.clone(),
            tool_use_seq,
            caption: caption.clone(),
            files: sent.clone(),
        });
        let mut ev = NewEvent::of(&body)?;
        ev.at = at;
        let seq = events::append(t, id, ev).await?;
        for (f, (_, facts)) in sent.iter().zip(&put) {
            t.execute(
                "insert into sent_files (conversation_id, tool_use_id, path, name, size, mime, is_image, blob,
                                         caption, sent_at, seq)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &id,
                    &tool_use_id,
                    &f.path,
                    &f.name,
                    &facts.size,
                    &f.mime_type,
                    &f.is_image,
                    &facts.sha.as_slice(),
                    &caption,
                    &at,
                    &seq,
                ],
            )
            .await?;
            blobs::reference(t, id, "conversation", &facts.sha).await?;
        }
        magpie::changes::upserted(t, person, id).await?;
        Ok(())
    })
    .await
}
