//! The per-person change feed (M12, magpie 7.6).
//!
//! [`emit`] inserts a change in the caller's transaction, after the conversation row lock (one lock order: the
//! conversation, then the person): `persons.change_seq + 1` is the change's seq, so the feed is dense per person.
//! A `conversation_upserted` change stores no conversation: the reader puts in the conversation's state when the
//! change is read (pigeon 14.1), so a burst of upserts never sends stale states.
//!
//! [`read`] answers `after=latest` at once, 410 `cursor_expired` for a position older than retention keeps (checked
//! in the page's own snapshot, on every look), and otherwise long-polls on the person's watch.
//!
//! The cursor: a reader asks for the next page `after` the seq of the last change it got (`headSeq` when a page
//! came back empty). `headSeq` and the page come from one snapshot, so `headSeq` is at least every returned seq, and
//! a page with fewer than `limit` changes ends at `headSeq`; with exactly `limit`, more may follow. A held poll
//! that times out answers `headSeq = after`: its last look found nothing past `after`, which is the head then.

use std::collections::HashMap;
use std::time::Duration;

use aviary_core::drain::Draining;
use aviary_pg::{Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::changes::{After, Change, ChangeBody, ChangesResponse, RemovedReason};
use magpie_api::errors::ErrorCode;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations::{self, PersonRow};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::fanout::{self, Fanout};
use crate::limits;

/// Appends `body` to `person`'s change feed about `conversation`, in `tx`; its seq. The caller has locked the
/// conversation row first (or the conversation is gone). A `conversation_upserted` body's conversation is not
/// stored ([`upserted`] is the shorter way to write one).
///
/// # Errors
/// When a statement fails, or the person row does not exist.
pub async fn emit(tx: &Tx<'_>, person: &str, conversation: Uuid, body: &ChangeBody) -> Result<i64> {
    let ty = body
        .type_name()
        .ok_or_else(|| Error::Internal("change of an unknown type".into()))?;
    let data = match body {
        ChangeBody::ConversationUpserted { .. } => Value::Object(Map::new()),
        other => {
            let mut v = serde_json::to_value(other)?;
            if let Some(o) = v.as_object_mut() {
                o.remove("type");
            }
            v
        }
    };
    insert(tx, person, conversation, ty, &data).await
}

/// A `conversation_upserted` change: the conversation's state is read when the change is read.
///
/// # Errors
/// As [`emit`].
pub async fn upserted(tx: &Tx<'_>, person: &str, conversation: Uuid) -> Result<i64> {
    insert(
        tx,
        person,
        conversation,
        "conversation_upserted",
        &Value::Object(Map::new()),
    )
    .await
}

async fn insert(tx: &Tx<'_>, person: &str, conversation: Uuid, ty: &str, data: &Value) -> Result<i64> {
    let row = tx
        .query_opt(
            "update persons set change_seq = change_seq + 1 where person = $1 returning change_seq, person_key",
            &[&person],
        )
        .await?
        .ok_or_else(|| Error::Internal("change for a person without a row".into()))?;
    let seq: i64 = row.get(0);
    let key: String = row.get(1);
    tx.execute(
        "insert into changes (person, seq, type, conversation_id, data) values ($1, $2, $3, $4, $5)",
        &[&person, &seq, &ty, &conversation, data],
    )
    .await?;
    fanout::notify(tx, fanout::CHANGES, &key, seq).await?;
    Ok(seq)
}

/// The generation string of a person's feed (M1 `list.generation` and M12 `generation`, the same value).
#[must_use]
pub fn generation(p: &PersonRow) -> String {
    p.generation.to_string()
}

/// What M12 needs from the app.
#[derive(Debug, Clone, Copy)]
pub struct Read<'a> {
    /// The pool.
    pub pool: &'a Pool,
    /// The replica's fan-out.
    pub fanout: &'a Fanout,
    /// The drain handle (held polls answer at once while draining).
    pub draining: &'a Draining,
}

/// M12: the person's changes after `after`.
///
/// # Errors
/// 400 `bad_request` for a limit outside 1-500; 410 `cursor_expired` for a position older than retention keeps;
/// a statement error.
pub async fn read(
    app: Read<'_>,
    person: &str,
    after: After,
    limit: Option<u32>,
    wait_ms: Option<u64>,
) -> Result<ChangesResponse> {
    let limit = limit.unwrap_or(limits::CHANGES_MAX);
    if limit == 0 || limit > limits::CHANGES_MAX {
        return Err(Error::bad_request("limit must be 1-500"));
    }
    let conn = app.pool.get().await?;
    let p = conversations::ensure_person(&conn, person).await?;
    drop(conn);
    let generation = generation(&p);
    let after = match after {
        After::Latest => {
            return Ok(ChangesResponse {
                generation,
                head_seq: u64::try_from(p.change_seq).unwrap_or(0),
                changes: Vec::new(),
            });
        }
        After::Seq(n) => i64::try_from(n).map_err(|_| Error::bad_request("after is too large"))?,
    };
    if after > p.change_seq {
        // a position this feed never had (another deployment's, or a reset): start over from M1
        return Err(Error::code(ErrorCode::CursorExpired, "cursor is past the feed's head"));
    }
    let wait = Duration::from_millis(wait_ms.unwrap_or(0));
    let key = p.person_key.clone();
    let generation_empty = generation.clone();
    app.fanout
        .hold(
            fanout::CHANGES,
            &key,
            wait,
            app.draining,
            || {
                let generation = generation.clone();
                async move {
                    let (head, changes) = page(app.pool, person, after, i64::from(limit)).await?;
                    if changes.is_empty() && head <= after {
                        return Ok(None);
                    }
                    Ok(Some(ChangesResponse {
                        generation,
                        head_seq: u64::try_from(head).unwrap_or(0),
                        changes,
                    }))
                }
            },
            move || ChangesResponse {
                generation: generation_empty,
                head_seq: u64::try_from(after.max(0)).unwrap_or(0),
                changes: Vec::new(),
            },
        )
        .await
}

/// The head and up to `limit` changes after `after`, upserts filled with the conversations' current state (a
/// purged conversation's upsert reads as its removal). All in one snapshot, so the head is never older than a change
/// of the page, and a page shorter than `limit` ends at the head. The retention check is in the same snapshot and
/// runs on every look of a held poll: 410 `cursor_expired` when changes after `after` were already deleted (the
/// oldest kept change is more than one past it), so a retention pass between looks can never make a page skip
/// changes.
async fn page(pool: &Pool, person: &str, after: i64, limit: i64) -> Result<(i64, Vec<Change>)> {
    aviary_pg::tx!(pool, |t| -> Result<(i64, Vec<Change>)> {
        crate::db::snapshot(t).await?;
        page_in(t, person, after, limit).await
    })
    .await
}

async fn page_in(conn: &impl Db, person: &str, after: i64, limit: i64) -> Result<(i64, Vec<Change>)> {
    let head: i64 = conn
        .query_one("select change_seq from persons where person = $1", &[&person])
        .await?
        .get(0);
    if after < head {
        let oldest: Option<i64> = conn
            .query_one("select min(seq) from changes where person = $1", &[&person])
            .await?
            .get(0);
        // with nothing kept, the next change would be head + 1
        if after < oldest.unwrap_or(head + 1) - 1 {
            return Err(Error::code(
                ErrorCode::CursorExpired,
                "changes after the cursor are no longer kept",
            ));
        }
    }
    let rows = conn
        .query(
            "select seq, at, type, conversation_id, data from changes
             where person = $1 and seq > $2 order by seq limit $3",
            &[&person, &after, &limit],
        )
        .await?;
    let upserted: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = rows
            .iter()
            .filter(|r| r.get::<_, String>(2) == "conversation_upserted")
            .map(|r| r.get(3))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let mut convs: HashMap<Uuid, _> = conversations::load_many(conn, &upserted)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let seq: i64 = r.get(0);
        let at: OffsetDateTime = r.get(1);
        let ty: String = r.get(2);
        let conversation_id: Uuid = r.get(3);
        let body = if ty == "conversation_upserted" {
            match convs.get(&conversation_id) {
                Some(c) => ChangeBody::ConversationUpserted {
                    conversation: Box::new(c.clone()),
                },
                None => ChangeBody::ConversationRemoved {
                    reason: RemovedReason::Purged,
                },
            }
        } else {
            let mut data: Value = r.get(4);
            if let Some(o) = data.as_object_mut() {
                o.insert("type".into(), Value::String(ty));
            }
            serde_json::from_value(data).unwrap_or(ChangeBody::Unknown)
        };
        if matches!(body, ChangeBody::Unknown) {
            continue;
        }
        out.push(Change {
            seq: u64::try_from(seq).unwrap_or(0),
            at: Timestamp(at),
            conversation_id,
            body,
        });
    }
    convs.clear();
    Ok((head, out))
}
