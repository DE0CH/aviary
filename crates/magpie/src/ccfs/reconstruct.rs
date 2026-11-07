//! The read path (magpie 6.11): `GET /v1/sandbox/cc/tree` and `GET /v1/sandbox/cc/file`.
//!
//! A lines file streams: lines in batches in file order (found through `cc_lines_off`), each reconstructed from its
//! stub and blobs (staged bytes, else the bucket's: 16 at a time, the next batch's fetched while the current one is
//! sent, no database connection held while the bucket answers), its sha256 checked on every line in a debug build and on one in
//! [`limits::CC_SHA_SAMPLE`] in a release build. The first batch is made before the answer starts, so a mismatch or
//! a missing blob there answers 500 / 503; a later one ends the body early (the supervisor's read fails rather than
//! seeing wrong bytes). The file's version is compared at the end: a file changed while it streamed ends the body
//! early too; a read that fits in one batch compares it before it answers, and reads again when a write landed
//! between its statements.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use magpie_api::sandbox::{CcEntry, CcFileQuery, CcTree};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::extract::{self, Sha};
use super::lines::{Row, rows_from};
use super::{Caller, File, Kind, check_path, load_file, n64, nus, pages};
use crate::app::Magpie;
use crate::blobs;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// Lines read so far by this process (the sample's counter).
static READ_LINES: AtomicU64 = AtomicU64::new(0);

fn check_sha(line: &[u8], r: &Row) -> Result<()> {
    let n = READ_LINES.fetch_add(1, Ordering::Relaxed);
    if (cfg!(debug_assertions) || n.is_multiple_of(limits::CC_SHA_SAMPLE)) && extract::sha(line) != r.sha {
        tracing::error!(ord = r.ord, sha = %extract::hex(&r.sha), "reconstruction sha mismatch");
        return Err(Error::Internal("reconstruction sha mismatch".into()));
    }
    Ok(())
}

/// 404 unless the conversation exists and is the sandbox person's.
async fn owned(db: &impl Db, caller: &Caller) -> Result<i64> {
    let row = db
        .query_opt(
            "select person, cc_bytes from conversations where id = $1",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let person: String = row.get(0);
    if person != caller.person {
        return Err(Error::not_found());
    }
    Ok(row.get(1))
}

/// `GET /v1/sandbox/cc/tree`.
///
/// # Errors
/// 404; a statement error.
pub async fn tree(app: &Magpie, caller: &Caller) -> Result<CcTree> {
    let conn = app.pool.get().await?;
    let cc_bytes = owned(&conn, caller).await?;
    let rows = conn
        .query(
            "select path, kind, size, mtime_ns, mode, version from cc_files where scope = $1 order by path",
            &[&caller.conversation],
        )
        .await?;
    let files = rows
        .iter()
        .map(|r| CcEntry {
            path: r.get(0),
            kind: Kind::of_db(r.get(1)).api(),
            size: u64::try_from(r.get::<_, i64>(2)).unwrap_or(0),
            mtime_ns: r.get(3),
            mode: u32::try_from(r.get::<_, i32>(4)).unwrap_or(0),
            version: u64::try_from(r.get::<_, i64>(5)).unwrap_or(0),
        })
        .collect();
    Ok(CcTree {
        files,
        cc_bytes: u64::try_from(cc_bytes).unwrap_or(0),
        quota_bytes: u64::try_from(limits::CC_QUOTA).unwrap_or(0),
    })
}

/// The bytes of blobs `shas`: staged, else from the bucket (no connection held while the bucket is called).
pub(crate) async fn blob_bytes(app: &Magpie, shas: &[Sha]) -> Result<HashMap<Sha, Bytes>> {
    let mut got: HashMap<Sha, Bytes> = blobs::staging::read(&app.pool.get().await?, shas)
        .await?
        .into_iter()
        .map(|(s, b)| (s, Bytes::from(b)))
        .collect();
    let missing: Vec<Sha> = shas.iter().filter(|s| !got.contains_key(*s)).copied().collect();
    if !missing.is_empty() {
        got.extend(blobs::fetch_stored(app, &missing).await?);
    }
    Ok(got)
}

/// One batch of a lines file from `off` (up to `to`): its lines, and where the next batch starts (`None`: this is
/// the last, which carries the tail).
#[derive(Debug)]
struct Batch {
    off: i64,
    rows: Vec<Row>,
    next: Option<i64>,
}

/// The lines of the batch from `off`.
async fn plan(db: &impl Db, f: &File, off: i64, to: i64) -> Result<Batch> {
    let tail_start = f.size - n64(f.tail.len());
    let rows: Vec<Row> = if off < tail_start {
        rows_from(db, f.id, off, Some(limits::CC_READ_BATCH))
            .await?
            .into_iter()
            .take_while(|r| r.start_off < to)
            .collect()
    } else {
        Vec::new()
    };
    let next = rows
        .last()
        .map(|r| r.start_off + r.len)
        .filter(|n| *n < tail_start && *n < to);
    Ok(Batch { off, rows, next })
}

/// A batch's bytes from its offset (cut to `to`): its lines reconstructed (their blobs staged, else fetched from the
/// bucket), and the tail when it is the last.
async fn render(app: &Magpie, f: &File, b: Batch, to: i64) -> Result<(Vec<u8>, Option<i64>)> {
    let tail_start = f.size - n64(f.tail.len());
    let Batch { off, rows, next } = b;
    let mut out = Vec::new();
    let mut base = off;
    if let Some(first) = rows.first() {
        base = first.start_off;
        let mut shas: Vec<Sha> = rows.iter().flat_map(|r| r.spans.iter().map(|s| s.1)).collect();
        shas.sort_unstable();
        shas.dedup();
        let bytes = if shas.is_empty() {
            HashMap::new()
        } else {
            blob_bytes(app, &shas).await?
        };
        for r in &rows {
            let line = extract::splice(&r.stub, &r.spans, |s| bytes.get(s).map(|b| &b[..])).map_err(Error::Internal)?;
            check_sha(&line, r)?;
            out.extend_from_slice(&line);
        }
        if next.is_some() {
            let lo = nus(off - base);
            return Ok((out.split_off(lo.min(out.len())), next));
        }
    }
    if to > tail_start {
        if out.is_empty() {
            base = tail_start;
        }
        out.extend_from_slice(&f.tail);
    }
    let lo = nus(off - base).min(out.len());
    let hi = nus(to - base).min(out.len());
    Ok((out[lo..hi].to_vec(), None))
}

/// `GET /v1/sandbox/cc/file`: the bytes (206 with a range).
///
/// # Errors
/// 400 `bad_path`, 404; 500 / 503 for the first batch.
pub async fn file(app: Arc<Magpie>, caller: &Caller, q: &CcFileQuery) -> Result<Response> {
    check_path(&q.path)?;
    let conn = app.pool.get().await?;
    owned(&conn, caller).await?;
    drop(conn);
    scope_file(app, caller.conversation, q).await
}

/// A file of any cc scope, a share snapshot's included, with no caller check: what [`file`] serves once the caller is
/// checked.
///
/// # Errors
/// 400 `bad_path`, 404; 500 / 503 for the first batch.
pub async fn scope_file(app: Arc<Magpie>, scope: Uuid, q: &CcFileQuery) -> Result<Response> {
    check_path(&q.path)?;
    let ranged = q.offset.is_some() || q.length.is_some();
    let status = if ranged {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    // the file's row and its lines (or pages) are separate statements: a write committed between them would mix two
    // versions, so a read that answers at once checks the version afterwards and reads again
    let mut attempt = 0;
    let (f, start, end, body) = loop {
        // a connection per step, none held while blobs come from the bucket (6.11)
        let conn = app.pool.get().await?;
        let f = load_file(&conn, scope, &q.path)
            .await?
            .filter(|f| f.kind != Kind::Dir)
            .ok_or_else(Error::not_found)?;
        let start = i64::try_from(q.offset.unwrap_or(0)).unwrap_or(i64::MAX).min(f.size);
        let end = q.length.map_or(f.size, |l| {
            start.saturating_add(i64::try_from(l).unwrap_or(i64::MAX)).min(f.size)
        });
        let (first, next) = if f.kind == Kind::Bytes {
            (pages::read(&conn, f.id, f.size, start, end).await?, None)
        } else if start >= end {
            (Vec::new(), None)
        } else {
            let b = plan(&conn, &f, start, end).await?;
            drop(conn);
            render(&app, &f, b, end).await?
        };
        if let Some(next) = next {
            // a streamed read compares the version at its end (`stream`)
            break (f, start, end, Err((first, next)));
        }
        let now = app
            .pool
            .get()
            .await?
            .query_opt("select version from cc_files where id = $1", &[&f.id])
            .await?
            .map(|r| r.get::<_, i64>(0));
        if now == Some(f.version) {
            break (f, start, end, Ok(first));
        }
        attempt += 1;
        if attempt >= limits::CC_READ_RETRIES {
            return Err(Error::code(
                magpie_api::errors::ErrorCode::Unavailable,
                "the file kept changing while it was read",
            ));
        }
    };
    let size = f.size;
    let body = match body {
        Ok(bytes) => Body::from(bytes),
        Err((first, next)) => {
            let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(4);
            let _ = tx.send(Ok(Bytes::from(first))).await;
            tokio::spawn(stream(app.clone(), f, next, end, tx));
            Body::from_stream(tokio_stream_of(rx))
        }
    };
    let mut res = (status, body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = HeaderValue::from_str(&(end - start).to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if ranged
        && end > start
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1))
    {
        h.insert(header::CONTENT_RANGE, v);
    }
    Ok(res)
}

/// The batches after the first, in order: each batch's lines are read as soon as the one before is planned, and
/// [`limits::CC_READ_AHEAD`] batches have their blobs fetched at once, so the next batch's blobs come from the bucket
/// while the current one is sent (6.11).
async fn stream(
    app: Arc<Magpie>,
    f: File,
    off: i64,
    end: i64,
    tx: mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
) {
    use futures_util::{StreamExt as _, TryStreamExt as _};
    let f = Arc::new(f);
    let plans = futures_util::stream::try_unfold(Some(off), {
        let (app, f) = (app.clone(), f.clone());
        move |at| {
            let (app, f) = (app.clone(), f.clone());
            async move {
                let Some(at) = at else {
                    return Ok::<_, Error>(None);
                };
                let b = plan(&app.pool.get().await?, &f, at, end).await?;
                let next = b.next;
                Ok(Some((b, next)))
            }
        }
    });
    let mut batches = plans
        .map_ok(|b| {
            let (app, f) = (app.clone(), f.clone());
            async move { render(&app, &f, b, end).await }
        })
        .try_buffered(limits::CC_READ_AHEAD)
        .boxed();
    loop {
        let r = async {
            let Some((bytes, next)) = batches.try_next().await? else {
                return Ok(None);
            };
            if next.is_none() {
                // a file that changed while it streamed ends early rather than hand out a mix of two versions
                let row = app
                    .pool
                    .get()
                    .await?
                    .query_opt("select version from cc_files where id = $1", &[&f.id])
                    .await?;
                if row.map(|r| r.get::<_, i64>(0)) != Some(f.version) {
                    return Err(Error::Internal("the file changed while it was read".into()));
                }
            }
            Ok::<_, Error>(Some((bytes, next)))
        }
        .await;
        match r {
            Ok(Some((bytes, next))) => {
                if tx.send(Ok(Bytes::from(bytes))).await.is_err() {
                    return;
                }
                if next.is_none() {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "cc file read ended early");
                let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                return;
            }
        }
    }
}

/// A receiver as a body stream.
fn tokio_stream_of(
    mut rx: mpsc::Receiver<std::result::Result<Bytes, std::io::Error>>,
) -> impl futures_util::Stream<Item = std::result::Result<Bytes, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}
