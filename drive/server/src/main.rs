//! yyc-drive: one shared folder behind OpenCloud's web page.
//!
//! One program: it answers the page's WebDAV, upload, search and preview calls on one folder of the disk, for the
//! person the edge in front names in a header. The page itself, its settings and its sign-in are the edge's
//! (`edge/`). No database, no per-person folders, no sharing.

// A refusal is a whole response; boxing it would buy nothing here.
#![allow(clippy::result_large_err)]

use std::{
    env,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode},
    Router,
};
use base64::{engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL}, Engine};
use futures_util::StreamExt;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const STORAGE: &str = "00000000-0000-4000-8000-000000000001";
const UPLOADS: &str = ".drive-uploads";
/// What stays unescaped in an href: unreserved characters and `/`.
const HREF: &AsciiSet = &CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'`').add(b'{').add(b'}')
    .add(b'&').add(b'+').add(b'[').add(b']').add(b'^').add(b'|').add(b'\\').add(b';').add(b'=').add(b'@').add(b':')
    .add(b'!').add(b'$').add(b'\'').add(b'(').add(b')').add(b'*').add(b',');

struct Cfg {
    root: PathBuf,
    name: String,
    user_header: String,
    dev_user: Option<String>,
    url: Option<String>,
}

type S = Arc<Cfg>;
type Res = Response<Body>;

fn drive_id() -> String {
    format!("{STORAGE}${STORAGE}")
}

/// A setting: `--name value` on the command line, else the environment variable `DRIVE_NAME`.
fn setting(flag: &str) -> Option<String> {
    let args: Vec<String> = env::args().collect();
    let name = format!("--{flag}");
    args.iter()
        .position(|a| *a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| env::var(format!("DRIVE_{}", flag.replace('-', "_").to_uppercase())).ok())
        .filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() {
    let cfg = Arc::new(Cfg {
        root: PathBuf::from(setting("root").expect("--root or DRIVE_ROOT: the folder to serve")).canonicalize().expect("the folder to serve"),
        name: setting("name").unwrap_or_else(|| "Drive".into()),
        user_header: setting("user-header").unwrap_or_else(|| "cf-access-authenticated-user-email".into()),
        dev_user: setting("dev-user"),
        url: setting("url").map(|u| u.trim_end_matches('/').to_string()),
    });
    tokio::fs::create_dir_all(cfg.root.join(UPLOADS)).await.expect("uploads folder");
    let listen = setting("listen").unwrap_or_else(|| "0.0.0.0:8080".into());
    let app = Router::new().fallback(handle).with_state(cfg);
    let listener = tokio::net::TcpListener::bind(&listen).await.expect("listen");
    eprintln!("yyc-drive on {listen}");
    // Asked to stop (a rolling update): no new connections, and the transfers in flight run to their end.
    let stop = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
        eprintln!("yyc-drive stopping: finishing what is in flight");
    };
    axum::serve(listener, app).with_graceful_shutdown(stop).await.expect("serve");
}

/// The largest body read whole before an answer (the page's settings, sign-in and WebDAV calls are small).
const SMALL_BODY: usize = 1 << 20;

async fn handle(State(s): State<S>, req: Request) -> Res {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    // A body is read to its end before the answer, except an upload's (PUT, tus POST and PATCH), which its handler
    // streams to disk. Answered with its body still arriving, a connection is closed after the answer, under the
    // proxy in front, which may already be sending its next request there (cloudflared: a 502 on that request when
    // it is a POST, the page's "access denied" after sign-in).
    let streams = (matches!(method, Method::PUT | Method::POST) && path.starts_with("/dav/")) || path.starts_with("/data/tus/");
    let req = if streams {
        req
    } else {
        let (parts, body) = req.into_parts();
        match axum::body::to_bytes(body, SMALL_BODY).await {
            Ok(b) => Request::from_parts(parts, Body::from(b)),
            Err(_) => return status(StatusCode::PAYLOAD_TOO_LARGE),
        }
    };
    let res = route(&s, req).await.unwrap_or_else(|e| e);
    if res.status().is_client_error() || res.status().is_server_error() {
        eprintln!("{} {} -> {}", method, path, res.status().as_u16());
    }
    res
}

async fn route(s: &S, req: Request) -> Result<Res, Res> {
    let method = req.method().as_str().to_string();
    let path = percent_decode_str(req.uri().path()).decode_utf8_lossy().to_string();

    // for the cluster: alive, and ready while the folder is there (a lost mount takes the pod out of service)
    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => return Ok(status(StatusCode::OK)),
        ("GET", "/readyz") => {
            let there = tokio::fs::metadata(&s.root).await.map(|m| m.is_dir()).unwrap_or(false);
            return Ok(status(if there { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }));
        }
        _ => {}
    }

    let user = person(s, req.headers()).ok_or_else(|| status(StatusCode::UNAUTHORIZED))?;

    match (method.as_str(), path.as_str()) {
        ("REPORT", p) if p.starts_with("/dav/spaces") => search(s, req).await,
        (_, p) if p.starts_with("/dav/spaces/") => dav(s, &user, &p["/dav/spaces/".len()..], req).await,
        (_, p) if p.starts_with("/data/tus/") => tus_chunk(s, &p["/data/tus/".len()..], req).await,
        _ => Err(status(StatusCode::NOT_FOUND)),
    }
}

// ---------------------------------------------------------------------------------------------- who is asking, and where

fn base_url(s: &S, h: &HeaderMap) -> String {
    if let Some(u) = &s.url {
        return u.clone();
    }
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    let proto = h.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    format!("{proto}://{host}")
}

fn person(s: &S, h: &HeaderMap) -> Option<String> {
    h.get(s.user_header.as_str()).and_then(|v| v.to_str().ok()).map(str::to_string).filter(|v| !v.is_empty()).or_else(|| s.dev_user.clone())
}

// --------------------------------------------------------------------------------------------------------- the folder

/// A request path made safe: no `..`, no absolute parts, no hidden names.
fn clean(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(path.trim_start_matches('/')).components() {
        match c {
            Component::Normal(p) => {
                if p.to_string_lossy().starts_with('.') {
                    return None;
                }
                out.push(p)
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn node_id(rel: &Path) -> String {
    if rel.as_os_str().is_empty() { STORAGE.to_string() } else { B64URL.encode(rel.to_string_lossy().as_bytes()) }
}

fn file_id(rel: &Path) -> String {
    format!("{}!{}", drive_id(), node_id(rel))
}

/// `<drive>/<path>` or `<drive>!<node>/<path>` after `/dav/spaces/` -> the path below the root.
fn resolve(rest: &str) -> Option<PathBuf> {
    let (first, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let mut rel = PathBuf::new();
    if let Some((drive, node)) = first.split_once('!') {
        if drive != drive_id() {
            return None;
        }
        if node != STORAGE {
            rel = clean(&String::from_utf8(B64URL.decode(node).ok()?).ok()?)?;
        }
    } else if first != drive_id() {
        return None;
    }
    rel.push(clean(tail)?);
    Some(rel)
}

fn href(rel: &Path, dir: bool) -> String {
    let p = rel.to_string_lossy();
    let mut h = format!("/dav/spaces/{}/{}", drive_id(), utf8_percent_encode(&p, HREF));
    if dir && !h.ends_with('/') {
        h.push('/');
    }
    h
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn etag(m: &std::fs::Metadata) -> String {
    let t = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    format!("\"{:x}-{:x}\"", t, m.len())
}

fn entry_xml(s: &S, base: &str, user: &str, rel: &Path, m: &std::fs::Metadata) -> String {
    let dir = m.is_dir();
    let name = if rel.as_os_str().is_empty() { s.name.clone() } else { rel.file_name().unwrap().to_string_lossy().to_string() };
    let parent = rel.parent().map(file_id).unwrap_or_else(drive_id);
    let modified = httpdate::fmt_http_date(m.modified().unwrap_or(UNIX_EPOCH));
    let mut x = format!(
        "<d:response><d:href>{}</d:href><d:propstat><d:prop><oc:permissions>{}</oc:permissions><oc:favorite>0</oc:favorite><oc:fileid>{}</oc:fileid><oc:file-parent>{}</oc:file-parent><oc:name>{}</oc:name><oc:owner-id>{}</oc:owner-id><oc:owner-display-name>{}</oc:owner-display-name><oc:privatelink>{}/f/{}</oc:privatelink><d:getlastmodified>{}</d:getlastmodified><d:getetag>{}</d:getetag>",
        esc(&href(rel, dir)), if dir { "RDNVCKZP" } else { "RDNVWZP" }, esc(&file_id(rel)), esc(&parent), esc(&name), esc(user), esc(user), esc(base),
        utf8_percent_encode(&file_id(rel), HREF), modified, esc(&etag(m)),
    );
    if dir {
        x.push_str("<d:resourcetype><d:collection/></d:resourcetype>");
    } else {
        let mime = mime_guess::from_path(rel).first_or_octet_stream();
        x.push_str(&format!(
            "<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength><oc:size>{}</oc:size><d:getcontenttype>{}</d:getcontenttype><oc:has-preview>{}</oc:has-preview><oc:downloadURL>{}{}</oc:downloadURL>",
            m.len(), m.len(), mime, u8::from(previewable(rel)), esc(base), esc(&href(rel, false))
        ));
    }
    x.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
    x
}

fn multistatus(body: String) -> Res {
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .header("dav", "1, 3, extended-mkcol")
        .header("tus-resumable", "1.0.0")
        .header("tus-version", "1.0.0")
        .header("tus-extension", "creation,creation-with-upload")
        .body(Body::from(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><d:multistatus xmlns:s=\"http://sabredav.org/ns\" xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\">{body}</d:multistatus>"
        )))
        .unwrap()
}

// ------------------------------------------------------------------------------------------------------- previews

/// Pictures ffmpeg draws a preview of: every video, and the common still images.
fn previewable(rel: &Path) -> bool {
    let mime = mime_guess::from_path(rel).first_or_octet_stream();
    mime.type_() == "video" || matches!(mime.essence_str(), "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff")
}

/// At most this many ffmpeg runs at once in one server (each decodes one frame on one thread).
static PREVIEWS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// The page's preview of a file: one frame (a second in, for a video) as a JPEG that fits in `x` by `y`, its shape
/// kept; 404 when there is none, so the page shows the file's icon.
async fn preview(full: &Path, rel: &Path, q: &[(String, String)]) -> Result<Res, Res> {
    if !previewable(rel) {
        return Err(status(StatusCode::NOT_FOUND));
    }
    let dim = |k: &str| q.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.parse::<u32>().ok()).unwrap_or(256).clamp(16, 1024);
    let (x, y) = (dim("x"), dim("y"));
    let video = mime_guess::from_path(rel).first_or_octet_stream().type_() == "video";
    let _turn = PREVIEWS.acquire().await.map_err(|_| status(StatusCode::SERVICE_UNAVAILABLE))?;
    let mut jpeg = frame(full, x, y, video.then_some("1")).await;
    if jpeg.is_none() && video {
        // shorter than a second: the first frame
        jpeg = frame(full, x, y, None).await;
    }
    let jpeg = jpeg.ok_or_else(|| status(StatusCode::NOT_FOUND))?;
    // the page puts the file's etag in the address (`c`), so a preview it asked for that way never goes stale
    let cache = if q.iter().any(|(k, _)| k == "c") { "private, max-age=31536000, immutable" } else { "no-cache" };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(jpeg))
        .unwrap())
}

/// One frame of `file` at `at` seconds (keyframes only, one thread: little memory), scaled to fit `x` by `y`.
async fn frame(file: &Path, x: u32, y: u32, at: Option<&str>) -> Option<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1"]);
    if let Some(t) = at {
        cmd.args(["-skip_frame", "nokey", "-ss", t]);
    }
    cmd.arg("-i").arg(file);
    cmd.args(["-frames:v", "1", "-vf", &format!("scale={x}:{y}:force_original_aspect_ratio=decrease"), "-q:v", "5"]);
    cmd.args(["-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"]);
    cmd.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output()).await.ok()?.ok()?;
    (out.status.success() && out.stdout.starts_with(&[0xff, 0xd8])).then_some(out.stdout)
}

async fn dav(s: &S, user: &str, rest: &str, req: Request) -> Result<Res, Res> {
    let rel = resolve(rest).ok_or_else(|| status(StatusCode::NOT_FOUND))?;
    let full = s.root.join(&rel);
    let base = base_url(s, req.headers());
    let h = req.headers().clone();
    match req.method().as_str() {
        "PROPFIND" => {
            let m = tokio::fs::metadata(&full).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
            let mut body = entry_xml(s, &base, user, &rel, &m);
            let depth = h.get("depth").and_then(|v| v.to_str().ok()).unwrap_or("1");
            if m.is_dir() && depth != "0" {
                let mut rd = tokio::fs::read_dir(&full).await.map_err(io)?;
                let mut items = vec![];
                while let Some(e) = rd.next_entry().await.map_err(io)? {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with('.') {
                        continue;
                    }
                    // metadata() follows links, so a link shows as what it points at; a broken one is left out.
                    if let Ok(m) = tokio::fs::metadata(e.path()).await {
                        items.push((n, m));
                    }
                }
                items.sort_by(|a, b| a.0.cmp(&b.0));
                for (n, m) in items {
                    body.push_str(&entry_xml(s, &base, user, &rel.join(n), &m));
                }
            }
            Ok(multistatus(body))
        }
        "GET" | "HEAD" => {
            let m = tokio::fs::metadata(&full).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
            if m.is_dir() {
                return Err(status(StatusCode::METHOD_NOT_ALLOWED));
            }
            let q = parse_query(req.uri().query().unwrap_or(""));
            if req.method() == Method::GET && q.iter().any(|(k, v)| k == "preview" && v == "1") {
                return preview(&full, &rel, &q).await;
            }
            let mut res = send_file(&full, &h).await?;
            res.headers_mut().insert("oc-fileid", HeaderValue::from_str(&file_id(&rel)).unwrap());
            if req.method() == Method::HEAD {
                *res.body_mut() = Body::empty();
            }
            Ok(res)
        }
        "PUT" => {
            let existed = tokio::fs::metadata(&full).await.is_ok();
            let tmp = s.root.join(UPLOADS).join(format!("put-{}", unique()));
            let mut f = tokio::fs::File::create(&tmp).await.map_err(io)?;
            let mut stream = req.into_body().into_data_stream();
            while let Some(chunk) = stream.next().await {
                f.write_all(&chunk.map_err(|_| status(StatusCode::BAD_REQUEST))?).await.map_err(io)?;
            }
            f.flush().await.map_err(io)?;
            drop(f);
            tokio::fs::rename(&tmp, &full).await.map_err(|_| status(StatusCode::CONFLICT))?;
            let m = tokio::fs::metadata(&full).await.map_err(io)?;
            Ok(Response::builder()
                .status(if existed { StatusCode::NO_CONTENT } else { StatusCode::CREATED })
                .header(header::ETAG, etag(&m))
                .header("oc-etag", etag(&m))
                .header("oc-fileid", file_id(&rel))
                .body(Body::empty())
                .unwrap())
        }
        "MKCOL" => match tokio::fs::create_dir(&full).await {
            Ok(()) => Ok(Response::builder().status(StatusCode::CREATED).header("oc-fileid", file_id(&rel)).body(Body::empty()).unwrap()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(status(StatusCode::METHOD_NOT_ALLOWED)),
            Err(_) => Err(status(StatusCode::CONFLICT)),
        },
        "DELETE" => {
            if rel.as_os_str().is_empty() {
                return Err(status(StatusCode::FORBIDDEN));
            }
            let m = tokio::fs::symlink_metadata(&full).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
            if m.is_dir() { tokio::fs::remove_dir_all(&full).await } else { tokio::fs::remove_file(&full).await }.map_err(io)?;
            Ok(status(StatusCode::NO_CONTENT))
        }
        m @ ("MOVE" | "COPY") => {
            if rel.as_os_str().is_empty() {
                return Err(status(StatusCode::FORBIDDEN));
            }
            let dest = h.get("destination").and_then(|v| v.to_str().ok()).ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
            let dest = percent_decode_str(dest).decode_utf8_lossy().to_string();
            let at = dest.find("/dav/spaces/").ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
            let to_rel = resolve(&dest[at + "/dav/spaces/".len()..]).ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
            let to = s.root.join(&to_rel);
            let exists = tokio::fs::symlink_metadata(&to).await.is_ok();
            if exists && h.get("overwrite").and_then(|v| v.to_str().ok()) != Some("T") {
                return Err(status(StatusCode::PRECONDITION_FAILED));
            }
            if to.starts_with(&full) {
                return Err(status(StatusCode::CONFLICT));
            }
            let (from, to2, copy) = (full.clone(), to.clone(), m == "COPY");
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                if exists {
                    if to2.is_dir() { std::fs::remove_dir_all(&to2)? } else { std::fs::remove_file(&to2)? }
                }
                if copy { copy_tree(&from, &to2) } else { std::fs::rename(&from, &to2) }
            })
            .await
            .map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR))?
            .map_err(|_| status(StatusCode::CONFLICT))?;
            Ok(Response::builder()
                .status(if exists { StatusCode::NO_CONTENT } else { StatusCode::CREATED })
                .header("oc-fileid", file_id(&to_rel))
                .body(Body::empty())
                .unwrap())
        }
        "POST" => tus_create(s, &rel, req).await,
        "OPTIONS" => Ok(Response::builder().status(StatusCode::NO_CONTENT).header("dav", "1, 3, extended-mkcol").header("tus-resumable", "1.0.0").body(Body::empty()).unwrap()),
        _ => Err(status(StatusCode::METHOD_NOT_ALLOWED)),
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::metadata(from)?.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// A file, whole or the byte range asked for (video players seek with ranges).
async fn send_file(file: &Path, h: &HeaderMap) -> Result<Res, Res> {
    let mut f = tokio::fs::File::open(file).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
    let m = f.metadata().await.map_err(io)?;
    let len = m.len();
    let mime = mime_guess::from_path(file).first_or_octet_stream();
    let b = Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag(&m))
        .header(header::LAST_MODIFIED, httpdate::fmt_http_date(m.modified().unwrap_or(UNIX_EPOCH)));
    let range = h.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes=")).and_then(|v| v.split_once('-'));
    let (start, end) = match range {
        Some((a, b)) if !a.is_empty() => (a.parse::<u64>().ok(), b.parse::<u64>().ok().unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1))),
        Some((_, b)) => (b.parse::<u64>().ok().map(|n| len.saturating_sub(n)), len.saturating_sub(1)),
        None => (None, len.saturating_sub(1)),
    };
    match start {
        Some(start) if range.is_some() => {
            if start >= len || start > end {
                return Err(Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{len}")).body(Body::empty()).unwrap());
            }
            f.seek(std::io::SeekFrom::Start(start)).await.map_err(io)?;
            let n = end - start + 1;
            Ok(b.status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                .header(header::CONTENT_LENGTH, n)
                .body(Body::from_stream(tokio_util::io::ReaderStream::with_capacity(f.take(n), 256 * 1024)))
                .unwrap())
        }
        _ => Ok(b.status(StatusCode::OK).header(header::CONTENT_LENGTH, len).body(Body::from_stream(tokio_util::io::ReaderStream::with_capacity(f, 256 * 1024))).unwrap()),
    }
}

// ---------------------------------------------------------------------------------------------- uploads in pieces (tus)

fn unique() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:x}-{:x}-{:x}", t, std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// An upload starts with a POST to the folder it goes into; the first piece may ride along.
async fn tus_create(s: &S, dir: &Path, req: Request) -> Result<Res, Res> {
    let h = req.headers().clone();
    let length: u64 = h.get("upload-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
    let meta = h.get("upload-metadata").and_then(|v| v.to_str().ok()).unwrap_or("");
    let name = meta
        .split(',')
        .filter_map(|kv| kv.trim().split_once(' '))
        .find(|(k, _)| *k == "filename")
        .and_then(|(_, v)| B64.decode(v.trim()).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
    // A dropped folder arrives as files named with their path below it.
    if name.starts_with('/') {
        return Err(status(StatusCode::BAD_REQUEST));
    }
    let target = dir.join(clean(&name).ok_or_else(|| status(StatusCode::BAD_REQUEST))?);
    let id = unique();
    let up = s.root.join(UPLOADS);
    tokio::fs::write(up.join(format!("{id}.json")), json!({"length": length, "target": target.to_string_lossy()}).to_string()).await.map_err(io)?;
    tokio::fs::File::create(up.join(format!("{id}.part"))).await.map_err(io)?;
    let base = base_url(s, &h);
    let res = tus_write(s, &id, 0, req).await?;
    let mut out = Response::builder().status(StatusCode::CREATED).header(header::LOCATION, format!("{base}/data/tus/{id}")).header("tus-resumable", "1.0.0");
    for k in ["upload-offset", "oc-fileid", "oc-etag", "etag"] {
        if let Some(v) = res.headers().get(k) {
            out = out.header(k, v);
        }
    }
    Ok(out.body(Body::empty()).unwrap())
}

async fn tus_chunk(s: &S, id: &str, req: Request) -> Result<Res, Res> {
    let id = id.split('/').next().unwrap_or("").to_string();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(status(StatusCode::NOT_FOUND));
    }
    let part = s.root.join(UPLOADS).join(format!("{id}.part"));
    match req.method().as_str() {
        "HEAD" => {
            let have = tokio::fs::metadata(&part).await.map_err(|_| status(StatusCode::NOT_FOUND))?.len();
            let info = tus_info(s, &id).await?;
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("upload-offset", have)
                .header("upload-length", info["length"].as_u64().unwrap_or(0))
                .header("tus-resumable", "1.0.0")
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::empty())
                .unwrap())
        }
        "PATCH" => {
            let offset: u64 = req.headers().get("upload-offset").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or_else(|| status(StatusCode::BAD_REQUEST))?;
            tus_write(s, &id, offset, req).await
        }
        "DELETE" => {
            let _ = tokio::fs::remove_file(&part).await;
            let _ = tokio::fs::remove_file(s.root.join(UPLOADS).join(format!("{id}.json"))).await;
            Ok(status(StatusCode::NO_CONTENT))
        }
        _ => Err(status(StatusCode::METHOD_NOT_ALLOWED)),
    }
}

async fn tus_info(s: &S, id: &str) -> Result<Value, Res> {
    let raw = tokio::fs::read(s.root.join(UPLOADS).join(format!("{id}.json"))).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
    serde_json::from_slice(&raw).map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR))
}

/// Append a piece at `offset`; when the last byte is in, the file moves to its place in the folder.
async fn tus_write(s: &S, id: &str, offset: u64, req: Request) -> Result<Res, Res> {
    let up = s.root.join(UPLOADS);
    let part = up.join(format!("{id}.part"));
    let info = tus_info(s, id).await?;
    let length = info["length"].as_u64().unwrap_or(0);
    let mut f = tokio::fs::OpenOptions::new().write(true).open(&part).await.map_err(|_| status(StatusCode::NOT_FOUND))?;
    let have = f.metadata().await.map_err(io)?.len();
    if offset != have {
        return Err(Response::builder().status(StatusCode::CONFLICT).header("upload-offset", have).body(Body::empty()).unwrap());
    }
    f.seek(std::io::SeekFrom::End(0)).await.map_err(io)?;
    let mut now = have;
    let mut stream = req.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| status(StatusCode::BAD_REQUEST))?;
        if now + chunk.len() as u64 > length {
            return Err(status(StatusCode::PAYLOAD_TOO_LARGE));
        }
        f.write_all(&chunk).await.map_err(io)?;
        now += chunk.len() as u64;
    }
    f.flush().await.map_err(io)?;
    drop(f);
    let mut res = Response::builder().status(StatusCode::NO_CONTENT).header("upload-offset", now).header("tus-resumable", "1.0.0");
    if now == length {
        let rel = PathBuf::from(info["target"].as_str().unwrap_or(""));
        let full = s.root.join(&rel);
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io)?;
        }
        tokio::fs::rename(&part, &full).await.map_err(io)?;
        let _ = tokio::fs::remove_file(up.join(format!("{id}.json"))).await;
        let m = tokio::fs::metadata(&full).await.map_err(io)?;
        res = res.header("oc-fileid", file_id(&rel)).header("oc-etag", etag(&m)).header(header::ETAG, etag(&m));
    }
    Ok(res.body(Body::empty()).unwrap())
}

// ---------------------------------------------------------------------------------------------------------- search

/// Search by name: every file and folder whose name contains the words typed.
async fn search(s: &S, req: Request) -> Result<Res, Res> {
    let user = person(s, req.headers()).unwrap_or_default();
    let base = base_url(s, req.headers());
    let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.map_err(|_| status(StatusCode::BAD_REQUEST))?;
    let body = String::from_utf8_lossy(&body).replace("&quot;", "\"");
    let pattern = body.split("<oc:pattern>").nth(1).and_then(|p| p.split("</oc:pattern>").next()).unwrap_or("");
    // The page sends e.g. `(name:"*word*" OR tag:"*word*")`: the first quoted value is what was typed.
    let needle = pattern.split('"').nth(1).unwrap_or(pattern).trim_matches('*').to_lowercase();
    if needle.is_empty() {
        return Ok(multistatus(String::new()));
    }
    let root = s.root.clone();
    let found = tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        let mut stack = vec![PathBuf::new()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(root.join(&dir)) else { continue };
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with('.') {
                    continue;
                }
                let Ok(m) = std::fs::metadata(e.path()) else { continue };
                let rel = dir.join(&n);
                if n.to_lowercase().contains(&needle) {
                    out.push((rel.clone(), m.clone()));
                    if out.len() >= 200 {
                        return out;
                    }
                }
                if m.is_dir() {
                    stack.push(rel);
                }
            }
        }
        out
    })
    .await
    .map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR))?;
    let n = found.len();
    let xml: String = found.iter().map(|(rel, m)| entry_xml(s, &base, &user, rel, m)).collect();
    let mut res = multistatus(xml);
    res.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("rows 0-{}/{}", n.saturating_sub(1), n)).unwrap());
    Ok(res)
}

// ----------------------------------------------------------------------------------------------------------- helpers

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().to_string()))
        .collect()
}

fn status(code: StatusCode) -> Res {
    Response::builder().status(code).body(Body::empty()).unwrap()
}

fn io(e: std::io::Error) -> Res {
    status(match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    })
}
