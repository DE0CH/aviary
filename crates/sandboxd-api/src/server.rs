//! The server side: a [`Server`] trait (what fake-sandboxd and test doubles implement) and [`router`], which serves it
//! with sandboxd's routes, bodies, limits and error shape. TLS and the client-certificate check are the listener's.

use std::future::Future;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

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

fn body<T>(r: Result<Json<T>, JsonRejection>) -> Answer<T> {
    // as sandboxd: 413 for a body over the limit, 400 for anything else wrong with it
    r.map(|Json(v)| v).map_err(|e| {
        let status = if e.status() == StatusCode::PAYLOAD_TOO_LARGE { 413 } else { 400 };
        ApiError::new(status, e.body_text())
    })
}

fn name_ok(name: String) -> Answer<String> {
    if valid_name(&name) {
        Ok(name)
    } else {
        Err(ApiError::new(400, "bad sandbox name"))
    }
}

#[derive(Deserialize)]
struct Wait {
    wait: Option<f64>,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to [0, RECEIVE_WAIT_MAX_MS] first
fn wait_ms(w: Option<f64>) -> u64 {
    #[allow(clippy::cast_precision_loss)] // 120000 is exact
    let max = RECEIVE_WAIT_MAX_MS as f64;
    w.filter(|x| x.is_finite()).unwrap_or(0.0).clamp(0.0, max) as u64
}

/// sandboxd's routes over `server`, with its 64 KiB body limit and `{"error"}` answers (404 `not found` for any other
/// path).
pub fn router<S: Server>(server: Arc<S>) -> Router {
    Router::new()
        .route(routes::HOST, get(|State(s): State<Arc<S>>| async move { s.host().await.map(Json) }))
        .route(
            routes::PEERS,
            put(|State(s): State<Arc<S>>, b: Result<Json<SetPeers>, JsonRejection>| async move {
                s.set_peers(body(b)?).await.map(Json)
            }),
        )
        .route(
            routes::EGRESS,
            put(|State(s): State<Arc<S>>, b: Result<Json<EgressTargets>, JsonRejection>| async move {
                s.set_egress(body(b)?).await.map(Json)
            }),
        )
        .route(
            routes::IMAGES,
            post(|State(s): State<Arc<S>>, b: Result<Json<EnsureImage>, JsonRejection>| async move {
                s.ensure_image(body(b)?).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOXES,
            get(|State(s): State<Arc<S>>| async move { s.list().await.map(Json) }).post(
                |State(s): State<Arc<S>>, b: Result<Json<Spec>, JsonRejection>| async move {
                    let spec = body(b)?;
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
                |State(s): State<Arc<S>>, Path(name): Path<String>, b: Result<Json<Spec>, JsonRejection>| async move {
                    let name = name_ok(name)?;
                    let spec = body(b)?;
                    if spec.name != name {
                        return Err(ApiError::new(400, "name mismatch"));
                    }
                    s.receive(spec).await.map(Json)
                },
            )
            .get(|State(s): State<Arc<S>>, Path(name): Path<String>, Query(w): Query<Wait>| async move {
                s.receive_result(name_ok(name)?, wait_ms(w.wait)).await.map(Json)
            }),
        )
        .route(
            routes::SANDBOX_SEND,
            post(
                |State(s): State<Arc<S>>,
                 Path(name): Path<String>,
                 b: Result<Json<SendRequest>, JsonRejection>| async move {
                    s.send(name_ok(name)?, body(b)?).await.map(Json)
                },
            ),
        )
        .fallback(|| async { ApiError::new(404, "not found") })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(server)
}
