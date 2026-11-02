//! List (M1) and details (M2), magpie 9.3.
//!
//! Active conversations by `activity_at desc, id desc`, Recently deleted by `deleted_at desc, id desc`, with a
//! keyset cursor. `list.headSeq` and the page are read in one snapshot (a repeatable-read, read-only transaction,
//! magpie 9.3): the page shows exactly the changes up to that seq, so pigeon's tail of the feed from it neither
//! misses nor replays one. The person's row is made on first sight before the snapshot opens.

use aviary_pg::Pool;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use magpie_api::conversations::{Conversation, ListPosition, ListQuery, ListResponse, ListState};
use magpie_api::errors::ErrorCode;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Person, ensure_person, load_many, load_owned, person_row};
use crate::changes;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// The opaque cursor: the sort key of the last conversation of the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    /// `activity_at` (active) or `deleted_at` (deleted), microseconds since the epoch.
    a: i64,
    /// The id.
    i: Uuid,
    /// The list it belongs to: `a` or `d`.
    s: String,
}

fn micros(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000).unwrap_or(i64::MAX)
}

fn from_micros(us: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(us) * 1_000).ok()
}

fn state_tag(state: ListState) -> &'static str {
    match state {
        ListState::Active => "a",
        ListState::Deleted => "d",
    }
}

fn encode(c: &Cursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(c).unwrap_or_default())
}

fn decode(s: &str, state: ListState) -> Result<(OffsetDateTime, Uuid)> {
    let bad = || Error::code(ErrorCode::BadCursor, "the cursor does not parse");
    let bytes = URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| bad())?;
    let c: Cursor = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if c.s != state_tag(state) {
        return Err(bad());
    }
    Ok((from_micros(c.a).ok_or_else(bad)?, c.i))
}

/// `%`, `_` and `\` taken literally in an `ilike` pattern.
fn like_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// M1.
///
/// # Errors
/// 400 `bad_cursor` for a cursor that does not parse, 400 `bad_request` for a limit outside 1-200; a statement
/// error.
pub async fn list(pool: &Pool, person: &Person, q: ListQuery) -> Result<ListResponse> {
    let state = q.state.unwrap_or_default();
    let limit = q.limit.unwrap_or(limits::LIST_MAX);
    if limit == 0 || limit > limits::LIST_MAX {
        return Err(Error::bad_request("limit must be 1-200"));
    }
    let after = q.cursor.as_deref().map(|c| decode(c, state)).transpose()?;
    let conn = pool.get().await?;
    ensure_person(&conn, &person.email).await?;
    drop(conn);
    let (after_t, after_id) = match after {
        Some((t, id)) => (Some(t), Some(id)),
        None => (None, None),
    };
    let search = q
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", like_literal(s)));
    let page = Page {
        person: &person.email,
        state,
        limit,
        after_t,
        after_id,
        search: search.as_deref(),
    };
    aviary_pg::tx!(pool, |t| -> Result<ListResponse> {
        crate::db::snapshot(t).await?;
        page.read(t).await
    })
    .await
}

/// One page of the list, read in the caller's snapshot.
struct Page<'a> {
    person: &'a str,
    state: ListState,
    limit: u32,
    after_t: Option<OffsetDateTime>,
    after_id: Option<Uuid>,
    search: Option<&'a str>,
}

impl Page<'_> {
    async fn read(&self, db: &impl Db) -> Result<ListResponse> {
        let (person, limit, after_t, after_id) = (self.person, self.limit, self.after_t, self.after_id);
        let p = person_row(db, person).await?;
        let fetch = i64::from(limit) + 1;
        let rows = match self.state {
            ListState::Active => {
                db.query(
                    "select id, activity_at as k from conversations
                     where person = $1 and deleted_at is null
                       and ($2::timestamptz is null or (activity_at, id) < ($2, $3))
                     order by activity_at desc, id desc limit $4",
                    &[&person, &after_t, &after_id, &fetch],
                )
                .await?
            }
            ListState::Deleted => {
                db.query(
                    "select id, deleted_at as k from conversations
                     where person = $1 and deleted_at is not null
                       and ($2::timestamptz is null or (deleted_at, id) < ($2, $3))
                       and ($5::text is null or title ilike $5)
                     order by deleted_at desc, id desc limit $4",
                    &[&person, &after_t, &after_id, &fetch, &self.search],
                )
                .await?
            }
        };
        let more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        let page: Vec<(Uuid, OffsetDateTime)> = rows
            .iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let next_cursor = if more {
            page.last().map(|(id, k)| {
                encode(&Cursor {
                    a: micros(*k),
                    i: *id,
                    s: state_tag(self.state).to_owned(),
                })
            })
        } else {
            None
        };
        let ids: Vec<Uuid> = page.iter().map(|(id, _)| *id).collect();
        let conversations = load_many(db, &ids).await?;
        Ok(ListResponse {
            conversations,
            next_cursor,
            list: ListPosition {
                generation: changes::generation(&p),
                head_seq: u64::try_from(p.change_seq).unwrap_or(0),
            },
        })
    }
}

/// M2: the person's conversation, deleted ones included.
///
/// # Errors
/// 404 `not_found`; a statement error.
pub async fn get(pool: &Pool, person: &Person, id: Uuid) -> Result<Conversation> {
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_cursor_round_trips_and_belongs_to_its_list() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_793_000_000_123_456_000).unwrap();
        let id = Uuid::new_v4();
        let c = encode(&Cursor {
            a: micros(t),
            i: id,
            s: "a".into(),
        });
        assert_eq!(decode(&c, ListState::Active).unwrap(), (t, id));
        assert!(decode(&c, ListState::Deleted).is_err());
        assert!(decode("%%%", ListState::Active).is_err());
        assert_eq!(like_literal("100%_\\x"), "100\\%\\_\\\\x");
    }
}
