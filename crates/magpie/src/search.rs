//! The conversation search (Deyao, 2026-11-17): a person's messages found across their conversations, by their words and by
//! their meaning.
//!
//! A message is a user or assistant row's first event (its `text`, at most 64 KiB). The meaning half needs a vector
//! of each piece of that text ([`pieces`]): the conversation's own sandbox makes them after its turns (the todo and
//! put routes below), and magpie keeps them in `embeddings` (pgvector, an HNSW index on cosine distance). A query
//! compares only vectors of the model it was embedded with, so a deployment that changes its embedding model simply
//! fills the new model's vectors in as its conversations run again.
//!
//! The two halves are merged by reciprocal rank fusion: a message found by both comes first, then the best of either.
//! A message found only by its meaning is kept when it is about as near as the nearest one (within
//! [`MEANING_SPREAD`] of it) and not far from the query at all ([`MEANING_FAR`]): the meaning half always has a
//! nearest message, and a search with nothing near should not show the person unrelated ones.

use std::collections::HashMap;

use aviary_pg::Pool;
use magpie_api::Timestamp;
use magpie_api::search::{
    CHUNK_CHARS, CHUNKS_MAX, ConversationSearchHit, ConversationSearchRequest, ConversationSearchResponse, DIMENSIONS,
    EmbeddingPiece, EmbeddingsPut, EmbeddingsTodo, EmbeddingsTodoQuery, SEARCH_MAX, SNIPPET_CHARS, TODO_MAX,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::turns::caller::{self, Sandbox};

/// Hits a search answers when the request names no limit.
pub const SEARCH_DEFAULT: u32 = 20;
/// Messages each half brings to the merge.
const HALF: i64 = 60;
/// A conversation shows at most this many of its messages.
const PER_CONVERSATION: usize = 3;
/// Reciprocal rank fusion's constant.
const RRF_K: f64 = 60.0;
/// A message found only by its meaning is kept when its distance is at most the nearest one's plus this.
pub const MEANING_SPREAD: f64 = 0.12;
/// … and below this cosine distance.
pub const MEANING_FAR: f64 = 0.75;
/// The words of a query that each must appear (the rest are ignored).
const WORDS_MAX: usize = 6;
/// A query longer than this many characters is refused.
const QUERY_MAX_CHARS: usize = 500;

/// Reciprocal rank fusion's share of the hit at `rank` (from 0).
fn rrf(rank: usize) -> f64 {
    1.0 / (RRF_K + f64::from(u32::try_from(rank).unwrap_or(u32::MAX)) + 1.0)
}

/// The one place a message is: a user or assistant row starts with this event.
const IS_MESSAGE: &str = "exists (select 1 from rows r where r.conversation_id = e.conversation_id
    and r.source_seqs[1] = e.seq and r.kind in ('user', 'assistant'))";

/// The pieces of `text` as byte ranges: at most [`CHUNK_CHARS`] characters each, cut at the last paragraph break,
/// else line break, else sentence end, else space inside the window (a hard cut only where there is none), leading
/// whitespace skipped, whitespace-only pieces dropped, at most [`CHUNKS_MAX`] of them.
#[must_use]
pub fn pieces(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while out.len() < CHUNKS_MAX {
        // skip whitespace
        let Some(skip) = text[start..].find(|c: char| !c.is_whitespace()) else {
            break;
        };
        start += skip;
        let rest = &text[start..];
        let window_end = rest.char_indices().nth(CHUNK_CHARS).map_or(rest.len(), |(i, _)| i);
        let end = if window_end == rest.len() {
            window_end
        } else {
            cut_point(&rest[..window_end]).unwrap_or(window_end)
        };
        let piece = &rest[..end];
        let trimmed = piece.trim_end();
        if !trimmed.is_empty() {
            out.push((start, start + trimmed.len()));
        }
        start += end;
        if start >= text.len() {
            break;
        }
    }
    out
}

/// Where to end a piece inside `window` (its byte length so far): after the last break of the best kind found in its
/// second half, so no piece is tiny.
fn cut_point(window: &str) -> Option<usize> {
    let half = window.len() / 2;
    let after = |i: usize, len: usize| (i >= half).then_some(i + len);
    if let Some(i) = window.rfind("\n\n").and_then(|i| after(i, 2)) {
        return Some(i);
    }
    if let Some(i) = window.rfind('\n').and_then(|i| after(i, 1)) {
        return Some(i);
    }
    let sentence = window
        .char_indices()
        .rev()
        .find(|&(i, c)| {
            i >= half
                && (matches!(c, '。' | '！' | '？' | '；')
                    || (matches!(c, '.' | '!' | '?' | ';')
                        && window[i + c.len_utf8()..].starts_with(char::is_whitespace)))
        })
        .map(|(i, c)| i + c.len_utf8());
    if sentence.is_some() {
        return sentence;
    }
    window
        .char_indices()
        .rev()
        .find(|&(i, c)| i >= half && c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
}

fn i64_of(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| Error::bad_request("a seq is out of range"))
}

/// The vector as pgvector's text form (`[0.5,-0.25,…]`), checked: [`DIMENSIONS`] finite values.
fn vector_text(v: &[f32]) -> Result<String> {
    if v.len() != DIMENSIONS {
        return Err(Error::bad_request(format!("a vector must have {DIMENSIONS} values")));
    }
    if !v.iter().all(|x| x.is_finite()) {
        return Err(Error::bad_request("a vector's values must be finite numbers"));
    }
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    Ok(s)
}

fn model_of(model: &str) -> Result<&str> {
    let m = model.trim();
    if m.is_empty() || m.len() > 200 {
        return Err(Error::bad_request("model must be 1-200 bytes"));
    }
    Ok(m)
}

/// `GET /v1/sandbox/embeddings/todo`: the caller's conversation's message pieces with no vector of the model,
/// oldest message first, whole messages only.
///
/// # Errors
/// 400 for a bad model or limit; 404, 409 `lease_lost`; a statement error.
pub async fn todo(pool: &Pool, caller: &Sandbox, q: EmbeddingsTodoQuery) -> Result<EmbeddingsTodo> {
    let model = model_of(&q.model)?.to_owned();
    let limit = q.limit.unwrap_or(TODO_MAX);
    if limit == 0 || limit > TODO_MAX {
        return Err(Error::bad_request(format!("limit must be 1-{TODO_MAX}")));
    }
    let conn = pool.get().await?;
    let row = conn
        .query_opt(
            "select person, lease_id, lease_sandbox from conversations where id = $1",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    caller::check(&row, caller)?;
    let rows = conn
        .query(
            &format!(
                "select e.seq, e.text from events e
                 where e.conversation_id = $1 and e.text ~ '\\S' and {IS_MESSAGE}
                   and not exists (select 1 from embeddings m
                                   where m.conversation_id = e.conversation_id and m.seq = e.seq and m.model = $2)
                 order by e.seq limit $3"
            ),
            &[&caller.conversation, &model, &i64::from(limit)],
        )
        .await?;
    let mut items = Vec::new();
    for r in rows {
        if items.len() >= limit as usize {
            break;
        }
        let seq: i64 = r.get(0);
        let text: String = r.get(1);
        for (chunk, (a, b)) in pieces(&text).into_iter().enumerate() {
            items.push(EmbeddingPiece {
                seq: u64::try_from(seq).unwrap_or(0),
                chunk: u32::try_from(chunk).unwrap_or(0),
                text: text[a..b].to_owned(),
            });
        }
    }
    Ok(EmbeddingsTodo { items })
}

/// `POST /v1/sandbox/embeddings`: stores the vectors (a piece stored before keeps its vector).
///
/// # Errors
/// 400 for a bad model, vector, or a piece that is not one of the conversation's messages; 404, 409 `lease_lost`;
/// a statement error.
pub async fn put(pool: &Pool, caller: &Sandbox, req: EmbeddingsPut) -> Result<()> {
    let model = model_of(&req.model)?.to_owned();
    if req.items.len() > 4 * TODO_MAX as usize {
        return Err(Error::bad_request("too many vectors in one call"));
    }
    let mut items = Vec::with_capacity(req.items.len());
    for it in &req.items {
        items.push((i64_of(it.seq)?, it.chunk, vector_text(&it.vector)?));
    }
    let seqs: Vec<i64> = items.iter().map(|(s, _, _)| *s).collect();
    let (items, seqs, model) = (&items, &seqs, &model); // borrowed: the transaction may run more than once
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let row = t
            .query_opt(
                "select person, lease_id, lease_sandbox from conversations where id = $1",
                &[&caller.conversation],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        caller::check(&row, caller)?;
        let person: String = row.get(0);
        let texts: HashMap<i64, String> = t
            .query(
                &format!(
                    "select e.seq, e.text from events e
                     where e.conversation_id = $1 and e.seq = any($2) and e.text is not null and {IS_MESSAGE}"
                ),
                &[&caller.conversation, seqs],
            )
            .await?
            .into_iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        for (seq, chunk, vec) in items {
            let Some(text) = texts.get(seq) else {
                return Err(Error::bad_request(format!(
                    "seq {seq} is not a message of this conversation"
                )));
            };
            let Some(&(a, b)) = pieces(text).get(*chunk as usize) else {
                return Err(Error::bad_request(format!("message {seq} has no piece {chunk}")));
            };
            t.execute(
                "insert into embeddings (conversation_id, seq, chunk, model, person, piece_start, piece_end, vec)
                 values ($1, $2, $3, $4, $5, $6, $7, ($8::text)::vector)
                 on conflict do nothing",
                &[
                    &caller.conversation,
                    seq,
                    &i32::try_from(*chunk).unwrap_or(i32::MAX),
                    model,
                    &person,
                    &i32::try_from(a).unwrap_or(i32::MAX),
                    &i32::try_from(b).unwrap_or(i32::MAX),
                    vec,
                ],
            )
            .await?;
        }
        Ok(())
    })
    .await
}

/// `like` pattern for one word, escaped.
fn like(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('%');
    for c in word.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// What a hit shows of its message: the conversation's title and epoch, the row's `seqEnd` and kind, when it was
/// written, its text.
type Detail = (Option<String>, i64, i64, String, OffsetDateTime, String);

/// A message one half found.
#[derive(Debug, Clone)]
struct Found {
    conversation: Uuid,
    seq: i64,
    /// Where in the text the snippet starts looking (bytes).
    at_byte: usize,
}

/// About [`SNIPPET_CHARS`] characters of `text` around byte `at`: a little before it, whitespace collapsed, `…`
/// where it was cut.
#[must_use]
pub fn snippet(text: &str, at: usize) -> String {
    let at = (0..=at.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let lead = SNIPPET_CHARS / 4;
    let before: Vec<(usize, char)> = text[..at].char_indices().collect();
    let from = if before.len() > lead {
        before[before.len() - lead].0
    } else {
        0
    };
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    let mut n = 0;
    let mut space = false;
    let mut cut = false;
    for c in text[from..].chars() {
        if n >= SNIPPET_CHARS {
            cut = true;
            break;
        }
        if c.is_whitespace() {
            space = !out.is_empty() && !out.ends_with('…');
            continue;
        }
        if space {
            out.push(' ');
            n += 1;
            space = false;
        }
        out.push(c);
        n += 1;
    }
    if cut {
        out.push('…');
    }
    out
}

/// Byte offset of the first case-insensitive occurrence of any of `words` in `text` (0 when none is found the
/// simple way: the lowercase text must keep the original's byte offsets).
fn first_word(text: &str, words: &[String]) -> usize {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return 0;
    }
    words
        .iter()
        .filter_map(|w| lower.find(&w.to_lowercase()))
        .min()
        .unwrap_or(0)
}

/// `POST /v1/search`: the person's messages across their conversations (deleted ones left out).
///
/// # Errors
/// 400 for an empty or long query, a bad limit or vector; a statement error.
#[allow(clippy::too_many_lines)] // one query plan, read top to bottom
pub async fn conversation_search(
    pool: &Pool,
    person: &str,
    req: ConversationSearchRequest,
) -> Result<ConversationSearchResponse> {
    let q = req.q.trim().to_owned();
    if q.is_empty() {
        return Err(Error::bad_request("q is empty"));
    }
    if q.chars().count() > QUERY_MAX_CHARS {
        return Err(Error::bad_request(format!(
            "q is longer than {QUERY_MAX_CHARS} characters"
        )));
    }
    let limit = req.limit.unwrap_or(SEARCH_DEFAULT);
    if limit == 0 || limit > SEARCH_MAX {
        return Err(Error::bad_request(format!("limit must be 1-{SEARCH_MAX}")));
    }
    let words: Vec<String> = q.split_whitespace().take(WORDS_MAX).map(str::to_owned).collect();
    let patterns: Vec<String> = words.iter().map(|w| like(w)).collect();
    let query_vec = match &req.embedding {
        Some(e) => Some((model_of(&e.model)?.to_owned(), vector_text(&e.vector)?)),
        None => None,
    };
    let person = person.to_owned();
    let (person, patterns, query_vec, words) = (&person, &patterns, &query_vec, &words);
    aviary_pg::tx!(pool, |t| -> Result<ConversationSearchResponse> {
        crate::db::snapshot(t).await?;
        // the words half: every word in the message, newest first
        let by_words: Vec<Found> = t
            .query(
                &format!(
                    "select e.conversation_id, e.seq, e.text from events e
                     where e.conversation_id in (select id from conversations where person = $1 and deleted_at is null)
                       and e.text ilike all($2) and {IS_MESSAGE}
                     order by e.at desc limit $3"
                ),
                &[person, patterns, &HALF],
            )
            .await?
            .into_iter()
            .map(|r| {
                let text: String = r.get(2);
                Found {
                    conversation: r.get(0),
                    seq: r.get(1),
                    at_byte: first_word(&text, words),
                }
            })
            .collect();
        // the meaning half: the nearest pieces of the person's messages
        let mut semantic = false;
        let mut by_meaning: Vec<(Found, f64)> = Vec::new();
        if let Some((model, vec)) = query_vec {
            t.batch_execute("set local hnsw.ef_search = 100; set local hnsw.iterative_scan = relaxed_order")
                .await?;
            let near = t
                .query(
                    "select m.conversation_id, m.seq, m.piece_start, (m.vec <=> ($2::text)::vector)::float8 as d
                     from embeddings m where m.person = $1 and m.model = $3
                     order by m.vec <=> ($2::text)::vector limit $4",
                    &[person, vec, model, &(HALF * 2)],
                )
                .await?;
            semantic = !near.is_empty();
            let mut seen = std::collections::HashSet::new();
            let mut all: Vec<(Found, f64)> = near
                .into_iter()
                .map(|r| {
                    let start: i32 = r.get(2);
                    (
                        Found {
                            conversation: r.get(0),
                            seq: r.get(1),
                            at_byte: usize::try_from(start).unwrap_or(0),
                        },
                        r.get::<_, f64>(3),
                    )
                })
                .collect();
            all.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (f, d) in all {
                if seen.insert((f.conversation, f.seq)) {
                    by_meaning.push((f, d));
                }
            }
            by_meaning.truncate(usize::try_from(HALF).unwrap_or(60));
        }
        // the merge
        let word_keys: std::collections::HashSet<(Uuid, i64)> =
            by_words.iter().map(|f| (f.conversation, f.seq)).collect();
        let nearest = by_meaning.first().map_or(0.0, |(_, d)| *d);
        let mut merged: HashMap<(Uuid, i64), (f64, bool, bool, Found)> = HashMap::new();
        for (rank, f) in by_words.iter().enumerate() {
            let e = merged
                .entry((f.conversation, f.seq))
                .or_insert((0.0, false, false, f.clone()));
            e.0 += rrf(rank);
            e.1 = true;
        }
        for (rank, (f, d)) in by_meaning.iter().enumerate() {
            let key = (f.conversation, f.seq);
            if !word_keys.contains(&key) && (*d > nearest + MEANING_SPREAD || *d >= MEANING_FAR) {
                continue;
            }
            let e = merged.entry(key).or_insert((0.0, false, false, f.clone()));
            e.0 += rrf(rank);
            e.2 = true;
            if !e.1 {
                e.3 = f.clone();
            }
        }
        let mut ranked: Vec<(f64, bool, bool, Found)> = merged.into_values().collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.3.seq.cmp(&b.3.seq)));
        let mut per: HashMap<Uuid, usize> = HashMap::new();
        ranked.retain(|(_, _, _, f)| {
            let n = per.entry(f.conversation).or_insert(0);
            *n += 1;
            *n <= PER_CONVERSATION
        });
        ranked.truncate(limit as usize);
        if ranked.is_empty() {
            return Ok(ConversationSearchResponse {
                hits: Vec::new(),
                semantic,
            });
        }
        // what each hit shows: its conversation, row, time and text
        let convs: Vec<Uuid> = ranked.iter().map(|r| r.3.conversation).collect();
        let seqs: Vec<i64> = ranked.iter().map(|r| r.3.seq).collect();
        let details: HashMap<(Uuid, i64), Detail> = t
            .query(
                "select e.conversation_id, e.seq, c.title, c.epoch, r.seq_end, r.kind, e.at, e.text
                 from unnest($1::uuid[], $2::bigint[]) as h(conversation_id, seq)
                 join events e on e.conversation_id = h.conversation_id and e.seq = h.seq
                 join conversations c on c.id = e.conversation_id and c.person = $3 and c.deleted_at is null
                 join lateral (select seq_end, kind from rows r where r.conversation_id = e.conversation_id
                                 and r.source_seqs[1] = e.seq and r.kind in ('user', 'assistant')
                               order by row_id limit 1) r on true",
                &[&convs, &seqs, person],
            )
            .await?
            .into_iter()
            .map(|r| {
                (
                    (r.get(0), r.get(1)),
                    (r.get(2), r.get(3), r.get(4), r.get(5), r.get(6), r.get(7)),
                )
            })
            .collect();
        let hits = ranked
            .into_iter()
            .filter_map(|(score, words_hit, meaning_hit, f)| {
                let (title, epoch, row, kind, at, text) = details.get(&(f.conversation, f.seq))?.clone();
                Some(ConversationSearchHit {
                    conversation_id: f.conversation,
                    title,
                    epoch: epoch.to_string(),
                    seq: u64::try_from(row).unwrap_or(0),
                    kind,
                    at: Timestamp(at),
                    snippet: snippet(&text, f.at_byte),
                    matched: match (words_hit, meaning_hit) {
                        (true, true) => "both",
                        (true, false) => "words",
                        _ => "meaning",
                    }
                    .to_owned(),
                    score,
                })
            })
            .collect();
        Ok(ConversationSearchResponse { hits, semantic })
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_short_message_is_one_piece_and_whitespace_none() {
        assert_eq!(pieces("hello world"), vec![(0, 11)]);
        assert_eq!(pieces("  \n hi  "), vec![(4, 6)]);
        assert_eq!(pieces(" \n\t "), vec![]);
        assert_eq!(pieces(""), vec![]);
    }

    #[test]
    fn a_long_message_is_cut_at_its_breaks_and_never_mid_character() {
        let para = "火車票退票要在開車前辦理。".repeat(60); // 780 characters
        let text = format!("{para}\n\n{para}\n\n{para}");
        let p = pieces(&text);
        assert_eq!(p.len(), 3, "{p:?}");
        for &(a, b) in &p {
            assert!(text.is_char_boundary(a) && text.is_char_boundary(b));
            assert_eq!(&text[a..b], para);
        }
        // no break at all: a hard cut at the window
        let solid = "x".repeat(CHUNK_CHARS * 2 + 5);
        let p = pieces(&solid);
        assert_eq!(
            p,
            vec![
                (0, CHUNK_CHARS),
                (CHUNK_CHARS, 2 * CHUNK_CHARS),
                (2 * CHUNK_CHARS, solid.len())
            ]
        );
        // sentences: cut after a full stop followed by a space
        let sentence = "This is a sentence about the shuttle bus. ".repeat(60);
        for &(a, b) in &pieces(&sentence) {
            assert!(sentence[a..b].ends_with('.'), "{}", &sentence[a..b]);
            assert!(sentence[a..b].chars().count() <= CHUNK_CHARS);
        }
    }

    #[test]
    fn a_very_long_message_has_at_most_the_cap() {
        let text = "word ".repeat(CHUNK_CHARS * (CHUNKS_MAX + 5) / 5);
        assert_eq!(pieces(&text).len(), CHUNKS_MAX);
    }

    #[test]
    fn a_snippet_starts_a_little_before_the_match() {
        let text = format!("{} needle {}", "a ".repeat(200), "b ".repeat(200));
        let at = text.find("needle").unwrap();
        let s = snippet(&text, at);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
        assert_eq!(snippet("short\n\n  text", 0), "short text");
        assert_eq!(first_word("Hello World", &["world".into()]), 6);
        assert_eq!(
            first_word("İx world", &["world".into()]),
            0,
            "a text whose lowercase moves bytes"
        );
    }

    #[test]
    fn a_vector_is_checked_and_written_as_pgvector_text() {
        assert!(vector_text(&[0.5; 3]).is_err());
        let mut v = vec![0.0_f32; DIMENSIONS];
        v[0] = 0.5;
        v[1] = -0.25;
        let s = vector_text(&v).unwrap();
        assert!(s.starts_with("[0.5,-0.25,0,"), "{}", &s[..20]);
        v[2] = f32::NAN;
        assert!(vector_text(&v).is_err());
    }
}
