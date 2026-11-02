//! The server side: a [`Server`] trait (what fake-sandboxd and test doubles implement) and [`router`], which serves it
//! with sandboxd's routes, limits and error shape, and reads bodies as sandboxd does: the content type is ignored, an
//! empty (or whitespace) body and a JSON value that is not an object are `{}`, a body that is not JSON is 400, over
//! [`MAX_BODY`] is 413, and a request with `transfer-encoding` (a chunked body) is 411. One difference is left: sandboxd
//! also accepts a number sent as a string and a string sent as a number in a field, the typed router does not (400)
//! (docs/CONTRACTS.md, "The sandboxd contract"). TLS and the client-certificate check are the listener's.

use std::future::Future;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::de::DeserializeOwned;

use crate::{
    Deleted, EgressTargets, EnsureImage, ErrorBody, Host, ImageDigest, Listed, MAX_BODY, Peers, RECEIVE_WAIT_MAX_MS,
    ReceiveResult, Receiving, Sandboxes, SendRequest, SendResult, SetPeers, Spec, routes, valid_name,
};

/// An error answer: `status` with `{"error": error}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// The HTTP status.
    pub status: u16,
    /// The message.
    pub error: String,
}

impl ApiError {
    /// An error answer.
    pub fn new(status: u16, error: impl Into<String>) -> Self {
        Self {
            status,
            error: error.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(ErrorBody { error: self.error })).into_response()
    }
}

/// What a route's handler returns.
pub type Answer<T> = Result<T, ApiError>;

/// sandboxd's API. Names reaching the per-sandbox methods are already [`valid_name`]s.
pub trait Server: Send + Sync + 'static {
    /// `GET /v1/host`.
    fn host(&self) -> impl Future<Output = Answer<Host>> + Send;
    /// `PUT /v1/peers`.
    fn set_peers(&self, req: SetPeers) -> impl Future<Output = Answer<Peers>> + Send;
    /// `PUT /v1/egress`.
    fn set_egress(&self, req: EgressTargets) -> impl Future<Output = Answer<EgressTargets>> + Send;
    /// `POST /v1/images`.
    fn ensure_image(&self, req: EnsureImage) -> impl Future<Output = Answer<ImageDigest>> + Send;
    /// `GET /v1/sandboxes`.
    fn list(&self) -> impl Future<Output = Answer<Sandboxes>> + Send;
    /// `POST /v1/sandboxes`.
    fn create(&self, spec: Spec) -> impl Future<Output = Answer<Listed>> + Send;
    /// `DELETE /v1/sandboxes/{name}`.
    fn delete(&self, name: String) -> impl Future<Output = Answer<Deleted>> + Send;
    /// `GET /v1/sandboxes/{name}/spec`.
    fn spec(&self, name: String) -> impl Future<Output = Answer<Spec>> + Send;
    /// `POST /v1/sandboxes/{name}/receive` (the router has checked `spec.name == name`).
    fn receive(&self, spec: Spec) -> impl Future<Output = Answer<Receiving>> + Send;
    /// `GET /v1/sandboxes/{name}/receive?wait=<ms>` (`wait_ms` clamped to [`RECEIVE_WAIT_MAX_MS`]).
    fn receive_result(&self, name: String, wait_ms: u64) -> impl Future<Output = Answer<ReceiveResult>> + Send;
    /// `POST /v1/sandboxes/{name}/send`.
    fn send(&self, name: String, req: SendRequest) -> impl Future<Output = Answer<SendResult>> + Send;
}

/// A request body as sandboxd reads it (the module docs).
type Body = Result<Bytes, BytesRejection>;

fn body<T: DeserializeOwned>(r: Body) -> Answer<T> {
    let bytes = r.map_err(|e| {
        let status = if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            413
        } else {
            400
        };
        ApiError::new(status, e.body_text())
    })?;
    let value = if bytes.iter().all(u8::is_ascii_whitespace) {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v @ serde_json::Value::Object(_)) => v,
            Ok(_) => serde_json::Value::Object(serde_json::Map::new()),
            Err(e) => return Err(ApiError::new(400, format!("bad json: {e}"))),
        }
    };
    serde_json::from_value(value).map_err(|e| ApiError::new(400, e.to_string()))
}

/// sandboxd reads only `content-length` bodies: any `transfer-encoding` is 411.
async fn length_required(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::TRANSFER_ENCODING) {
        return ApiError::new(411, "send a content-length").into_response();
    }
    next.run(req).await
}

fn name_ok(name: String) -> Answer<String> {
    if valid_name(&name) {
        Ok(name)
    } else {
        Err(ApiError::new(400, "bad sandbox name"))
    }
}

/// The `wait` of `GET …/receive` as sandboxd reads it: the first `wait=` of the query as a number, clamped to
/// [0, [`RECEIVE_WAIT_MAX_MS`]]; missing, not a number or not finite is 0 (never a 400).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to [0, RECEIVE_WAIT_MAX_MS] first
fn wait_ms(query: Option<&str>) -> u64 {
    #[allow(clippy::cast_precision_loss)] // 120000 is exact
    let max = RECEIVE_WAIT_MAX_MS as f64;
    query
        .unwrap_or("")
        .split('&')
        .find_map(|kv| kv.strip_prefix("wait="))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, max) as u64
}

/// sandboxd's routes over `server`, with its 64 KiB body limit, its reading of bodies (the module docs) and `{"error"}`
/// answers: 404 `not found` for any other path and for a known path with another method, as sandboxd answers both.
pub fn router<S: Server>(server: Arc<S>) -> Router {
    Router::new()
        .route(routes::HOST, get(|State(s): State<Arc<S>>| async move { s.host().await.map(Json) }))
        .route(
            routes::PEERS,
            put(|State(s): State<Arc<S>>, b: Body| async move {
                s.set_peers(body::<SetPeers>(b)?).await.map(Json)
            }),
        )
        .route(
            routes::EGRESS,
            put(|State(s): State<Arc<S>>, b: Body| async move {
                s.set_egress(body::<EgressTargets>(b)?).await.map(Json)
            }),
        )
        .route(
            routes::IMAGES,
            post(|State(s): State<Arc<S>>, b: Body| async move {
                s.ensure_image(body::<EnsureImage>(b)?).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOXES,
            get(|State(s): State<Arc<S>>| async move { s.list().await.map(Json) }).post(
                |State(s): State<Arc<S>>, b: Body| async move {
                    let spec: Spec = body(b)?;
                    name_ok(spec.name.clone())?;
                    s.create(spec).await.map(Json)
                },
            ),
        )
        .route(
            routes::SANDBOX,
            delete(|State(s): State<Arc<S>>, Path(name): Path<String>| async move {
                s.delete(name_ok(name)?).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOX_SPEC,
            get(|State(s): State<Arc<S>>, Path(name): Path<String>| async move {
                s.spec(name_ok(name)?).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOX_RECEIVE,
            post(
                |State(s): State<Arc<S>>, Path(name): Path<String>, b: Body| async move {
                    let name = name_ok(name)?;
                    let spec: Spec = body(b)?;
                    if spec.name != name {
                        return Err(ApiError::new(400, "name mismatch"));
                    }
                    s.receive(spec).await.map(Json)
                },
            )
            .get(|State(s): State<Arc<S>>, Path(name): Path<String>, RawQuery(q): RawQuery| async move {
                s.receive_result(name_ok(name)?, wait_ms(q.as_deref())).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOX_SEND,
            post(
                |State(s): State<Arc<S>>,
                 Path(name): Path<String>,
                 b: Body| async move {
                    s.send(name_ok(name)?, body::<SendRequest>(b)?).await.map(Json)
                },
            ),
        )
        .fallback(|| async { ApiError::new(404, "not found") })
        .method_not_allowed_fallback(|| async { ApiError::new(404, "not found") })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(length_required))
        .with_state(server)
}
