//! Scripted answers and the call log, shared by every fake.
//!
//! Each fake names its routes by the method names of its contract crate's `Server` trait (`put_sandbox`,
//! `cc_write`, …; for magpie these are also the route ids of `magpie_api::routes::ROUTES`). For one route a test
//! may queue answers given once ([`Script::once`]), install a handler that sees the call and may answer
//! ([`Script::handle`]), or set an answer given every time ([`Script::always`]); they are tried in that order. A
//! route nobody scripted falls through to the fake's own model (if it has one for that route) and then to the
//! contract's 501 `not_supported`.
//!
//! Every call is recorded before it is answered ([`Script::calls`], [`Script::wait_calls`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::http::ApiError;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Notify;

/// One request as a fake received it, after the contract router parsed it.
#[derive(Debug, Clone)]
pub struct Call {
    /// The route's name (the trait method).
    pub route: &'static str,
    /// Path parameters, as JSON strings by the contract's parameter names (`{"id": "…"}`).
    pub params: Value,
    /// The parsed query, as JSON (`None` when the route takes none).
    pub query: Option<Value>,
    /// The parsed JSON body (`None` when the route takes none or a raw body).
    pub body: Option<Value>,
    /// The raw body of routes that take bytes.
    pub bytes: Option<Bytes>,
    /// Every request header.
    pub headers: HeaderMap,
}

impl Call {
    /// A header's value as a string.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// A path parameter.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).and_then(Value::as_str)
    }
}

/// The body of a scripted answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyBody {
    /// No body (204, 304).
    None,
    /// A JSON body.
    Json(Value),
    /// Raw bytes (file and blob routes).
    Bytes(Bytes),
}

/// A scripted answer: a status, extra headers and a body. A status of 400 or more is an error, and its JSON body is
/// the error envelope (`{"code", "message", …}`).
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// Extra response headers.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: ReplyBody,
}

impl Reply {
    /// 200 with a JSON body.
    #[must_use]
    pub fn ok(body: Value) -> Self {
        Self::json(200, body)
    }

    /// A JSON body with `status`.
    #[must_use]
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ReplyBody::Json(body),
        }
    }

    /// A status with no body.
    #[must_use]
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ReplyBody::None,
        }
    }

    /// Raw bytes with `status`.
    #[must_use]
    pub fn bytes(status: u16, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ReplyBody::Bytes(body.into()),
        }
    }

    /// An error envelope `{"code", "message"}` with `status`.
    #[must_use]
    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(status, serde_json::json!({"code": code, "message": message}))
    }

    /// The same answer with one more header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// One response entry of a golden fixture: `{"status", "body"?, "bytes"?, "headers"?}`.
    ///
    /// # Panics
    /// When the entry has no numeric `status` (a broken fixture is a test bug).
    #[must_use]
    pub fn from_fixture(entry: &Value) -> Self {
        let status = entry
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|s| u16::try_from(s).ok())
            .unwrap_or_else(|| panic!("fixture response without a status: {entry}"));
        let body = if let Some(b) = entry.get("bytes").and_then(Value::as_str) {
            ReplyBody::Bytes(Bytes::from(b.as_bytes().to_vec()))
        } else if let Some(b) = entry.get("body") {
            ReplyBody::Json(b.clone())
        } else {
            ReplyBody::None
        };
        let headers = entry
            .get("headers")
            .and_then(Value::as_object)
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Self { status, headers, body }
    }

    /// Is this an error answer (status 400 or more)?
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.status >= 400
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The error this answer stands for: the envelope's `code` and `message`, every other field kept, and
    /// `retry-after` from the headers.
    #[must_use]
    pub fn api_error(&self) -> ApiError {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let obj = match &self.body {
            ReplyBody::Json(Value::Object(m)) => m.clone(),
            _ => serde_json::Map::new(),
        };
        let code = obj.get("code").and_then(Value::as_str).unwrap_or("internal").to_owned();
        let message = obj.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
        let mut e = ApiError::new(status, code, message);
        for (k, v) in obj {
            if k != "code" && k != "message" {
                e = e.with(&k, v);
            }
        }
        if let Some(secs) = self.header("retry-after").and_then(|v| v.trim().parse().ok()) {
            e = e.retry_after(secs);
        }
        e
    }

    /// The JSON body, or `null`.
    #[must_use]
    pub fn json_body(&self) -> Value {
        match &self.body {
            ReplyBody::Json(v) => v.clone(),
            _ => Value::Null,
        }
    }

    /// The answer as a typed value: an error status becomes the error, a success body is parsed as `T`. A body
    /// that does not parse is a 500 naming the route (a script bug shows up in the test, not as a hang).
    ///
    /// # Errors
    /// The scripted error, or 500 when the body does not parse.
    pub fn typed<T: DeserializeOwned>(&self, route: &str) -> Result<T, ApiError> {
        if self.is_error() {
            return Err(self.api_error());
        }
        let v = match &self.body {
            // `{}` routes (magpie's `Empty`) may be scripted with no body
            ReplyBody::None => Value::Object(serde_json::Map::new()),
            _ => self.json_body(),
        };
        serde_json::from_value(v)
            .map_err(|e| ApiError::internal(format!("fake: the scripted answer of {route} does not parse: {e}")))
    }

    /// The answer of a route without a body: an error status becomes the error.
    ///
    /// # Errors
    /// The scripted error.
    pub fn unit(&self) -> Result<(), ApiError> {
        if self.is_error() { Err(self.api_error()) } else { Ok(()) }
    }

    /// The answer as an HTTP response (raw routes): status, headers and body as scripted.
    ///
    /// # Errors
    /// The scripted error.
    pub fn response(&self) -> Result<Response, ApiError> {
        if self.is_error() {
            return Err(self.api_error());
        }
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        let mut res = match &self.body {
            ReplyBody::None => status.into_response(),
            ReplyBody::Json(v) => aviary_core::http::json(status, v),
            ReplyBody::Bytes(b) => {
                let mut r = (status, b.clone()).into_response();
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                );
                r
            }
        };
        for (k, v) in &self.headers {
            if let (Ok(k), Ok(v)) = (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v)) {
                res.headers_mut().insert(k, v);
            }
        }
        Ok(res)
    }
}

/// A handler: sees the call and answers, or returns `None` to let the fake's model answer.
pub type Handler = Arc<dyn Fn(&Call) -> Option<Reply> + Send + Sync>;

#[derive(Default)]
struct RouteScript {
    once: VecDeque<Reply>,
    handler: Option<Handler>,
    always: Option<Reply>,
}

#[derive(Default)]
struct Inner {
    routes: HashMap<String, RouteScript>,
    calls: Vec<Call>,
}

/// The scripted answers and the call log of one fake.
#[derive(Default)]
pub struct Script {
    inner: Mutex<Inner>,
    called: Notify,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("Script")
            .field("routes", &inner.routes.keys().collect::<Vec<_>>())
            .field("calls", &inner.calls.len())
            .finish()
    }
}

impl Script {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Answer the next call of `route` with `reply` (queued behind earlier `once` answers).
    pub fn once(&self, route: &str, reply: Reply) {
        self.lock()
            .routes
            .entry(route.to_owned())
            .or_default()
            .once
            .push_back(reply);
    }

    /// Answer every call of `route` with `reply` (after queued `once` answers and the handler).
    pub fn always(&self, route: &str, reply: Reply) {
        self.lock().routes.entry(route.to_owned()).or_default().always = Some(reply);
    }

    /// Let `f` answer calls of `route`; `None` from it falls through to the next answer.
    pub fn handle(&self, route: &str, f: impl Fn(&Call) -> Option<Reply> + Send + Sync + 'static) {
        self.lock().routes.entry(route.to_owned()).or_default().handler = Some(Arc::new(f));
    }

    /// Forget everything scripted for `route`.
    pub fn clear(&self, route: &str) {
        self.lock().routes.remove(route);
    }

    /// Forget every script and every recorded call.
    pub fn reset(&self) {
        let mut inner = self.lock();
        inner.routes.clear();
        inner.calls.clear();
    }

    /// Every call so far, in arrival order.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// The calls of one route so far.
    #[must_use]
    pub fn calls_to(&self, route: &str) -> Vec<Call> {
        self.lock().calls.iter().filter(|c| c.route == route).cloned().collect()
    }

    /// Wait until `route` has been called at least `n` times, or `timeout` passes; the calls so far either way.
    pub async fn wait_calls(&self, route: &str, n: usize, timeout: Duration) -> Vec<Call> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let called = self.called.notified();
            let calls = self.calls_to(route);
            if calls.len() >= n {
                return calls;
            }
            if tokio::time::timeout_at(deadline, called).await.is_err() {
                return self.calls_to(route);
            }
        }
    }

    /// Record `call` and return its scripted answer, if any.
    pub fn take(&self, call: Call) -> Option<Reply> {
        let route = call.route;
        let (handler, queued) = {
            let mut inner = self.lock();
            let found = match inner.routes.get_mut(route) {
                None => (None, None),
                Some(e) => match e.once.pop_front() {
                    Some(r) => (None, Some(r)),
                    None => (e.handler.clone().map(|h| (h, call.clone())), None),
                },
            };
            inner.calls.push(call);
            found
        };
        self.called.notify_waiters();
        if queued.is_some() {
            return queued;
        }
        if let Some((h, call)) = handler
            && let Some(r) = h(&call)
        {
            return Some(r);
        }
        self.lock().routes.get(route).and_then(|e| e.always.clone())
    }
}
