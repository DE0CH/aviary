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
//! Classified lines removed by a truncate go to limbo (6.7), events or not, so a line written again is never
//! classified a second time; lines an overwrite or a rename-over drops are retracted at once. Only the main
//! transcript has visible events, so only its lines are put in limbo or retracted.
//!
//! A line once classified as the echo of a person's prompt is also remembered by its sha256 for the conversation's
//! life (`cc_echoes`, and by its uuid in `cc_echo_uuids`), so writing it again after its limbo was flushed is internal
//! again (6.5), with the same bytes or not.
//!
//! Every operation works on the lines it touches in bounded batches: a read seeks with `cc_lines_off` and takes
//! [`limits::CC_READ_BATCH`] lines at a time, a truncate removes its suffix (into limbo) in one statement, new lines
//! are inserted in batches with their limbo matches fetched and deleted in one statement each, and a promotion walks
//! the file in batches, and a rename-over matches the files in one set-based statement (6.6: never the whole suffix in
//! memory, no statement per line beyond the events themselves).

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

/// Up to `limit` lines of `file` (every one with `None`) from the first one that ends after `off`, in file order: a
/// seek on `cc_lines_off`, then that index in offset order (6.11).
///
/// # Errors
/// When a statement fails.
pub async fn rows_from(db: &impl crate::db::Db, file: i64, off: i64, limit: Option<i64>) -> Result<Vec<Row>> {
    let rows = db
        .query(
            &format!(
                "select {ROW_COLS} from cc_lines
                  where file_id = $1 and start_off >= coalesce(
                    (select max(start_off) from cc_lines where file_id = $1 and start_off <= $2), 0)
                    and start_off + len > $2
                  order by start_off
                  limit $3"
            ),
            &[&file, &off, &limit],
        )
        .await?;
    rows.iter().map(row_of).collect()
}

/// The lines of `file` holding a byte of `[from, to)`, in file order.
///
/// # Errors
/// When a statement fails.
pub async fn rows_between(db: &impl crate::db::Db, file: i64, from: i64, to: i64) -> Result<Vec<Row>> {
    let rows = db
        .query(
            &format!(
                "select {ROW_COLS} from cc_lines
                  where file_id = $1 and start_off >= coalesce(
                    (select max(start_off) from cc_lines where file_id = $1 and start_off <= $2), 0)
                    and start_off + len > $2 and start_off < $3
                  order by start_off"
            ),
            &[&file, &from, &to],
        )
        .await?;
    rows.iter().map(row_of).collect()
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
    /// The file's `truncated` limbo the new lines may take back.
    trunc: limbo::Candidates,
    /// The scope's `moved` limbo the new lines may take back (at the main path).
    moved: limbo::Candidates,
    /// New lines not inserted yet.
    rows: Vec<Row>,
    /// Events a new line of this operation rewrote (its `meta.replaces`): old lines of the rewritten suffix or lines
    /// in the file's truncate limbo. They are retracted at the operation's end whatever later lines match, so the
    /// projection's takeover of their rows (magpie 7.3 rule 4) is never undone by a take-back.
    superseded: Vec<i64>,
}

/// New lines are inserted this many at a time.
const INSERT_BATCH: usize = 512;

/// Inserts the writer's buffered lines in one statement.
async fn flush_rows(tx: &Tx<'_>, w: &mut Writer) -> Result<()> {
    if w.rows.is_empty() {
        return Ok(());
    }
    let ords: Vec<i64> = w.rows.iter().map(|r| r.ord).collect();
    let offs: Vec<i64> = w.rows.iter().map(|r| r.start_off).collect();
    let lens = w
        .rows
        .iter()
        .map(|r| i32::try_from(r.len).map_err(|_| Error::Internal("line length".into())))
        .collect::<Result<Vec<i32>>>()?;
    let shas: Vec<&[u8]> = w.rows.iter().map(|r| r.sha.as_slice()).collect();
    let stubs: Vec<&[u8]> = w.rows.iter().map(|r| r.stub.as_slice()).collect();
    let spans: Vec<Option<Value>> = w.rows.iter().map(|r| spans_json(&r.spans)).collect();
    let events: Vec<Option<i64>> = w.rows.iter().map(|r| r.event_seq).collect();
    let turns: Vec<Option<Uuid>> = w.rows.iter().map(|r| r.turn_id).collect();
    let classified: Vec<bool> = w.rows.iter().map(|r| r.classified).collect();
    tx.execute(
        "insert into cc_lines (file_id, ord, start_off, len, sha256, stub, spans, event_seq, turn_id, classified)
         select $1, * from unnest($2::bigint[], $3::bigint[], $4::integer[], $5::bytea[], $6::bytea[], $7::jsonb[],
                                  $8::bigint[], $9::uuid[], $10::boolean[])",
        &[
            &w.file,
            &ords,
            &offs,
            &lens,
            &shas,
            &stubs,
            &spans,
            &events,
            &turns,
            &classified,
        ],
    )
    .await?;
    w.rows.clear();
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

/// Whether a line with `sha` (or with `uuid`) was once classified as the echo of a person's prompt in
/// `conversation` (6.5): the same bytes, or the same message written again with other bytes.
async fn was_echo(tx: &Tx<'_>, conversation: Uuid, sha: &Sha, uuid: Option<&str>) -> Result<bool> {
    Ok(tx
        .query_opt(
            "select 1 from cc_echoes where scope = $1 and sha256 = $2
             union all select 1 from cc_echo_uuids where scope = $1 and uuid = $3
             limit 1",
            &[&conversation, &sha.as_slice(), &uuid],
        )
        .await?
        .is_some())
}

/// Whether a line with `uuid` was retracted in `conversation` (it was in the transcript before). A line without a
/// uuid (no line Claude Code writes, but a damaged one) is matched by its event data instead: a retracted `cc.user`
/// event with the same data is the same line written again. That lookup scans the conversation's retracted user
/// events, which is fine for a case this rare.
async fn was_retracted(tx: &Tx<'_>, conversation: Uuid, uuid: Option<&str>, data: Option<&Value>) -> Result<bool> {
    if let Some(uuid) = uuid {
        return Ok(limbo::replaced(tx, conversation, uuid).await?.is_some());
    }
    let Some(data) = data else {
        return Ok(false);
    };
    // a uuid-less line: the retracted `cc.user` events' data keys ([`limbo::retract`]), by the same index
    Ok(tx
        .query_one(
            "select exists (select 1 from cc_retracted
                             where conversation_id = $1 and uuid = 'data:' || md5($2::jsonb::text))",
            &[&conversation, data],
        )
        .await?
        .get(0))
}

/// Records a line with `sha` and `uuid` as the echo of a person's prompt in `conversation` (6.5).
async fn remember_echo(tx: &Tx<'_>, conversation: Uuid, sha: &Sha, uuid: Option<&str>) -> Result<()> {
    tx.execute(
        "insert into cc_echoes (scope, sha256) values ($1, $2) on conflict do nothing",
        &[&conversation, &sha.as_slice()],
    )
    .await?;
    if let Some(uuid) = uuid {
        tx.execute(
            "insert into cc_echo_uuids (scope, uuid) values ($1, $2) on conflict do nothing",
            &[&conversation, &uuid],
        )
        .await?;
    }
    Ok(())
}

/// The uuid of a line's event data.
fn uuid_of(data: Option<&Value>) -> Option<&str> {
    data.and_then(|d| d.get("uuid")).and_then(Value::as_str)
}

/// [`class_of`], with a resumed parked turn's answer line made the event of its questions' result ([`answer_of`]).
async fn class_in(tx: &Tx<'_>, op: &mut Op<'_>, sha: &Sha, data: Option<&Value>) -> Result<Class> {
    match class_of(tx, op, sha, data).await? {
        Class::Answer(e) => answer_of(tx, op, *e).await,
        c => Ok(c),
    }
}

/// A resumed parked turn's answer line (DECISIONS 17) as the result of the calls the person answered
/// (`turns.answer_tool_uses`, cleared: the next prompt of the turn is an ordinary line again).
async fn answer_of(tx: &Tx<'_>, op: &mut Op<'_>, mut e: classify::Event) -> Result<Class> {
    let Some(t) = op.turn.as_mut() else {
        return Ok(Class::Event(Box::new(e)));
    };
    let ids: Vec<String> = tx
        .query_one(
            "update turns t set answer_tool_uses = null from (select answer_tool_uses from turns where id = $1) old
             where t.id = $1 returning coalesce(old.answer_tool_uses, '{}')",
            &[&t.id],
        )
        .await?
        .get(0);
    t.answering = false;
    e.meta.results = Some(
        ids.iter()
            .map(|id| magpie_api::events::ToolResultLink {
                tool_use_id: id.clone(),
                tool_use_seq: None,
            })
            .collect(),
    );
    e.results = ids;
    Ok(Class::Event(Box::new(e)))
}

/// The class of a main-transcript line with sha256 `sha` and event data `data`.
///
/// A line once classified as the echo of a person's prompt is remembered by its sha256 and its uuid for the
/// conversation's life (`cc_echoes`, `cc_echo_uuids`), so writing it again (the same bytes, or the same message with
/// other bytes) after its limbo was flushed (a truncate whose turn ended, a rewrite retried past
/// the 10 s debounce) is internal again: it neither takes a later turn's echo nor becomes a prompt event. A line with
/// the uuid of a retracted line is a line written again, never the echo of the turn now running (6.5).
async fn class_of(tx: &Tx<'_>, op: &Op<'_>, sha: &Sha, data: Option<&Value>) -> Result<Class> {
    let pending = op.turn.is_some_and(|t| t.echo_pending);
    let answering = op.turn.is_some_and(|t| t.answering);
    let class = classify::classify(
        data,
        Context {
            echo_pending: pending,
            answering,
        },
    );
    let prompt_like = match &class {
        Class::Echo => true,
        Class::Event(e) => e.is_prompt,
        _ => false,
    };
    if prompt_like && was_echo(tx, op.conv.id, sha, uuid_of(data)).await? {
        return Ok(Class::Internal);
    }
    if class == Class::Echo && was_retracted(tx, op.conv.id, uuid_of(data), data).await? {
        return Ok(classify::classify(
            data,
            Context {
                echo_pending: false,
                answering,
            },
        ));
    }
    Ok(class)
}

/// Makes the event (or the internal effect) of a main-transcript line with sha256 `sha`; its event seq.
async fn apply(
    tx: &Tx<'_>,
    op: &mut Op<'_>,
    sha: &Sha,
    stub: &[u8],
    spans: &[(usize, Sha)],
    meta: &HashMap<Sha, blobs::Meta>,
    w: Option<&mut Writer>,
) -> Result<Option<i64>> {
    let data = event_data(stub, spans, meta);
    match class_in(tx, op, sha, data.as_ref()).await? {
        Class::Internal | Class::Answer(_) => Ok(None),
        Class::Echo => {
            if let Some(t) = op.turn.as_mut() {
                tx.execute("update turns set echo_matched = true where id = $1", &[&t.id])
                    .await?;
                t.echo_pending = false;
            }
            remember_echo(tx, op.conv.id, sha, uuid_of(data.as_ref())).await?;
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
                let r = replaces(tx, op, u, w.as_deref()).await?;
                if let (Some((seq, from)), Some(w)) = (r, w)
                    && from != From::Retracted
                {
                    if from == From::Limbo {
                        w.trunc.supersede(tx, op.conv.id, seq).await?;
                    }
                    w.superseded.push(seq);
                }
                m.replaces = r.and_then(|(s, _)| u64::try_from(s).ok());
            }
            let at = OffsetDateTime::now_utc();
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
                at,
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

/// Where the line a new line replaces is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum From {
    /// Retracted already.
    Retracted,
    /// An old line of the suffix this operation rewrites.
    Pending,
    /// The file's truncate limbo.
    Limbo,
}

/// The event a new line with `uuid` replaces (6.7): the most recent of a retracted line's, one this operation is
/// about to retract (an old line of the rewritten suffix not already rewritten) and one that waits in the file's
/// limbo. The most recent, because that is the version the transcript showed last (truncate, rewrite, truncate,
/// rewrite again: the second rewrite replaces the first, not the original).
async fn replaces(tx: &Tx<'_>, op: &Op<'_>, uuid: &str, w: Option<&Writer>) -> Result<Option<(i64, From)>> {
    let mut best = limbo::replaced(tx, op.conv.id, uuid)
        .await?
        .map(|s| (s, From::Retracted));
    let Some(w) = w else {
        return Ok(best);
    };
    let mut consider = |seq: Option<i64>, from: From| {
        if let Some(seq) = seq
            && best.is_none_or(|(b, _)| seq > b)
        {
            best = Some((seq, from));
        }
    };
    let pending: Vec<i64> = w
        .pending
        .iter()
        .copied()
        .filter(|s| !w.superseded.contains(s))
        .collect();
    if !pending.is_empty() {
        let row = tx
            .query_one(
                "select max(seq) from events where conversation_id = $1 and seq = any($2) and data->>'uuid' = $3",
                &[&op.conv.id, &pending, &uuid],
            )
            .await?;
        consider(row.get(0), From::Pending);
    }
    if w.limbo_truncated {
        // limbo entries another new line of this operation took back are not replaced
        let row = tx
            .query_one(
                "select max(e.seq) from cc_limbo l join events e on e.conversation_id = l.scope and e.seq = l.event_seq
                  where l.scope = $1 and l.reason = 'truncated' and l.file_id = $2 and e.data->>'uuid' = $3
                    and l.id <> all($4)",
                &[&op.conv.id, &w.file, &uuid, &w.trunc.used],
            )
            .await?;
        consider(row.get(0), From::Limbo);
    }
    Ok(best)
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

/// Stores one new complete line at the writer's position: matched (keeping its stub, spans, event, turn and
/// classification) or extracted, its blobs staged, then taken back from limbo (its event, turn and classification)
/// or, on the main transcript, classified.
async fn put_line(tx: &Tx<'_>, op: &mut Op<'_>, w: &mut Writer, line: &[u8], sha: Sha, m: &mut Matcher) -> Result<()> {
    let matched = m.take(&sha).cloned();
    let row = if let Some(old) = matched {
        if old.event_seq.is_some_and(|s| w.superseded.contains(&s)) {
            // the old line's event was rewritten earlier in this operation and is retracted at its end: the line
            // makes a new event
            let shas: Vec<Sha> = old.spans.iter().map(|s| s.1).collect();
            let meta = blobs::meta(tx, &shas).await?;
            let event_seq = apply(tx, op, &old.sha, &old.stub, &old.spans, &meta, Some(&mut *w)).await?;
            Row {
                ord: w.ord,
                start_off: w.off,
                event_seq,
                turn_id: op.turn_id(),
                classified: true,
                ..old
            }
        } else {
            Row {
                ord: w.ord,
                start_off: w.off,
                ..old
            }
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
        let back = w.trunc.take(&sha).or_else(|| w.moved.take(&sha));
        let (event_seq, turn_id, classified) = match back {
            // a line set aside comes back as it was: its event, its own turn (6.3), never classified again (6.5)
            Some(e) => (e.event_seq, e.turn_id, true),
            None if w.main => (
                apply(tx, op, &sha, &x.stub, &x.spans, &meta, Some(&mut *w)).await?,
                op.turn_id(),
                true,
            ),
            None => (None, op.turn_id(), false),
        };
        Row {
            ord: w.ord,
            start_off: w.off,
            len: n64(line.len()),
            sha,
            stub: x.stub,
            spans: x.spans,
            event_seq,
            turn_id,
            classified,
        }
    };
    op.cc_delta += n64(row.stub.len());
    w.ord += 1;
    w.off += row.len;
    w.rows.push(row);
    if w.rows.len() >= INSERT_BATCH {
        flush_rows(tx, w).await?;
    }
    Ok(())
}

/// Stores the complete lines of `bytes` at the writer's position; the bytes after the last `\n` (the new tail).
async fn put_lines(tx: &Tx<'_>, op: &mut Op<'_>, w: &mut Writer, bytes: &[u8], m: &mut Matcher) -> Result<Vec<u8>> {
    let mut lines = Vec::new();
    let mut at = 0;
    while let Some(nl) = bytes[at..].iter().position(|&b| b == b'\n') {
        let end = at + nl + 1;
        if n64(end - at) > limits::CC_LINE_MAX {
            return Err(too_large("a line"));
        }
        lines.push(&bytes[at..end]);
        at = end;
    }
    let rest = bytes[at..].to_vec();
    if n64(rest.len()) > limits::CC_LINE_MAX {
        return Err(too_large("a line being written"));
    }
    let shas: Vec<Sha> = lines.iter().map(|l| extract::sha(l)).collect();
    // limbo is fetched once for every new line (a superset of the lines the old suffix does not take)
    if w.limbo_truncated && !shas.is_empty() {
        w.trunc = limbo::Candidates::truncated(tx, op.conv.id, w.file, &shas).await?;
    }
    if w.limbo_moved && !shas.is_empty() {
        w.moved = limbo::Candidates::moved(tx, op.conv.id, &shas).await?;
    }
    for (line, sha) in lines.iter().zip(shas) {
        put_line(tx, op, w, line, sha, m).await?;
    }
    flush_rows(tx, w).await?;
    w.trunc.commit(tx).await?;
    w.moved.commit(tx).await?;
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
        trunc: limbo::Candidates::default(),
        moved: limbo::Candidates::default(),
        rows: Vec::new(),
        superseded: Vec::new(),
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
        if w.main {
            limbo::retract(tx, op.conv.id, op.turn_id(), w.superseded).await?;
        }
        return Ok(());
    }
    // the lines the write touches (and the one it ends at); the lines after them keep their bytes
    let end = offset + n64(data.len());
    let region = rows_between(tx, f.id, offset, end + 1).await?;
    let (Some(first_row), Some(last_row)) = (region.first(), region.last()) else {
        return Err(Error::Internal("no line at the write's offset".into()));
    };
    let region_start = first_row.start_off;
    let (first_ord, last_ord) = (first_row.ord, last_row.ord);
    let later = tx
        .query_one(
            "select min(ord), max(ord) from cc_lines where file_id = $1 and start_off > $2",
            &[&f.id, &last_row.start_off],
        )
        .await?;
    let later: Option<(i64, i64)> = later.get::<_, Option<i64>>(0).zip(later.get::<_, Option<i64>>(1));
    let with_tail = later.is_none();
    let region = region.as_slice();
    let mut bytes = bytes_of(tx, op, region).await?;
    if with_tail {
        bytes.extend_from_slice(&f.tail);
    }
    overwrite(&mut bytes, nus(offset - region_start), data);
    delete_rows(tx, f.id, first_ord, Some(last_ord)).await?;
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
    if let Some((first, last)) = later {
        // the unchanged lines after the region keep their bytes and offsets and follow the new lines in order
        tx.execute(
            "update cc_lines set ord = ord - $2 + $3 where file_id = $1 and ord between $2 and $4",
            &[&f.id, &first, &w.ord, &last],
        )
        .await?;
        w.ord += last - first + 1;
    }
    f.next_ord = w.ord;
    f.size = f.size.max(end);
    if w.main {
        let mut gone = m.unused_events();
        gone.extend(&w.superseded);
        limbo::retract(tx, op.conv.id, op.turn_id(), gone).await?;
    }
    Ok(())
}

/// A truncate of a lines file to `size` (<= its size). On the main transcript the removed classified lines go to
/// limbo, in the same statement that removes them, with the truncate's turn (6.7).
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
    let first = rows_from(tx, f.id, size, Some(1))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Internal("no line at the cut".into()))?;
    let new_tail = if first.start_off < size {
        let mut b = bytes_of(tx, op, std::slice::from_ref(&first)).await?;
        b.truncate(nus(size - first.start_off));
        b
    } else {
        Vec::new()
    };
    let main = op.conv.is_main(&f.path);
    let removed: i64 = tx
        .query_one(
            "with d as (delete from cc_lines where file_id = $1 and ord >= $2
                        returning ord, sha256, event_seq, turn_id, classified, octet_length(stub) as n),
                  l as (insert into cc_limbo (scope, file_id, reason, sha256, event_seq, turn_id, op_turn)
                        select $3, $1, 'truncated', sha256, event_seq, turn_id, $5 from d
                         where $4::boolean and classified order by ord
                        on conflict do nothing)
             select coalesce(sum(n), 0)::bigint from d",
            &[&f.id, &first.ord, &op.conv.id, &main, &op.turn_id()],
        )
        .await?
        .get(0);
    op.cc_delta -= removed + tail_len;
    op.cc_delta += n64(new_tail.len());
    f.tail = new_tail;
    f.size = size;
    Ok(())
}

/// The whole file's bytes (a kind change on rename, tests).
///
/// # Errors
/// As [`bytes_of`].
pub async fn read_all(tx: &Tx<'_>, op: &mut Op<'_>, f: &File) -> Result<Vec<u8>> {
    let rows = rows_from(tx, f.id, 0, None).await?;
    let mut b = bytes_of(tx, op, &rows).await?;
    b.extend_from_slice(&f.tail);
    Ok(b)
}

/// Whether bytes `[from, from + data.len())` of the file (inside it) are exactly `data` (rule 0, 6.6). A line the range
/// covers whole is compared by its sha256 and length, so a resend after a checkpoint needs no blob bytes and answers
/// while the bucket is down (6.13); only a line the range cuts is reconstructed (its blobs, when it has any, staged or
/// fetched).
///
/// # Errors
/// As [`bytes_of`] for a line the range cuts.
pub async fn holds(tx: &Tx<'_>, op: &mut Op<'_>, f: &File, from: i64, data: &[u8]) -> Result<bool> {
    let to = from + n64(data.len());
    let tail_start = f.size - n64(f.tail.len());
    if from < tail_start {
        for r in rows_between(tx, f.id, from, to.min(tail_start)).await? {
            let end = r.start_off + r.len;
            let (lo, hi) = (r.start_off.max(from), end.min(to));
            let piece = &data[nus(lo - from)..nus(hi - from)];
            let same = if lo == r.start_off && hi == end {
                extract::sha(piece) == r.sha
            } else {
                let line = bytes_of(tx, op, std::slice::from_ref(&r)).await?;
                line[nus(lo - r.start_off)..nus(hi - r.start_off)] == *piece
            };
            if !same {
                return Ok(false);
            }
        }
    }
    if to > tail_start {
        let lo = from.max(tail_start);
        if f.tail[nus(lo - tail_start)..nus(to - tail_start)] != data[nus(lo - from)..] {
            return Ok(false);
        }
    }
    Ok(true)
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

/// The main transcript renamed away (a relocation, a set-aside): its classified lines keep their events and enter
/// `moved` limbo (6.2).
///
/// # Errors
/// A statement error.
pub async fn moved_away(tx: &Tx<'_>, op: &Op<'_>, file: i64) -> Result<()> {
    limbo::put_moved(tx, op.conv.id, file).await
}

/// Lines that change their event, turn and classification (`(ord, event, turn)`), set in one statement.
async fn set_classified(tx: &Tx<'_>, file: i64, ups: &[(i64, Option<i64>, Option<Uuid>)]) -> Result<()> {
    if ups.is_empty() {
        return Ok(());
    }
    let ords: Vec<i64> = ups.iter().map(|u| u.0).collect();
    let seqs: Vec<Option<i64>> = ups.iter().map(|u| u.1).collect();
    let turns: Vec<Option<Uuid>> = ups.iter().map(|u| u.2).collect();
    tx.execute(
        "update cc_lines c set event_seq = u.seq, turn_id = coalesce(u.turn, c.turn_id), classified = true
           from unnest($2::bigint[], $3::bigint[], $4::uuid[]) as u(ord, seq, turn)
          where c.file_id = $1 and c.ord = u.ord",
        &[&file, &ords, &seqs, &turns],
    )
    .await?;
    Ok(())
}

/// A lines file that now sits at the main path (renamed onto a free main path, or the main path set): its classified
/// lines are back (their `moved` limbo goes), the others are taken back from `moved` limbo or classified, in file
/// order and in batches, each credited to its own turn (6.3).
///
/// # Errors
/// A statement error.
pub async fn promote(tx: &Tx<'_>, op: &mut Op<'_>, file: i64) -> Result<()> {
    limbo::drop_moved(tx, op.conv.id, file).await?;
    let saved = op.turn;
    let mut credited: Option<Option<Uuid>> = None;
    let mut after = -1i64;
    loop {
        let rows = tx
            .query(
                &format!(
                    "select {ROW_COLS} from cc_lines where file_id = $1 and not classified and ord > $2
                      order by ord limit $3"
                ),
                &[&file, &after, &limits::CC_READ_BATCH],
            )
            .await?
            .iter()
            .map(row_of)
            .collect::<Result<Vec<Row>>>()?;
        let Some(last) = rows.last() else {
            break;
        };
        after = last.ord;
        let shas: Vec<Sha> = rows.iter().map(|r| r.sha).collect();
        let mut moved = limbo::Candidates::moved(tx, op.conv.id, &shas).await?;
        let mut blob_shas: Vec<Sha> = rows.iter().flat_map(|r| r.spans.iter().map(|s| s.1)).collect();
        blob_shas.sort_unstable();
        blob_shas.dedup();
        let meta = blobs::meta(tx, &blob_shas).await?;
        let mut ups = Vec::with_capacity(rows.len());
        for row in &rows {
            // a line a set-aside took off the main path comes back with its event (6.2)
            if let Some(e) = moved.take(&row.sha) {
                ups.push((row.ord, e.event_seq, e.turn_id));
                continue;
            }
            if credited != Some(row.turn_id) {
                op.turn = None;
                if let Some(t) = row.turn_id {
                    op.credit(tx, Some(t)).await?;
                }
                credited = Some(row.turn_id);
            }
            let seq = apply(tx, op, &row.sha, &row.stub, &row.spans, &meta, None).await?;
            ups.push((row.ord, seq, None));
        }
        moved.commit(tx).await?;
        set_classified(tx, file, &ups).await?;
    }
    op.turn = saved;
    Ok(())
}

/// Sets the main transcript path (the supervisor's `running` report, magpie 8.5), promoting the lines
/// already written there in the same transaction (6.2). The conversation is locked by the caller and its
/// `provider_session_id` is set: `path` must be `projects/-workspace/<provider_session_id>.jsonl`. When the main path
/// changes, the file at the old one becomes an ordinary lines file, as a rename away from the main path does: its
/// lines keep their events and enter `moved` limbo, so a file at the new path with the same lines makes no new events.
///
/// # Errors
/// 400 `bad_request` when `path` is not the conversation's provider session's transcript; a statement error.
pub async fn set_main_transcript(tx: &Tx<'_>, conversation: Uuid, path: &str) -> Result<()> {
    let row = tx
        .query_one(
            "select person, main_transcript, active_turn, cc_bytes, title, name_source, model, provider_session_id
             from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    let session: Option<String> = row.get(7);
    let valid = session.as_deref().is_some_and(|sid| {
        !sid.is_empty()
            && sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && path == format!("projects/-workspace/{sid}.jsonl")
    });
    if !valid {
        return Err(Error::bad_request(
            "transcriptPath is not projects/-workspace/<providerSessionId>.jsonl",
        ));
    }
    let current: Option<String> = row.get(1);
    if current.as_deref() == Some(path) {
        return Ok(());
    }
    tx.execute(
        "update conversations set main_transcript = $2 where id = $1",
        &[&conversation, &path],
    )
    .await?;
    if let Some(old) = &current
        && let Some(f) = super::load_file(tx, conversation, old).await?
        && f.kind == super::Kind::Lines
    {
        limbo::put_moved(tx, conversation, f.id).await?;
    }
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
/// target's content. A source line not classified yet whose sha matches a classified target line (or the target's
/// `truncated` limbo) takes that line's event, turn and classification, so it is not classified again; on the main
/// path the target's other events are retracted at once and the source's remaining lines are classified (the
/// promotion that follows).
///
/// The matching is one set-based statement, so its memory is Postgres's (sorts spill to disk), never the file's lines
/// in magpie: the target's classified lines (in file order) then its `truncated` limbo (oldest first) form the pool;
/// entries whose event a source line already has are kept by that line; the k-th unclassified source line with a
/// sha256 takes the k-th remaining pool entry with that sha256. What it returns is the pool's events no source line
/// took (the ones to retract).
///
/// # Errors
/// A statement error.
pub async fn rename_over(tx: &Tx<'_>, op: &mut Op<'_>, source: &File, target: &File) -> Result<()> {
    let target_main = op.conv.is_main(&target.path);
    let dropped: Vec<i64> = tx
        .query(
            "with lim as (delete from cc_limbo where scope = $3 and reason = 'truncated' and file_id = $2
                          returning id, sha256, event_seq, turn_id),
                  pool as (select sha256, event_seq, turn_id, 0 as src, ord as k from cc_lines
                            where file_id = $2 and classified
                           union all
                           select sha256, event_seq, turn_id, 1, id from lim),
                  held as (select event_seq from cc_lines where file_id = $1 and event_seq is not null),
                  free as (select sha256, event_seq, turn_id,
                                  row_number() over (partition by sha256 order by src, k) as rn
                             from pool p
                            where p.event_seq is null or not exists (select 1 from held h where h.event_seq = p.event_seq)),
                  want as (select ord, sha256, row_number() over (partition by sha256 order by ord) as rn
                             from cc_lines where file_id = $1 and not classified),
                  upd as (update cc_lines c set event_seq = f.event_seq, turn_id = coalesce(f.turn_id, c.turn_id),
                                 classified = true
                            from want w join free f on f.sha256 = w.sha256 and f.rn = w.rn
                           where c.file_id = $1 and c.ord = w.ord
                          returning c.event_seq)
             select f.event_seq from free f
              where f.event_seq is not null
                and not exists (select 1 from upd u where u.event_seq = f.event_seq)",
            &[&source.id, &target.id, &op.conv.id],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    if target_main {
        limbo::retract(tx, op.conv.id, op.turn_id(), dropped).await?;
    }
    Ok(())
}
