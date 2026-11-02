//! The `Server` trait (every hermit route as a method) and two routers that serve an implementation of it over
//! HTTP with this crate's route constants and types. The fakes implement the trait (WP-08); a real implementation
//! may too. Every method defaults to 501 `not_supported`, the normal answer for a call an implementation lacks
//! (hermit 3.1).
//!
//! - [`guarded_router`] enforces hermit 3.2 itself: the caller's pair token (401 `unauthenticated`), the
//!   route-to-caller table [`routes::RULES`] for the matched route (403 `wrong_caller`), the `Aviary-*` headers that
//!   caller may set (others removed), and `Aviary-Protocol` (400 `unsupported_protocol`). Handlers see the admitted
//!   caller in [`Ctx::caller`]. hermit and every fake serve this one.
//! - [`router`] applies none of that (a conformance test driving the routes with no tokens).

use std::future::Future;
use std::sync::Arc;

use aviary_core::auth::{Caller, Callers};
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, MatchedPath, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use crate::deployment::Deployment;
use crate::error::{self, ApiError, codes};
use crate::exec::{ExecRequest, ExecResult};
use crate::headers;
use crate::ops::SnapshotQuery;
use crate::people::{People, Person, WarmReply, WarmRequest};
use crate::routes;
use crate::sandbox::{
    PutSandbox, PutSandboxRequest, ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents,
    SandboxEventsQuery, SandboxQuery, SnapshotReply,
};
use crate::sandbox_surface::{ActivityReply, ActivityReport, SandboxSelf, SecretKeys, SecretValue};
use crate::token::{TokenKeys, TokenLiveness};
use crate::workspaces::{
    ByteRange, CloneRequest, Entries, EntriesQuery, FileContent, FilesQuery, PutWorkspace, Workspace,
};
use crate::{CORE, SANDBOX, Surface, accepts};

/// The largest JSON request body (hermit 3.1): larger is 413 `too_large`.
pub const MAX_JSON_BODY: usize = 1024 * 1024;
/// The largest body of `POST /v1/admin/sandboxes/{id}/exec`, which carries supervisor 15.4's exec body unchanged:
/// stdin of up to 1 MiB is about 1.4 MiB of base64 in JSON, so this route takes what the supervisor's own listener
/// takes (`supervisor_api::server::MAX_BODY`) rather than hermit 3.1's 1 MiB.
pub const MAX_EXEC_BODY: usize = 2 * 1024 * 1024;

/// The verified sandbox behind a forwarded `/v1/sandbox/**` call (the gateway's three headers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxIdentity {
    pub id: Uuid,
    pub conversation: Uuid,
    pub person: String,
}

/// What a handler knows about the request beyond its typed inputs. Its `Debug` form redacts every credential
/// (`Authorization`, `Proxy-Authorization`, `Cookie`) and every `Aviary-*` value but `Aviary-Request` and
/// `Aviary-Protocol`, so a `?ctx` in a log line never writes a token or an identity.
#[derive(Clone)]
pub struct Ctx {
    pub method: Method,
    /// The matched route constant.
    pub route: &'static str,
    pub headers: HeaderMap,
    /// The caller [`guarded_router`] admitted (`None` under [`router`]).
    pub caller: Option<Caller>,
}

/// Whether a header's value stays out of `Debug` output.
#[must_use]
pub fn is_sensitive_header(name: &HeaderName) -> bool {
    name == header::AUTHORIZATION
        || name == header::PROXY_AUTHORIZATION
        || name == header::COOKIE
        || name == header::SET_COOKIE
        || (aviary_core::http::headers::is_aviary(name) && name != headers::REQUEST && name != headers::PROTOCOL)
}

/// A header map for `Debug`: names with their values, sensitive ones as `<redacted>`.
pub struct RedactedHeaders<'a>(pub &'a HeaderMap);

impl std::fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut m = f.debug_map();
        for (n, v) in self.0 {
            if is_sensitive_header(n) {
                m.entry(&n.as_str(), &"<redacted>");
            } else {
                m.entry(&n.as_str(), &v.to_str().unwrap_or("<binary>"));
            }
        }
        m.finish()
    }
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("method", &self.method)
            .field("route", &self.route)
            .field("caller", &self.caller)
            .field("headers", &RedactedHeaders(&self.headers))
            .finish()
    }
}

impl Ctx {
    #[must_use]
    pub fn new(method: Method, route: &'static str, headers: HeaderMap) -> Self {
        Self {
            method,
            route,
            headers,
            caller: None,
        }
    }

    /// The same context with the admitted caller.
    #[must_use]
    pub fn with_caller(mut self, caller: Option<Caller>) -> Self {
        self.caller = caller;
        self
    }

    fn header(&self, name: &HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The `Authorization: Bearer` token.
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        self.header(&header::AUTHORIZATION)
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.header(&headers::REQUEST)
    }

    /// `Aviary-Protocol` parsed as `(surface, number)`.
    #[must_use]
    pub fn protocol(&self) -> Option<(&str, u32)> {
        let (surface, n) = self.header(&headers::PROTOCOL)?.split_once('=')?;
        Some((surface.trim(), n.trim().parse().ok()?))
    }

    #[must_use]
    pub fn person(&self) -> Option<&str> {
        self.header(&headers::PERSON)
    }

    #[must_use]
    pub fn admin(&self) -> Option<&str> {
        self.header(&headers::ADMIN)
    }

    /// The three `Aviary-Sandbox-*` headers, when all are present and well-formed.
    #[must_use]
    pub fn sandbox(&self) -> Option<SandboxIdentity> {
        Some(SandboxIdentity {
            id: self.header(&headers::SANDBOX_ID)?.parse().ok()?,
            conversation: self.header(&headers::SANDBOX_CONVERSATION)?.parse().ok()?,
            person: self.header(&headers::SANDBOX_PERSON)?.to_owned(),
        })
    }
}

/// `GET /v1/sandbox/self`: the descriptor, or 304 when `If-None-Match` names the current etag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelfAnswer {
    Fresh(Box<SandboxSelf>),
    NotModified { etag: String },
}

/// The result type of every method.
pub type Answer<T> = Result<T, ApiError>;

fn unsupported<T: Send>() -> impl Future<Output = Answer<T>> + Send {
    std::future::ready(Err(error::not_supported()))
}

/// Every hermit route (hermit 3, 16, 26). Default: 501 `not_supported`.
#[allow(unused_variables)]
pub trait Server: Send + Sync + 'static {
    // H1-H3 and people (3.3-3.5)
    fn deployment(&self, ctx: Ctx) -> impl Future<Output = Answer<Deployment>> + Send {
        unsupported()
    }
    fn person(&self, ctx: Ctx, email: String) -> impl Future<Output = Answer<Person>> + Send {
        unsupported()
    }
    fn people(&self, ctx: Ctx) -> impl Future<Output = Answer<People>> + Send {
        unsupported()
    }
    fn warm(&self, ctx: Ctx, body: WarmRequest) -> impl Future<Output = Answer<WarmReply>> + Send {
        unsupported()
    }
    // sandboxes (3.6)
    fn put_sandbox(
        &self,
        ctx: Ctx,
        cid: Uuid,
        body: PutSandboxRequest,
    ) -> impl Future<Output = Answer<PutSandbox>> + Send {
        unsupported()
    }
    fn get_sandbox(&self, ctx: Ctx, cid: Uuid, q: SandboxQuery) -> impl Future<Output = Answer<Sandbox>> + Send {
        unsupported()
    }
    fn release_sandbox(
        &self,
        ctx: Ctx,
        cid: Uuid,
        body: ReleaseSandboxRequest,
    ) -> impl Future<Output = Answer<ReleaseSandboxReply>> + Send {
        unsupported()
    }
    /// 202.
    fn pause_sandbox(&self, ctx: Ctx, cid: Uuid) -> impl Future<Output = Answer<Sandbox>> + Send {
        unsupported()
    }
    /// 202.
    fn resume_sandbox(&self, ctx: Ctx, cid: Uuid) -> impl Future<Output = Answer<Sandbox>> + Send {
        unsupported()
    }
    /// 201.
    fn snapshot_sandbox(&self, ctx: Ctx, cid: Uuid) -> impl Future<Output = Answer<SnapshotReply>> + Send {
        unsupported()
    }
    fn sandbox_events(&self, ctx: Ctx, q: SandboxEventsQuery) -> impl Future<Output = Answer<SandboxEvents>> + Send {
        unsupported()
    }
    // workspaces (3.8)
    fn put_workspace(&self, ctx: Ctx, wid: Uuid) -> impl Future<Output = Answer<PutWorkspace>> + Send {
        unsupported()
    }
    fn get_workspace(&self, ctx: Ctx, wid: Uuid) -> impl Future<Output = Answer<Workspace>> + Send {
        unsupported()
    }
    /// 204, also when absent.
    fn delete_workspace(&self, ctx: Ctx, wid: Uuid) -> impl Future<Output = Answer<()>> + Send {
        unsupported()
    }
    fn workspace_entries(&self, ctx: Ctx, wid: Uuid, q: EntriesQuery) -> impl Future<Output = Answer<Entries>> + Send {
        unsupported()
    }
    fn workspace_file(
        &self,
        ctx: Ctx,
        wid: Uuid,
        q: FilesQuery,
        range: Option<ByteRange>,
    ) -> impl Future<Output = Answer<FileContent>> + Send {
        unsupported()
    }
    /// 201.
    fn clone_workspace(
        &self,
        ctx: Ctx,
        wid: Uuid,
        body: CloneRequest,
    ) -> impl Future<Output = Answer<Workspace>> + Send {
        unsupported()
    }
    // token keys and liveness (3.9)
    fn token_keys(&self, ctx: Ctx) -> impl Future<Output = Answer<TokenKeys>> + Send {
        unsupported()
    }
    fn token_liveness(&self, ctx: Ctx, sandbox_id: Uuid) -> impl Future<Output = Answer<TokenLiveness>> + Send {
        unsupported()
    }
    // the sandbox's own routes (3.7)
    fn sandbox_self(&self, ctx: Ctx, if_none_match: Option<String>) -> impl Future<Output = Answer<SelfAnswer>> + Send {
        unsupported()
    }
    fn sandbox_secrets(&self, ctx: Ctx) -> impl Future<Output = Answer<SecretKeys>> + Send {
        unsupported()
    }
    fn sandbox_secret(&self, ctx: Ctx, key: String) -> impl Future<Output = Answer<SecretValue>> + Send {
        unsupported()
    }
    fn sandbox_activity(&self, ctx: Ctx, body: ActivityReport) -> impl Future<Output = Answer<ActivityReply>> + Send {
        unsupported()
    }
    // operator page (3.11)
    /// The HTML page.
    fn ops_page(&self, ctx: Ctx) -> impl Future<Output = Answer<String>> + Send {
        unsupported()
    }
    fn ops_snapshot(&self, ctx: Ctx, q: SnapshotQuery) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    // admin (16)
    fn admin_hosts(&self, ctx: Ctx) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_patch_host(&self, ctx: Ctx, name: String, body: HostPatch) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_sandboxes_enabled(&self, ctx: Ctx, body: SandboxesEnabled) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_fresh_host(&self, ctx: Ctx, body: FreshHost) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_sandboxes(&self, ctx: Ctx, q: AdminSandboxesQuery) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_sandbox_log(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_release(&self, ctx: Ctx, id: Uuid, body: AdminRelease) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    fn admin_exec(&self, ctx: Ctx, id: Uuid, body: ExecRequest) -> impl Future<Output = Answer<ExecResult>> + Send {
        unsupported()
    }
    fn admin_recycle(&self, ctx: Ctx, body: Recycle) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
    /// 202.
    fn admin_purge(
        &self,
        ctx: Ctx,
        email: String,
        body: PurgeRequest,
    ) -> impl Future<Output = Answer<PurgeReply>> + Send {
        unsupported()
    }
    fn admin_events(&self, ctx: Ctx, q: AdminEventsQuery) -> impl Future<Output = Answer<Value>> + Send {
        unsupported()
    }
}

fn json_bytes<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec())
}

fn set_json(res: &mut Response) {
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
}

/// A JSON answer with a status.
fn json<T: Serialize>(status: u16, v: &T) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let mut res = (status, json_bytes(v)).into_response();
    set_json(&mut res);
    res
}

fn reply<T: Serialize>(status: u16, r: Answer<T>) -> Response {
    match r {
        Ok(v) => json(status, &v),
        Err(e) => e.into_response(),
    }
}

fn body<T: DeserializeOwned>(b: Result<Bytes, BytesRejection>) -> Answer<T> {
    let b = b.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::too_large("request body over the route's limit")
        } else {
            ApiError::bad_request(e.body_text())
        }
    })?;
    let b: &[u8] = if b.is_empty() { b"{}" } else { &b };
    serde_json::from_slice(b).map_err(|e| ApiError::bad_request(format!("bad JSON body: {e}")))
}

fn path<T>(p: Result<Path<T>, PathRejection>) -> Answer<T> {
    p.map(|Path(v)| v).map_err(|e| ApiError::bad_request(e.body_text()))
}

fn query<T>(q: Result<Query<T>, QueryRejection>) -> Answer<T> {
    q.map(|Query(v)| v).map_err(|e| ApiError::bad_request(e.body_text()))
}

async fn common_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    res
}

fn file_response(f: FileContent) -> Response {
    let meta = f.meta;
    let mut res = Response::new(f.body);
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.content_length()));
    if let Ok(v) = HeaderValue::from_str(&meta.mtime.http_date()) {
        h.insert(header::LAST_MODIFIED, v);
    }
    if let Ok(v) = HeaderValue::from_str(&meta.etag) {
        h.insert(header::ETAG, v);
    }
    if let Some((first, last)) = meta.served {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {first}-{last}/{}", meta.size)) {
            h.insert(header::CONTENT_RANGE, v);
        }
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    res
}

/// A 416 answer gets `content-range: bytes */<size>` when the error names the size.
fn file_error(e: ApiError) -> Response {
    let size = (e.status() == StatusCode::RANGE_NOT_SATISFIABLE)
        .then(|| e.body().fields.get("size").and_then(Value::as_u64))
        .flatten();
    let mut res = e.into_response();
    if let Some(size) = size
        && let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}"))
    {
        res.headers_mut().insert(header::CONTENT_RANGE, v);
    }
    res
}

/// The parts of a request every handler turns into a [`Ctx`].
struct Req {
    method: Method,
    headers: HeaderMap,
    caller: Option<Caller>,
}

impl<S: Send + Sync> FromRequestParts<S> for Req {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(parts: &mut Parts, _state: &S) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self {
            method: parts.method.clone(),
            headers: parts.headers.clone(),
            caller: parts.extensions.get::<Caller>().copied(),
        }))
    }
}

macro_rules! ctx {
    ($c:expr, $route:expr) => {
        Ctx::new($c.method, $route, $c.headers).with_caller($c.caller)
    };
}

/// Serves `server` on every route of this crate with no authentication (see [`guarded_router`]). Unknown paths are
/// 404 `not_found`, wrong methods 405, JSON bodies over 1 MiB ([`MAX_EXEC_BODY`] on the admin exec route) 413
/// `too_large`; every answer carries `cache-control: no-store` and `x-content-type-options: nosniff`.
pub fn router<S: Server>(server: Arc<S>) -> Router {
    finish(routes_of::<S>()).with_state(server)
}

/// Serves `server` as [`router`] does, behind `guard` (hermit 3.2) on every route.
pub fn guarded_router<S: Server>(server: Arc<S>, guard: RouteGuard) -> Router {
    let routes = routes_of::<S>().route_layer(axum::middleware::from_fn_with_state(Arc::new(guard), check));
    finish(routes).with_state(server)
}

fn finish<S: Server>(routes: Router<Arc<S>>) -> Router<Arc<S>> {
    routes
        .fallback(|| async { ApiError::not_found("no such route").into_response() })
        .method_not_allowed_fallback(|| async {
            error::api(405, codes::METHOD_NOT_ALLOWED, "method not allowed").into_response()
        })
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY))
        .layer(axum::middleware::map_response(common_headers))
}

/// The `Aviary-*` headers each caller may set (hermit 3.2's identity column; `Aviary-Request` and `Aviary-Protocol`
/// for every caller besides). Any other `Aviary-*` header is removed before the handler runs.
#[must_use]
pub fn identity_headers(caller: Caller) -> Vec<HeaderName> {
    match caller {
        Caller::Pigeon => vec![headers::PERSON],
        Caller::Gateway => vec![
            headers::SANDBOX_ID,
            headers::SANDBOX_CONVERSATION,
            headers::SANDBOX_PERSON,
        ],
        Caller::Admin => vec![headers::ADMIN],
        Caller::Magpie | Caller::Hermit | Caller::Tools | Caller::Monitor => Vec::new(),
    }
}

/// The surface a route belongs to: `hermit-sandbox` for `/v1/sandbox/**`, `hermit-core` for the rest.
#[must_use]
pub fn surface_of(route: &str) -> Surface {
    if route.starts_with(routes::SANDBOX_SURFACE_PREFIX) {
        SANDBOX
    } else {
        CORE
    }
}

/// hermit 3.2 in code: who is calling, whether that caller may call the matched route, and which protocol it
/// speaks. Built from hermit's accepted pair tokens (`Callers::from_env("HERMIT", &[…])`).
#[derive(Clone, Debug)]
pub struct RouteGuard {
    callers: Callers,
}

impl RouteGuard {
    #[must_use]
    pub fn new(callers: Callers) -> Self {
        Self { callers }
    }

    /// The decision for a request to `method route` (`route` the matched constant). On success the `Aviary-*`
    /// headers the caller may not set are gone from `headers`.
    ///
    /// # Errors
    /// 401 `unauthenticated` without a known token; 405 `method_not_allowed` for a method no caller may use on the
    /// route (`HEAD` on `/v1/*`); 403 `wrong_caller` for a caller [`routes::RULES`] does not list; 400
    /// `unsupported_protocol` when `Aviary-Protocol` is missing, names another surface, or a number this build
    /// does not accept (its current and previous).
    pub fn admit(&self, method: &Method, route: &str, headers: &mut HeaderMap) -> Result<Caller, ApiError> {
        let caller = self
            .callers
            .identify_headers(headers)
            .ok_or_else(ApiError::unauthenticated)?;
        if !Caller::ALL.iter().any(|c| routes::allowed(*c, method.as_str(), route)) {
            return Err(error::api(405, codes::METHOD_NOT_ALLOWED, "method not allowed"));
        }
        if !routes::allowed(caller, method.as_str(), route) {
            return Err(ApiError::wrong_caller(caller));
        }
        aviary_core::http::headers::strip_aviary(headers, &identity_headers(caller));
        let surface = surface_of(route);
        let spoken = headers
            .get(&headers::PROTOCOL)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once('='))
            .and_then(|(s, n)| Some((s.trim().to_owned(), n.trim().parse::<u32>().ok()?)));
        match spoken {
            Some((s, n)) if s == surface.0 && accepts(surface, n) => Ok(caller),
            _ => Err(error::api(
                400,
                codes::UNSUPPORTED_PROTOCOL,
                format!("this route speaks {}={} (or the previous number)", surface.0, surface.1),
            )
            .with("surface", surface.0)
            .with("protocol", surface.1)),
        }
    }
}

async fn check(State(guard): State<Arc<RouteGuard>>, mut req: Request, next: Next) -> Response {
    let Some(route) = req.extensions().get::<MatchedPath>().map(|m| m.as_str().to_owned()) else {
        return ApiError::not_found("no such route").into_response();
    };
    let method = req.method().clone();
    match guard.admit(&method, &route, req.headers_mut()) {
        Ok(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

/// Every route, unauthenticated and without the common layers.
#[allow(clippy::too_many_lines)] // one entry per route, kept together so the table is readable
fn routes_of<S: Server>() -> Router<Arc<S>> {
    use routes as r;
    type St<S> = State<Arc<S>>;
    type B = Result<Bytes, BytesRejection>;
    type P<T> = Result<Path<T>, PathRejection>;
    type Q<T> = Result<Query<T>, QueryRejection>;

    Router::new()
        .route(
            r::DEPLOYMENT,
            get(|State(s): St<S>, c: Req| async move {
                reply(200, s.deployment(ctx!(c, r::DEPLOYMENT)).await)
            }),
        )
        .route(
            r::PEOPLE,
            get(|State(s): St<S>, c: Req| async move {
                reply(200, s.people(ctx!(c, r::PEOPLE)).await)
            }),
        )
        .route(
            r::PERSON,
            get(|State(s): St<S>, c: Req, p: P<String>| async move {
                let r = async { s.person(ctx!(c, r::PERSON), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WARM,
            post(|State(s): St<S>, c: Req, b: B| async move {
                let r = async { s.warm(ctx!(c, r::WARM), body(b)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX,
            put(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async { s.put_sandbox(ctx!(c, r::SANDBOX), path(p)?, body(b)?).await }.await;
                match r {
                    Ok(a) => json(a.status(), a.sandbox()),
                    Err(e) => e.into_response(),
                }
            })
            .get(|State(s): St<S>, c: Req, p: P<Uuid>, q: Q<SandboxQuery>| async move {
                let r = async { s.get_sandbox(ctx!(c, r::SANDBOX), path(p)?, query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_RELEASE,
            post(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async { s.release_sandbox(ctx!(c, r::SANDBOX_RELEASE), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_PAUSE,
            post(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.pause_sandbox(ctx!(c, r::SANDBOX_PAUSE), path(p)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX_RESUME,
            post(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.resume_sandbox(ctx!(c, r::SANDBOX_RESUME), path(p)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX_SNAPSHOT,
            post(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.snapshot_sandbox(ctx!(c, r::SANDBOX_SNAPSHOT), path(p)?).await }.await;
                reply(201, r)
            }),
        )
        .route(
            r::SANDBOX_EVENTS,
            get(|State(s): St<S>, c: Req, q: Q<SandboxEventsQuery>| async move {
                let r = async { s.sandbox_events(ctx!(c, r::SANDBOX_EVENTS), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WORKSPACE,
            put(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async {
                    let _: crate::workspaces::PutWorkspaceRequest = body(b)?;
                    s.put_workspace(ctx!(c, r::WORKSPACE), path(p)?).await
                }
                .await;
                match r {
                    Ok(a) => json(a.status(), a.workspace()),
                    Err(e) => e.into_response(),
                }
            })
            .get(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.get_workspace(ctx!(c, r::WORKSPACE), path(p)?).await }.await;
                reply(200, r)
            })
            .delete(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.delete_workspace(ctx!(c, r::WORKSPACE), path(p)?).await }.await;
                match r {
                    Ok(()) => StatusCode::NO_CONTENT.into_response(),
                    Err(e) => e.into_response(),
                }
            }),
        )
        .route(
            r::WORKSPACE_ENTRIES,
            get(|State(s): St<S>, c: Req, p: P<Uuid>, q: Q<EntriesQuery>| async move {
                let r = async { s.workspace_entries(ctx!(c, r::WORKSPACE_ENTRIES), path(p)?, query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WORKSPACE_FILES,
            get(|State(s): St<S>, c: Req, p: P<Uuid>, q: Q<FilesQuery>| async move {
                // a Range this contract does not support (a suffix, a list, another unit, bad syntax) is ignored
                // and the whole file served (RFC 9110 14.2); 416 is the implementation's, for start >= size
                let range = c
                    .headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(ByteRange::parse);
                let r = async { s.workspace_file(ctx!(c, r::WORKSPACE_FILES), path(p)?, query(q)?, range).await }.await;
                match r {
                    Ok(f) => file_response(f),
                    Err(e) => file_error(e),
                }
            }),
        )
        .route(
            r::WORKSPACE_CLONE,
            post(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async { s.clone_workspace(ctx!(c, r::WORKSPACE_CLONE), path(p)?, body(b)?).await }.await;
                reply(201, r)
            }),
        )
        .route(
            r::SANDBOX_TOKEN_KEYS,
            get(|State(s): St<S>, c: Req| async move {
                reply(200, s.token_keys(ctx!(c, r::SANDBOX_TOKEN_KEYS)).await)
            }),
        )
        .route(
            r::SANDBOX_TOKEN,
            get(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.token_liveness(ctx!(c, r::SANDBOX_TOKEN), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_SELF,
            get(|State(s): St<S>, c: Req| async move {
                let inm = c
                    .headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                match s.sandbox_self(ctx!(c, r::SANDBOX_SELF), inm).await {
                    Ok(SelfAnswer::Fresh(d)) => {
                        let mut res = json(200, &d);
                        if let Ok(v) = HeaderValue::from_str(&d.etag) {
                            res.headers_mut().insert(header::ETAG, v);
                        }
                        res
                    }
                    Ok(SelfAnswer::NotModified { etag }) => {
                        let mut res = StatusCode::NOT_MODIFIED.into_response();
                        if let Ok(v) = HeaderValue::from_str(&etag) {
                            res.headers_mut().insert(header::ETAG, v);
                        }
                        res
                    }
                    Err(e) => e.into_response(),
                }
            }),
        )
        .route(
            r::SANDBOX_SECRETS,
            get(|State(s): St<S>, c: Req| async move {
                reply(200, s.sandbox_secrets(ctx!(c, r::SANDBOX_SECRETS)).await)
            }),
        )
        .route(
            r::SANDBOX_SECRET,
            get(|State(s): St<S>, c: Req, p: P<String>| async move {
                let r = async { s.sandbox_secret(ctx!(c, r::SANDBOX_SECRET), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_ACTIVITY,
            post(|State(s): St<S>, c: Req, b: B| async move {
                let r = async { s.sandbox_activity(ctx!(c, r::SANDBOX_ACTIVITY), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::OPS_PAGE,
            get(|State(s): St<S>, c: Req| async move {
                match s.ops_page(ctx!(c, r::OPS_PAGE)).await {
                    Ok(html) => {
                        let mut res = html.into_response();
                        res.headers_mut().insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        );
                        res
                    }
                    Err(e) => e.into_response(),
                }
            }),
        )
        .route(
            r::OPS_SNAPSHOT,
            get(|State(s): St<S>, c: Req, q: Q<SnapshotQuery>| async move {
                let r = async { s.ops_snapshot(ctx!(c, r::OPS_SNAPSHOT), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_HOSTS,
            get(|State(s): St<S>, c: Req| async move {
                reply(200, s.admin_hosts(ctx!(c, r::ADMIN_HOSTS)).await)
            }),
        )
        .route(
            r::ADMIN_HOST,
            patch(|State(s): St<S>, c: Req, p: P<String>, b: B| async move {
                let r = async { s.admin_patch_host(ctx!(c, r::ADMIN_HOST), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOXES_ENABLED,
            put(|State(s): St<S>, c: Req, b: B| async move {
                let r = async { s.admin_sandboxes_enabled(ctx!(c, r::ADMIN_SANDBOXES_ENABLED), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_FRESH_HOST,
            post(|State(s): St<S>, c: Req, b: B| async move {
                let r = async { s.admin_fresh_host(ctx!(c, r::ADMIN_FRESH_HOST), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOXES,
            get(|State(s): St<S>, c: Req, q: Q<AdminSandboxesQuery>| async move {
                let r = async { s.admin_sandboxes(ctx!(c, r::ADMIN_SANDBOXES), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_LOG,
            get(|State(s): St<S>, c: Req, p: P<Uuid>| async move {
                let r = async { s.admin_sandbox_log(ctx!(c, r::ADMIN_SANDBOX_LOG), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_RELEASE,
            post(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async { s.admin_release(ctx!(c, r::ADMIN_SANDBOX_RELEASE), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_EXEC,
            post(|State(s): St<S>, c: Req, p: P<Uuid>, b: B| async move {
                let r = async { s.admin_exec(ctx!(c, r::ADMIN_SANDBOX_EXEC), path(p)?, body(b)?).await }.await;
                reply(200, r)
            })
            .layer(DefaultBodyLimit::max(MAX_EXEC_BODY)),
        )
        .route(
            r::ADMIN_RECYCLE,
            post(|State(s): St<S>, c: Req, b: B| async move {
                let r = async { s.admin_recycle(ctx!(c, r::ADMIN_RECYCLE), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_PERSON_PURGE,
            post(|State(s): St<S>, c: Req, p: P<String>, b: B| async move {
                let r = async { s.admin_purge(ctx!(c, r::ADMIN_PERSON_PURGE), path(p)?, body(b)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::ADMIN_EVENTS,
            get(|State(s): St<S>, c: Req, q: Q<AdminEventsQuery>| async move {
                let r = async { s.admin_events(ctx!(c, r::ADMIN_EVENTS), query(q)?).await }.await;
                reply(200, r)
            }),
        )
}
