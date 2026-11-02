//! The gateway suite's fakes (platform 8): magpie, hermit, tools and the model upstream on one listener that
//! answers by `Host` (`magpie`, `hermit`, `tools`; any other host is the model upstream), so the implementation
//! under test reaches them at its configured upstreams (Docker network aliases, Services, or local addresses).
//!
//! - **hermit** publishes the suite's token keys, answers liveness as each check sets it, serves `/v1/sandbox/**`,
//!   and can be taken down ([`Outage`]: connections dropped, answers held past the gateway's 1 s timeout, 503).
//! - **magpie** counts request bodies (`{"bytes": n}`) and holds `GET /v1/sandbox/poll?ms=` that long.
//! - **tools** answers `/mcp` after `?delayMs=`.
//! - **model** serves the models route and the Messages API: a JSON reply, or an SSE stream whose length the
//!   request's `x-conformance-stream-ms` header sets, recording when it sent its first data byte.
//!
//! Every request is recorded with its headers, keyed by the suite's `x-conformance-case` header, and checked for
//! the credential the gateway must present (401 otherwise).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aviary_fakes::serve::{ConnKill, Served, serve_on};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use gateway_api::{TokenKey, TokenKeys, TokenLiveness};
use hermit_api::Timestamp;
use hermit_api::sandbox::SandboxState;
use serde_json::{Value, json};
use uuid::Uuid;

/// The suite's case header: every request a check sends carries it, so the fakes' records can be told apart.
pub const CASE: &str = "x-conformance-case";
/// The length of a model stream in milliseconds.
pub const STREAM_MS: &str = "x-conformance-stream-ms";
/// Gap between two stream events.
pub const STREAM_TICK: Duration = Duration::from_millis(500);
/// The model fake waits this long after the response head before the first data byte, so the relay of the body
/// (not of the head) is what the first-byte check measures.
pub const FIRST_BYTE_DELAY: Duration = Duration::from_millis(400);

/// Which fake answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Name {
    Magpie,
    Hermit,
    Tools,
    Model,
}

/// The pair tokens and model credential the gateway must present to the fakes.
#[derive(Clone)]
pub struct Expected {
    pub magpie: String,
    pub hermit: String,
    pub tools: String,
    pub model: String,
}

impl std::fmt::Debug for Expected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Expected(..)")
    }
}

/// hermit's state for one sandbox id, as the liveness route answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    Live,
    NotLive,
    /// 404 `unknown_sandbox`.
    Unknown,
}

/// How hermit is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outage {
    Up,
    /// The connection is closed with no answer.
    Refuse,
    /// Answers 503 after 5 s (past the gateway's 1 s timeout).
    Hang,
    /// 503 at once.
    Unavailable,
}

/// One recorded request.
#[derive(Clone, Debug)]
pub struct Seen {
    pub name: Name,
    pub method: String,
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub case: Option<String>,
    pub body_bytes: u64,
    pub at: Instant,
}

impl Seen {
    /// A header's value as text.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

#[derive(Debug)]
struct Sbx {
    live: Live,
    conversation: Uuid,
    person: String,
}

#[derive(Debug)]
struct St {
    expected: Expected,
    keys: Mutex<Vec<TokenKey>>,
    key_fetches: AtomicU64,
    liveness: Mutex<HashMap<Uuid, Sbx>>,
    liveness_lookups: Mutex<HashMap<Uuid, u64>>,
    outage: Mutex<Outage>,
    seen: Mutex<Vec<Seen>>,
    first_byte: Mutex<HashMap<String, Instant>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fakes and their controls.
#[derive(Clone, Debug)]
pub struct Fakes {
    st: Arc<St>,
}

impl Fakes {
    /// Fakes expecting `expected` and publishing `keys`.
    #[must_use]
    pub fn new(expected: Expected, keys: Vec<TokenKey>) -> Self {
        Self {
            st: Arc::new(St {
                expected,
                keys: Mutex::new(keys),
                key_fetches: AtomicU64::new(0),
                liveness: Mutex::new(HashMap::new()),
                liveness_lookups: Mutex::new(HashMap::new()),
                outage: Mutex::new(Outage::Up),
                seen: Mutex::new(Vec::new()),
                first_byte: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Serves the fakes on `listener` until the returned handle is dropped.
    ///
    /// # Errors
    /// When the listener's address cannot be read.
    pub fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<Served> {
        let router = Router::new().fallback(dispatch).with_state(self.st.clone());
        serve_on(listener, router)
    }

    /// Publishes one more key (a rotation).
    pub fn publish_key(&self, key: TokenKey) {
        lock(&self.st.keys).push(key);
    }

    /// How many times the key set was fetched.
    #[must_use]
    pub fn key_fetches(&self) -> u64 {
        self.st.key_fetches.load(Ordering::SeqCst)
    }

    /// Sets what liveness answers for `sandbox`.
    pub fn set_live(&self, sandbox: Uuid, conversation: Uuid, person: &str, live: Live) {
        lock(&self.st.liveness).insert(
            sandbox,
            Sbx {
                live,
                conversation,
                person: person.to_owned(),
            },
        );
    }

    /// Changes an existing sandbox's liveness.
    pub fn flip(&self, sandbox: Uuid, live: Live) {
        if let Some(s) = lock(&self.st.liveness).get_mut(&sandbox) {
            s.live = live;
        }
    }

    /// How many liveness lookups hermit answered for `sandbox` (answered: not during an outage).
    #[must_use]
    pub fn liveness_lookups(&self, sandbox: Uuid) -> u64 {
        lock(&self.st.liveness_lookups).get(&sandbox).copied().unwrap_or(0)
    }

    /// Takes hermit down or brings it back.
    pub fn set_outage(&self, o: Outage) {
        *lock(&self.st.outage) = o;
    }

    /// The requests recorded for a case.
    #[must_use]
    pub fn seen(&self, case: &str) -> Vec<Seen> {
        lock(&self.st.seen)
            .iter()
            .filter(|s| s.case.as_deref() == Some(case))
            .cloned()
            .collect()
    }

    /// Every recorded request whose path starts with `prefix`.
    #[must_use]
    pub fn seen_path(&self, prefix: &str) -> Vec<Seen> {
        lock(&self.st.seen)
            .iter()
            .filter(|s| s.path_and_query.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// When the model fake sent the first data byte of a case's stream.
    #[must_use]
    pub fn first_byte(&self, case: &str) -> Option<Instant> {
        lock(&self.st.first_byte).get(case).copied()
    }
}

fn which(headers: &HeaderMap) -> Name {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let name = host
        .rsplit_once(':')
        .map_or(host, |(h, p)| if p.parse::<u16>().is_ok() { h } else { host });
    match name.to_ascii_lowercase().as_str() {
        "magpie" => Name::Magpie,
        "hermit" => Name::Hermit,
        "tools" => Name::Tools,
        _ => Name::Model,
    }
}

fn credential_ok(st: &St, name: Name, h: &HeaderMap) -> bool {
    let bearer = aviary_core::auth::bearer(h);
    let want = match name {
        Name::Magpie => &st.expected.magpie,
        Name::Hermit => &st.expected.hermit,
        Name::Tools => &st.expected.tools,
        Name::Model => {
            let key = h.get("x-api-key").and_then(|v| v.to_str().ok());
            return key == Some(st.expected.model.as_str()) || bearer == Some(st.expected.model.as_str());
        }
    };
    bearer == Some(want.as_str())
}

fn error(status: StatusCode, code: &str) -> Response {
    let mut r = (
        status,
        axum::Json(json!({"code": code, "message": format!("fake: {code}")})),
    )
        .into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        r.headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    }
    r
}

fn query_u64(pq: &str, key: &str) -> Option<u64> {
    let q = pq.split_once('?')?.1;
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

async fn dispatch(State(st): State<Arc<St>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let name = which(&parts.headers);
    let pq = parts.uri.path_and_query().map_or("/", |p| p.as_str()).to_owned();
    let path = parts.uri.path().to_owned();
    let case = parts.headers.get(CASE).and_then(|v| v.to_str().ok()).map(str::to_owned);

    if name == Name::Hermit {
        let outage = *lock(&st.outage);
        match outage {
            Outage::Up => {}
            Outage::Refuse => {
                if let Some(k) = parts.extensions.get::<ConnKill>() {
                    k.kill();
                }
                std::future::pending::<()>().await;
            }
            Outage::Hang => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
            }
            Outage::Unavailable => return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        }
    }

    // count the body as it arrives, never holding it
    let mut stream = body.into_data_stream();
    let mut body_bytes: u64 = 0;
    let mut json_body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(b) => {
                body_bytes += b.len() as u64;
                if name == Name::Model && json_body.len() < (1 << 20) {
                    json_body.extend_from_slice(&b);
                }
            }
            Err(_) => return error(StatusCode::BAD_REQUEST, "body_failed"),
        }
    }
    lock(&st.seen).push(Seen {
        name,
        method: parts.method.to_string(),
        path_and_query: pq.clone(),
        headers: parts.headers.clone(),
        case: case.clone(),
        body_bytes,
        at: Instant::now(),
    });
    if !credential_ok(&st, name, &parts.headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }

    match name {
        Name::Hermit => hermit(&st, &parts.method, &path),
        Name::Magpie => {
            if path == "/v1/sandbox/poll" {
                let ms = query_u64(&pq, "ms").unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(ms)).await;
                return axum::Json(json!({"waited": ms})).into_response();
            }
            axum::Json(json!({"bytes": body_bytes})).into_response()
        }
        Name::Tools => {
            let ms = query_u64(&pq, "delayMs").unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            axum::Json(json!({"jsonrpc": "2.0", "id": 1, "result": {"delayedMs": ms}})).into_response()
        }
        Name::Model => model(&st, &path, &parts.headers, case, &json_body),
    }
}

fn hermit(st: &St, method: &axum::http::Method, path: &str) -> Response {
    if method == axum::http::Method::GET && path == gateway_api::SANDBOX_TOKEN_KEYS {
        st.key_fetches.fetch_add(1, Ordering::SeqCst);
        return axum::Json(TokenKeys {
            keys: lock(&st.keys).clone(),
        })
        .into_response();
    }
    if let Some(id) = path.strip_prefix("/v1/sandbox-tokens/") {
        let Ok(id) = id.parse::<Uuid>() else {
            return error(StatusCode::BAD_REQUEST, "bad_request");
        };
        *lock(&st.liveness_lookups).entry(id).or_insert(0) += 1;
        let live = lock(&st.liveness);
        let Some(s) = live.get(&id) else {
            return error(StatusCode::NOT_FOUND, "unknown_sandbox");
        };
        if s.live == Live::Unknown {
            return error(StatusCode::NOT_FOUND, "unknown_sandbox");
        }
        let is_live = s.live == Live::Live;
        return axum::Json(TokenLiveness {
            sandbox_id: id,
            conversation_id: s.conversation,
            person: s.person.clone(),
            state: if is_live {
                SandboxState::Ready
            } else {
                SandboxState::Gone
            },
            live: is_live,
            expires_at: Timestamp::now(),
        })
        .into_response();
    }
    if path.starts_with(hermit_api::routes::SANDBOX_SURFACE_PREFIX) {
        return axum::Json(json!({"ok": true})).into_response();
    }
    error(StatusCode::NOT_FOUND, "not_found")
}

const MODELS: &str = include_str!("../../../crates/gateway-api/fixtures/models.json");

fn sse(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

/// The events of one scripted stream lasting about `ms`: the opening events, one text delta per tick, the closing
/// events. The number of deltas is `max(1, ms / tick)`.
#[must_use]
pub fn stream_events(ms: u64) -> Vec<Bytes> {
    let deltas = (ms / u64::try_from(STREAM_TICK.as_millis()).unwrap_or(500)).max(1);
    let mut out = vec![
        sse(
            "message_start",
            &json!({"type": "message_start", "message": {"id": "msg_conformance", "type": "message", "role": "assistant",
                "model": "conformance", "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 3, "output_tokens": 0}}}),
        ),
        sse(
            "content_block_start",
            &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
    ];
    for i in 0..deltas {
        out.push(sse(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": format!("t{i} ")}}),
        ));
    }
    out.push(sse(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": 0}),
    ));
    out.push(sse(
        "message_delta",
        &json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": deltas}}),
    ));
    out.push(sse("message_stop", &json!({"type": "message_stop"})));
    out
}

fn model(st: &Arc<St>, path: &str, headers: &HeaderMap, case: Option<String>, body: &[u8]) -> Response {
    match path {
        gateway_api::models::MODELS => {
            let mut r = (StatusCode::OK, MODELS).into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            r
        }
        gateway_api::models::COUNT_TOKENS => axum::Json(json!({"input_tokens": 7})).into_response(),
        gateway_api::models::MESSAGES => {
            let req: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            if req.get("stream").and_then(Value::as_bool) != Some(true) {
                return axum::Json(json!({"id": "msg_conformance", "type": "message", "role": "assistant",
                    "model": "conformance", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
                    "stop_sequence": null, "usage": {"input_tokens": 3, "output_tokens": 1}}))
                .into_response();
            }
            let ms = headers
                .get(STREAM_MS)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(2000);
            let events = stream_events(ms);
            let st = st.clone();
            let stream = futures_util::stream::unfold((0usize, events), move |(i, events)| {
                let st = st.clone();
                let case = case.clone();
                async move {
                    let ev = events.get(i)?.clone();
                    if i == 0 {
                        tokio::time::sleep(FIRST_BYTE_DELAY).await;
                        if let Some(c) = case {
                            lock(&st.first_byte).insert(c, Instant::now());
                        }
                    } else if i > 2 && i < events.len() - 3 {
                        tokio::time::sleep(STREAM_TICK).await;
                    }
                    Some((Ok::<_, std::convert::Infallible>(ev), (i + 1, events)))
                }
            });
            let mut r = Response::new(Body::from_stream(stream));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            r.headers_mut()
                .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache"));
            r
        }
        _ => error(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// The address a listener for the fakes binds, from `--serve-fakes` (`:80` means every address, port 80).
///
/// # Errors
/// When the value is not `[host]:port`.
pub fn listen_addr(v: &str) -> Result<SocketAddr, String> {
    let v = if v.starts_with(':') {
        format!("0.0.0.0{v}")
    } else {
        v.to_owned()
    };
    v.parse().map_err(|e| format!("--serve-fakes {v}: {e}"))
}
