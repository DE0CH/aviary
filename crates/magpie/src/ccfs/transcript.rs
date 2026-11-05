//! The raw view's read (`GET /v1/conversations/{id}/transcript`, pigeon's view-as raw transcript, Deyao,
//! 2026-11-05): the main transcript's lines in file order, each reconstructed from its stub and blobs. Long text is
//! put back whole; base64 media is replaced by a short placeholder (`[media <mime> <size> bytes]`, still a valid JSON
//! string), so a page stays small. Only while an admin views as the person: anyone else gets 404, as for a
//! conversation that is not theirs. Reads are not audited per call (the view's `start` row covers it, magpie 13).

use std::collections::HashMap;

use axum::body::Bytes;
use magpie_api::rows::{TranscriptLine, TranscriptPage, TranscriptQuery};
use uuid::Uuid;

use super::extract::{self, Sha};
use super::lines::spans_of;
use super::reconstruct::blob_bytes;
use crate::app::Magpie;
use crate::conversations::Person;
use crate::error::{Error, Result};

/// Lines per page by default.
pub const DEFAULT_LIMIT: u32 = 200;
/// Lines per page at most.
pub const MAX_LIMIT: u32 = 500;
/// A page stops adding lines past this many bytes (the first line always comes).
pub const MAX_PAGE_BYTES: usize = 8 << 20;

/// The placeholder put where a media blob's base64 was.
#[must_use]
pub fn media_placeholder(mime: Option<&str>, size: i64) -> String {
    format!("[media {} {size} bytes]", mime.unwrap_or("unknown"))
}

/// A line as stored: its `ord`, stub and spans.
type Stored = (i64, Vec<u8>, Vec<(usize, Sha)>);

/// A page of `id`'s main transcript for `person` (who must be viewed as by an admin).
///
/// # Errors
/// 404 when nobody is viewing as `person`, or the conversation is not theirs; a statement or bucket error.
pub async fn page(app: &Magpie, person: &Person, id: Uuid, q: &TranscriptQuery) -> Result<TranscriptPage> {
    if person.viewer.is_none() {
        return Err(Error::not_found());
    }
    let conn = app.pool.get().await?;
    let row = conn
        .query_opt(
            "select person, main_transcript, active_turn from conversations where id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let owner: String = row.get(0);
    if owner != person.email {
        return Err(Error::not_found());
    }
    let main: Option<String> = row.get(1);
    let running = row.get::<_, Option<Uuid>>(2).is_some();
    let after = q.after.unwrap_or(-1);
    let empty = |path| TranscriptPage {
        path,
        lines: Vec::new(),
        next: q.after,
        more: false,
        running,
    };
    let Some(path) = main else {
        return Ok(empty(None));
    };
    let Some(file) = super::load_file(&conn, id, &path).await? else {
        return Ok(empty(Some(path)));
    };
    let limit = i64::from(q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT));
    let rows = conn
        .query(
            "select ord, stub, spans from cc_lines where file_id = $1 and ord > $2 order by ord limit $3",
            &[&file.id, &after, &(limit + 1)],
        )
        .await?;
    let mut lines: Vec<Stored> = Vec::with_capacity(rows.len());
    for r in &rows {
        lines.push((r.get(0), r.get(1), spans_of(r.get(2))?));
    }
    let mut more = lines.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    lines.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let mut shas: Vec<Sha> = lines.iter().flat_map(|l| l.2.iter().map(|s| s.1)).collect();
    shas.sort_unstable();
    shas.dedup();
    // media blobs are never fetched: their placeholder stands in
    let mut media: HashMap<Sha, Bytes> = HashMap::new();
    if !shas.is_empty() {
        let keys: Vec<&[u8]> = shas.iter().map(<[u8; 32]>::as_slice).collect();
        for r in conn
            .query(
                "select sha256, mime, size from blobs where sha256 = any($1) and kind = 'media'",
                &[&keys],
            )
            .await?
        {
            let sha: Vec<u8> = r.get(0);
            if let Ok(sha) = Sha::try_from(sha.as_slice()) {
                let mime: Option<String> = r.get(1);
                media.insert(sha, Bytes::from(media_placeholder(mime.as_deref(), r.get(2))));
            }
        }
    }
    drop(conn);
    let text_shas: Vec<Sha> = shas.into_iter().filter(|s| !media.contains_key(s)).collect();
    let mut bytes = if text_shas.is_empty() {
        HashMap::new()
    } else {
        blob_bytes(app, &text_shas).await?
    };
    bytes.extend(media);
    let mut out = Vec::with_capacity(lines.len());
    let mut total = 0usize;
    for (ord, stub, spans) in &lines {
        let line = extract::splice(stub, spans, |s| bytes.get(s).map(|b| &b[..])).map_err(Error::Internal)?;
        if !out.is_empty() && total + line.len() > MAX_PAGE_BYTES {
            more = true;
            break;
        }
        total += line.len();
        let mut line = String::from_utf8_lossy(&line).into_owned();
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        out.push(TranscriptLine { ord: *ord, line });
    }
    let next = out.last().map(|l| l.ord).or(q.after);
    Ok(TranscriptPage {
        path: Some(path),
        lines: out,
        next,
        more,
        running,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_stays_a_json_string() {
        let p = media_placeholder(Some("image/png"), 1234);
        assert_eq!(p, "[media image/png 1234 bytes]");
        let line = format!(r#"{{"data":"{p}"}}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&line).is_ok());
        assert_eq!(media_placeholder(None, 0), "[media unknown 0 bytes]");
    }
}
