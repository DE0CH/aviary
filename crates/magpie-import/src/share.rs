//! Shares (cutover 7.4.4, magpie 19.1): each `cc_shared.shares` row whose fingerprint (`import_shares.src_fp`: the
//! row and the listing of its folder) changed becomes, or updates, the magpie share with the same id.
//!
//! - A live share of an imported conversation is frozen from magpie's copy of that conversation: `upto_seq` covers
//!   the events of the main-transcript lines the share's `transcript.jsonl` holds (compared line by line by sha256
//!   from offset 0), `share_rows` are the conversation's rows within it, and the snapshot scope is the conversation's
//!   cc scope with the main transcript cut where the frozen transcript ends. When the frozen transcript is a byte
//!   prefix of the live one (every live share today) the snapshot's transcript is the frozen file byte for byte.
//!   When it is not (a line differs), the share keeps the common lines (`prefix: false` in the report and the
//!   share's log line).
//! - A revoked share becomes revoked (its frozen data removed, its workspace's deletion queued); one revoked
//!   before it was ever imported is stored revoked, with no data.
//! - A share of a conversation that is not in magpie (its sessions row is gone) is stored revoked (`deleted`), as
//!   the old stack answers 404 for it.
//!
//! The frozen workspace is `ws/<uuidv5(NS, "share:" + id)>`, the clone hermit's share pass makes; magpie answers 404
//! for its file downloads until the clone exists. Every write locks the conversation and requires `migrating`
//! (cutover 6.2 items 1-2): a share of a released conversation is left as magpie has it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use magpie::ccfs::extract;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::source::ShareRow;
use crate::{Error, Importer, Result};

/// What one share pass did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Copied {
    /// Shares frozen (new and live).
    pub frozen: usize,
    /// Of those, the ones whose frozen transcript was not a byte prefix of the live one.
    pub not_prefix: Vec<String>,
    /// Shares stored or made revoked.
    pub revoked: usize,
    /// Shares whose row changed in place (title, owner name).
    pub updated: usize,
    /// Per share errors (the share is retried next pass).
    pub errors: Vec<(String, String)>,
}

/// A share's folder on the mount: `users/<owner's key>/shares/<id>`.
#[must_use]
pub fn folder(jfs: &Path, s: &ShareRow) -> PathBuf {
    let owner = aviary_core::log::normalise_email(&s.owner);
    jfs.join("users")
        .join(aviary_core::log::person_key(&owner))
        .join("shares")
        .join(&s.id)
}

fn listing(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    let here = if rel.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    };
    let entries = match std::fs::read_dir(&here) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for e in entries {
        let e = e?;
        if let Some(n) = e.file_name().to_str() {
            names.push((n.to_owned(), e.metadata()?));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, m) in names {
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        // the workspace copy is hermit's (its clone); the importer reads only the transcript and the sent files
        if child == "ws" {
            continue;
        }
        if m.is_dir() {
            listing(dir, &child, out)?;
        } else {
            use std::os::unix::fs::MetadataExt as _;
            out.push(format!(
                "{child}\t{}\t{}",
                m.len(),
                m.mtime_nsec() + m.mtime() * 1_000_000_000
            ));
        }
    }
    Ok(())
}

/// The fingerprint of a share (cutover 7.3 `src_fp`): its row and its folder's listing (the workspace copy left
/// out).
///
/// # Errors
/// An I/O error other than "not found".
pub fn fingerprint(jfs: &Path, s: &ShareRow) -> Result<String> {
    let mut files = Vec::new();
    listing(&folder(jfs, s), "", &mut files)?;
    let v = serde_json::json!({"row": s, "files": files});
    Ok(crate::sha_hex(v.to_string().as_bytes()))
}

fn time_of(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|e| Error::Source(format!("share time {s}: {e}")))
}

/// Where a share's frozen transcript ends in magpie's copy of its conversation's main transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Bytes of the main transcript the share keeps (ends at a line boundary).
    pub bytes: i64,
    /// The last event seq the share includes.
    pub upto: i64,
    /// The frozen transcript is exactly those bytes.
    pub prefix: bool,
}

/// Matches the frozen transcript `frozen` against the main transcript's lines (`(start_off, len, sha256,
/// event_seq)`, in order) and the conversation's `head_seq`. `None` when magpie's copy is shorter than the frozen
/// transcript and equal as far as it goes (the conversation's sync is behind; the share waits for the next pass).
#[must_use]
pub fn cut(frozen: &[u8], lines: &[(i64, i64, Vec<u8>, Option<i64>)], head: i64) -> Option<Cut> {
    let mut bytes = 0i64;
    let mut next = None;
    let mut matched_all = true;
    for (i, (start, len, sha, _)) in lines.iter().enumerate() {
        let (s, e) = (usize::try_from(*start).ok()?, usize::try_from(start + len).ok()?);
        if *start != bytes || e > frozen.len() || extract::sha(&frozen[s..e]).as_slice() != sha.as_slice() {
            next = Some(i);
            matched_all = false;
            break;
        }
        bytes = start + len;
    }
    let total = i64::try_from(frozen.len()).ok()?;
    if matched_all && bytes < total {
        return None;
    }
    // the share includes every event before the first line it leaves out that has one
    let upto = next
        .and_then(|i| lines[i..].iter().find_map(|l| l.3))
        .map_or(head, |s| s - 1);
    Some(Cut {
        bytes,
        upto,
        prefix: bytes == total,
    })
}

async fn known(imp: &Importer) -> Result<std::collections::HashMap<String, String>> {
    Ok(imp
        .magpie
        .pool
        .get()
        .await?
        .query("select share_id, src_fp from import_shares", &[])
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect())
}

/// One share pass over `shares`; `skip`: conversations whose sync failed in this pass (their shares wait).
///
/// # Errors
/// The read of `import_shares`; per-share errors are in the result.
pub async fn pass(imp: &Importer, shares: &[ShareRow], skip: &BTreeSet<Uuid>) -> Result<Copied> {
    let known = known(imp).await?;
    let mut out = Copied::default();
    for s in shares {
        let fp = match fingerprint(&imp.jfs, s) {
            Ok(f) => f,
            Err(e) => {
                out.errors.push((s.id.clone(), e.to_string()));
                continue;
            }
        };
        if known.get(&s.id) == Some(&fp) {
            continue;
        }
        let conversation = crate::conv::id_of(&s.session_id);
        if conversation.is_some_and(|c| skip.contains(&c)) {
            continue;
        }
        match one(imp, s, conversation, &fp).await {
            Ok(Done::Frozen(c)) => {
                out.frozen += 1;
                if !c.prefix {
                    out.not_prefix.push(s.id.clone());
                }
            }
            Ok(Done::Revoked) => out.revoked += 1,
            Ok(Done::Updated) => out.updated += 1,
            Ok(Done::Unchanged) => {}
            Err(e) => {
                tracing::warn!(error = %e, "share not imported");
                out.errors.push((s.id.clone(), e.to_string()));
            }
        }
    }
    Ok(out)
}

enum Done {
    Frozen(Cut),
    Revoked,
    Updated,
    Unchanged,
}

async fn record(t: &aviary_pg::Tx<'_>, id: &str, fp: &str) -> Result<()> {
    t.execute(
        "insert into import_shares (share_id, src_fp, imported_at) values ($1, $2, now())
         on conflict (share_id) do update set src_fp = excluded.src_fp, imported_at = now()",
        &[&id, &fp],
    )
    .await?;
    Ok(())
}

/// The main transcript's lines of `conversation` in magpie: `(start_off, len, sha256, event_seq)` in order.
async fn main_lines(db: &impl magpie::db::Db, conversation: Uuid) -> Result<Vec<(i64, i64, Vec<u8>, Option<i64>)>> {
    Ok(db
        .query(
            "select l.start_off, l.len::int8, l.sha256, l.event_seq from cc_lines l
             join cc_files f on f.id = l.file_id
             join conversations c on c.id = f.scope and f.path = c.main_transcript
             where c.id = $1 order by l.ord",
            &[&conversation],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect())
}

#[allow(clippy::too_many_lines)] // one transaction per outcome, in the order 7.4.4 lists them
async fn one(imp: &Importer, s: &ShareRow, conversation: Option<Uuid>, fp: &str) -> Result<Done> {
    let created = time_of(&s.created_at)?;
    let revoked = s.revoked_at.as_deref().map(time_of).transpose()?;
    let owner = aviary_core::log::normalise_email(&s.owner);
    let workspace = magpie::shares::workspace_of(&s.id);
    // the frozen transcript, read before any transaction (a live share only)
    let frozen = if revoked.is_none() {
        match std::fs::read(folder(&imp.jfs, s).join("transcript.jsonl")) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<Done> {
        // the conversation first (a purge locks it before its shares), and only while it is migrating
        let conv = match conversation {
            Some(c) => t
                .query_opt(
                    "select migrating, head_seq from conversations where id = $1 for update",
                    &[&c],
                )
                .await?
                .map(|r| (c, r.get::<_, bool>(0), r.get::<_, i64>(1))),
            None => None,
        };
        if conv.is_some_and(|(_, migrating, _)| !migrating) {
            return Ok(Done::Unchanged);
        }
        let existing = t
            .query_opt(
                "select snapshot, revoked_at is not null from shares where id = $1 for update",
                &[&s.id],
            )
            .await?
            .map(|r| (r.get::<_, Uuid>(0), r.get::<_, bool>(1)));
        if let Some((snapshot, was_revoked)) = existing {
            let done = match (revoked, was_revoked) {
                (Some(at), false) => {
                    t.execute(
                        "update shares set revoked_at = $2, revoke_reason = 'owner' where id = $1",
                        &[&s.id, &at],
                    )
                    .await?;
                    magpie::shares::drop_frozen(t, &s.id, snapshot, workspace).await?;
                    Done::Revoked
                }
                (None, false) => {
                    t.execute(
                        "update shares set title = $2, owner_name = $3 where id = $1 and
                                (title is distinct from $2 or owner_name is distinct from $3)",
                        &[&s.id, &s.title, &s.owner_name],
                    )
                    .await?;
                    Done::Updated
                }
                _ => Done::Unchanged,
            };
            record(t, &s.id, fp).await?;
            return Ok(done);
        }
        let snapshot = Uuid::new_v4();
        let live = match (revoked, conv, frozen.as_deref()) {
            (None, Some((c, _, head)), Some(f)) => Some((c, head, f)),
            _ => None,
        };
        let Some((c, head, frozen)) = live else {
            // revoked before it was imported, or of a conversation that is gone: stored revoked, with no data
            let reason = if revoked.is_some() { "owner" } else { "deleted" };
            t.execute(
                "insert into shares (id, conversation_id, owner, owner_name, title, upto_seq, snapshot, workspace_id,
                                     created_at, revoked_at, revoke_reason)
                 values ($1, $2, $3, $4, $5, 0, $6, $7, $8, coalesce($9, now()), $10)",
                &[
                    &s.id,
                    &conv.map(|x| x.0),
                    &owner,
                    &s.owner_name,
                    &s.title,
                    &snapshot,
                    &workspace,
                    &created,
                    &revoked,
                    &reason,
                ],
            )
            .await?;
            record(t, &s.id, fp).await?;
            return Ok(Done::Revoked);
        };
        let lines = main_lines(t, c).await?;
        let Some(k) = cut(frozen, &lines, head) else {
            return Err(Error::Source(
                "the conversation's copy is behind the share's frozen transcript (retried next pass)".into(),
            ));
        };
        t.execute(
            "insert into shares (id, conversation_id, owner, owner_name, title, upto_seq, snapshot,
                                 provider_session_id, workspace_id, model, created_at)
             select $1, c.id, $3, $4, $5, $6, $7, c.provider_session_id, $8, c.model, $9
             from conversations c where c.id = $2",
            &[
                &s.id,
                &c,
                &owner,
                &s.owner_name,
                &s.title,
                &k.upto,
                &snapshot,
                &workspace,
                &created,
            ],
        )
        .await?;
        let frozen_rows = format!(
            "insert into share_rows (share_id, row_id, seq_end, kind, at, turn_id, item, seqs, source_seqs,
                                     tool_use_id, message_id, block_index, is_prompt)
             select $1, row_id, seq_end, kind, at, turn_id, item, seqs, source_seqs, tool_use_id, message_id,
                    block_index, is_prompt
             from rows where conversation_id = $2 and {}",
            magpie::shares::within(3)
        );
        t.execute(&frozen_rows, &[&s.id, &c, &k.upto]).await?;
        magpie::shares::copy_scope(t, c, snapshot, None).await?;
        // the snapshot's main transcript ends where the frozen one does
        t.execute(
            "with m as (select f.id from cc_files f join conversations c on c.id = $1 and f.path = c.main_transcript
                        where f.scope = $2)
             delete from cc_lines l using m where l.file_id = m.id and l.start_off >= $3",
            &[&c, &snapshot, &k.bytes],
        )
        .await?;
        t.execute(
            "update cc_files f set size = $3, tail = ''
             from conversations c where c.id = $1 and f.scope = $2 and f.path = c.main_transcript and f.size <> $3",
            &[&c, &snapshot, &k.bytes],
        )
        .await?;
        magpie::shares::copy_refs(t, c, snapshot, "share").await?;
        record(t, &s.id, fp).await?;
        tracing::info!(conversation = %c, prefix = k.prefix, upto = k.upto, "share imported");
        Ok(Done::Frozen(k))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &[u8], seqs: &[Option<i64>]) -> Vec<(i64, i64, Vec<u8>, Option<i64>)> {
        let mut out = Vec::new();
        let mut at = 0usize;
        for (i, l) in text.split_inclusive(|b| *b == b'\n').enumerate() {
            out.push((
                i64::try_from(at).unwrap(),
                i64::try_from(l.len()).unwrap(),
                extract::sha(l).to_vec(),
                seqs[i],
            ));
            at += l.len();
        }
        out
    }

    #[test]
    fn a_prefix_a_changed_line_and_a_copy_that_is_behind() {
        let live = b"a\nb\nc\nd\n";
        let lines = lines_of(live, &[Some(2), None, Some(4), Some(6)]);
        // a prefix: everything before the next line's event
        assert_eq!(
            cut(b"a\nb\n", &lines, 9),
            Some(Cut {
                bytes: 4,
                upto: 3,
                prefix: true
            })
        );
        // the whole transcript: up to the head
        assert_eq!(
            cut(live, &lines, 9),
            Some(Cut {
                bytes: 8,
                upto: 9,
                prefix: true
            })
        );
        // a line differs: the common lines
        assert_eq!(
            cut(b"a\nX\nc\n", &lines, 9),
            Some(Cut {
                bytes: 2,
                upto: 3,
                prefix: false
            })
        );
        // magpie has less than the share: wait
        assert_eq!(cut(b"a\nb\nc\nd\ne\n", &lines, 9), None);
    }
}
