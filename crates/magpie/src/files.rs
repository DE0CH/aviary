//! Files magpie serves: blobs by id (M15) and to the sandbox, the sandbox's blob `PUT`s (magpie 5.4, 11.2).

use axum::body::{Body, Bytes};
use axum::http::HeaderMap;
use axum::response::Response;
use magpie_api::errors::ErrorCode;
use magpie_api::server::Ctx;
use magpie_api::uploads::{BlobVariant, SandboxBlobAs};
use magpie_api::{Admitted, Empty};

use crate::app::Magpie;
use crate::blobs::read::respond;
use crate::blobs::{self, RawBlob, Sha, images};
use crate::error::{Error, Result};
use crate::sandbox;

/// Reads `body`, at most `max` bytes (413 `too_large` beyond).
///
/// # Errors
/// That refusal; 400 when the body cannot be read.
pub async fn read_body(body: Body, max: u64) -> Result<Bytes> {
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    match axum::body::to_bytes(body, limit).await {
        Ok(b) => Ok(b),
        Err(e) => {
            let too_large = std::error::Error::source(&e)
                .is_some_and(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
                || e.to_string().contains("length limit");
            if too_large {
                Err(Error::code(ErrorCode::TooLarge, format!("over {max} bytes")))
            } else {
                Err(Error::bad_request(format!("body: {e}")))
            }
        }
    }
}

/// Reads `body` whole, at most `max` bytes, under the buffered-body memory budget ([`blobs::Blobs::hold_body`]):
/// the body's `Content-Length` (413 at once when it is over `max`), else `max`, is taken before a byte is read, the
/// buffer is allocated at that size once (no growth, no final copy: the bytes take their size once), and what the
/// body did not use is given back. The rest is held for as long as the answer's [`blobs::BodyMemory`] is.
///
/// # Errors
/// As [`read_body`].
pub async fn read_body_held(app: &Magpie, body: Body, max: u64) -> Result<(Bytes, blobs::BodyMemory)> {
    use http_body_util::BodyExt as _;
    let declared = axum::body::HttpBody::size_hint(&body).exact();
    if declared.is_some_and(|n| n > max) {
        return Err(Error::code(ErrorCode::TooLarge, format!("over {max} bytes")));
    }
    let want = declared.unwrap_or(max);
    let mut held = app.blobs.hold_body(want).await?;
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    let mut buf: Vec<u8> = Vec::with_capacity(usize::try_from(want).unwrap_or(limit));
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| Error::bad_request(format!("body: {e}")))?;
        if let Ok(data) = frame.into_data() {
            if buf.len() + data.len() > limit {
                return Err(Error::code(ErrorCode::TooLarge, format!("over {max} bytes")));
            }
            buf.extend_from_slice(&data);
        }
    }
    held.keep(u64::try_from(buf.len()).unwrap_or(u64::MAX));
    Ok((Bytes::from(buf), held))
}

/// The blob of a hex id (404 when it is not one, or no such blob exists).
async fn blob_row(app: &Magpie, hex: &str) -> Result<blobs::BlobRow> {
    let sha = blobs::from_hex(hex).ok_or_else(Error::not_found)?;
    let conn = app.pool.get().await?;
    blobs::row(&conn, &sha).await?.ok_or_else(Error::not_found)
}

/// M15: the blob `hex` in `variant`, if anything references it (a purged conversation's unshared blobs disappear at
/// once for readers, before the GC frees the bytes). A revalidation naming its etag gets 304 before anything is
/// fetched or decoded.
///
/// # Errors
/// 404, 415, 503; a statement error.
pub async fn serve_blob(app: &Magpie, hex: &str, variant: BlobVariant, request: &HeaderMap) -> Result<Response> {
    let row = blob_row(app, hex).await?;
    let conn = app.pool.get().await?;
    let referenced = conn
        .query_opt(
            "select 1 from blob_refs where blob = $1 limit 1",
            &[&row.sha.as_slice()],
        )
        .await?
        .is_some();
    if !referenced {
        return Err(Error::not_found());
    }
    let tag = match variant {
        BlobVariant::Full => "full",
        BlobVariant::Display => "display",
        BlobVariant::Thumb => "thumb",
    };
    if (variant == BlobVariant::Full || blobs::read::is_image(&row))
        && let Some(r) = blobs::read::not_modified(&row.sha, tag, request)
    {
        return Ok(r);
    }
    let model = if variant == BlobVariant::Display {
        let m = conn
            .query_opt(
                "select model_blob from uploads where blob = $1 and model_blob is not null limit 1",
                &[&row.sha.as_slice()],
            )
            .await?;
        match m.and_then(|r| r.get::<_, Option<Vec<u8>>>(0)) {
            Some(m) => match Sha::try_from(m.as_slice()) {
                Ok(m) => blobs::row(&conn, &m).await?,
                Err(_) => None,
            },
            None => None,
        }
    } else {
        None
    };
    drop(conn);
    let served = app.blobs.variant(&app.pool, &row, variant, model.as_ref()).await?;
    Ok(respond(&row.sha, served, request))
}

/// `GET /v1/sandbox/blobs/{sha}`: a blob the sandbox's conversation references (304 before any fetch, as M15).
///
/// # Errors
/// 404, 415, 503; a statement error.
pub async fn serve_sandbox_blob(app: &Magpie, ctx: &Ctx, hex: &str, as_: SandboxBlobAs) -> Result<Response> {
    let db = app.pool.get().await?;
    let conv = sandbox::conversation(&db, ctx, false).await?;
    drop(db);
    let row = blob_row(app, hex).await?;
    let referenced = app
        .pool
        .get()
        .await?
        .query_opt(
            "select 1 from blob_refs where blob = $1 and owner = $2",
            &[&row.sha.as_slice(), &conv.id],
        )
        .await?
        .is_some();
    if !referenced {
        return Err(Error::not_found());
    }
    let (tag, fits) = match as_ {
        SandboxBlobAs::Raw => ("raw", true),
        SandboxBlobAs::Media => ("media", row.encoding != "json-string"),
        SandboxBlobAs::Text => ("text", row.encoding == "json-string"),
    };
    if fits && let Some(r) = blobs::read::not_modified(&row.sha, tag, &ctx.headers) {
        return Ok(r);
    }
    let served = app.blobs.for_sandbox(&app.pool, &row, as_).await?;
    Ok(respond(&row.sha, served, &ctx.headers))
}

/// A raw blob's facts from its bytes: `media` for images and PDF (with the image's dimensions), `file` otherwise.
#[must_use]
pub fn raw_facts(sha: Sha, bytes: &[u8], mime: Option<&str>) -> RawBlob {
    let sniffed = images::sniff(bytes);
    let mime = sniffed.or(mime).map(str::to_owned);
    let media = sniffed.is_some();
    let (width, height) = match sniffed {
        Some(m) if images::is_image(m) => {
            images::dimensions(bytes, m).map_or((None, None), |(w, h)| (Some(w), Some(h)))
        }
        _ => (None, None),
    };
    RawBlob {
        sha,
        size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        kind: if media { "media" } else { "file" },
        mime,
        width,
        height,
    }
}

/// `PUT /v1/sandbox/blobs/{sha}` (lease): the raw bytes of a sent file, verified against the sha; 201 when new,
/// 200 when magpie had them.
///
/// # Errors
/// 400 `sha_mismatch`, 404, 409 `lease_lost`, 413, 503; a statement error.
pub async fn put_sandbox_blob(app: &Magpie, ctx: &Ctx, hex: &str, body: Body) -> Result<Admitted<Empty>> {
    let sha = blobs::from_hex(hex).ok_or_else(|| Error::code(ErrorCode::ShaMismatch, "not a sha256"))?;
    {
        let conn = app.pool.get().await?;
        sandbox::conversation(&conn, ctx, true).await?;
        // a blob magpie has: its last_ref_at bumped, so the GC leaves it for the send's commit (magpie 5.3)
        let had = conn
            .execute(
                "update blobs set last_ref_at = now() where sha256 = $1 and stored",
                &[&sha.as_slice()],
            )
            .await?;
        if had > 0 {
            return Ok(Admitted::Repeat(Empty {}));
        }
    }
    let (bytes, _held) = read_body_held(app, body, magpie_api::sandbox::SENT_FILE_MAX_BYTES).await?;
    if blobs::sha256(&bytes) != sha {
        return Err(Error::code(
            ErrorCode::ShaMismatch,
            "the body's sha256 is not the path's",
        ));
    }
    let facts = raw_facts(sha, &bytes, None);
    let (_, lock) = app.blobs.put_bytes(&app.pool, bytes).await?;
    let held = &lock;
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        blobs::record_stored(t, &facts).await?;
        held.check(t).await
    })
    .await;
    lock.release().await;
    r?;
    Ok(Admitted::New(Empty {}))
}

// ---------------------------------------------------------------------------------------------------------------
// Paths

fn bad_path(why: &str) -> Error {
    Error::code(ErrorCode::BadPath, why.to_owned())
}

/// A path relative to `/workspace`, as the routes take it: relative, normalised (no empty, `.` or `..`
/// component), no NUL, at most 4,096 bytes (400 `bad_path` otherwise).
///
/// # Errors
/// That refusal.
pub fn relative_path(path: &str) -> Result<String> {
    if path.is_empty() || path.len() > magpie_api::files::PATH_MAX_BYTES || path.contains('\0') {
        return Err(bad_path("a path must be 1-4096 bytes without NUL"));
    }
    if path.starts_with('/') {
        return Err(bad_path("a path is relative to /workspace"));
    }
    if path.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(bad_path("a path must be normalised"));
    }
    Ok(path.to_owned())
}

/// A sent file's path (magpie 11.2): absolute under `/workspace` (a `file://` prefix is stripped), normalised, no
/// `..`, no NUL, at most 4,096 bytes; answered relative to `/workspace`.
///
/// # Errors
/// 400 `bad_path`.
pub fn sent_path(path: &str) -> Result<String> {
    let p = path.strip_prefix("file://").unwrap_or(path);
    if p.len() > magpie_api::files::PATH_MAX_BYTES || p.contains('\0') {
        return Err(bad_path("a path must be at most 4096 bytes without NUL"));
    }
    let rest = p
        .strip_prefix("/workspace/")
        .ok_or_else(|| bad_path("a sent file must be under /workspace"))?;
    let mut parts = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err(bad_path("a sent file's path may not contain ..")),
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return Err(bad_path("a sent file must name a file"));
    }
    Ok(parts.join("/"))
}

/// The last component of a path.
#[must_use]
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether the app shows a file of `mime` as an image.
#[must_use]
pub fn shown_as_image(mime: &str) -> bool {
    mime.starts_with("image/")
}

// ---------------------------------------------------------------------------------------------------------------
// Downloads

/// The headers of a file download (magpie 11.3).
fn download_headers(name: &str, inline: bool, h: &mut axum::http::HeaderMap) {
    use axum::http::{HeaderValue, header};
    let ct = magpie_api::mime::from_name(name);
    let set = |h: &mut axum::http::HeaderMap, k: header::HeaderName, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set(h, header::CONTENT_TYPE, ct);
    let disposition = if inline && crate::blobs::read::inline_type(ct) {
        "inline".to_owned()
    } else {
        format!("attachment; filename*={}", crate::blobs::read::filename_star(name))
    };
    set(h, header::CONTENT_DISPOSITION, &disposition);
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(h, header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox");
    set(h, header::CACHE_CONTROL, "private, no-cache");
    set(h, header::ACCEPT_RANGES, "bytes");
}

fn range_of(ctx: &Ctx) -> Option<hermit_api::workspaces::ByteRange> {
    ctx.headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(hermit_api::workspaces::ByteRange::parse)
}

fn not_satisfiable(size: u64) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let mut r = aviary_core::http::ApiError::new(
        axum::http::StatusCode::RANGE_NOT_SATISFIABLE,
        "bad_range",
        "the range is beyond the file",
    )
    .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("bytes */{size}")) {
        r.headers_mut().insert(axum::http::header::CONTENT_RANGE, v);
    }
    r
}

/// A download of bytes magpie holds (a sent-file snapshot, an attachment), honouring one `Range`.
#[must_use]
pub fn bytes_download(ctx: &Ctx, name: &str, inline: bool, etag: &str, bytes: Bytes) -> axum::response::Response {
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::IntoResponse as _;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let mut h = axum::http::HeaderMap::new();
    download_headers(name, inline, &mut h);
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    match range_of(ctx) {
        Some(r) => {
            let Some((first, last)) = r.resolve(size) else {
                return not_satisfiable(size);
            };
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {first}-{last}/{size}")) {
                h.insert(header::CONTENT_RANGE, v);
            }
            let (a, b) = (
                usize::try_from(first).unwrap_or(0),
                usize::try_from(last).unwrap_or(0) + 1,
            );
            (StatusCode::PARTIAL_CONTENT, h, Body::from(bytes.slice(a..b))).into_response()
        }
        None => (StatusCode::OK, h, Body::from(bytes)).into_response(),
    }
}

/// A file of a hermit workspace (the live one of a conversation, or a share's frozen clone), streamed with
/// hermit's `Range`, `etag` and `last-modified` (magpie 11.3).
///
/// # Errors
/// 404 for hermit's `no_path`, `not_a_file` and `no_workspace`; 503 when hermit or the workspace is unavailable.
pub async fn workspace_download(
    app: &Magpie,
    ctx: &Ctx,
    workspace: uuid::Uuid,
    path: &str,
    inline: bool,
) -> Result<axum::response::Response> {
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::IntoResponse as _;
    let q = hermit_api::workspaces::FilesQuery { path: path.to_owned() };
    let dl = match app.hermit.workspace_file(workspace, &q, range_of(ctx)).await {
        Ok(d) => d,
        Err(e) => {
            return match e.status() {
                Some(404 | 409) => Err(Error::not_found()),
                Some(416) => Ok(not_satisfiable(0)),
                _ => {
                    tracing::warn!(error = %e, "workspace file: hermit did not answer");
                    Err(Error::code(ErrorCode::Unavailable, "the workspace is unavailable"))
                }
            };
        }
    };
    let mut h = axum::http::HeaderMap::new();
    download_headers(file_name(path), inline, &mut h);
    let meta = dl.meta.clone();
    if let Ok(v) = HeaderValue::from_str(&meta.etag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&meta.mtime.http_date()) {
        h.insert(header::LAST_MODIFIED, v);
    }
    let status = if let Some((first, last)) = meta.served {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {first}-{last}/{}", meta.size)) {
            h.insert(header::CONTENT_RANGE, v);
        }
        if let Ok(v) = HeaderValue::from_str(&(last - first + 1).to_string()) {
            h.insert(header::CONTENT_LENGTH, v);
        }
        StatusCode::PARTIAL_CONTENT
    } else {
        if let Ok(v) = HeaderValue::from_str(&meta.size.to_string()) {
            h.insert(header::CONTENT_LENGTH, v);
        }
        StatusCode::OK
    };
    let stream = futures_util::stream::unfold(Some(dl), |state| async move {
        let mut dl = state?;
        match dl.chunk().await {
            Ok(Some(b)) => Some((Ok::<_, std::io::Error>(b), Some(dl))),
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e.to_string())), None)),
        }
    });
    Ok((status, h, Body::from_stream(stream)).into_response())
}

/// M14 `GET /v1/conversations/{id}/files/raw`: with `sent`, that send's snapshot; without, the live workspace file.
///
/// # Errors
/// 400 `bad_path`, 404, 503; a statement error.
pub async fn files_raw(
    app: &Magpie,
    ctx: &Ctx,
    person: &crate::conversations::Person,
    id: uuid::Uuid,
    q: &magpie_api::files::RawFileQuery,
) -> Result<axum::response::Response> {
    let path = relative_path(&q.path)?;
    let conn = app.pool.get().await?;
    crate::conversations::load_owned(&conn, &person.email, id).await?;
    let Some(sent) = &q.sent else {
        drop(conn);
        return workspace_download(app, ctx, id, &path, q.inline).await;
    };
    let row = conn
        .query_opt(
            "select blob from sent_files where conversation_id = $1 and tool_use_id = $2 and path = $3",
            &[&id, sent, &path],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    drop(conn);
    snapshot_download(app, ctx, &path, q.inline, &row.get::<_, Vec<u8>>(0)).await
}

/// The bytes of a stored blob (`sha`) as a download of `path`.
///
/// # Errors
/// 404, 503; a statement error.
pub async fn snapshot_download(
    app: &Magpie,
    ctx: &Ctx,
    path: &str,
    inline: bool,
    sha: &[u8],
) -> Result<axum::response::Response> {
    let sha = Sha::try_from(sha).map_err(|_| Error::Internal("a blob id is not 32 bytes".into()))?;
    let row = blobs::row(&app.pool.get().await?, &sha)
        .await?
        .ok_or_else(Error::not_found)?;
    let bytes = app.blobs.original(&app.pool, &row).await?;
    let etag = format!("\"{}\"", blobs::to_hex(&sha));
    Ok(bytes_download(ctx, file_name(path), inline, &etag, bytes))
}

/// `GET /v1/conversations/{id}/files`: the files panel, newest send first, each path once with its latest send.
///
/// # Errors
/// 404; a statement error.
pub async fn panel(
    db: &impl crate::db::Db,
    person: &crate::conversations::Person,
    id: uuid::Uuid,
) -> Result<magpie_api::files::FilesPanel> {
    crate::conversations::load_owned(db, &person.email, id).await?;
    let rows = db
        .query(
            "select * from (
               select distinct on (path) path, name, size, mime, is_image, sent_at, caption, tool_use_id, blob, seq
               from sent_files where conversation_id = $1
               order by path, sent_at desc, seq desc) latest
             order by sent_at desc, seq desc, path",
            &[&id],
        )
        .await?;
    let files = rows
        .into_iter()
        .map(|r| {
            let blob: Vec<u8> = r.get("blob");
            let mime: Option<String> = r.get("mime");
            magpie_api::files::PanelFile {
                path: r.get("path"),
                name: r.get("name"),
                size: u64::try_from(r.get::<_, i64>("size")).unwrap_or(0),
                mime_type: mime.unwrap_or_else(|| "application/octet-stream".into()),
                is_image: r.get("is_image"),
                sent_at: magpie_api::Timestamp(r.get("sent_at")),
                caption: r.get("caption"),
                tool_use_id: r.get("tool_use_id"),
                blob_id: Sha::try_from(blob.as_slice())
                    .map(|s| blobs::to_hex(&s))
                    .unwrap_or_default(),
            }
        })
        .collect();
    Ok(magpie_api::files::FilesPanel { files })
}

// ---------------------------------------------------------------------------------------------------------------
// Sent-file snapshots (magpie 11.2)

fn tool_use_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A staged file of a send.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StagedFile {
    path: String,
    size: u64,
    sha256: String,
    mime_type: String,
}

/// `POST /v1/sandbox/sent-files` (lease): phase 1 of a snapshot; the blobs magpie does not have yet.
///
/// # Errors
/// 400 (`bad_request`, `bad_path`), 404, 409 `lease_lost` / `conversation_migrating`, 413; a statement error.
pub async fn sent_files(
    app: &Magpie,
    ctx: &Ctx,
    req: &magpie_api::sandbox::SentFilesRequest,
) -> Result<magpie_api::sandbox::SentFilesResponse> {
    use magpie_api::sandbox::{SENT_FILE_MAX_BYTES, SENT_FILES_MAX, SENT_TOTAL_MAX_BYTES};
    if !tool_use_id_ok(&req.tool_use_id) {
        return Err(Error::bad_request("toolUseId must match ^[A-Za-z0-9_-]{1,128}$"));
    }
    if req.files.is_empty() || req.files.len() > SENT_FILES_MAX {
        return Err(Error::bad_request("a send has 1-100 files"));
    }
    let mut staged = Vec::with_capacity(req.files.len());
    let mut total: u64 = 0;
    for f in &req.files {
        let path = sent_path(&f.path)?;
        if f.size > SENT_FILE_MAX_BYTES {
            return Err(Error::code(ErrorCode::TooLarge, "a sent file is over 64 MiB"));
        }
        total = total.saturating_add(f.size);
        let sha = blobs::from_hex(&f.sha256).ok_or_else(|| Error::bad_request("sha256 is not 64 hex digits"))?;
        staged.push(StagedFile {
            path,
            size: f.size,
            sha256: blobs::to_hex(&sha),
            mime_type: f.mime_type.clone(),
        });
    }
    if total > SENT_TOTAL_MAX_BYTES {
        return Err(Error::code(ErrorCode::TooLarge, "a send is over 256 MiB"));
    }
    let files = serde_json::to_value(&staged)?;
    let shas: Vec<Vec<u8>> = staged
        .iter()
        .filter_map(|f| blobs::from_hex(&f.sha256).map(|s| s.to_vec()))
        .collect();
    let (tool_use_id, caption) = (&req.tool_use_id, &req.caption);
    let missing = aviary_pg::tx!(&app.pool, |t| -> Result<Vec<String>> {
        let conv = sandbox::conversation(t, ctx, true).await?;
        let done = t
            .query_opt(
                "select 1 from sent_files where conversation_id = $1 and tool_use_id = $2 limit 1",
                &[&conv.id, tool_use_id],
            )
            .await?
            .is_some();
        if done {
            return Ok(Vec::new());
        }
        t.execute(
            "insert into sent_staging (conversation_id, tool_use_id, files, caption) values ($1, $2, $3, $4)
             on conflict (conversation_id, tool_use_id) do update set files = $3, caption = $4, created_at = now()",
            &[&conv.id, tool_use_id, &files, caption],
        )
        .await?;
        // the blobs magpie has get their last_ref_at bumped under their row locks, so the GC (which needs 24 h
        // without one) leaves them for the commit; a blob the GC freed before this is missing and put again
        t.execute(
            "update blobs set last_ref_at = now() where sha256 = any($1::bytea[])",
            &[&shas],
        )
        .await?;
        let rows = t
            .query(
                "select s from unnest($1::bytea[]) s where not exists (select 1 from blobs b where b.sha256 = s)",
                &[&shas],
            )
            .await?;
        let mut missing: Vec<String> = rows
            .into_iter()
            .filter_map(|r| Sha::try_from(r.get::<_, Vec<u8>>(0).as_slice()).ok())
            .map(|s| blobs::to_hex(&s))
            .collect();
        missing.sort();
        missing.dedup();
        Ok(missing)
    })
    .await?;
    Ok(magpie_api::sandbox::SentFilesResponse { missing })
}

/// `POST /v1/sandbox/sent-files/{toolUseId}/commit` (lease): one transaction makes the `sent_files` rows, the
/// conversation's blob references and the `files.sent` event (whose projection updates the tool row, magpie 7.3
/// rule 6), and drops the staging row. A repeat answers the first commit.
///
/// # Errors
/// 400 when a blob of the send has not been put, 404 without a phase 1, 409 `lease_lost` /
/// `conversation_migrating`; a statement error.
#[allow(clippy::too_many_lines)] // one transaction, in the order magpie 11.2 lists
pub async fn sent_files_commit(
    app: &Magpie,
    ctx: &Ctx,
    tool_use_id: &str,
) -> Result<magpie_api::sandbox::SentFilesCommitted> {
    use magpie_api::events::{EventBody, FilesSent};
    use magpie_api::uploads::{BlobKind, BlobPointer, BlobRef, SentFile};
    if !tool_use_id_ok(tool_use_id) {
        return Err(Error::not_found());
    }
    let tool_use_id = tool_use_id.to_owned();
    let tid = &tool_use_id;
    let (kept, seq) = aviary_pg::tx!(&app.pool, |t| -> Result<(u32, i64)> {
        let conv = sandbox::conversation(t, ctx, true).await?;
        crate::conversations::lock(t, &conv.person, conv.id).await?;
        if let Some(r) = t
            .query_opt(
                "select count(*)::bigint, max(seq) from sent_files where conversation_id = $1 and tool_use_id = $2
                 having count(*) > 0",
                &[&conv.id, tid],
            )
            .await?
        {
            return Ok((u32::try_from(r.get::<_, i64>(0)).unwrap_or(0), r.get(1)));
        }
        let staged = t
            .query_opt(
                "select files, caption from sent_staging where conversation_id = $1 and tool_use_id = $2",
                &[&conv.id, tid],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        let files: Vec<StagedFile> = serde_json::from_value(staged.get(0))?;
        let caption: Option<String> = staged.get(1);
        // one entry per path (the last wins), in the order sent
        let mut by_path: Vec<StagedFile> = Vec::new();
        for f in files {
            by_path.retain(|g| g.path != f.path);
            by_path.push(f);
        }
        let mut sent = Vec::with_capacity(by_path.len());
        let mut shas = Vec::with_capacity(by_path.len());
        for f in &by_path {
            let sha = blobs::from_hex(&f.sha256).ok_or_else(|| Error::Internal("staged sha".into()))?;
            let row = blobs::row(t, &sha)
                .await?
                .ok_or_else(|| Error::bad_request("a blob of the send has not been put"))?;
            let mime = if f.mime_type.trim().is_empty() {
                magpie_api::mime::from_name(&f.path).to_owned()
            } else {
                f.mime_type.clone()
            };
            sent.push(SentFile {
                path: f.path.clone(),
                name: file_name(&f.path).to_owned(),
                size: u64::try_from(row.size).unwrap_or(0),
                mime_type: mime.clone(),
                is_image: shown_as_image(&mime),
                blob: BlobPointer {
                    blob: BlobRef {
                        blob_id: blobs::to_hex(&sha),
                        bytes: u64::try_from(row.size).unwrap_or(0),
                        kind: if row.kind == "media" {
                            BlobKind::Media
                        } else {
                            BlobKind::File
                        },
                        mime: Some(mime),
                        width: row.width.and_then(|w| u32::try_from(w).ok()),
                        height: row.height.and_then(|h| u32::try_from(h).ok()),
                        preview: None,
                        lines: None,
                    },
                },
            });
            shas.push(sha);
        }
        let tool_use_seq = t
            .query_opt(
                "select source_seqs[1] from rows where conversation_id = $1 and tool_use_id = $2
                 order by row_id limit 1",
                &[&conv.id, tid],
            )
            .await?
            .and_then(|r| r.get::<_, Option<i64>>(0))
            .and_then(|s| u64::try_from(s).ok());
        let active: Option<uuid::Uuid> = t
            .query_one("select active_turn from conversations where id = $1", &[&conv.id])
            .await?
            .get(0);
        let body = EventBody::FilesSent(FilesSent {
            tool_use_id: tid.clone(),
            tool_use_seq,
            caption: caption.clone(),
            files: sent.clone(),
        });
        let seq = crate::events::append(t, conv.id, crate::events::NewEvent::of(&body)?.turn(active)).await?;
        for (f, sha) in sent.iter().zip(&shas) {
            t.execute(
                "insert into sent_files (conversation_id, tool_use_id, path, name, size, mime, is_image, blob,
                                         caption, sent_at, seq)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), $10)",
                &[
                    &conv.id,
                    tid,
                    &f.path,
                    &f.name,
                    &i64::try_from(f.size).unwrap_or(i64::MAX),
                    &f.mime_type,
                    &f.is_image,
                    &sha.as_slice(),
                    &caption,
                    &seq,
                ],
            )
            .await?;
            blobs::reference(t, conv.id, "conversation", sha).await?;
        }
        t.execute(
            "delete from sent_staging where conversation_id = $1 and tool_use_id = $2",
            &[&conv.id, tid],
        )
        .await?;
        Ok((u32::try_from(sent.len()).unwrap_or(u32::MAX), seq))
    })
    .await?;
    Ok(magpie_api::sandbox::SentFilesCommitted {
        kept,
        seq: u64::try_from(seq).unwrap_or(0),
    })
}
