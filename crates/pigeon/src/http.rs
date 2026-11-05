//! Port 8080 (spec 5): the routing table of 5.1, path normalisation, the apps' error shape, the pages pigeon writes
//! itself (5.2.3), same-origin rules (5.13), the model list (5.5) and the warm hint (5.6).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::identity::{Identity, Sources, percent_decode, percent_decode_bytes};
use crate::paseo::compat;
use crate::{App, blobs, ops, shares, shell, signin, static_files, uploads, viewas};

/// Small JSON and form bodies (spec 5.13).
pub const SMALL_BODY: usize = 4096;
/// The warm map's size (spec 5.6).
pub const WARM_KEYS: usize = 1000;
const HERMIT_WARM_TIMEOUT: Duration = Duration::from_secs(10);
const MODEL_TIMEOUT: Duration = Duration::from_secs(5);
/// A body on a GET is read up to this much, then dropped (spec 5.13).
const GET_BODY_DRAIN: usize = 64 * 1024;
/// …for at most this long.
const GET_BODY_WAIT: Duration = Duration::from_secs(5);

/// The router of port 8080: one handler that applies the table of 5.1 in order, behind the stripping of every
/// client-supplied `Aviary-*` header, a fresh request id and the `server` header.
pub fn router(app: App) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(app)
        .layer(axum::middleware::from_fn(server_header))
        .layer(axum::middleware::from_fn(aviary_core::http::defaults))
        .layer(axum::middleware::from_fn(aviary_core::http::request_id))
        .layer(axum::middleware::from_fn(aviary_core::http::strip_all_aviary))
}

/// `server: pigeon/<sha12>` on every answer (spec 5.13).
async fn server_header(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    if let Ok(v) = HeaderValue::from_str(&format!("pigeon/{}", aviary_core::version::sha12())) {
        r.headers_mut().insert(header::SERVER, v);
    }
    r
}

/// The class of a path: one of the three signed-in prefixes, or a public path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// `/api`, `/api/…`.
    Api,
    /// `/paseo-ws`, `/paseo-ws/…`.
    Socket,
    /// `/ops`, `/ops/…`.
    Ops,
    /// Everything else: files and page paths.
    Public,
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

/// The class of `path`.
#[must_use]
pub fn class(path: &str) -> Class {
    if under(path, "/api") {
        Class::Api
    } else if under(path, "/paseo-ws") {
        Class::Socket
    } else if under(path, "/ops") {
        Class::Ops
    } else {
        Class::Public
    }
}

/// Whether `path` is under a signed-in prefix.
#[must_use]
pub fn is_signed_in_path(path: &str) -> bool {
    class(path) != Class::Public
}

/// Spec 5.1's URL handling: percent-decode once, refuse NUL and backslashes, resolve `.` and `..` segments (empty
/// segments collapse); `None` (404) when the path is not UTF-8 or changes class on the way.
#[must_use]
pub fn normalise(raw: &str) -> Option<String> {
    let decoded = String::from_utf8(percent_decode_bytes(raw.as_bytes())).ok()?;
    if decoded.contains('\0') || decoded.contains('\\') || !decoded.starts_with('/') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    let mut trailing = false;
    for seg in decoded.split('/').skip(1) {
        trailing = false;
        // a dot segment encoded twice (`%252e%252e`) is one once decoded: whatever reads the path next could
        // resolve it, so it is refused rather than passed on
        if matches!(percent_decode(seg).as_str(), "." | "..") && !matches!(seg, "." | "..") {
            return None;
        }
        match seg {
            "" | "." => trailing = true,
            ".." => {
                out.pop();
                trailing = true;
            }
            s => out.push(s),
        }
    }
    let mut path = format!("/{}", out.join("/"));
    if trailing && !out.is_empty() {
        path.push('/');
    }
    (class(&decoded) == class(&path)).then_some(path)
}

/// A normalised path encoded again for a URL, segment by segment: every byte outside RFC 3986's `pchar` set (so
/// `%`, `?`, `#` and spaces) is percent-encoded, so the path reaches the upstream exactly as pigeon checked it.
#[must_use]
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b);
        if keep {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A query parameter, percent-decoded (`+` as a space).
#[must_use]
pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(&k.replace('+', " ")) == name).then(|| percent_decode(&v.replace('+', " ")))
    })
}

/// The query without `name` (the view-as parameter is consumed by pigeon and never forwarded, spec 5.1).
#[must_use]
pub fn query_without(query: Option<&str>, name: &str) -> Option<String> {
    let q = query?;
    let kept: Vec<&str> = q
        .split('&')
        .filter(|pair| {
            let k = pair.split_once('=').map_or(*pair, |(k, _)| k);
            !pair.is_empty() && percent_decode(&k.replace('+', " ")) != name
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|p| p.trim().eq_ignore_ascii_case("websocket")))
}

/// The apps' error shape on `/api/*` (spec 5.1): `{"success": false, "error": {"code", "message"}}`, no-store.
#[must_use]
pub fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    json(
        status,
        &json!({"success": false, "error": {"code": code, "message": message}}),
    )
}

/// A JSON answer, no-store.
#[must_use]
pub fn json(status: StatusCode, body: &Value) -> Response {
    let mut r = aviary_core::http::json(status, body);
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A plain-text answer, no-store.
#[must_use]
pub fn text(status: StatusCode, body: &'static str) -> Response {
    let mut r = (status, body).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// The 404 of a path pigeon refuses before routing.
#[must_use]
pub fn not_found() -> Response {
    text(StatusCode::NOT_FOUND, "Not found")
}

/// A redirect with `cache-control: no-store` (and no referrer when `no_referrer`).
#[must_use]
pub fn redirect(status: StatusCode, location: &str, no_referrer: bool) -> Response {
    let mut r = Response::builder().status(status);
    if let Ok(v) = HeaderValue::from_str(location) {
        r = r.header(header::LOCATION, v);
    }
    r = r.header(header::CACHE_CONTROL, "no-store");
    if no_referrer {
        r = r.header(header::REFERRER_POLICY, "no-referrer");
    }
    r.body(Body::empty()).unwrap_or_else(|_| not_found())
}

/// The pages pigeon writes itself (spec 5.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 401.
    SignedOut,
    /// 403 for a person who may not use the deployment.
    NotAllowed,
    /// 403 on an admin-only page.
    AdminsOnly,
    /// 405.
    ReadOnly,
    /// 502 from the operator page.
    OpsUnavailable,
    /// 503.
    Unavailable,
    /// 400 for a view-as start naming someone who is not a user (spec 6.1).
    NotAUser,
    /// 502 when the view-as start could not be recorded (spec 6.1).
    NotRecorded,
    /// 404 for a feature this deployment has turned off (app sign-in without an `appScheme`, spec 5.3.3).
    NotAvailable,
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A page of 5.2.3: both languages (zh first), `lang` from the locale, no-store.
#[must_use]
pub fn page(app: &App, which: Page, email: Option<&str>) -> Response {
    let product = app.0.identity.product_name();
    let email = email.unwrap_or("");
    let (status, zh_t, zh_p, en_t, en_p): (StatusCode, &str, String, &str, String) = match which {
        Page::SignedOut => (
            StatusCode::UNAUTHORIZED,
            "未登入",
            "請重新整理頁面登入。".into(),
            "Not signed in",
            "Reload the page to sign in.".into(),
        ),
        Page::NotAllowed => (
            StatusCode::FORBIDDEN,
            "沒有權限",
            format!("{email} 沒有使用 {product} 的權限。"),
            "Not allowed",
            format!("This account ({email}) may not use {product}."),
        ),
        Page::AdminsOnly => (
            StatusCode::FORBIDDEN,
            "只限管理員",
            "只有管理員可以開啟這個頁面。".into(),
            "Admins only",
            "Only an admin can open this page.".into(),
        ),
        Page::ReadOnly => (
            StatusCode::METHOD_NOT_ALLOWED,
            "唯讀",
            "這個頁面只可閱讀。".into(),
            "Read-only",
            "This page is read-only.".into(),
        ),
        Page::OpsUnavailable => (
            StatusCode::BAD_GATEWAY,
            "暫時未能使用",
            "控制台沒有回應，請稍後再試。".into(),
            "Unavailable",
            "The operator page did not answer. Try again shortly.".into(),
        ),
        Page::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "暫時未能使用",
            "系統繁忙，請稍後再試。".into(),
            "Unavailable",
            "The service is busy. Try again shortly.".into(),
        ),
        Page::NotAUser => (
            StatusCode::BAD_REQUEST,
            "不是用戶",
            "這個地址不是此服務的用戶。".into(),
            "Not a user",
            "That address is not a user of this instance.".into(),
        ),
        Page::NotRecorded => (
            StatusCode::BAD_GATEWAY,
            "未能開啟",
            "未能記錄這次代看，所以沒有開啟。請稍後再試。".into(),
            "Not opened",
            "The view could not be recorded, so it was not opened. Try again in a moment.".into(),
        ),
        Page::NotAvailable => (
            StatusCode::NOT_FOUND,
            "未能使用",
            "這項功能在這裏未有開放。".into(),
            "Not available",
            "This feature is not available here.".into(),
        ),
    };
    let lang = if app.0.identity.is_zh() { "zh-HK" } else { "en" };
    let html = format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\"><title>{} / {}</title></head><body>\
         <h1>{}</h1><p>{}</p><h1>{}</h1><p>{}</p></body></html>",
        escape(zh_t),
        escape(en_t),
        escape(zh_t),
        escape(&zh_p),
        escape(en_t),
        escape(&en_p),
    );
    let mut r = (status, html).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if which == Page::Unavailable {
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    }
    r
}

/// Spec 5.13's same-origin rule for state-changing requests: `Sec-Fetch-Site` `same-origin` or `none`, or without
/// that header, no `Origin` (native iOS) or an `Origin` whose host is `Host`.
#[must_use]
pub fn same_origin(headers: &HeaderMap, allow_none: bool) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        let site = site.trim().to_ascii_lowercase();
        return site == "same-origin" || (allow_none && site == "none");
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return allow_none;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/');
    !host.is_empty() && origin_host.eq_ignore_ascii_case(host)
}

/// The `/api` routes of 5.1 (rows 3-16), by method and normalised path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApiRoute {
    SignIn,
    AppSignIn,
    Me,
    ViewAsStart,
    Warm,
    Models,
    Upload,
    FilesRaw(String),
    Transcript(String),
    Blob(String),
    SessionShares(String),
    Share(String),
    ShareFiles(String),
    ShareContinue(String),
    Unknown,
}

impl ApiRoute {
    fn of(method: &Method, path: &str) -> Self {
        let get = *method == Method::GET;
        let get_head = get || *method == Method::HEAD;
        let post = *method == Method::POST;
        match path {
            p if p == compat::SIGNIN && get => return Self::SignIn,
            // app-signin answers its own 405 (spec 5.3.3), view-as its 405 page (6.1)
            p if p == compat::APP_SIGNIN => return Self::AppSignIn,
            p if p == compat::ME && get => return Self::Me,
            p if p == compat::VIEW_AS => return Self::ViewAsStart,
            p if p == compat::WARM && post => return Self::Warm,
            "/api/providers/claude/models" if get => return Self::Models,
            "/api/assets/files" if post => return Self::Upload,
            _ => {}
        }
        if let Some(rest) = path.strip_prefix("/api/blobs/")
            && !rest.is_empty()
            && !rest.contains('/')
            && get
        {
            return Self::Blob(rest.to_owned());
        }
        if let Some(rest) = path.strip_prefix(compat::SESSIONS) {
            if let Some(id) = rest.strip_suffix("/files/raw")
                && !id.contains('/')
                && get_head
            {
                return Self::FilesRaw(id.to_owned());
            }
            if let Some(id) = rest.strip_suffix("/transcript")
                && !id.contains('/')
                && get
            {
                return Self::Transcript(id.to_owned());
            }
            if let Some(id) = rest.strip_suffix("/shares")
                && !id.contains('/')
                && (get || post)
            {
                return Self::SessionShares(id.to_owned());
            }
        }
        if let Some(rest) = path.strip_prefix(compat::SHARES) {
            if let Some(id) = rest.strip_suffix("/files/raw")
                && !id.contains('/')
                && get_head
            {
                return Self::ShareFiles(id.to_owned());
            }
            if let Some(id) = rest.strip_suffix("/continue")
                && !id.contains('/')
                && post
            {
                return Self::ShareContinue(id.to_owned());
            }
            if !rest.is_empty() && !rest.contains('/') && (get || *method == Method::DELETE) {
                return Self::Share(rest.to_owned());
            }
        }
        Self::Unknown
    }

    fn name(&self) -> &'static str {
        match self {
            Self::SignIn => "signin",
            Self::AppSignIn => "app_signin",
            Self::Me => "me",
            Self::ViewAsStart => "view_as",
            Self::Warm => "warm",
            Self::Models => "models",
            Self::Upload => "upload",
            Self::FilesRaw(_) => "files_raw",
            Self::Transcript(_) => "transcript",
            Self::Blob(_) => "blob",
            Self::SessionShares(_) => "shares",
            Self::Share(_) => "share",
            Self::ShareFiles(_) => "share_files",
            Self::ShareContinue(_) => "share_continue",
            Self::Unknown => "unknown",
        }
    }

    /// The writes a view-as identity may make (spec 6.3).
    fn allowed_in_view_as(&self, method: &Method) -> bool {
        *method == Method::GET || *method == Method::HEAD || matches!(self, Self::Warm | Self::Upload)
    }
}

/// The identity failure of rows 2-18 on `/api` (spec 5.1); `None` for a usable identity.
fn api_identity_failure(id: &Identity) -> Option<Response> {
    Some(match id {
        Identity::SignedOut => api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in."),
        Identity::NotAllowed(_) => api_error(
            StatusCode::FORBIDDEN,
            "NOT_ALLOWED",
            "This account may not use this service.",
        ),
        Identity::ViewAsRefused => api_error(
            StatusCode::FORBIDDEN,
            "VIEW_AS_REFUSED",
            "This view-as link is not valid for this account, or has expired.",
        ),
        Identity::Unavailable => {
            let mut r = api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "UNAVAILABLE",
                "The service is busy. Try again shortly.",
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            r
        }
        Identity::Person(_) | Identity::ViewAs(_) => return None,
    })
}

/// 403 `VIEW_AS_READ_ONLY` (spec 6.3).
#[must_use]
pub fn view_as_read_only() -> Response {
    api_error(
        StatusCode::FORBIDDEN,
        "VIEW_AS_READ_ONLY",
        "Read-only while viewing as someone else.",
    )
}

async fn dispatch(State(app): State<App>, req: Request) -> Response {
    let (req, had_body) = drain_get_body(req).await;
    let (route, mut r) = route(&app, req).await;
    if had_body {
        r.headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    app.0.metrics.http(route, r.status().as_u16());
    r
}

/// Spec 5.13: a body on a GET or HEAD (other than a socket upgrade) is ignored: up to 64 KiB of it is read and
/// dropped, the request is routed without it, and the connection is closed after the answer.
async fn drain_get_body(req: Request) -> (Request, bool) {
    let get_head = req.method() == Method::GET || req.method() == Method::HEAD;
    let has_body = req.headers().contains_key(header::TRANSFER_ENCODING)
        || req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() != "0");
    if !get_head || !has_body || is_upgrade(req.headers()) {
        return (req, false);
    }
    let (parts, body) = req.into_parts();
    let mut data = body.into_data_stream();
    let mut read = 0usize;
    let drain = async {
        while read < GET_BODY_DRAIN {
            match futures_util::StreamExt::next(&mut data).await {
                Some(Ok(chunk)) => read += chunk.len(),
                Some(Err(_)) | None => break,
            }
        }
    };
    let _ = tokio::time::timeout(GET_BODY_WAIT, drain).await;
    (Request::from_parts(parts, Body::empty()), true)
}

async fn route(app: &App, req: Request) -> (&'static str, Response) {
    let Some(path) = normalise(req.uri().path()) else {
        return ("bad_path", not_found());
    };
    let upgrade = is_upgrade(req.headers());
    if upgrade && path != "/paseo-ws" {
        return ("upgrade", not_found());
    }
    match class(&path) {
        Class::Api | Class::Socket => api(app, req, &path, upgrade).await,
        Class::Ops => ("ops", ops::handle(app, req, &path).await),
        Class::Public => public(app, req, &path).await,
    }
}

async fn api(app: &App, req: Request, path: &str, upgrade: bool) -> (&'static str, Response) {
    let method = req.method().clone();
    let get_head = method == Method::GET || method == Method::HEAD;
    if !upgrade && get_head && matches!(path, "/api" | "/api/" | "/paseo-ws") {
        return ("api_root", redirect(StatusCode::FOUND, "/", false));
    }
    let query = req.uri().query().map(str::to_owned);
    let view_as_token = query_param(query.as_deref(), compat::VIEW_AS_QUERY);
    if path == "/paseo-ws" && upgrade {
        if method != Method::GET {
            return ("socket", text(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed"));
        }
        let id = app
            .0
            .identity
            .resolve(req.headers(), Sources::All, view_as_token.as_deref())
            .await;
        let r = match &id {
            Identity::Person(_) | Identity::ViewAs(_) => app.0.socket.upgrade(app.clone(), req, id).await,
            Identity::SignedOut => text(StatusCode::UNAUTHORIZED, "Unauthorized"),
            Identity::NotAllowed(_) | Identity::ViewAsRefused => text(StatusCode::FORBIDDEN, "Forbidden"),
            Identity::Unavailable => text(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable"),
        };
        return ("socket", r);
    }
    if method == Method::OPTIONS {
        return (
            "options",
            api_error(
                StatusCode::METHOD_NOT_ALLOWED,
                "METHOD_NOT_ALLOWED",
                "Method not allowed.",
            ),
        );
    }
    let route = if class(path) == Class::Api {
        ApiRoute::of(&method, path)
    } else {
        ApiRoute::Unknown
    };
    let name = route.name();
    // sign-in routes take the caller's own identity: a view-as token there is ignored (spec 5.3.3)
    let view_as = match route {
        ApiRoute::SignIn | ApiRoute::AppSignIn => None,
        _ => view_as_token.as_deref(),
    };
    let id = app.0.identity.resolve(req.headers(), Sources::All, view_as).await;
    if route == ApiRoute::ViewAsStart {
        return (name, viewas::start(app, req, &id).await);
    }
    let (parts, body) = req.into_parts();
    if route == ApiRoute::AppSignIn {
        return (name, signin::app_signin(app, &parts, &id));
    }
    if let Some(r) = api_identity_failure(&id) {
        return (name, r);
    }
    if matches!(id, Identity::ViewAs(_)) && !route.allowed_in_view_as(&method) {
        return (name, view_as_read_only());
    }
    if (method == Method::POST || method == Method::DELETE) && !same_origin(&parts.headers, true) {
        return (
            name,
            api_error(
                StatusCode::FORBIDDEN,
                "CROSS_ORIGIN",
                "Cross-origin requests are refused.",
            ),
        );
    }
    let r = match route {
        ApiRoute::SignIn => signin::signin(query.as_deref()),
        ApiRoute::Me => signin::me(app, &id).await,
        ApiRoute::Warm => warm(app, body, &id).await,
        ApiRoute::Models => models(app).await,
        ApiRoute::Upload => uploads::upload(app, Request::from_parts(parts, body), &id).await,
        ApiRoute::FilesRaw(cid) => blobs::files_raw(app, &parts, &id, &cid, query.as_deref()).await,
        ApiRoute::Transcript(cid) => crate::raw::transcript(app, &id, &cid, query.as_deref()).await,
        ApiRoute::Blob(blob) => blobs::blob(app, &parts, &blob, query.as_deref()).await,
        ApiRoute::SessionShares(cid) => shares::conversation_shares(app, &parts, &id, &cid).await,
        ApiRoute::Share(sid) => shares::share(app, &parts, &id, &sid).await,
        ApiRoute::ShareFiles(sid) => shares::share_files(app, &parts, &id, &sid, query.as_deref()).await,
        ApiRoute::ShareContinue(sid) => shares::continue_share(app, &parts, &id, &sid).await,
        ApiRoute::Unknown | ApiRoute::ViewAsStart | ApiRoute::AppSignIn => api_error(
            StatusCode::NOT_FOUND,
            "NOT_AVAILABLE",
            "This feature is not available here.",
        ),
    };
    (name, r)
}

async fn public(app: &App, req: Request, path: &str) -> (&'static str, Response) {
    let method = req.method();
    if *method != Method::GET && *method != Method::HEAD {
        return ("method", page(app, Page::ReadOnly, None));
    }
    if path.starts_with(crate::raw::PAGE_PREFIX) {
        return ("raw", crate::raw::serve_page(app, req.headers(), path).await);
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        return ("file", static_files::serve(app, path, req.headers()).await);
    }
    ("shell", shell::serve(app, req.headers()).await)
}

/// magpie's person-scoped calls for this identity (the viewer headers in view-as), forwarding the request id.
pub(crate) fn person_calls<'a>(
    client: &'a magpie_api::client::Client,
    id: &Identity,
) -> Option<magpie_api::client::PersonCalls<'a>> {
    let calls = match id {
        Identity::Person(p) => client.person(&p.email),
        Identity::ViewAs(v) => client.viewing_as(&v.person, &v.admin.email, &v.session),
        _ => return None,
    };
    Some(match aviary_core::http::current_request_id() {
        Some(rid) => calls.with_request_id(rid.to_string()),
        None => calls,
    })
}

/// Reads a small body (spec 5.13's per-route limits); `None` when it is larger.
pub async fn small_body(body: Body, limit: usize) -> Option<axum::body::Bytes> {
    axum::body::to_bytes(body, limit).await.ok()
}

/// The warm coalescing map (spec 5.6). Cheap to clone.
#[derive(Debug, Clone)]
pub struct Warm(std::sync::Arc<WarmInner>);

#[derive(Debug)]
pub struct WarmInner {
    every: Duration,
    sent: Mutex<HashMap<(String, Option<Uuid>), Instant>>,
}

impl Warm {
    pub(crate) fn new(every: Duration) -> Self {
        Self(std::sync::Arc::new(WarmInner {
            every,
            sent: Mutex::new(HashMap::new()),
        }))
    }

    /// Whether a hint for this key goes out now (and records it).
    fn take(&self, person: &str, conversation: Option<Uuid>) -> bool {
        let Ok(mut m) = self.0.sent.lock() else { return false };
        let key = (person.to_owned(), conversation);
        if m.get(&key).is_some_and(|at| at.elapsed() < self.0.every) {
            return false;
        }
        m.insert(key, Instant::now());
        if m.len() > WARM_KEYS
            && let Some(oldest) = m.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone())
        {
            m.remove(&oldest);
        }
        true
    }
}

/// The warm hint's conversation: `Ok(None)` for a new chat (no body, no `sessionId`, or `null`), `Ok(Some(id))` for
/// a valid one, `Err` (no hint) for a `sessionId` that is not a conversation id or a body that is not a JSON object.
fn warm_target(body: Option<&[u8]>) -> Result<Option<Uuid>, ()> {
    let body = body.ok_or(())?;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| ())?;
    match v.as_object().ok_or(())?.get("sessionId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Uuid::parse_str(s).map(Some).map_err(|_| ()),
        Some(_) => Err(()),
    }
}

async fn warm(app: &App, body: Body, id: &Identity) -> Response {
    let ok = json(StatusCode::ACCEPTED, &json!({"ok": true}));
    let Some(person) = id.subject().map(str::to_owned) else {
        return ok;
    };
    // the move's input (DESIGN 9.3): who is typing
    app.legacy().0.activity.warm(&person);
    let raw = small_body(body, SMALL_BODY).await;
    let Ok(conversation) = warm_target(raw.as_deref()) else {
        return ok;
    };
    // a legacy conversation before the gate: the old stack's warm, under a ticket
    if let (Some(c), Some(raw)) = (conversation, raw.clone())
        && app.legacy().warm(app, id, c, raw).await
    {
        return ok;
    }
    if app.0.warm.take(&person, conversation) {
        let hermit = app.0.hermit.clone();
        tokio::spawn(async move {
            let body = hermit_api::people::WarmRequest {
                person,
                conversation_id: conversation,
            };
            match tokio::time::timeout(HERMIT_WARM_TIMEOUT, hermit.warm(&body)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::debug!(error = %e, "warm hint failed"),
                Err(_) => tracing::debug!("warm hint timed out"),
            }
        });
    }
    ok
}

/// After a failed refresh of a cached model list, the next refresh starts no sooner than this.
const MODEL_RETRY: Duration = Duration::from_secs(5);

/// The model list cache (spec 5.5): one refresh at a time, run in the background; a stale list is answered at once
/// while it runs or after it failed. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Models(std::sync::Arc<ModelsInner>);

#[derive(Debug)]
struct ModelsInner {
    st: Mutex<ModelsState>,
    /// Bumped when a refresh ends; a request with nothing cached waits for it.
    done: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct ModelsState {
    value: Option<(Instant, Value)>,
    refreshing: bool,
    failed_at: Option<Instant>,
}

impl Default for Models {
    fn default() -> Self {
        Self(std::sync::Arc::new(ModelsInner {
            st: Mutex::new(ModelsState::default()),
            done: tokio::sync::watch::channel(0).0,
        }))
    }
}

async fn models(app: &App) -> Response {
    match models_value(app).await {
        Some(v) => answer_models(&v),
        None => models_unavailable(),
    }
}

/// The cached model list without waiting (a stale one too), starting a refresh when it is stale or missing.
pub(crate) fn models_cached(app: &App) -> Option<Value> {
    let mut st = app.0.models.0.st.lock().ok()?;
    let fresh = st
        .value
        .as_ref()
        .is_some_and(|(at, _)| at.elapsed() < app.0.tuning.model_ttl);
    if !fresh && !st.refreshing && st.failed_at.is_none_or(|t| t.elapsed() >= MODEL_RETRY) {
        st.refreshing = true;
        tokio::spawn(refresh_models(app.clone()));
    }
    st.value.as_ref().map(|(_, v)| v.clone())
}

/// The model list (5.5): `{"OPTIONS": [{value, label}], "DEFAULT": id}`, from the cache (60 s), refreshed in the
/// background; with nothing cached a caller waits for the refresh (at most its timeout). `None` when `model` cannot be
/// reached and nothing is cached.
pub(crate) async fn models_value(app: &App) -> Option<Value> {
    let mut ended = {
        let Ok(mut st) = app.0.models.0.st.lock() else {
            return None;
        };
        if let Some((at, v)) = &st.value
            && at.elapsed() < app.0.tuning.model_ttl
        {
            return Some(v.clone());
        }
        // a stale list waits out the retry pause after a failure; with nothing cached every request may try
        let may_start =
            !st.refreshing && (st.value.is_none() || st.failed_at.is_none_or(|t| t.elapsed() >= MODEL_RETRY));
        if may_start {
            st.refreshing = true;
            tokio::spawn(refresh_models(app.clone()));
        }
        if let Some((_, v)) = &st.value {
            return Some(v.clone());
        }
        app.0.models.0.done.subscribe()
    };
    let _ = tokio::time::timeout(MODEL_TIMEOUT + Duration::from_secs(1), ended.changed()).await;
    app.0
        .models
        .0
        .st
        .lock()
        .ok()
        .and_then(|st| st.value.as_ref().map(|(_, v)| v.clone()))
}

async fn refresh_models(app: App) {
    let fetched = tokio::time::timeout(
        MODEL_TIMEOUT,
        gateway_api::models::fetch(
            &app.0.model_http,
            &app.0.upstreams.model,
            Some(&app.0.secrets.token_model),
        ),
    )
    .await;
    let value = match fetched {
        Ok(Ok(m)) => {
            let options: Vec<Value> = m
                .models
                .iter()
                .map(|e| {
                    let label = if e.label.trim().is_empty() { &e.id } else { &e.label };
                    json!({"value": e.id, "label": label})
                })
                .collect();
            Some(json!({"OPTIONS": options, "DEFAULT": m.default}))
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "model list refresh failed");
            None
        }
        Err(_) => {
            tracing::warn!("model list refresh timed out");
            None
        }
    };
    if let Ok(mut st) = app.0.models.0.st.lock() {
        st.refreshing = false;
        match value {
            Some(v) => {
                st.value = Some((Instant::now(), v));
                st.failed_at = None;
            }
            None => st.failed_at = Some(Instant::now()),
        }
    }
    app.0.models.0.done.send_modify(|n| *n += 1);
}

fn models_unavailable() -> Response {
    json(
        StatusCode::BAD_GATEWAY,
        &json!({"success": false, "error": {"code": "MODEL_LIST_UNAVAILABLE"}}),
    )
}

fn answer_models(models: &Value) -> Response {
    json(StatusCode::OK, &json!({"success": true, "data": {"models": models}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation() {
        assert_eq!(normalise("/").as_deref(), Some("/"));
        assert_eq!(normalise("/a/./b/../c").as_deref(), Some("/a/c"));
        assert_eq!(normalise("/ops/").as_deref(), Some("/ops/"));
        assert_eq!(normalise("/ops/x/..").as_deref(), Some("/ops/"));
        assert_eq!(normalise("/ops/../api/x"), None, "changes class");
        assert_eq!(normalise("/x/../api/me"), None, "changes class");
        assert_eq!(normalise("/%2e%2e/api"), None, "changes class");
        assert_eq!(
            normalise("/ops/%252e%252e/v1/deployment"),
            None,
            "a dot segment encoded twice"
        );
        assert_eq!(normalise("/ops/%252E/x"), None, "upper-case too");
        assert_eq!(normalise("/ops/a%3Fb").as_deref(), Some("/ops/a?b"));
        assert_eq!(encode_path("/ops/a?b#c d%2e"), "/ops/a%3Fb%23c%20d%252e");
        assert_eq!(encode_path("/ops/people/a@b.c/~x"), "/ops/people/a@b.c/~x");
        assert_eq!(normalise("/a%00b"), None);
        let id = Uuid::new_v4();
        assert_eq!(warm_target(Some(b"")), Ok(None));
        assert_eq!(warm_target(Some(b"{}")), Ok(None));
        assert_eq!(warm_target(Some(br#"{"sessionId":null}"#)), Ok(None));
        assert_eq!(
            warm_target(Some(format!(r#"{{"sessionId":"{id}"}}"#).as_bytes())),
            Ok(Some(id))
        );
        assert_eq!(warm_target(Some(br#"{"sessionId":"x"}"#)), Err(()));
        assert_eq!(warm_target(Some(b"[1]")), Err(()));
        assert_eq!(warm_target(None), Err(()), "over 4 KiB");
        assert_eq!(normalise("/a%5cb"), None);
        assert_eq!(normalise("/a\\b"), None);
        assert_eq!(normalise("/api//x").as_deref(), Some("/api/x"));
        assert_eq!(normalise("/s/abc%20d").as_deref(), Some("/s/abc d"));
        assert_eq!(normalise("/%ff"), None, "not UTF-8");
    }

    #[test]
    fn classes() {
        assert_eq!(class("/api"), Class::Api);
        assert_eq!(class("/api/x"), Class::Api);
        assert_eq!(class("/apix"), Class::Public);
        assert_eq!(class("/paseo-ws"), Class::Socket);
        assert_eq!(class("/ops"), Class::Ops);
        assert_eq!(class("/opsx"), Class::Public);
    }

    #[test]
    fn queries() {
        let q = format!("a=1&{}=t%2Bk&b=x+y", compat::VIEW_AS_QUERY);
        let q = Some(q.as_str());
        assert_eq!(query_param(q, "b").as_deref(), Some("x y"));
        assert_eq!(query_param(q, compat::VIEW_AS_QUERY).as_deref(), Some("t+k"));
        assert_eq!(query_without(q, compat::VIEW_AS_QUERY).as_deref(), Some("a=1&b=x+y"));
        assert_eq!(
            query_without(Some(&format!("{}=1", compat::VIEW_AS_QUERY)), compat::VIEW_AS_QUERY),
            None
        );
    }

    #[test]
    fn origins() {
        let mut h = HeaderMap::new();
        assert!(same_origin(&h, true), "native: no Origin");
        assert!(!same_origin(&h, false));
        h.insert(header::HOST, "app.example.org".parse().unwrap());
        h.insert(header::ORIGIN, "https://app.example.org".parse().unwrap());
        assert!(same_origin(&h, false));
        h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!same_origin(&h, true));
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(same_origin(&h, true));
        h.insert("sec-fetch-site", "none".parse().unwrap());
        assert!(same_origin(&h, true));
        assert!(!same_origin(&h, false));
        h.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!same_origin(&h, true));
    }

    #[test]
    fn api_routes() {
        let r = |m: Method, p: &str| ApiRoute::of(&m, p);
        assert_eq!(r(Method::GET, compat::ME), ApiRoute::Me);
        assert_eq!(r(Method::POST, compat::ME), ApiRoute::Unknown);
        assert_eq!(
            r(Method::GET, &format!("{}abc/files/raw", compat::SESSIONS)),
            ApiRoute::FilesRaw("abc".into())
        );
        assert_eq!(
            r(Method::DELETE, &format!("{}s1", compat::SHARES)),
            ApiRoute::Share("s1".into())
        );
        assert_eq!(
            r(Method::POST, &format!("{}s1/continue", compat::SHARES)),
            ApiRoute::ShareContinue("s1".into())
        );
        assert_eq!(r(Method::GET, "/api/blobs/b1"), ApiRoute::Blob("b1".into()));
        assert_eq!(r(Method::GET, "/api/blobs/b1/x"), ApiRoute::Unknown);
        assert_eq!(r(Method::GET, "/api/unknown"), ApiRoute::Unknown);
    }
}
