//! Retraction and limbo (magpie 6.7, 6.2).
//!
//! Every classified line a truncate removes from the main transcript waits in `cc_limbo` (reason `truncated`), with
//! its event when it has one, the turn it was credited to, and the turn of the truncate (`op_turn`): a later write
//! whose new line has the same sha256 takes the entry back (the event, the turn, and the fact that the line was
//! classified), so Claude Code's truncate-then-rewrite patterns retract nothing and never classify a line twice (an
//! internal line such as the echo of a person's prompt stays internal; 6.5). Limbo is flushed only when the file has
//! had no operation for 10 s (the sweeper, [`sweep`]) or at the end of the turn whose operations put it there
//! ([`flush_scope`], called by the end report), never in the middle of a rewrite: the entries with events become one
//! `retract` event, the others are dropped. Lines an overwrite or a rename-over drops are retracted at once
//! ([`retract`]).
//!
//! A main transcript renamed away puts its classified lines in limbo with reason `moved` (no file): a file created at
//! the main path within 10 min takes them back by sha256; they are never retracted (the set-aside file still has
//! them) and expire (retention).

use std::collections::{HashMap, VecDeque};

use aviary_pg::{Pool, Tx};
use magpie_api::events::{EventBody, Retract};
use uuid::Uuid;

use super::extract::Sha;
use crate::error::Result;
use crate::events::{self, NewEvent};
use crate::limits;

/// A classified line in limbo or in a file: its sha256, its event (none for an internal line) and its turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// SHA-256 of the line.
    pub sha: Sha,
    /// Its event.
    pub event_seq: Option<i64>,
    /// The turn it was credited to.
    pub turn_id: Option<Uuid>,
}

/// The `moved` limbo of `scope` from the classified lines of `file` (the main transcript renamed away), in one
/// statement.
///
/// # Errors
/// When the statement fails.
pub async fn put_moved(tx: &Tx<'_>, scope: Uuid, file: i64) -> Result<()> {
    tx.execute(
        "insert into cc_limbo (scope, file_id, reason, sha256, event_seq, turn_id)
         select $1, null, 'moved', sha256, event_seq, turn_id from cc_lines where file_id = $2 and classified
          order by ord
         on conflict (scope, reason, event_seq) where event_seq is not null
           do update set sha256 = excluded.sha256, turn_id = excluded.turn_id, removed_at = now()",
        &[&scope, &file],
    )
    .await?;
    Ok(())
}

/// What limbo `file` (and, at the main path, the scope's `moved` limbo) holds, so the write path looks only when
/// there is something to match.
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

/// Limbo entries a batch of new lines may take back, fetched in one statement; the ones taken are deleted in one
/// statement by [`Candidates::commit`].
#[derive(Debug, Default)]
pub struct Candidates {
    by_sha: HashMap<Sha, VecDeque<(i64, Entry)>>,
    /// Ids taken and not yet deleted.
    pub used: Vec<i64>,
}

impl Candidates {
    /// The `truncated` limbo of `file` with one of `shas`, earliest first.
    ///
    /// # Errors
    /// When the statement fails.
    pub async fn truncated(tx: &Tx<'_>, scope: Uuid, file: i64, shas: &[Sha]) -> Result<Self> {
        let shas: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
        let rows = tx
            .query(
                "select id, sha256, event_seq, turn_id from cc_limbo
                  where scope = $1 and reason = 'truncated' and file_id = $2 and sha256 = any($3) order by id",
                &[&scope, &file, &shas],
            )
            .await?;
        Ok(Self::of(&rows))
    }

    /// The `moved` limbo of `scope` with one of `shas` still inside the match window, earliest first.
    ///
    /// # Errors
    /// When the statement fails.
    pub async fn moved(tx: &Tx<'_>, scope: Uuid, shas: &[Sha]) -> Result<Self> {
        let shas: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
        let rows = tx
            .query(
                "select id, sha256, event_seq, turn_id from cc_limbo
                  where scope = $1 and reason = 'moved' and sha256 = any($2)
                    and removed_at > now() - make_interval(secs => $3)
                  order by id",
                &[&scope, &shas, &limits::MOVED_LIMBO.as_secs_f64()],
            )
            .await?;
        Ok(Self::of(&rows))
    }

    fn of(rows: &[aviary_pg::Row]) -> Self {
        let mut by_sha: HashMap<Sha, VecDeque<(i64, Entry)>> = HashMap::new();
        for r in rows {
            let sha: Vec<u8> = r.get(1);
            let Ok(sha) = Sha::try_from(sha.as_slice()) else {
                continue;
            };
            by_sha.entry(sha).or_default().push_back((
                r.get(0),
                Entry {
                    sha,
                    event_seq: r.get(2),
                    turn_id: r.get(3),
                },
            ));
        }
        Self {
            by_sha,
            used: Vec::new(),
        }
    }

    /// Takes the earliest entry with `sha`.
    pub fn take(&mut self, sha: &Sha) -> Option<Entry> {
        let (id, e) = self.by_sha.get_mut(sha)?.pop_front()?;
        self.used.push(id);
        Some(e)
    }

    /// Deletes the entries taken.
    ///
    /// # Errors
    /// When the statement fails.
    pub async fn commit(&mut self, tx: &Tx<'_>) -> Result<()> {
        if !self.used.is_empty() {
            tx.execute("delete from cc_limbo where id = any($1)", &[&self.used])
                .await?;
            self.used.clear();
        }
        Ok(())
    }
}

/// Drops the `moved` limbo of lines that are back on the main path in `file` (a rename back onto it): entries with
/// the events its lines have, and entries without one whose sha256 one of its classified lines without an event has.
///
/// # Errors
/// When the statement fails.
pub async fn drop_moved(tx: &Tx<'_>, scope: Uuid, file: i64) -> Result<()> {
    tx.execute(
        "delete from cc_limbo l where l.scope = $1 and l.reason = 'moved'
           and (l.event_seq in (select event_seq from cc_lines where file_id = $2 and event_seq is not null)
                or (l.event_seq is null and l.sha256 in
                     (select sha256 from cc_lines where file_id = $2 and classified and event_seq is null)))",
        &[&scope, &file],
    )
    .await?;
    Ok(())
}

/// Takes every `truncated` limbo entry of `file` out (a rename-over replacing it matches them itself), earliest first.
///
/// # Errors
/// When the statement fails.
pub async fn take_all_truncated(tx: &Tx<'_>, scope: Uuid, file: i64) -> Result<Vec<Entry>> {
    let rows = tx
        .query(
            "with d as (delete from cc_limbo where scope = $1 and reason = 'truncated' and file_id = $2
                        returning id, sha256, event_seq, turn_id)
             select sha256, event_seq, turn_id from d order by id",
            &[&scope, &file],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let sha: Vec<u8> = r.get(0);
            Some(Entry {
                sha: Sha::try_from(sha.as_slice()).ok()?,
                event_seq: r.get(1),
                turn_id: r.get(2),
            })
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

/// Flushes `file`'s `truncated` limbo: its events into one `retract` (the conversation is locked), the entries
/// without one dropped. How many lines were retracted.
///
/// # Errors
/// When a statement fails.
pub async fn flush_file(tx: &Tx<'_>, scope: Uuid, file: i64, turn: Option<Uuid>) -> Result<usize> {
    let seqs: Vec<i64> = take_all_truncated(tx, scope, file)
        .await?
        .into_iter()
        .filter_map(|e| e.event_seq)
        .collect();
    let n = seqs.len();
    retract(tx, scope, turn, seqs).await?;
    Ok(n)
}

/// Flushes the `truncated` limbo that `turn`'s operations put there (`None`: operations credited to no turn) into
/// one `retract` (the conversation is locked): the end of that turn, for the turn package's end report. Limbo of
/// other turns' operations (a rewrite still running after this turn's end) waits for its own end or the sweeper.
/// How many lines were retracted.
///
/// # Errors
/// When a statement fails.
pub async fn flush_scope(tx: &Tx<'_>, conversation: Uuid, turn: Option<Uuid>) -> Result<usize> {
    let rows = tx
        .query(
            "delete from cc_limbo where scope = $1 and reason = 'truncated' and op_turn is not distinct from $2
             returning event_seq",
            &[&conversation, &turn],
        )
        .await?;
    let seqs: Vec<i64> = rows.iter().filter_map(|r| r.get::<_, Option<i64>>(0)).collect();
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
