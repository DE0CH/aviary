//! Server traits for the supervisor's two listeners, and routers that serve them with this crate's routes and
//! types. Every method defaults to 501 `not_supported`. Authentication (the control token on `:7070`) is the
//! implementation's: check [`Ctx::bearer`] with [`crate::control_token_matches`].

use std::future::Future;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hermit_api::error::{self, ApiError, codes};
pub use hermit_api::server::{Answer, Ctx};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::control::{Health, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, StateReport};
use crate::local::{ReceivedFds, SECRET, SEND_USER_FILE, SendUserFileReply, SendUserFileRequest};
use crate::{ExecRequest, ExecResult, SecretValue, routes};

/// Request bodies on both listeners: exec's stdin is at most 1 MiB of base64.
pub const MAX_BODY: usize = 2 * 1024 * 1024;

fn unsupported<T: Send>() -> impl Future<Output = Answer<T>> + Send {
    std::future::ready(Err(error::not_supported()))
}

/// The `:7070` API (surface `supervisor-control`).
#[allow(unused_variables)]
pub trait ControlServer: Send + Sync + 'static {
    fn health(&self, ctx: Ctx) -> impl Future<Output = Answer<Health>> + Send {
        unsupported()
    }
    fn state(&self, ctx: Ctx) -> impl Future<Output = Answer<StateReport>> + Send {
        unsupported()
    }
    /// 200, or 409 `sandbox_busy` with `run` ([`crate::control::busy`]).
    fn prepare(&self, ctx: Ctx, body: PrepareRequest) -> impl Future<Output = Answer<PrepareAnswer>> + Send {
        unsupported()
    }
    /// 204.
    fn resumed(&self, ctx: Ctx, body: Resumed) -> impl Future<Output = Answer<()>> + Send {
        unsupported()
    }
    /// 200 after the release sequence, or 409 `sandbox_busy` (nothing done).
    fn release(&self, ctx: Ctx, body: ReleaseRequest) -> impl Future<Output = Answer<ReleaseAnswer>> + Send {
        unsupported()
    }
    fn exec(&self, ctx: Ctx, body: ExecRequest) -> impl Future<Output = Answer<ExecResult>> + Send {
        unsupported()
    }
}

/// The local socket API.
#[allow(unused_variables)]
pub trait LocalServer: Send + Sync + 'static {
    /// `fds` are the descriptors attached to the request, in the order of `body.files`.
    fn send_user_file(
        &self,
        ctx: Ctx,
        body: SendUserFileRequest,
        fds: Vec<OwnedFd>,
    ) -> impl Future<Output = Answer<SendUserFileReply>> + Send {
        unsupported()
    }
    fn secret(&self, ctx: Ctx, key: String) -> impl Future<Output = Answer<SecretValue>> + Send {
        unsupported()
    }
}

fn json<T: Serialize>(status: StatusCode, v: &T) -> Response {
    aviary_core::http::json(status, v)
}

fn reply<T: Serialize>(r: Answer<T>) -> Response {
    match r {
        Ok(v) => json(StatusCode::OK, &v),
        Err(e) => e.into_response(),
    }
}

fn body<T: DeserializeOwned>(b: Result<Bytes, BytesRejection>) -> Answer<T> {
    let b = b.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::too_large("request body too large")
        } else {
            ApiError::bad_request(e.body_text())
        }
    })?;
    serde_json::from_slice(&b).map_err(|e| ApiError::bad_request(format!("bad JSON body: {e}")))
}

async fn common_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    res
}

fn finish<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router
        .fallback(|| async { ApiError::not_found("no such route").into_response() })
        .method_not_allowed_fallback(|| async {
            error::api(405, codes::METHOD_NOT_ALLOWED, "method not allowed").into_response()
        })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(axum::middleware::map_response(common_headers))
}

/// Serves `server` on `:7070`'s routes.
pub fn control_router<S: ControlServer>(server: Arc<S>) -> Router {
    use routes as r;
    type St<S> = State<Arc<S>>;
    type B = Result<Bytes, BytesRejection>;
    let router =
        Router::new()
            .route(
                r::HEALTH,
                get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                    reply(s.health(Ctx::new(m, r::HEALTH, h)).await)
                }),
            )
            .route(
                r::STATE,
                get(|State(s): St<S>, m: Method, h: HeaderMap| async move {
                    reply(s.state(Ctx::new(m, r::STATE, h)).await)
                }),
            )
            .route(
                r::PREPARE,
                post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                    reply(async { s.prepare(Ctx::new(m, r::PREPARE, h), body(b)?).await }.await)
                }),
            )
            .route(
                r::RESUMED,
                post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                    match async { s.resumed(Ctx::new(m, r::RESUMED, h), body(b)?).await }.await {
                        Ok(()) => StatusCode::NO_CONTENT.into_response(),
                        Err(e) => e.into_response(),
                    }
                }),
            )
            .route(
                r::RELEASE,
                post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                    reply(async { s.release(Ctx::new(m, r::RELEASE, h), body(b)?).await }.await)
                }),
            )
            .route(
                r::EXEC,
                post(|State(s): St<S>, m: Method, h: HeaderMap, b: B| async move {
                    reply(async { s.exec(Ctx::new(m, r::EXEC, h), body(b)?).await }.await)
                }),
            );
    finish(router).with_state(server)
}

/// Serves `server` on the local socket's routes. The acceptor attaches the received descriptors as a
/// [`ReceivedFds`] request extension.
pub fn local_router<S: LocalServer>(server: Arc<S>) -> Router {
    type St<S> = State<Arc<S>>;
    type B = Result<Bytes, BytesRejection>;
    let router = Router::new()
        .route(
            SEND_USER_FILE,
            post(
                |State(s): St<S>, m: Method, h: HeaderMap, fds: Option<axum::Extension<ReceivedFds>>, b: B| async move {
                    let fds = fds.map(|axum::Extension(f)| f.take()).unwrap_or_default();
                    reply(async { s.send_user_file(Ctx::new(m, SEND_USER_FILE, h), body(b)?, fds).await }.await)
                },
            ),
        )
        .route(
            SECRET,
            get(
                |State(s): St<S>, m: Method, h: HeaderMap, p: Result<Path<String>, PathRejection>| async move {
                    reply(
                        async {
                            let key = p.map(|Path(k)| k).map_err(|e| ApiError::bad_request(e.body_text()))?;
                            s.secret(Ctx::new(m, SECRET, h), key).await
                        }
                        .await,
                    )
                },
            ),
        );
    finish(router).with_state(server)
}
