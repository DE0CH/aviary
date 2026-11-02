//! Retraction and limbo (magpie 6.7, 6.2).
//!
//! Lines with events that a truncate removes from the main transcript wait in `cc_limbo` (reason `truncated`): a
//! later write whose new line has the same sha256 takes the event back, so Claude Code's truncate-then-rewrite
//! patterns retract nothing. Limbo is flushed into one `retract` event only when the file has had no operation for
//! 10 s (the sweeper, [`sweep`]) or at the end of the turn ([`flush_scope`], called by the end report), never in the
//! middle of a rewrite. Lines an overwrite or a rename-over drops are retracted at once ([`retract`]).
//!
//! A main transcript renamed away puts its lines with events in limbo with reason `moved` (no file): a file created
//! at the main path within 10 min takes them back by sha256; they are never retracted (the set-aside file still has
//! them) and expire (retention).

use aviary_pg::{Pool, Tx};
use magpie_api::events::{EventBody, Retract};
use uuid::Uuid;

use super::extract::Sha;
use crate::error::Result;
use crate::events::{self, NewEvent};
use crate::limits;

/// `(sha256, event seq)` of a line with an event.
pub type Entry = (Sha, i64);

/// Puts lines a truncate removed from `file` (the main transcript) in limbo.
///
/// # Errors
/// When a statement fails.
pub async fn put_truncated(tx: &Tx<'_>, scope: Uuid, file: i64, lines: &[Entry]) -> Result<()> {
    for (sha, seq) in lines {
        tx.execute(
            "insert into cc_limbo (scope, file_id, reason, sha256, event_seq) values ($1, $2, 'truncated', $3, $4)
             on conflict do nothing",
            &[&scope, &file, &sha.as_slice(), seq],
        )
        .await?;
    }
    Ok(())
}

/// Puts the lines of a main transcript renamed away in limbo (`moved`).
///
/// # Errors
/// When a statement fails.
pub async fn put_moved(tx: &Tx<'_>, scope: Uuid, lines: &[Entry]) -> Result<()> {
    for (sha, seq) in lines {
        tx.execute(
            "insert into cc_limbo (scope, file_id, reason, sha256, event_seq) values ($1, null, 'moved', $2, $3)
             on conflict (scope, reason, event_seq) do update set sha256 = excluded.sha256, removed_at = now()",
            &[&scope, &sha.as_slice(), seq],
        )
        .await?;
    }
    Ok(())
}

/// What limbo `file` (and, at the main path, the scope's `moved` limbo) holds, so the write path asks per line only
/// when there is something to match.
///
/// # Errors
/// When the statement fails.
pub async fn present(tx: &Tx<'_>, scope: Uuid, file: i64, main: bool) -> Result<(bool, bool)> {
    let row = tx
        .query_one(
            "select exists(select 1 from cc_limbo where scope = $1 and reason = 'truncated' and file_id = $2),
                    $3 and exists(select 1 from cc_limbo where scope = $1 and reason = 'moved'
                                   and removed_at > now() - make_interval(secs => $4))",
            &[&scope, &file, &main, &limits::MOVED_LIMBO.as_secs_f64()],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

/// Takes back the earliest `truncated` limbo line of `file` with `sha`: its event seq.
///
/// # Errors
/// When the statement fails.
pub async fn take_truncated(tx: &Tx<'_>, scope: Uuid, file: i64, sha: &Sha) -> Result<Option<i64>> {
    let row = tx
        .query_opt(
            "delete from cc_limbo where scope = $1 and reason = 'truncated' and event_seq =
               (select min(event_seq) from cc_limbo
                 where scope = $1 and reason = 'truncated' and file_id = $2 and sha256 = $3)
             returning event_seq",
            &[&scope, &file, &sha.as_slice()],
        )
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Takes back the earliest `moved` limbo line of the scope with `sha` still inside the match window.
///
/// # Errors
/// When the statement fails.
pub async fn take_moved(tx: &Tx<'_>, scope: Uuid, sha: &Sha) -> Result<Option<i64>> {
    let row = tx
        .query_opt(
            "delete from cc_limbo where scope = $1 and reason = 'moved' and event_seq =
               (select min(event_seq) from cc_limbo
                 where scope = $1 and reason = 'moved' and sha256 = $2
                   and removed_at > now() - make_interval(secs => $3))
             returning event_seq",
            &[&scope, &sha.as_slice(), &limits::MOVED_LIMBO.as_secs_f64()],
        )
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Drops `moved` limbo entries for events that are back on the main path (a rename back onto it).
///
/// # Errors
/// When the statement fails.
pub async fn drop_moved(tx: &Tx<'_>, scope: Uuid, seqs: &[i64]) -> Result<()> {
    if !seqs.is_empty() {
        tx.execute(
            "delete from cc_limbo where scope = $1 and reason = 'moved' and event_seq = any($2)",
            &[&scope, &seqs],
        )
        .await?;
    }
    Ok(())
}

/// Takes every `truncated` limbo entry of `file` out (a rename-over replacing it matches them itself).
///
/// # Errors
/// When the statement fails.
pub async fn take_all_truncated(tx: &Tx<'_>, scope: Uuid, file: i64) -> Result<Vec<Entry>> {
    let rows = tx
        .query(
            "delete from cc_limbo where scope = $1 and reason = 'truncated' and file_id = $2
             returning sha256, event_seq",
            &[&scope, &file],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let sha: Vec<u8> = r.get(0);
            Some((Sha::try_from(sha.as_slice()).ok()?, r.get(1)))
        })
        .collect())
}

/// Appends one `retract` event for `seqs` (the conversation is locked) and records the retracted lines' uuids so a
/// later line with the same uuid carries `meta.replaces` (6.7). Events are never deleted.
///
/// # Errors
/// When a statement fails.
pub async fn retract(tx: &Tx<'_>, conversation: Uuid, turn: Option<Uuid>, mut seqs: Vec<i64>) -> Result<()> {
    seqs.sort_unstable();
    seqs.dedup();
    if seqs.is_empty() {
        return Ok(());
    }
    tx.execute(
        "insert into cc_retracted (conversation_id, uuid, seq)
         select conversation_id, data->>'uuid', seq from events
          where conversation_id = $1 and seq = any($2) and data->>'uuid' is not null
         on conflict do nothing",
        &[&conversation, &seqs],
    )
    .await?;
    let body = EventBody::Retract(Retract {
        seqs: seqs.iter().filter_map(|s| u64::try_from(*s).ok()).collect(),
    });
    events::append(tx, conversation, NewEvent::of(&body)?.turn(turn)).await?;
    tracing::info!(conversation = %conversation, lines = seqs.len(), "transcript lines retracted");
    Ok(())
}

/// The most recent retracted event with `uuid` (for `meta.replaces`).
///
/// # Errors
/// When the statement fails.
pub async fn replaced(tx: &Tx<'_>, conversation: Uuid, uuid: &str) -> Result<Option<i64>> {
    let row = tx
        .query_one(
            "select max(seq) from cc_retracted where conversation_id = $1 and uuid = $2",
            &[&conversation, &uuid],
        )
        .await?;
    Ok(row.get(0))
}

/// Flushes `file`'s `truncated` limbo into one `retract` (the conversation is locked).
///
/// # Errors
/// When a statement fails.
pub async fn flush_file(tx: &Tx<'_>, scope: Uuid, file: i64, turn: Option<Uuid>) -> Result<usize> {
    let entries = take_all_truncated(tx, scope, file).await?;
    let n = entries.len();
    retract(tx, scope, turn, entries.into_iter().map(|e| e.1).collect()).await?;
    Ok(n)
}

/// Flushes every `truncated` limbo line of the conversation into one `retract` (the end of the turn the truncates
/// belonged to; the conversation is locked). For the turn package's end report.
///
/// # Errors
/// When a statement fails.
pub async fn flush_scope(tx: &Tx<'_>, conversation: Uuid, turn: Option<Uuid>) -> Result<usize> {
    let rows = tx
        .query(
            "delete from cc_limbo where scope = $1 and reason = 'truncated' returning event_seq",
            &[&conversation],
        )
        .await?;
    let seqs: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
    let n = seqs.len();
    retract(tx, conversation, turn, seqs).await?;
    Ok(n)
}

/// One sweeper pass (magpie 15): flushes the limbo of every file that has had no operation for 10 s. Each file in
/// its own transaction under its conversation's lock, re-checked there. How many lines were retracted.
///
/// # Errors
/// When a statement fails.
pub async fn sweep(pool: &Pool) -> Result<usize> {
    let quiet = limits::LIMBO_QUIET.as_secs_f64();
    let due = pool
        .get()
        .await?
        .query(
            "select distinct l.scope, l.file_id from cc_limbo l join cc_files f on f.id = l.file_id
              where l.reason = 'truncated' and f.last_op_at < now() - make_interval(secs => $1)
              limit 200",
            &[&quiet],
        )
        .await?;
    let mut total = 0;
    for r in due {
        let scope: Uuid = r.get(0);
        let file: i64 = r.get(1);
        total += aviary_pg::tx!(pool, |t| -> Result<usize> {
            let Some(c) = t
                .query_opt(
                    "select active_turn from conversations where id = $1 for update",
                    &[&scope],
                )
                .await?
            else {
                return Ok(0);
            };
            let active: Option<Uuid> = c.get(0);
            let still = t
                .query_opt(
                    "select 1 from cc_files where id = $1 and last_op_at < now() - make_interval(secs => $2)",
                    &[&file, &quiet],
                )
                .await?
                .is_some();
            if !still {
                return Ok(0);
            }
            flush_file(t, scope, file, active).await
        })
        .await?;
    }
    // limbo of files that no longer exist (removed with their file): retract what is left
    let orphans = pool
        .get()
        .await?
        .query(
            "select distinct l.scope, l.file_id from cc_limbo l
              where l.reason = 'truncated' and not exists (select 1 from cc_files f where f.id = l.file_id)
              limit 200",
            &[],
        )
        .await?;
    for r in orphans {
        let scope: Uuid = r.get(0);
        let file: i64 = r.get(1);
        total += aviary_pg::tx!(pool, |t| -> Result<usize> {
            let Some(c) = t
                .query_opt(
                    "select active_turn from conversations where id = $1 for update",
                    &[&scope],
                )
                .await?
            else {
                t.execute("delete from cc_limbo where scope = $1", &[&scope]).await?;
                return Ok(0);
            };
            flush_file(t, scope, file, c.get(0)).await
        })
        .await?;
    }
    Ok(total)
}
