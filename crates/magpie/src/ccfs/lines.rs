//! Lines files (magpie 6.2, 6.3, 6.6): each complete line is one `cc_lines` row (stub, spans, sha256, offset, order,
//! turn, event), the bytes after the last `\n` are the file's `tail`, and every operation runs one algorithm over the
//! affected suffix.
//!
//! For a cut point `C` (a write at `C`, a truncate to `C`): the old suffix starts at the first line ending after `C`
//! (or at the tail); the new suffix is the old one with the write applied (or cut); its lines are matched against
//! the old suffix's lines by sha256, in order first and then by hash, then against the file's `truncated` limbo and,
//! at the main path, the scope's `moved` limbo. A matched line keeps its stub, spans and event; an unmatched one is
//! extracted (its blobs staged) and, on the main transcript, classified into an event. The normal case (an append at
//! the end with an empty tail) matches against nothing and touches no other row.
//!
//! Lines removed by a truncate go to limbo (6.7); lines an overwrite or a rename-over drops are retracted at once.
//! Only the main transcript has visible events, so only its lines are put in limbo or retracted.

use std::collections::{HashMap, VecDeque};

use aviary_pg::Tx;
use magpie_api::errors::ErrorCode;
use magpie_api::events::{EventBody, NameSource, TitleChanged};
use magpie_api::uploads::{BlobKind, BlobPointer, BlobRef};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::classify::{self, Class, Context};
use super::extract::{self, Kind as BlobKindX, Sha, hex};
use super::{File, Op, limbo, n64, nus};
use crate::blobs::{self, staging};
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;

/// A stored line.
#[derive(Debug, Clone)]
pub struct Row {
    /// Order in the file.
    pub ord: i64,
    /// Offset of its first byte.
    pub start_off: i64,
    /// Bytes, the final `\n` included.
    pub len: i64,
    /// SHA-256 of the whole line.
    pub sha: Sha,
    /// The line without its blobs' content.
    pub stub: Vec<u8>,
    /// Where the blobs go.
    pub spans: Vec<(usize, Sha)>,
    /// Its event.
    pub event_seq: Option<i64>,
    /// The turn it is credited to.
    pub turn_id: Option<Uuid>,
    /// Classified (written at the main path, or promoted).
    pub classified: bool,
}

/// `spans` as stored: `[[offset, "<sha hex>"], …]`, null when there are none.
#[must_use]
pub fn spans_json(spans: &[(usize, Sha)]) -> Option<Value> {
    (!spans.is_empty()).then(|| Value::Array(spans.iter().map(|(o, s)| serde_json::json!([o, hex(s)])).collect()))
}

/// `spans` from storage.
///
/// # Errors
/// When the stored value is malformed.
pub fn spans_of(v: Option<Value>) -> Result<Vec<(usize, Sha)>> {
    let Some(Value::Array(items)) = v else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|i| {
            let off = i.get(0).and_then(Value::as_u64).and_then(|o| usize::try_from(o).ok());
            let sha = i.get(1).and_then(Value::as_str).and_then(extract::unhex);
            off.zip(sha)
                .ok_or_else(|| Error::Internal(format!("malformed span {i}")))
        })
        .collect()
}

const ROW_COLS: &str = "ord, start_off, len, sha256, stub, spans, event_seq, turn_id, classified";

fn row_of(r: &aviary_pg::Row) -> Result<Row> {
    let sha: Vec<u8> = r.get(3);
    Ok(Row {
        ord: r.get(0),
        start_off: r.get(1),
        len: i64::from(r.get::<_, i32>(2)),
        sha: Sha::try_from(sha.as_slice()).map_err(|_| Error::Internal("a line sha is not 32 bytes".into()))?,
        stub: r.get(4),
        spans: spans_of(r.get(5))?,
        event_seq: r.get(6),
        turn_id: r.get(7),
        classified: r.get(8),
    })
}

/// The lines of `file` from the first one that ends after `off`, in order.
///
/// # Errors
/// When a statement fails.
pub async fn rows_from(db: &impl crate::db::Db, file: i64, off: i64) -> Result<Vec<Row>> {
    let rows = db
        .query(
            &format!(
                "select {ROW_COLS} from cc_lines
                  where file_id = $1 and start_off >= coalesce(
                    (select max(start_off) from cc_lines where file_id = $1 and start_off <= $2), 0)
                  order by ord"
            ),
            &[&file, &off],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let row = row_of(r)?;
        if row.start_off + row.len > off {
            out.push(row);
        }
    }
    Ok(out)
}

/// The bytes of `rows`, reconstructed with their blobs (loaded into `op.blobs`; a blob only in the bucket fails the
/// transaction with [`Error::NeedBlobs`]).
///
/// # Errors
/// As described; 500 when a line does not reconstruct to its sha256.
pub async fn bytes_of(tx: &Tx<'_>, op: &mut Op<'_>, rows: &[Row]) -> Result<Vec<u8>> {
    let shas: Vec<Sha> = rows.iter().flat_map(|r| r.spans.iter().map(|s| s.1)).collect();
    op.blobs.load(tx, &shas).await?;
    let mut out = Vec::new();
    for r in rows {
        out.extend_from_slice(&line_bytes(&op.blobs, r)?);
    }
    Ok(out)
}

/// One line reconstructed and checked against its sha256 (6.4: a mismatch fails, never answers wrong bytes).
///
/// # Errors
/// 500 on a mismatch or a missing blob.
pub fn line_bytes(blobs: &blobs::Bytes<'_>, r: &Row) -> Result<Vec<u8>> {
    let line = extract::splice(&r.stub, &r.spans, |s| blobs.get(s)).map_err(Error::Internal)?;
    if extract::sha(&line) != r.sha || n64(line.len()) != r.len {
        tracing::error!(ord = r.ord, sha = %hex(&r.sha), "reconstruction sha mismatch");
        return Err(Error::Internal("reconstruction sha mismatch".into()));
    }
    Ok(line)
}

/// Overwrites `buf` at `at` with `data`, extending it when the write runs past the end.
pub fn overwrite(buf: &mut Vec<u8>, at: usize, data: &[u8]) {
    let end = at + data.len();
    if buf.len() < end {
        buf.resize(end, 0);
    }
    buf[at..end].copy_from_slice(data);
}

/// Old lines a new suffix is matched against: in order first, then by sha256.
#[derive(Debug, Default)]
struct Matcher {
    old: Vec<Row>,
    used: Vec<bool>,
    cursor: usize,
    by_sha: HashMap<Sha, VecDeque<usize>>,
}

impl Matcher {
    fn new(old: Vec<Row>) -> Self {
        let mut by_sha: HashMap<Sha, VecDeque<usize>> = HashMap::new();
        for (i, r) in old.iter().enumerate() {
            by_sha.entry(r.sha).or_default().push_back(i);
        }
        Self {
            used: vec![false; old.len()],
            old,
            cursor: 0,
            by_sha,
        }
    }

    fn take(&mut self, sha: &Sha) -> Option<&Row> {
        while self.cursor < self.old.len() && self.used[self.cursor] {
            self.cursor += 1;
        }
        let i = if self.cursor < self.old.len() && &self.old[self.cursor].sha == sha {
            self.cursor
        } else {
            let q = self.by_sha.get_mut(sha)?;
            while q.front().is_some_and(|&i| self.used[i]) {
                q.pop_front();
            }
            *q.front()?
        };
        self.used[i] = true;
        self.cursor = i + 1;
        Some(&self.old[i])
    }

    /// The events of old lines no new line took.
    fn unused_events(&self) -> Vec<i64> {
        self.old
            .iter()
            .zip(&self.used)
            .filter(|(_, u)| !**u)
            .filter_map(|(r, _)| r.event_seq)
            .collect()
    }
}

/// Where the next new line goes, and what it may be matched against.
#[derive(Debug)]
struct Writer {
    file: i64,
    main: bool,
    ord: i64,
    off: i64,
    limbo_truncated: bool,
    limbo_moved: bool,
    /// Events of the old lines this operation replaces (retracted at its end unless a new line takes them back).
    pending: Vec<i64>,
}

async fn insert_row(tx: &Tx<'_>, w: &Writer, r: &Row) -> Result<()> {
    let len = i32::try_from(r.len).map_err(|_| Error::Internal("line length".into()))?;
    tx.execute(
        "insert into cc_lines (file_id, ord, start_off, len, sha256, stub, spans, event_seq, turn_id, classified)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        &[
            &w.file,
            &w.ord,
            &w.off,
            &len,
            &r.sha.as_slice(),
            &r.stub,
            &spans_json(&r.spans),
            &r.event_seq,
            &r.turn_id,
            &r.classified,
        ],
    )
    .await?;
    Ok(())
}

fn too_large(what: &str) -> Error {
    Error::code(ErrorCode::TooLarge, format!("{what} is over 64 MiB"))
}

/// The blob reference JSON of an event (`{"$blob": {…}}`).
fn pointer(sha: &Sha, m: &blobs::Meta) -> String {
    let kind = match m.kind.as_str() {
        "media" => BlobKind::Media,
        "text" => BlobKind::Text,
        _ => BlobKind::File,
    };
    let p = BlobPointer {
        blob: BlobRef {
            blob_id: hex(sha),
            bytes: m.size,
            kind,
            mime: m.mime.clone(),
            width: m.width,
            height: m.height,
            preview: m.preview.clone(),
            lines: m.lines,
        },
    };
    serde_json::to_string(&p).unwrap_or_else(|_| "null".into())
}

/// A line's event data: the stub parsed with each span's `""` replaced by its blob reference. `None` when it is not
/// JSON (an unparsable or torn line).
fn event_data(stub: &[u8], spans: &[(usize, Sha)], meta: &HashMap<Sha, blobs::Meta>) -> Option<Value> {
    if spans.is_empty() {
        return serde_json::from_slice(stub).ok();
    }
    let mut out = Vec::with_capacity(stub.len() + spans.len() * 160);
    let mut at = 0;
    for (off, sha) in spans {
        // the stub holds `""` around each span's offset
        if *off == 0 || *off < at || stub.get(off - 1) != Some(&b'"') || stub.get(*off) != Some(&b'"') {
            return None;
        }
        out.extend_from_slice(&stub[at..off - 1]);
        out.extend_from_slice(pointer(sha, meta.get(sha)?).as_bytes());
        at = off + 1;
    }
    out.extend_from_slice(&stub[at..]);
    serde_json::from_slice(&out).ok()
}

/// Makes the event (or the internal effect) of a main-transcript line; its event seq.
async fn apply(
    tx: &Tx<'_>,
    op: &mut Op<'_>,
    stub: &[u8],
    spans: &[(usize, Sha)],
    meta: &HashMap<Sha, blobs::Meta>,
    w: Option<&Writer>,
) -> Result<Option<i64>> {
    let data = event_data(stub, spans, meta);
    let cx = Context {
        echo_pending: op.turn.is_some_and(|t| t.echo_pending),
    };
    match classify::classify(data.as_ref(), cx) {
        Class::Internal => Ok(None),
        Class::Echo => {
            if let Some(t) = op.turn.as_mut() {
                tx.execute("update turns set echo_matched = true where id = $1", &[&t.id])
                    .await?;
                t.echo_pending = false;
            }
            Ok(None)
        }
        Class::Title(title) => {
            title_record(tx, op, &title).await?;
            Ok(None)
        }
        Class::Event(e) => {
            let mut m = e.meta;
            if let Some(results) = m.results.as_mut() {
                for r in results {
                    r.tool_use_seq = tx
                        .query_opt(
                            "select use_seq from tool_links where conversation_id = $1 and tool_use_id = $2",
                            &[&op.conv.id, &r.tool_use_id],
                        )
                        .await?
                        .and_then(|row| u64::try_from(row.get::<_, i64>(0)).ok());
                }
            }
            if let Some(u) = &e.uuid {
                m.replaces = replaces(tx, op, u, w).await?.and_then(|s| u64::try_from(s).ok());
            }
            let meta_json = serde_json::to_value(&m)?;
            let meta_json = meta_json
                .as_object()
                .is_some_and(|o| !o.is_empty())
                .then_some(meta_json);
            let ev = NewEvent {
                kind: e.kind.to_owned(),
                data: data.unwrap_or(Value::Null),
                meta: meta_json,
                turn_id: op.turn_id(),
                text: e.text,
                is_prompt: e.is_prompt,
                viewer: None,
                at: OffsetDateTime::now_utc(),
            };
            let seq = events::append(tx, op.conv.id, ev).await?;
            for (id, name) in &e.tool_uses {
                tx.execute(
                    "insert into tool_links (conversation_id, tool_use_id, use_seq, name) values ($1, $2, $3, $4)
                     on conflict (conversation_id, tool_use_id) do update set use_seq = excluded.use_seq,
                       name = excluded.name",
                    &[&op.conv.id, id, &seq, name],
                )
                .await?;
            }
            for id in &e.results {
                tx.execute(
                    "update tool_links set result_seq = $3 where conversation_id = $1 and tool_use_id = $2",
                    &[&op.conv.id, id, &seq],
                )
                .await?;
            }
            if let Some((model, tokens)) = e.model {
                let tokens = tokens.and_then(|t| i32::try_from(t).ok());
                tx.execute(
                    "update conversations set model = $2, context_tokens = coalesce($3, context_tokens) where id = $1",
                    &[&op.conv.id, &model, &tokens],
                )
                .await?;
                if op.conv.model.as_deref() != Some(model.as_str()) {
                    op.conv.model = Some(model);
                    op.upserted(tx).await?;
                }
            }
            Ok(Some(seq))
        }
    }
}

/// The event a new line with `uuid` replaces (6.7): a retracted line's, or one this operation is about to retract
/// (an old line of the rewritten suffix) or that waits in the file's limbo.
async fn replaces(tx: &Tx<'_>, op: &Op<'_>, uuid: &str, w: Option<&Writer>) -> Result<Option<i64>> {
    if let Some(seq) = limbo::replaced(tx, op.conv.id, uuid).await? {
        return Ok(Some(seq));
    }
    let Some(w) = w else {
        return Ok(None);
    };
    if !w.pending.is_empty() {
        let row = tx
            .query_one(
                "select max(seq) from events where conversation_id = $1 and seq = any($2) and data->>'uuid' = $3",
                &[&op.conv.id, &w.pending, &uuid],
            )
            .await?;
        if let Some(seq) = row.get::<_, Option<i64>>(0) {
            return Ok(Some(seq));
        }
    }
    if w.limbo_truncated {
        let row = tx
            .query_one(
                "select max(e.seq) from cc_limbo l join events e on e.conversation_id = l.scope and e.seq = l.event_seq
                  where l.scope = $1 and l.reason = 'truncated' and l.file_id = $2 and e.data->>'uuid' = $3",
                &[&op.conv.id, &w.file, &uuid],
            )
            .await?;
        return Ok(row.get(0));
    }
    Ok(None)
}

/// Claude Code's own title record (`custom-title`, `ai-title`): the provisional title while no title model or rename
/// named the conversation (6.5).
async fn title_record(tx: &Tx<'_>, op: &mut Op<'_>, title: &str) -> Result<()> {
    if op.conv.name_source.is_some() {
        return Ok(());
    }
    let title: String = title.trim().chars().take(limits::RENAME_MAX_CHARS).collect();
    if title.is_empty() || op.conv.title.as_deref() == Some(title.as_str()) {
        return Ok(());
    }
    tx.execute(
        "update conversations set title = $2 where id = $1 and name_source is null",
        &[&op.conv.id, &title],
    )
    .await?;
    let body = EventBody::TitleChanged(TitleChanged {
        title: title.clone(),
        name_source: None::<NameSource>,
    });
    events::append(tx, op.conv.id, NewEvent::of(&body)?.turn(op.turn_id())).await?;
    op.conv.title = Some(title);
    op.upserted(tx).await
}

/// Stores one new complete line at the writer's position: matched (keeping its stub, spans and event) or extracted,
/// its blobs staged and, on the main transcript, classified.
async fn put_line(tx: &Tx<'_>, op: &mut Op<'_>, w: &mut Writer, line: &[u8], m: &mut Matcher) -> Result<()> {
    if n64(line.len()) > limits::CC_LINE_MAX {
        return Err(too_large("a line"));
    }
    let sha = extract::sha(line);
    let row = if let Some(old) = m.take(&sha) {
        Row {
            ord: w.ord,
            start_off: w.off,
            ..old.clone()
        }
    } else {
        let x = extract::extract(line);
        let mut meta: HashMap<Sha, blobs::Meta> = HashMap::new();
        for b in &x.blobs {
            if meta.contains_key(&b.sha) {
                continue;
            }
            let content = &line[b.range.clone()];
            let preview = (b.kind == BlobKindX::Text).then(|| {
                let (p, cut) = extract::preview(content);
                (p, cut, extract::line_count(content))
            });
            op.cc_delta += staging::stage(
                tx,
                op.conv.id,
                &staging::NewBlob {
                    sha: b.sha,
                    bytes: content,
                    kind: b.kind.as_str(),
                    encoding: b.kind.encoding(),
                    mime: b.mime,
                    width: b.width,
                    height: b.height,
                    preview: preview.as_ref().map(|(p, c, l)| (p.as_str(), *c, *l)),
                },
            )
            .await?;
            meta.insert(
                b.sha,
                blobs::Meta {
                    size: u64::try_from(content.len()).unwrap_or(0),
                    kind: b.kind.as_str().to_owned(),
                    mime: b.mime.map(str::to_owned),
                    width: b.width,
                    height: b.height,
                    preview: preview.as_ref().map(|p| p.0.clone()),
                    lines: preview.as_ref().map(|p| p.2),
                },
            );
        }
        let mut event_seq = None;
        if w.limbo_truncated {
            event_seq = limbo::take_truncated(tx, op.conv.id, w.file, &sha).await?;
        }
        if event_seq.is_none() && w.limbo_moved {
            event_seq = limbo::take_moved(tx, op.conv.id, &sha).await?;
        }
        if event_seq.is_none() && w.main {
            event_seq = apply(tx, op, &x.stub, &x.spans, &meta, Some(&*w)).await?;
        }
        Row {
            ord: w.ord,
            start_off: w.off,
            len: n64(line.len()),
            sha,
            stub: x.stub,
            spans: x.spans,
            event_seq,
            turn_id: op.turn_id(),
            classified: w.main || event_seq.is_some(),
        }
    };
    insert_row(tx, w, &row).await?;
    op.cc_delta += n64(row.stub.len());
    w.ord += 1;
    w.off += row.len;
    Ok(())
}

/// Stores the complete lines of `bytes` at the writer's position; the bytes after the last `\n` (the new tail).
async fn put_lines(tx: &Tx<'_>, op: &mut Op<'_>, w: &mut Writer, bytes: &[u8], m: &mut Matcher) -> Result<Vec<u8>> {
    let mut at = 0;
    while let Some(nl) = bytes[at..].iter().position(|&b| b == b'\n') {
        let end = at + nl + 1;
        put_line(tx, op, w, &bytes[at..end], m).await?;
        at = end;
    }
    let rest = bytes[at..].to_vec();
    if n64(rest.len()) > limits::CC_LINE_MAX {
        return Err(too_large("a line being written"));
    }
    Ok(rest)
}

async fn writer(tx: &Tx<'_>, op: &Op<'_>, f: &File) -> Result<Writer> {
    let main = op.conv.is_main(&f.path);
    let (limbo_truncated, limbo_moved) = if main {
        limbo::present(tx, op.conv.id, f.id, true).await?
    } else {
        (false, false)
    };
    Ok(Writer {
        file: f.id,
        main,
        ord: f.next_ord,
        off: 0,
        limbo_truncated,
        limbo_moved,
        pending: Vec::new(),
    })
}

async fn delete_rows(tx: &Tx<'_>, file: i64, from_ord: i64, to_ord: Option<i64>) -> Result<()> {
    tx.execute(
        "delete from cc_lines where file_id = $1 and ord >= $2 and ($3::bigint is null or ord <= $3)",
        &[&file, &from_ord, &to_ord],
    )
    .await?;
    Ok(())
}

/// A write of `data` at `offset` (<= the file's size; rule 1 and rule 0 are the caller's) to a lines file.
///
/// # Errors
/// 413 `too_large` for a line over its cap; 503 when staging is full; a statement error.
pub async fn write(tx: &Tx<'_>, op: &mut Op<'_>, f: &mut File, offset: i64, data: &[u8]) -> Result<()> {
    let tail_len = n64(f.tail.len());
    let tail_start = f.size - tail_len;
    let mut w = writer(tx, op, f).await?;
    if offset >= tail_start {
        // the old suffix is the tail (the fast path when it is empty and the write is at the end)
        let mut suffix = std::mem::take(&mut f.tail);
        overwrite(&mut suffix, nus(offset - tail_start), data);
        op.cc_delta -= tail_len;
        w.off = tail_start;
        let rest = put_lines(tx, op, &mut w, &suffix, &mut Matcher::default()).await?;
        op.cc_delta += n64(rest.len());
        f.tail = rest;
        f.next_ord = w.ord;
        f.size = tail_start + n64(suffix.len());
        return Ok(());
    }
    let old = rows_from(tx, f.id, offset).await?;
    let end = offset + n64(data.len());
    let take = old.iter().take_while(|l| l.start_off <= end).count().max(1);
    let (region, later) = old.split_at(take);
    let region_start = region[0].start_off;
    let with_tail = later.is_empty();
    let mut bytes = bytes_of(tx, op, region).await?;
    if with_tail {
        bytes.extend_from_slice(&f.tail);
    }
    overwrite(&mut bytes, nus(offset - region_start), data);
    delete_rows(tx, f.id, region[0].ord, Some(region[take - 1].ord)).await?;
    op.cc_delta -= region.iter().map(|r| n64(r.stub.len())).sum::<i64>();
    if with_tail {
        op.cc_delta -= tail_len;
    }
    w.off = region_start;
    w.pending = region.iter().filter_map(|r| r.event_seq).collect();
    let mut m = Matcher::new(region.to_vec());
    let rest = put_lines(tx, op, &mut w, &bytes, &mut m).await?;
    if with_tail {
        op.cc_delta += n64(rest.len());
        f.tail = rest;
    } else if !rest.is_empty() {
        return Err(Error::Internal(
            "an overwrite inside the file left a partial line".into(),
        ));
    }
    if let (Some(first), Some(last)) = (later.first(), later.last()) {
        // the unchanged lines after the region keep their bytes and offsets and follow the new lines in order
        tx.execute(
            "update cc_lines set ord = ord - $2 + $3 where file_id = $1 and ord between $2 and $4",
            &[&f.id, &first.ord, &w.ord, &last.ord],
        )
        .await?;
        w.ord += last.ord - first.ord + 1;
    }
    f.next_ord = w.ord;
    f.size = f.size.max(end);
    if w.main {
        limbo::retract(tx, op.conv.id, op.turn_id(), m.unused_events()).await?;
    }
    Ok(())
}

/// A truncate of a lines file to `size` (<= its size). Removed main-transcript lines with events go to limbo.
///
/// # Errors
/// A statement error; 503 when a partial line's blob is only in the bucket and was not fetched.
pub async fn truncate(tx: &Tx<'_>, op: &mut Op<'_>, f: &mut File, size: i64) -> Result<()> {
    let tail_len = n64(f.tail.len());
    let tail_start = f.size - tail_len;
    if size >= tail_start {
        let keep = nus(size - tail_start);
        op.cc_delta -= tail_len - n64(keep);
        f.tail.truncate(keep);
        f.size = size;
        return Ok(());
    }
    let old = rows_from(tx, f.id, size).await?;
    let first = old
        .first()
        .ok_or_else(|| Error::Internal("no line at the cut".into()))?;
    let new_tail = if first.start_off < size {
        let mut b = bytes_of(tx, op, std::slice::from_ref(first)).await?;
        b.truncate(nus(size - first.start_off));
        b
    } else {
        Vec::new()
    };
    delete_rows(tx, f.id, first.ord, None).await?;
    op.cc_delta -= old.iter().map(|r| n64(r.stub.len())).sum::<i64>() + tail_len;
    op.cc_delta += n64(new_tail.len());
    if op.conv.is_main(&f.path) {
        let removed: Vec<limbo::Entry> = old.iter().filter_map(|r| Some((r.sha, r.event_seq?))).collect();
        limbo::put_truncated(tx, op.conv.id, f.id, &removed).await?;
    }
    f.tail = new_tail;
    f.size = size;
    Ok(())
}

/// The whole file's bytes (a kind change on rename, rule 0's compare, tests).
///
/// # Errors
/// As [`bytes_of`].
pub async fn read_all(tx: &Tx<'_>, op: &mut Op<'_>, f: &File) -> Result<Vec<u8>> {
    let rows = rows_from(tx, f.id, 0).await?;
    let mut b = bytes_of(tx, op, &rows).await?;
    b.extend_from_slice(&f.tail);
    Ok(b)
}

/// Bytes `[from, to)` of the file (`to` <= its size).
///
/// # Errors
/// As [`bytes_of`].
pub async fn read_range(tx: &Tx<'_>, op: &mut Op<'_>, f: &File, from: i64, to: i64) -> Result<Vec<u8>> {
    let tail_start = f.size - n64(f.tail.len());
    let (base, mut out) = if from < tail_start {
        let rows: Vec<Row> = rows_from(tx, f.id, from)
            .await?
            .into_iter()
            .take_while(|r| r.start_off < to)
            .collect();
        let base = rows.first().map_or(tail_start, |r| r.start_off);
        (base, bytes_of(tx, op, &rows).await?)
    } else {
        (tail_start, Vec::new())
    };
    if to > tail_start {
        out.extend_from_slice(&f.tail);
    }
    let lo = nus(from - base).min(out.len());
    let hi = nus(to - base).min(out.len());
    Ok(out[lo..hi].to_vec())
}

/// Stores `bytes` as the whole content of an empty lines file (a byte file renamed to a lines name).
///
/// # Errors
/// As [`write`].
pub async fn fill(tx: &Tx<'_>, op: &mut Op<'_>, f: &mut File, bytes: &[u8]) -> Result<()> {
    f.size = 0;
    f.tail.clear();
    write(tx, op, f, 0, bytes).await
}

/// `(sha, event seq)` of the file's lines with events, in order.
async fn event_lines(tx: &Tx<'_>, file: i64) -> Result<Vec<limbo::Entry>> {
    let rows = tx
        .query(
            "select sha256, event_seq from cc_lines where file_id = $1 and event_seq is not null order by ord",
            &[&file],
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

/// The main transcript renamed away (a relocation, a set-aside): its lines keep their events and enter `moved` limbo
/// (6.2).
///
/// # Errors
/// A statement error.
pub async fn moved_away(tx: &Tx<'_>, op: &Op<'_>, file: i64) -> Result<()> {
    let lines = event_lines(tx, file).await?;
    limbo::put_moved(tx, op.conv.id, &lines).await
}

/// A lines file that now sits at the main path (renamed onto a free main path, or the main path set): its lines with
/// events are back (their `moved` limbo goes), its lines without events are classified in file order.
///
/// # Errors
/// A statement error.
pub async fn promote(tx: &Tx<'_>, op: &mut Op<'_>, file: i64) -> Result<()> {
    let back: Vec<i64> = event_lines(tx, file).await?.into_iter().map(|e| e.1).collect();
    limbo::drop_moved(tx, op.conv.id, &back).await?;
    let rows = tx
        .query(
            &format!("select {ROW_COLS} from cc_lines where file_id = $1 and not classified order by ord"),
            &[&file],
        )
        .await?;
    let saved = op.turn;
    for r in &rows {
        let row = row_of(r)?;
        let shas: Vec<Sha> = row.spans.iter().map(|s| s.1).collect();
        let meta = blobs::meta(tx, &shas).await?;
        op.turn = None;
        if let Some(t) = row.turn_id {
            op.credit(tx, Some(t)).await?;
        }
        // a line a set-aside took off the main path comes back with its event (6.2)
        let seq = match limbo::take_moved(tx, op.conv.id, &row.sha).await? {
            Some(seq) => Some(seq),
            None => apply(tx, op, &row.stub, &row.spans, &meta, None).await?,
        };
        tx.execute(
            "update cc_lines set event_seq = $3, classified = true where file_id = $1 and ord = $2",
            &[&file, &row.ord, &seq],
        )
        .await?;
    }
    op.turn = saved;
    Ok(())
}

/// Sets the main transcript path (the supervisor's `running` report, magpie 8.5; the importer), promoting the lines
/// already written there in the same transaction (6.2). The conversation is locked by the caller.
///
/// # Errors
/// A statement error.
pub async fn set_main_transcript(tx: &Tx<'_>, conversation: Uuid, path: &str) -> Result<()> {
    let row = tx
        .query_one(
            "select person, main_transcript, active_turn, cc_bytes, title, name_source, model
             from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    let current: Option<String> = row.get(1);
    if current.as_deref() == Some(path) {
        return Ok(());
    }
    tx.execute(
        "update conversations set main_transcript = $2 where id = $1",
        &[&conversation, &path],
    )
    .await?;
    let Some(f) = super::load_file(tx, conversation, path).await? else {
        return Ok(());
    };
    if f.kind != super::Kind::Lines {
        return Ok(());
    }
    let pre = HashMap::new();
    let mut op = Op {
        conv: super::Conv {
            id: conversation,
            person: row.get(0),
            main: Some(path.to_owned()),
            active_turn: row.get(2),
            cc_bytes: row.get(3),
            title: row.get(4),
            name_source: row.get(5),
            model: row.get(6),
            migrating: false,
            lease_id: None,
            lease_sandbox: None,
        },
        cc_delta: 0,
        turn: None,
        blobs: blobs::Bytes::with(&pre),
        upserted: false,
    };
    promote(tx, &mut op, f.id).await
}

/// A lines file renamed over another lines file (compaction's rename-over, 6.6): the source's rows become the
/// target's content; source lines whose sha matches a target line with an event take that event; on the main path
/// the target's other events are retracted at once and the source's lines without events are classified.
///
/// # Errors
/// A statement error.
pub async fn rename_over(tx: &Tx<'_>, op: &mut Op<'_>, source: &File, target: &File) -> Result<()> {
    let target_main = op.conv.is_main(&target.path);
    let mut pool: HashMap<Sha, VecDeque<i64>> = HashMap::new();
    let mut targets = event_lines(tx, target.id).await?;
    targets.extend(limbo::take_all_truncated(tx, op.conv.id, target.id).await?);
    for (sha, seq) in &targets {
        pool.entry(*sha).or_default().push_back(*seq);
    }
    let src = tx
        .query(
            "select ord, sha256, event_seq from cc_lines where file_id = $1 order by ord",
            &[&source.id],
        )
        .await?;
    let mut taken = std::collections::HashSet::new();
    for r in &src {
        let ord: i64 = r.get(0);
        let sha: Vec<u8> = r.get(1);
        let Ok(sha) = Sha::try_from(sha.as_slice()) else {
            continue;
        };
        let has: Option<i64> = r.get(2);
        let Some(q) = pool.get_mut(&sha) else {
            continue;
        };
        match has {
            Some(seq) => {
                if let Some(i) = q.iter().position(|s| *s == seq) {
                    q.remove(i);
                    taken.insert(seq);
                }
            }
            None => {
                if let Some(seq) = q.pop_front() {
                    taken.insert(seq);
                    tx.execute(
                        "update cc_lines set event_seq = $3, classified = true where file_id = $1 and ord = $2",
                        &[&source.id, &ord, &seq],
                    )
                    .await?;
                }
            }
        }
    }
    if target_main {
        let dropped: Vec<i64> = targets.iter().map(|t| t.1).filter(|s| !taken.contains(s)).collect();
        limbo::retract(tx, op.conv.id, op.turn_id(), dropped).await?;
    }
    Ok(())
}
