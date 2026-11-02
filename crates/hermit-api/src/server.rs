//! The `Server` trait (every hermit route as a method) and [`router`], which serves an implementation of it over
//! HTTP with this crate's route constants and types. The fakes implement the trait (WP-08); a real implementation
//! may too. Every method defaults to 501 `not_supported`, the normal answer for a call an implementation lacks
//! (hermit 3.1). Authentication is the implementation's: [`Ctx`] carries the bearer and the identity headers, and
//! [`routes::allowed`] is the caller table.

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
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

/// The largest JSON request body (hermit 3.1): larger is 413 `too_large`.
pub const MAX_JSON_BODY: usize = 1024 * 1024;

/// The verified sandbox behind a forwarded `/v1/sandbox/**` call (the gateway's three headers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxIdentity {
    pub id: Uuid,
    pub conversation: Uuid,
    pub person: String,
}

/// What a handler knows about the request beyond its typed inputs.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub method: Method,
    /// The matched route constant.
    pub route: &'static str,
    pub headers: HeaderMap,
}

impl Ctx {
    #[must_use]
    pub fn new(method: Method, route: &'static str, headers: HeaderMap) -> Self {
        Self { method, route, headers }
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
            ApiError::too_large("request body over 1 MiB")
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
    let len = f.body.len();
    let mut res = Response::new(axum::body::Body::from(f.body));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&f.mtime.http_date()) {
        h.insert(header::LAST_MODIFIED, v);
    }
    if let Ok(v) = HeaderValue::from_str(&f.etag) {
        h.insert(header::ETAG, v);
    }
    if let Some((first, last)) = f.served {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {first}-{last}/{}", f.size)) {
            h.insert(header::CONTENT_RANGE, v);
        }
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    res
}

macro_rules! ctx {
    ($m:expr, $route:expr, $h:expr) => {
        Ctx::new($m, $route, $h)
    };
}

/// Serves `server` on every route of this crate. Unknown paths are 404 `not_found`, wrong methods 405, JSON bodies
/// over 1 MiB 413 `too_large`; every answer carries `cache-control: no-store` and `x-content-type-options: nosniff`.
#[allow(clippy::too_many_lines)] // one entry per route, kept together so the table is readable
pub fn router<S: Server>(server: Arc<S>) -> Router {
    use routes as r;
    type St<S> = State<Arc<S>>;
    type B = Result<Bytes, BytesRejection>;
    type P<T> = Result<Path<T>, PathRejection>;
    type Q<T> = Result<Query<T>, QueryRejection>;

    Router::new()
        .route(
            r::DEPLOYMENT,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                reply(200, s.deployment(ctx!(m, r::DEPLOYMENT, h)).await)
            }),
        )
        .route(
            r::PEOPLE,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                reply(200, s.people(ctx!(m, r::PEOPLE, h)).await)
            }),
        )
        .route(
            r::PERSON,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<String>| async move {
                let r = async { s.person(ctx!(m, r::PERSON, h), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WARM,
            post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                let r = async { s.warm(ctx!(m, r::WARM, h), body(b)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX,
            put(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async { s.put_sandbox(ctx!(m, r::SANDBOX, h), path(p)?, body(b)?).await }.await;
                match r {
                    Ok(a) => json(a.status(), a.sandbox()),
                    Err(e) => e.into_response(),
                }
            })
            .get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, q: Q<SandboxQuery>| async move {
                let r = async { s.get_sandbox(ctx!(m, r::SANDBOX, h), path(p)?, query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_RELEASE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async { s.release_sandbox(ctx!(m, r::SANDBOX_RELEASE, h), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_PAUSE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.pause_sandbox(ctx!(m, r::SANDBOX_PAUSE, h), path(p)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX_RESUME,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.resume_sandbox(ctx!(m, r::SANDBOX_RESUME, h), path(p)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::SANDBOX_SNAPSHOT,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.snapshot_sandbox(ctx!(m, r::SANDBOX_SNAPSHOT, h), path(p)?).await }.await;
                reply(201, r)
            }),
        )
        .route(
            r::SANDBOX_EVENTS,
            get(|State(s): St<S>, m: Method, h: HeaderMap, q: Q<SandboxEventsQuery>| async move {
                let r = async { s.sandbox_events(ctx!(m, r::SANDBOX_EVENTS, h), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WORKSPACE,
            put(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async {
                    let _: crate::workspaces::PutWorkspaceRequest = body(b)?;
                    s.put_workspace(ctx!(m, r::WORKSPACE, h), path(p)?).await
                }
                .await;
                match r {
                    Ok(a) => json(a.status(), a.workspace()),
                    Err(e) => e.into_response(),
                }
            })
            .get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.get_workspace(ctx!(m, r::WORKSPACE, h), path(p)?).await }.await;
                reply(200, r)
            })
            .delete(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.delete_workspace(ctx!(m, r::WORKSPACE, h), path(p)?).await }.await;
                match r {
                    Ok(()) => StatusCode::NO_CONTENT.into_response(),
                    Err(e) => e.into_response(),
                }
            }),
        )
        .route(
            r::WORKSPACE_ENTRIES,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, q: Q<EntriesQuery>| async move {
                let r = async { s.workspace_entries(ctx!(m, r::WORKSPACE_ENTRIES, h), path(p)?, query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::WORKSPACE_FILES,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, q: Q<FilesQuery>| async move {
                let range = match h.get(header::RANGE).and_then(|v| v.to_str().ok()) {
                    None => Ok(None),
                    Some(v) => ByteRange::parse(v)
                        .map(Some)
                        .ok_or_else(|| error::api(416, codes::BAD_RANGE, "one byte range only")),
                };
                let r = async {
                    let range = range?;
                    s.workspace_file(ctx!(m, r::WORKSPACE_FILES, h), path(p)?, query(q)?, range).await
                }
                .await;
                match r {
                    Ok(f) => file_response(f),
                    Err(e) => e.into_response(),
                }
            }),
        )
        .route(
            r::WORKSPACE_CLONE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async { s.clone_workspace(ctx!(m, r::WORKSPACE_CLONE, h), path(p)?, body(b)?).await }.await;
                reply(201, r)
            }),
        )
        .route(
            r::SANDBOX_TOKEN_KEYS,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                reply(200, s.token_keys(ctx!(m, r::SANDBOX_TOKEN_KEYS, h)).await)
            }),
        )
        .route(
            r::SANDBOX_TOKEN,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.token_liveness(ctx!(m, r::SANDBOX_TOKEN, h), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_SELF,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                let inm = h
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                match s.sandbox_self(ctx!(m, r::SANDBOX_SELF, h), inm).await {
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
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                reply(200, s.sandbox_secrets(ctx!(m, r::SANDBOX_SECRETS, h)).await)
            }),
        )
        .route(
            r::SANDBOX_SECRET,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<String>| async move {
                let r = async { s.sandbox_secret(ctx!(m, r::SANDBOX_SECRET, h), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::SANDBOX_ACTIVITY,
            post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                let r = async { s.sandbox_activity(ctx!(m, r::SANDBOX_ACTIVITY, h), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::OPS_PAGE,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                match s.ops_page(ctx!(m, r::OPS_PAGE, h)).await {
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
            get(|State(s): St<S>, m: Method, h: HeaderMap, q: Q<SnapshotQuery>| async move {
                let r = async { s.ops_snapshot(ctx!(m, r::OPS_SNAPSHOT, h), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_HOSTS,
            get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                reply(200, s.admin_hosts(ctx!(m, r::ADMIN_HOSTS, h)).await)
            }),
        )
        .route(
            r::ADMIN_HOST,
            patch(|State(s): St<S>, m: Method, h: HeaderMap, p: P<String>, b: B| async move {
                let r = async { s.admin_patch_host(ctx!(m, r::ADMIN_HOST, h), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOXES_ENABLED,
            put(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                let r = async { s.admin_sandboxes_enabled(ctx!(m, r::ADMIN_SANDBOXES_ENABLED, h), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_FRESH_HOST,
            post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                let r = async { s.admin_fresh_host(ctx!(m, r::ADMIN_FRESH_HOST, h), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOXES,
            get(|State(s): St<S>, m: Method, h: HeaderMap, q: Q<AdminSandboxesQuery>| async move {
                let r = async { s.admin_sandboxes(ctx!(m, r::ADMIN_SANDBOXES, h), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_LOG,
            get(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>| async move {
                let r = async { s.admin_sandbox_log(ctx!(m, r::ADMIN_SANDBOX_LOG, h), path(p)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_RELEASE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async { s.admin_release(ctx!(m, r::ADMIN_SANDBOX_RELEASE, h), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_SANDBOX_EXEC,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<Uuid>, b: B| async move {
                let r = async { s.admin_exec(ctx!(m, r::ADMIN_SANDBOX_EXEC, h), path(p)?, body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_RECYCLE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                let r = async { s.admin_recycle(ctx!(m, r::ADMIN_RECYCLE, h), body(b)?).await }.await;
                reply(200, r)
            }),
        )
        .route(
            r::ADMIN_PERSON_PURGE,
            post(|State(s): St<S>, m: Method, h: HeaderMap, p: P<String>, b: B| async move {
                let r = async { s.admin_purge(ctx!(m, r::ADMIN_PERSON_PURGE, h), path(p)?, body(b)?).await }.await;
                reply(202, r)
            }),
        )
        .route(
            r::ADMIN_EVENTS,
            get(|State(s): St<S>, m: Method, h: HeaderMap, q: Q<AdminEventsQuery>| async move {
                let r = async { s.admin_events(ctx!(m, r::ADMIN_EVENTS, h), query(q)?).await }.await;
                reply(200, r)
            }),
        )
        .fallback(|| async { ApiError::not_found("no such route").into_response() })
        .method_not_allowed_fallback(|| async {
            error::api(405, codes::METHOD_NOT_ALLOWED, "method not allowed").into_response()
        })
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY))
        .layer(axum::middleware::map_response(common_headers))
        .with_state(server)
}
