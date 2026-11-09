//! Reading rows (magpie 7.4): timeline pages (M9) with the long-poll, prompts (M10) and search.
//!
//! A page is read in one snapshot (repeatable read): the epoch, `row_head`, the rows and the live state agree. Cost
//! is proportional to the page: rows come from the primary key (`tail`, `before`) or from `row_seqs` joined on it
//! (`after`); blobs stay references.

use std::time::Duration;

use aviary_pg::{Pool, Row as PgRow};
use magpie_api::Timestamp;
use magpie_api::rows::{
    Prompt, PromptsResponse, Row, RowItem, SearchLocation, SearchResponse, TIMELINE_DEFAULT, TIMELINE_MAX,
    TIMELINE_MAX_JSON_BYTES, TimelinePage, TimelineQuery,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{self, Db};
use crate::error::{Error, Result};
use crate::fanout::{self, Fanout};
use crate::live;
use crate::rows::derive;

/// A prompt's preview: this many characters (M10).
pub const PROMPT_PREVIEW_CHARS: usize = 200;
/// Search: locations per page by default, and at most.
pub const SEARCH_DEFAULT: u32 = 50;
/// See [`SEARCH_DEFAULT`].
pub const SEARCH_MAX: u32 = 500;

/// Which page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The last rows by `seqStart`.
    Tail,
    /// Rows with `seqStart < S`.
    Before(i64),
    /// One entry per row seq `> S`.
    After(i64),
}

/// A checked M9 query.
#[derive(Debug, Clone, Copy)]
pub struct PageQuery {
    /// The window.
    pub window: Window,
    /// 1-500.
    pub limit: i64,
    /// `after` only.
    pub wait: Duration,
    /// `after` only: `(liveTurn, liveAfter)`.
    pub live_after: Option<(Uuid, u64)>,
}

fn seq_param(v: u64, what: &str) -> Result<i64> {
    i64::try_from(v).map_err(|_| Error::bad_request(format!("{what} is too large")))
}

impl PageQuery {
    /// Checks M9's query: exactly one of `tail=1`, `before`, `after`; `limit` 1-500 (default 40); `waitMs` and
    /// `liveAfter` only with `after` (`waitMs` above 25,000 is held for 25,000); `liveAfter` and `liveTurn` together
    /// (frames are numbered per turn, so a frame number means nothing without its turn).
    ///
    /// # Errors
    /// 400 `bad_request`.
    pub fn check(q: &TimelineQuery) -> Result<Self> {
        let window = match (q.tail, q.before, q.after) {
            (true, None, None) => Window::Tail,
            (false, Some(b), None) => Window::Before(seq_param(b, "before")?),
            (false, None, Some(a)) => Window::After(seq_param(a, "after")?),
            _ => return Err(Error::bad_request("exactly one of tail=1, before, after")),
        };
        let limit = q.limit.unwrap_or(TIMELINE_DEFAULT);
        if limit == 0 || limit > TIMELINE_MAX {
            return Err(Error::bad_request("limit must be 1-500"));
        }
        let after = matches!(window, Window::After(_));
        if !after && (q.wait_ms.is_some() || q.live_after.is_some() || q.live_turn.is_some()) {
            return Err(Error::bad_request("waitMs, liveAfter and liveTurn go with after"));
        }
        let live_after = match (q.live_turn, q.live_after) {
            (Some(turn), Some(n)) => Some((turn, n)),
            (None, None) => None,
            _ => return Err(Error::bad_request("liveAfter and liveTurn go together")),
        };
        Ok(Self {
            window,
            limit: i64::from(limit),
            wait: Duration::from_millis(q.wait_ms.unwrap_or(0)),
            live_after,
        })
    }
}

/// The columns [`row_from`] reads, over `rows r`.
const ROW_COLS: &str = "r.row_id, r.seq_end, r.seqs, r.at, r.turn_id, r.item";

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// A Row from [`ROW_COLS`] (starting at column `at`), as the version `seq_end` (the current one when `None`).
fn row_from(r: &PgRow, at: usize, version: Option<i64>) -> Result<Row> {
    let row_id: i64 = r.get(at);
    let current: i64 = r.get(at + 1);
    let seq_end = version.unwrap_or(current);
    let seqs: Vec<i64> = r.get(at + 2);
    let ts: OffsetDateTime = r.get(at + 3);
    let item: RowItem = serde_json::from_value(r.get(at + 5))?;
    Ok(Row {
        seq_start: u(row_id),
        seq_end: u(seq_end),
        seqs: seqs.into_iter().filter(|s| *s <= seq_end).map(u).collect(),
        updated: seq_end != row_id,
        timestamp: Timestamp(ts),
        turn_id: r.get(at + 4),
        item,
    })
}

/// The conversation's timeline header, when `person` owns it (`None` person: any owner, for admin reads).
struct Head {
    epoch: i64,
    max_seq: i64,
    active_turn: Option<Uuid>,
    min_seq: Option<i64>,
}

async fn head(db: &impl Db, conversation: Uuid, person: Option<&str>) -> Result<Head> {
    let r = db
        .query_opt(
            "select c.epoch, c.row_head, c.active_turn, c.person,
                    (select min(r.row_id) from rows r where r.conversation_id = c.id)
             from conversations c where c.id = $1",
            &[&conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let owner: String = r.get(3);
    if person.is_some_and(|p| p != owner) {
        return Err(Error::not_found());
    }
    Ok(Head {
        epoch: r.get(0),
        max_seq: r.get(1),
        active_turn: r.get(2),
        min_seq: r.get(4),
    })
}

async fn exists(db: &impl Db, sql: &str, conversation: Uuid, seq: i64) -> Result<bool> {
    Ok(db.query_one(sql, &[&conversation, &seq]).await?.get(0))
}

const OLDER: &str = "select exists (select 1 from rows where conversation_id = $1 and row_id < $2)";
const NEWER: &str = "select exists (select 1 from rows where conversation_id = $1 and row_id > $2)";
const NEWER_SEQ: &str = "select exists (select 1 from row_seqs s join rows r
    on r.conversation_id = s.conversation_id and r.row_id = s.row_id where s.conversation_id = $1 and s.seq > $2)";

/// Adds rows while the JSON stays under 4 MiB (the first row always).
fn fits(rows: &[Row], total: &mut usize, next: &Row) -> bool {
    let n = serde_json::to_vec(next).map_or(0, |v| v.len());
    if !rows.is_empty() && *total + n > TIMELINE_MAX_JSON_BYTES {
        return false;
    }
    *total += n;
    true
}

/// One page in the caller's snapshot.
async fn page_in(db: &impl Db, conversation: Uuid, person: Option<&str>, q: &PageQuery) -> Result<TimelinePage> {
    let h = head(db, conversation, person).await?;
    let mut rows: Vec<Row> = Vec::new();
    let mut total = 0usize;
    let (has_older, has_newer) = match q.window {
        Window::Tail | Window::Before(_) => {
            let bound = match q.window {
                Window::Before(s) => s,
                _ => i64::MAX,
            };
            let found = db
                .query(
                    &format!(
                        "select {ROW_COLS} from rows r where r.conversation_id = $1 and r.row_id < $2
                         order by r.row_id desc limit $3"
                    ),
                    &[&conversation, &bound, &q.limit],
                )
                .await?;
            let mut cut = false;
            for r in &found {
                let row = row_from(r, 0, None)?;
                if !fits(&rows, &mut total, &row) {
                    cut = true;
                    break;
                }
                rows.push(row);
            }
            rows.reverse();
            let first = rows
                .first()
                .map_or(bound, |r| i64::try_from(r.seq_start).unwrap_or(i64::MAX));
            let older = cut || exists(db, OLDER, conversation, first).await?;
            let newer = match rows.last() {
                Some(last) => {
                    exists(
                        db,
                        NEWER,
                        conversation,
                        i64::try_from(last.seq_start).unwrap_or(i64::MAX),
                    )
                    .await?
                }
                None => bound != i64::MAX && exists(db, NEWER, conversation, bound - 1).await?,
            };
            (older, newer)
        }
        Window::After(s) => {
            let found = db
                .query(
                    &format!(
                        "select s.seq, {ROW_COLS} from row_seqs s
                         join rows r on r.conversation_id = s.conversation_id and r.row_id = s.row_id
                         where s.conversation_id = $1 and s.seq > $2 order by s.seq limit $3"
                    ),
                    &[&conversation, &s, &q.limit],
                )
                .await?;
            let mut cut = false;
            let mut last = s;
            for r in &found {
                let seq: i64 = r.get(0);
                let row = row_from(r, 1, Some(seq))?;
                if !fits(&rows, &mut total, &row) {
                    cut = true;
                    break;
                }
                rows.push(row);
                last = seq;
            }
            let newer = cut || exists(db, NEWER_SEQ, conversation, last).await?;
            let older = h.min_seq.is_some_and(|m| m <= s);
            (older, newer)
        }
    };
    let live = match h.active_turn {
        Some(turn) => live::state(db, conversation, turn, q.live_after).await?,
        None => None,
    };
    Ok(TimelinePage {
        epoch: h.epoch.to_string(),
        min_seq: h.min_seq.map_or(0, u),
        max_seq: u(h.max_seq),
        has_older,
        has_newer,
        rows,
        live,
    })
}

/// One page, read in one snapshot.
///
/// # Errors
/// 404 when the conversation does not exist or is not `person`'s; a statement error.
pub async fn page(pool: &Pool, conversation: Uuid, person: Option<&str>, q: &PageQuery) -> Result<TimelinePage> {
    aviary_pg::tx!(pool, |t| -> Result<TimelinePage> {
        db::snapshot(t).await?;
        page_in(t, conversation, person, q).await
    })
    .await
}

/// What M9 needs from the app.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    /// The pool.
    pub pool: &'a Pool,
    /// The replica's fan-out.
    pub fanout: &'a Fanout,
    /// The drain handle.
    pub draining: &'a aviary_core::drain::Draining,
}

/// M9: a page; an `after` page with `waitMs` holds until a row seq past `after` exists, the running turn's live
/// frames change, or the epoch changes, or `waitMs` passes (then `rows: []`). No connection is held while waiting.
///
/// # Errors
/// As [`page`]; 400 for a query [`PageQuery::check`] refuses.
pub async fn timeline(
    r: Reader<'_>,
    conversation: Uuid,
    person: Option<&str>,
    q: &TimelineQuery,
) -> Result<TimelinePage> {
    let q = PageQuery::check(q)?;
    if !matches!(q.window, Window::After(_)) || q.wait.is_zero() {
        return page(r.pool, conversation, person, &q).await;
    }
    let key = conversation.to_string();
    // the epoch of the first look: a retract that only deleted rows issues no row seq, but its new epoch answers
    let epoch: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let epoch = &epoch;
    r.fanout
        .hold_until(
            &[(fanout::ROWS, &key), (fanout::LIVE, &key)],
            q.wait,
            r.draining,
            || async move {
                let p = page(r.pool, conversation, person, &q).await?;
                let seen = {
                    let mut e = epoch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    e.get_or_insert_with(|| p.epoch.clone()).clone()
                };
                let done = ready(&p, &q, &seen);
                Ok((done, p))
            },
        )
        .await
}

/// Whether a held `after` poll answers: something past `after`, live frames the reader has not seen, a new epoch.
fn ready(p: &TimelinePage, q: &PageQuery, epoch: &str) -> bool {
    !p.rows.is_empty() || p.epoch != epoch || p.live.as_ref().is_some_and(|l| Some((l.turn_id, l.n)) != q.live_after)
}

/// M10: every prompt row.
///
/// # Errors
/// 404; a statement error.
pub async fn prompts(pool: &Pool, conversation: Uuid, person: Option<&str>) -> Result<PromptsResponse> {
    aviary_pg::tx!(pool, |t| -> Result<PromptsResponse> {
        db::snapshot(t).await?;
        let h = head(t, conversation, person).await?;
        let rows = t
            .query(
                "select row_id, at, item from rows where conversation_id = $1 and is_prompt order by row_id",
                &[&conversation],
            )
            .await?;
        let mut prompts = Vec::with_capacity(rows.len());
        for r in rows {
            let item: RowItem = serde_json::from_value(r.get(2))?;
            prompts.push(Prompt {
                seq: u(r.get(0)),
                timestamp: Timestamp(r.get(1)),
                preview: derive::chars(derive::row_text(&item).unwrap_or_default(), PROMPT_PREVIEW_CHARS).to_owned(),
            });
        }
        Ok(PromptsResponse {
            epoch: h.epoch.to_string(),
            prompts,
        })
    })
    .await
}

/// `q` as a `like` pattern matching it anywhere (`\` escapes).
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Non-overlapping case-insensitive occurrences of `needle` in `hay`.
#[must_use]
pub fn occurrences(hay: &str, needle: &str) -> u32 {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    u32::try_from(hay.to_lowercase().matches(needle.as_str()).count()).unwrap_or(u32::MAX)
}

/// Search: a case-insensitive substring over the events' text, mapped to the rows they made. The cursor is the
/// event seq after which the next page starts.
///
/// # Errors
/// 400 `bad_request` for an empty `q` or a limit outside 1-500, 400 `bad_cursor`; 404; a statement error.
pub async fn search(
    pool: &Pool,
    conversation: Uuid,
    person: Option<&str>,
    q: &str,
    cursor: Option<&str>,
    limit: Option<u32>,
) -> Result<SearchResponse> {
    if q.trim().is_empty() {
        return Err(Error::bad_request("q is empty"));
    }
    let limit = limit.unwrap_or(SEARCH_DEFAULT);
    if limit == 0 || limit > SEARCH_MAX {
        return Err(Error::bad_request("limit must be 1-500"));
    }
    let after: i64 = match cursor {
        None => 0,
        Some(c) => c
            .parse()
            .map_err(|_| Error::code(magpie_api::errors::ErrorCode::BadCursor, "cursor does not parse"))?,
    };
    let pattern = like_pattern(q);
    let q = q.to_owned();
    let pattern = &pattern; // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let q = &q;
    aviary_pg::tx!(pool, |t| -> Result<SearchResponse> {
        db::snapshot(t).await?;
        let h = head(t, conversation, person).await?;
        let mut locations = Vec::new();
        let mut from = after;
        let batch = i64::from(limit);
        let next_cursor = loop {
            let events = t
                .query(
                    "select seq from events where conversation_id = $1 and seq > $2 and text ilike $3 escape '\\'
                     order by seq limit $4",
                    &[&conversation, &from, &pattern, &batch],
                )
                .await?;
            let Some(last) = events.last().map(|r| r.get::<_, i64>(0)) else {
                break None;
            };
            let seqs: Vec<i64> = events.iter().map(|r| r.get(0)).collect();
            let rows = t
                .query(
                    "select source_seqs[1], row_id, kind, item from rows
                     where conversation_id = $1 and source_seqs[1] = any($2) and kind in ('user', 'assistant')
                     order by source_seqs[1], row_id",
                    &[&conversation, &seqs],
                )
                .await?;
            let mut stopped_at = None;
            let mut current = None;
            for r in rows {
                let ev: i64 = r.get(0);
                if current != Some(ev) {
                    if locations.len() >= limit as usize {
                        stopped_at = current;
                        break;
                    }
                    current = Some(ev);
                }
                let item: RowItem = serde_json::from_value(r.get(3))?;
                let count = occurrences(derive::row_text(&item).unwrap_or_default(), &q);
                if count > 0 {
                    locations.push(SearchLocation {
                        seq: u(r.get(1)),
                        kind: r.get(2),
                        count,
                    });
                }
            }
            if let Some(s) = stopped_at {
                break Some(s.to_string());
            }
            if events.len() < limit as usize {
                break None;
            }
            if locations.len() >= limit as usize {
                break Some(last.to_string());
            }
            from = last;
        };
        Ok(SearchResponse {
            epoch: h.epoch.to_string(),
            locations,
            next_cursor,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_escapes_its_wildcards() {
        assert_eq!(like_pattern("50%_a\\b"), "%50\\%\\_a\\\\b%");
    }

    #[test]
    fn occurrences_ignore_case() {
        assert_eq!(occurrences("Bridge bridge BRIDGE", "bridge"), 3);
        assert_eq!(occurrences("港珠澳大橋", "大橋"), 1);
        assert_eq!(occurrences("abc", ""), 0);
    }
}
