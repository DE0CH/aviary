//! The event log (magpie 7.1-7.2): `append` allocates the next seq of a conversation and writes the event, then
//! hands it to the timeline projection ([`crate::rows::project`]) in the same transaction.
//!
//! The caller holds the conversation row lock (`select … for update`, [`crate::conversations::lock`]): the lock
//! serialises writers per conversation, so `head_seq` is contiguous from 1, gap-free and never reused (magpie 4).
//! The kinds' rules (rows, retracts) are the projection's ([`crate::rows`]); this module is the seam the other packages
//! call.

use aviary_pg::Tx;
use magpie_api::events::EventBody;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result};

/// Plain text kept for search and previews, at most this many bytes (magpie 4).
pub const TEXT_MAX_BYTES: usize = 64 * 1024;

/// An event to append.
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// `kind`.
    pub kind: String,
    /// `data`.
    pub data: Value,
    /// Facts computed at ingest (`meta`), never changed afterwards.
    pub meta: Option<Value>,
    /// The turn it belongs to.
    pub turn_id: Option<Uuid>,
    /// Plain text for search and previews (cut to [`TEXT_MAX_BYTES`]).
    pub text: Option<String>,
    /// A prompt (M10).
    pub is_prompt: bool,
    /// The admin who sent it while viewing as the person.
    pub viewer: Option<String>,
    /// When.
    pub at: OffsetDateTime,
}

impl NewEvent {
    /// An event of a typed kind, now, with no meta, turn, text or viewer.
    ///
    /// # Errors
    /// When the body does not serialise (an [`EventBody::Unknown`]).
    pub fn of(body: &EventBody) -> Result<Self> {
        let mut v = serde_json::to_value(body)?;
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Internal("event without a kind".into()))?
            .to_owned();
        let data = v
            .get_mut("data")
            .map_or_else(|| Value::Object(serde_json::Map::new()), Value::take);
        Ok(Self {
            kind,
            data,
            meta: None,
            turn_id: None,
            text: None,
            is_prompt: false,
            viewer: None,
            at: OffsetDateTime::now_utc(),
        })
    }

    /// With `turn_id`.
    #[must_use]
    pub fn turn(mut self, turn_id: Option<Uuid>) -> Self {
        self.turn_id = turn_id;
        self
    }

    /// With `viewer`.
    #[must_use]
    pub fn viewer(mut self, viewer: Option<String>) -> Self {
        self.viewer = viewer;
        self
    }

    /// With `text`.
    #[must_use]
    pub fn text(mut self, text: Option<String>) -> Self {
        self.text = text;
        self
    }
}

/// An event as stored.
#[derive(Debug, Clone)]
pub struct Stored {
    /// The conversation.
    pub conversation_id: Uuid,
    /// Its seq.
    pub seq: i64,
    /// The event.
    pub event: NewEvent,
}

/// Postgres refuses NUL in `text` and `\u0000` in `jsonb` (SQLSTATE 22021, 22P05): a tool's output with a NUL
/// byte (`head` of a binary) would fail every write of its turn. Each NUL becomes U+FFFD, as a terminal shows it.
#[must_use]
pub fn denul(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\0') {
        std::borrow::Cow::Owned(s.replace('\0', "\u{FFFD}"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// [`denul`] on every string (keys too) inside `v`.
pub fn denul_value(v: &mut Value) {
    match v {
        Value::String(s) if s.contains('\0') => *s = s.replace('\0', "\u{FFFD}"),
        Value::Array(a) => a.iter_mut().for_each(denul_value),
        Value::Object(o) if o.keys().any(|k| k.contains('\0')) || o.values().any(has_nul) => {
            let old = std::mem::take(o);
            for (k, mut x) in old {
                denul_value(&mut x);
                o.insert(denul(&k).into_owned(), x);
            }
        }
        _ => {}
    }
}

fn has_nul(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains('\0'),
        Value::Array(a) => a.iter().any(has_nul),
        Value::Object(o) => o.iter().any(|(k, x)| k.contains('\0') || has_nul(x)),
        _ => false,
    }
}

/// Cuts `s` to at most `max` bytes on a character boundary.
#[must_use]
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Appends one event to `conversation` (whose row the caller has locked) and projects it into rows; its seq.
///
/// # Errors
/// When a statement fails or the conversation row is gone.
pub async fn append(tx: &Tx<'_>, conversation: Uuid, mut event: NewEvent) -> Result<i64> {
    denul_value(&mut event.data);
    if let Some(m) = event.meta.as_mut() {
        denul_value(m);
    }
    if let Some(t) = event.text.as_mut().filter(|t| t.contains('\0')) {
        *t = denul(t).into_owned();
    }
    let row = tx
        .query_opt(
            "update conversations set head_seq = head_seq + 1 where id = $1 returning head_seq",
            &[&conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let seq: i64 = row.get(0);
    let text = event.text.as_deref().map(|t| cut(t, TEXT_MAX_BYTES));
    tx.execute(
        "insert into events (conversation_id, seq, kind, at, turn_id, data, meta, text, is_prompt, viewer)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        &[
            &conversation,
            &seq,
            &event.kind,
            &event.at,
            &event.turn_id,
            &event.data,
            &event.meta,
            &text,
            &event.is_prompt,
            &event.viewer,
        ],
    )
    .await?;
    let stored = Stored {
        conversation_id: conversation,
        seq,
        event,
    };
    crate::rows::project(tx, &stored).await?;
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_nul_anywhere_in_an_event_becomes_a_replacement_character() {
        let mut v = json!({"result": {"text": "ELF\u{0}\u{0}x", "k\u{0}": ["a\u{0}", 1]}, "ok": "clean"});
        denul_value(&mut v);
        assert_eq!(
            v,
            json!({"result": {"text": "ELF\u{FFFD}\u{FFFD}x", "k\u{FFFD}": ["a\u{FFFD}", 1]}, "ok": "clean"})
        );
        assert_eq!(denul("a\u{0}b"), "a\u{FFFD}b");
        assert!(matches!(denul("clean"), std::borrow::Cow::Borrowed(_)));
    }
}
