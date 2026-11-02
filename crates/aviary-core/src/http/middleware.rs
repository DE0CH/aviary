//! Middleware every service's router uses, and the JSON response helper.

use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracing::Instrument as _;
use uuid::Uuid;

use super::error::ApiError;
use super::headers::{self, AVIARY_REQUEST};

/// The JSON content type of every answer (platform 3.6).
pub const JSON: &str = "application/json; charset=utf-8";

/// A JSON answer with `status` and the full content type.
#[must_use]
pub fn json<T: Serialize + ?Sized>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => {
            let mut r = (status, bytes).into_response();
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
            r
        }
        Err(e) => ApiError::internal(format!("response encoding failed: {e}")).into_response(),
    }
}

tokio::task_local! {
    static CURRENT: Uuid;
}

/// The request id of the request being handled by the current task, if any (set by [`request_id`]).
#[must_use]
pub fn current_request_id() -> Option<Uuid> {
    CURRENT.try_with(|id| *id).ok()
}

/// The request's correlation id (`Aviary-Request`), as an extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub Uuid);

impl<S: Send + Sync> FromRequestParts<S> for RequestId {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(parts
            .extensions
            .get::<Self>()
            .copied()
            .unwrap_or_else(|| Self(Uuid::now_v7()))))
    }
}

/// Middleware: takes the request's `Aviary-Request` when it is a version 7 UUID (platform 3.5) or mints a v7 one, makes it the request's
/// [`RequestId`], the `request` field of every log line written while handling it, the value
/// [`current_request_id`] returns (so outbound calls forward it) and the `aviary-request` header of the answer.
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&AVIARY_REQUEST)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .filter(|id| id.get_version_num() == 7)
        .unwrap_or_else(Uuid::now_v7);
    if let Ok(v) = HeaderValue::from_str(&id.to_string()) {
        req.headers_mut().insert(AVIARY_REQUEST, v);
    }
    req.extensions_mut().insert(RequestId(id));
    let span = tracing::info_span!("request", request = %id);
    let mut r = CURRENT.scope(id, next.run(req).instrument(span)).await;
    if let Ok(v) = HeaderValue::from_str(&id.to_string()) {
        r.headers_mut().insert(AVIARY_REQUEST, v);
    }
    r
}

/// Middleware: `cache-control: no-store` unless the route set one, `x-content-type-options: nosniff`, and the
/// charset on a bare `application/json`.
pub async fn defaults(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if h.get(header::CONTENT_TYPE).is_some_and(|v| v == "application/json") {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
    }
    r
}

/// The envelope code for an error status that was not answered with an envelope: platform 3.6's codes (403
/// `wrong_caller`, 410 `cursor_expired`), plus `method_not_allowed` (405) and `internal` (500), the two codes
/// hermit's contract also uses, which 3.6's list does not name. Any other 4xx is a client error, `bad_request`
/// (409, 412, 416, 431…; the status itself is kept); any other 5xx is `internal`.
fn code_for(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 => "unauthenticated",
        403 => "wrong_caller",
        404 => "not_found",
        405 => "method_not_allowed",
        410 => "cursor_expired",
        413 => "too_large",
        415 => "unsupported_type",
        429 => "rate_limited",
        408 | 502 | 503 | 504 => "unavailable",
        507 => "quota_exceeded",
        400..=499 => "bad_request",
        _ => "internal",
    }
}

/// How much of a non-JSON error body becomes the envelope's `message`.
const MESSAGE_LIMIT: usize = 4096;

/// Middleware: turns every non-2xx answer that is not JSON (axum's extractor rejections, an unmatched route, a
/// method not allowed, a body over the limit) into the envelope, keeping its other headers. A 422 from an extractor
/// becomes 400 `bad_request` (the platform has no 422).
pub async fn envelope(req: Request, next: Next) -> Response {
    let r = next.run(req).await;
    let status = r.status();
    let is_json = r
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if status.is_success() || status.is_informational() || status.is_redirection() || is_json {
        return r;
    }
    let (mut parts, body) = r.into_parts();
    let text = match axum::body::to_bytes(body, MESSAGE_LIMIT).await {
        Ok(b) => String::from_utf8_lossy(&b).trim().to_owned(),
        Err(_) => String::new(),
    };
    let message = if text.is_empty() {
        status.canonical_reason().unwrap_or("error").to_owned()
    } else {
        text
    };
    let status = if status == StatusCode::UNPROCESSABLE_ENTITY {
        StatusCode::BAD_REQUEST
    } else {
        status
    };
    let mut e = ApiError::new(status, code_for(status), message).into_response();
    parts.headers.remove(header::CONTENT_TYPE);
    parts.headers.remove(header::CONTENT_LENGTH);
    for (name, value) in &parts.headers {
        if !e.headers().contains_key(name) {
            e.headers_mut().append(name.clone(), value.clone());
        }
    }
    e
}

/// Middleware for an edge that takes requests from outside: removes every `Aviary-*` header before anything reads
/// it. Add it outside [`standard`] (a later `.layer`), so [`request_id`] then mints a fresh id.
pub async fn strip_all_aviary(mut req: Request, next: Next) -> Response {
    headers::strip_all(req.headers_mut());
    next.run(req).await
}

/// Wraps a service's API router: [`request_id`] (outermost), [`defaults`], [`envelope`] and a request body limit
/// of `body_limit` bytes (answered 413 `too_large`). Also answers unmatched routes with the 404 envelope.
///
/// Only for the `/v1` service APIs (platform 3.6). It rewrites every non-JSON error answer into the JSON envelope,
/// so it must not wrap pigeon's public paths, which speak the frozen protocol of shipped apps and serve the public
/// site with their own bodies (platform 3.4, pigeon 1.1); those routers use [`request_id`] and [`defaults`] alone.
pub fn standard<S: Clone + Send + Sync + 'static>(router: Router<S>, body_limit: usize) -> Router<S> {
    router
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(axum::middleware::from_fn(envelope))
        .layer(axum::middleware::from_fn(defaults))
        .layer(axum::middleware::from_fn(request_id))
}

async fn fallback() -> ApiError {
    ApiError::not_found("no such route")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::RequestBuilderExt as _;
    use axum::body::Body;
    use axum::routing::{get, post};

    async fn bind(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn client() -> reqwest::Client {
        crate::http::client_builder(&crate::version::Build::new("test"), None)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn request_id_is_minted_echoed_and_forwarded() {
        // downstream: echoes the request id it received
        let downstream = bind(standard(
            Router::new().route("/echo", get(|RequestId(id): RequestId| async move { id.to_string() })),
            1024,
        ))
        .await;
        // upstream: calls downstream with the current request id
        let down = downstream.clone();
        let upstream = bind(standard(
            Router::new().route(
                "/call",
                get(move |RequestId(id): RequestId| {
                    let down = down.clone();
                    async move {
                        assert_eq!(current_request_id(), Some(id));
                        let seen = client()
                            .get(format!("{down}/echo"))
                            .forward_request_id()
                            .send()
                            .await
                            .unwrap()
                            .text()
                            .await
                            .unwrap();
                        format!("{id} {seen}")
                    }
                }),
            ),
            1024,
        ))
        .await;
        let c = client();

        // minted: a v7 id, echoed in the answer, the same one reaching downstream
        let r = c.get(format!("{upstream}/call")).send().await.unwrap();
        let echoed = r.headers()["aviary-request"].to_str().unwrap().to_owned();
        let body = r.text().await.unwrap();
        let (mine, seen) = body.split_once(' ').unwrap();
        assert_eq!(mine, echoed);
        assert_eq!(seen, echoed);
        assert_eq!(Uuid::parse_str(mine).unwrap().get_version_num(), 7);

        // forwarded: an incoming id is kept unchanged
        let given = "0190f3b2-7c1a-7d2e-9f00-0123456789ab";
        let r = c
            .get(format!("{upstream}/call"))
            .header("aviary-request", given)
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["aviary-request"], given);
        assert_eq!(r.text().await.unwrap(), format!("{given} {given}"));

        // not a UUID: replaced by a minted one
        let r = c
            .get(format!("{downstream}/echo"))
            .header("aviary-request", "x y")
            .send()
            .await
            .unwrap();
        assert!(Uuid::parse_str(r.headers()["aviary-request"].to_str().unwrap()).is_ok());

        // a UUID that is not v7 (a v4 from an outside client): replaced by a minted v7, also downstream
        let v4 = "9b2f6c1e-3d4a-4f5b-8c6d-7e8f90a1b2c3";
        assert_eq!(Uuid::parse_str(v4).unwrap().get_version_num(), 4);
        let r = c
            .get(format!("{upstream}/call"))
            .header("aviary-request", v4)
            .send()
            .await
            .unwrap();
        let echoed = r.headers()["aviary-request"].to_str().unwrap().to_owned();
        assert_ne!(echoed, v4);
        assert_eq!(Uuid::parse_str(&echoed).unwrap().get_version_num(), 7);
        assert_eq!(r.text().await.unwrap(), format!("{echoed} {echoed}"));

        // outside any request, forward_request_id mints a fresh id
        assert_eq!(current_request_id(), None);
        let id = c
            .get(format!("{downstream}/echo"))
            .forward_request_id()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 7);
    }

    #[tokio::test]
    async fn request_id_reaches_log_lines() {
        let capture = crate::log::Capture::new();
        let sub = crate::log::subscriber("test", None, capture.clone());
        let _guard = tracing::subscriber::set_default(sub);
        // other tests hit the `request` span's callsite on threads with no subscriber; tracing caches a callsite's
        // interest globally, so recompute it now that this thread's subscriber exists
        tracing::callsite::rebuild_interest_cache();
        let given = "0190f3b2-7c1a-7d2e-9f00-0123456789ab";
        let app: Router = standard(
            Router::new().route(
                "/y",
                get(|| async {
                    tracing::info!("handled");
                    "ok"
                }),
            ),
            1024,
        );
        let req = axum::http::Request::get("/y")
            .header("aviary-request", given)
            .body(Body::empty())
            .unwrap();
        let svc = app.into_service::<Body>();
        let r = tower_call(svc, req).await;
        assert_eq!(r.status(), 200);
        let line = capture.lines().into_iter().find(|l| l["msg"] == "handled").unwrap();
        assert_eq!(line["request"], given);
    }

    async fn tower_call<Svc>(mut svc: Svc, req: axum::http::Request<Body>) -> Response
    where
        Svc: tower::Service<axum::http::Request<Body>, Response = Response, Error = std::convert::Infallible>,
    {
        std::future::poll_fn(|cx| svc.poll_ready(cx)).await.unwrap();
        svc.call(req).await.unwrap()
    }

    #[tokio::test]
    async fn defaults_and_envelope() {
        let base = bind(standard(
            Router::new()
                .route(
                    "/json",
                    post(|axum::Json(v): axum::Json<serde_json::Value>| async move { axum::Json(v) }),
                )
                .route(
                    "/cached",
                    get(|| async { ([(header::CACHE_CONTROL, "max-age=60")], "x") }),
                )
                .route(
                    "/teapot",
                    get(|| async { (StatusCode::BAD_GATEWAY, "upstream said no") }),
                )
                .route(
                    "/own",
                    get(|| async { ApiError::conflict("turn_active", "a turn is running").with("turnId", "t1") }),
                ),
            64,
        ))
        .await;
        let c = client();

        let r = c
            .post(format!("{base}/json"))
            .json(&serde_json::json!({"a": 1}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        assert_eq!(r.headers()["content-type"], JSON);

        let r = c.get(format!("{base}/cached")).send().await.unwrap();
        assert_eq!(r.headers()["cache-control"], "max-age=60");

        // unmatched route
        let r = c.get(format!("{base}/none")).send().await.unwrap();
        assert_eq!(r.status(), 404);
        assert_eq!(r.headers()["content-type"], JSON);
        assert_eq!(r.json::<serde_json::Value>().await.unwrap()["code"], "not_found");

        // wrong method: keeps `allow`
        let r = c.get(format!("{base}/json")).send().await.unwrap();
        assert_eq!(r.status(), 405);
        assert!(r.headers().contains_key("allow"));
        assert_eq!(
            r.json::<serde_json::Value>().await.unwrap()["code"],
            "method_not_allowed"
        );

        // a body over the limit
        let big = serde_json::json!({"a": "x".repeat(200)});
        let r = c.post(format!("{base}/json")).json(&big).send().await.unwrap();
        assert_eq!(r.status(), 413);
        assert_eq!(r.json::<serde_json::Value>().await.unwrap()["code"], "too_large");

        // a malformed body: axum's rejection becomes 400 bad_request with its text as the message
        let r = c
            .post(format!("{base}/json"))
            .header("content-type", "application/json")
            .body("{not json")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["code"], "bad_request");
        assert_ne!(v["message"].as_str().unwrap(), "");

        // missing content type
        let r = c.post(format!("{base}/json")).body("{}").send().await.unwrap();
        assert_eq!(r.status(), 415);
        assert_eq!(r.json::<serde_json::Value>().await.unwrap()["code"], "unsupported_type");

        // a handler's plain-text error
        let r = c.get(format!("{base}/teapot")).send().await.unwrap();
        assert_eq!(r.status(), 502);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "unavailable", "message": "upstream said no"})
        );

        // a handler's own envelope passes untouched
        let r = c.get(format!("{base}/own")).send().await.unwrap();
        assert_eq!(r.status(), 409);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "turn_active", "message": "a turn is running", "turnId": "t1"})
        );
    }

    #[test]
    fn unenveloped_statuses_get_a_code_of_their_class() {
        let code = |n: u16| code_for(StatusCode::from_u16(n).unwrap());
        assert_eq!(code(403), "wrong_caller");
        assert_eq!(code(410), "cursor_expired");
        // client errors without a code of their own stay client errors
        for n in [400, 409, 411, 412, 416, 418, 422, 431] {
            assert_eq!(code(n), "bad_request", "{n}");
        }
        for n in [500, 501, 505] {
            assert_eq!(code(n), "internal", "{n}");
        }
        assert_eq!(code(503), "unavailable");
    }

    #[tokio::test]
    async fn bare_client_errors_are_not_server_faults() {
        let base = bind(standard(
            Router::new()
                .route("/forbidden", get(|| async { StatusCode::FORBIDDEN }))
                .route("/conflict", get(|| async { StatusCode::CONFLICT }))
                .route("/range", get(|| async { StatusCode::RANGE_NOT_SATISFIABLE })),
            1024,
        ))
        .await;
        let c = client();
        for (path, status, code) in [
            ("forbidden", 403, "wrong_caller"),
            ("conflict", 409, "bad_request"),
            ("range", 416, "bad_request"),
        ] {
            let r = c.get(format!("{base}/{path}")).send().await.unwrap();
            assert_eq!(r.status().as_u16(), status);
            assert_eq!(r.json::<serde_json::Value>().await.unwrap()["code"], code, "{path}");
        }
    }

    #[tokio::test]
    async fn edge_strips_every_aviary_header_and_mints() {
        let router = standard(
            Router::new().route(
                "/h",
                get(|h: axum::http::HeaderMap| async move {
                    let mut n: Vec<String> = h
                        .keys()
                        .filter(|k| k.as_str().starts_with("aviary-"))
                        .map(ToString::to_string)
                        .collect();
                    n.sort();
                    n.join(",")
                }),
            ),
            1024,
        )
        .layer(axum::middleware::from_fn(strip_all_aviary));
        let base = bind(router).await;
        let given = "0190f3b2-7c1a-7d2e-9f00-0123456789ab";
        let r = client()
            .get(format!("{base}/h"))
            .header("aviary-person", "spoof@b.c")
            .header("aviary-sandbox-id", "spoof")
            .header("aviary-request", given)
            .send()
            .await
            .unwrap();
        assert_ne!(r.headers()["aviary-request"], given, "an outside id is not trusted");
        // only the id minted by request_id is left
        assert_eq!(r.text().await.unwrap(), "aviary-request");
    }
}
