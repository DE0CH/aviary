//! fake-tools (platform 7.6): a Streamable-HTTP MCP server at `/mcp` with two tools, `echo` and `slow_echo`
//! (`delayMs` before answering), behind the gateway's tools pair token, recording every call with the
//! `Aviary-Sandbox-Person` the gateway added.
//!
//! JSON-RPC over `POST /mcp`, answered as `application/json` (the Streamable-HTTP transport allows a JSON answer
//! instead of an SSE stream): `initialize` (with a fresh `Mcp-Session-Id`), `notifications/initialized` (202),
//! `ping`, `tools/list`, `tools/call`. Every request after `initialize` must carry a session `initialize` handed
//! out and that has not ended: none is 400, an unknown or ended one 404 (MCP's "session management").
//! `GET /mcp` is 405 (no server-initiated stream), `DELETE /mcp` ends the session (204; then 404 for it).
//!
//! It accepts a list of bearers (each the gateway's tools pair token), so a rotation through the gateway (the
//! current token and the `_NEXT` one both accepted, then the old one dropped) can be driven: [`FakeTools::with_tokens`],
//! [`FakeTools::set_tokens`], or one token per line of a token file ([`FakeTools::from_token_file`], `--token-file`),
//! read again on every request, so a mounted Secret that changes in place changes the accepted tokens at once (a
//! file that cannot be read or holds no token keeps the last set).
//! Assertions read `GET /__calls`; `POST /__reset` clears them and the sessions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq as _;

/// The MCP protocol revision answered to `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// The longest `slow_echo` waits.
pub const MAX_DELAY: Duration = Duration::from_secs(600);

/// One recorded tool call.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// The tool.
    pub tool: String,
    /// Its arguments.
    pub arguments: Value,
    /// `Aviary-Sandbox-Person`.
    pub person: Option<String>,
    /// `Aviary-Sandbox-Conversation`.
    pub conversation: Option<String>,
    /// `Mcp-Session-Id`.
    pub session: Option<String>,
}

/// The server.
#[derive(Debug)]
pub struct FakeTools {
    tokens: Mutex<Vec<String>>,
    token_file: Option<PathBuf>,
    calls: Mutex<Vec<ToolCall>>,
    sessions: Mutex<HashSet<String>>,
}

fn tools() -> Value {
    json!([
        {"name": "echo", "description": "Answer the text given.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "slow_echo", "description": "Answer the text given after delayMs milliseconds.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}, "delayMs": {"type": "integer"}},
                         "required": ["text", "delayMs"]}}
    ])
}

fn rpc_result(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn header_str(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

impl FakeTools {
    /// A server accepting `token` as the bearer.
    #[must_use]
    pub fn new(token: &str) -> Arc<Self> {
        Self::with_tokens(&[token])
    }

    /// A server accepting any of `tokens` (a rotation's overlap).
    #[must_use]
    pub fn with_tokens(tokens: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(tokens.iter().map(|t| (*t).to_owned()).collect()),
            token_file: None,
            calls: Mutex::new(Vec::new()),
            sessions: Mutex::new(HashSet::new()),
        })
    }

    /// A server accepting the tokens of `path` (one per line), read again on every request.
    ///
    /// # Errors
    /// When the file cannot be read or holds no token now.
    pub fn from_token_file(path: &Path) -> std::io::Result<Arc<Self>> {
        let tokens = read_tokens(path)?;
        Ok(Arc::new(Self {
            tokens: Mutex::new(tokens),
            token_file: Some(path.to_owned()),
            calls: Mutex::new(Vec::new()),
            sessions: Mutex::new(HashSet::new()),
        }))
    }

    /// Replace the accepted bearers (the rotation's next step).
    pub fn set_tokens(&self, tokens: &[&str]) {
        *lock(&self.tokens) = tokens.iter().map(|t| (*t).to_owned()).collect();
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ToolCall>> {
        lock(&self.calls)
    }

    /// The sessions open now.
    #[must_use]
    pub fn sessions(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// `Ok` for a live session; the status otherwise (400 without one, 404 for an unknown or ended one).
    fn session(&self, h: &HeaderMap) -> Result<String, StatusCode> {
        let s = header_str(h, "mcp-session-id").ok_or(StatusCode::BAD_REQUEST)?;
        if lock(&self.sessions).contains(&s) {
            Ok(s)
        } else {
            Err(StatusCode::NOT_FOUND)
        }
    }

    /// The tool calls so far.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolCall> {
        self.lock().clone()
    }

    /// The router.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/mcp", post(mcp).get(no_stream).delete(end_session))
            .route("/__calls", get(calls))
            .route("/__reset", post(reset))
            .with_state(self.clone())
    }

    fn authorised(&self, h: &HeaderMap) -> bool {
        let Some(t) = aviary_core::auth::bearer(h) else {
            return false;
        };
        if let Some(path) = &self.token_file {
            match read_tokens(path) {
                Ok(fresh) => *lock(&self.tokens) = fresh,
                Err(e) => eprintln!("fake-tools: {}: {e}; keeping the last tokens", path.display()),
            }
        }
        // compare against every token, without stopping at a match
        lock(&self.tokens)
            .iter()
            .fold(false, |found, ok| found | bool::from(t.as_bytes().ct_eq(ok.as_bytes())))
    }

    async fn call_tool(&self, headers: &HeaderMap, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or_default();
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let text = args["text"].as_str().map(str::to_owned);
        let delay = match name {
            "echo" => None,
            "slow_echo" => Some(
                Duration::from_millis(args["delayMs"].as_u64().ok_or((-32602, "delayMs".to_owned()))?).min(MAX_DELAY),
            ),
            other => return Err((-32602, format!("unknown tool {other}"))),
        };
        let text = text.ok_or((-32602, "text".to_owned()))?;
        self.lock().push(ToolCall {
            tool: name.to_owned(),
            arguments: args,
            person: header_str(headers, "aviary-sandbox-person"),
            conversation: header_str(headers, "aviary-sandbox-conversation"),
            session: header_str(headers, "mcp-session-id"),
        });
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        Ok(json!({"content": [{"type": "text", "text": text}], "isError": false}))
    }
}

/// The non-empty, trimmed lines of `path`; an error when there is none.
fn read_tokens(path: &Path) -> std::io::Result<Vec<String>> {
    let tokens: Vec<String> = std::fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "no token"));
    }
    Ok(tokens)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn json_response(status: StatusCode, v: &Value, session: Option<&str>) -> Response {
    let mut res = aviary_core::http::json(status, v);
    if let Some(s) = session.and_then(|s| HeaderValue::from_str(s).ok()) {
        res.headers_mut().insert("mcp-session-id", s);
    }
    res
}

async fn mcp(State(t): State<Arc<FakeTools>>, headers: HeaderMap, body: Bytes) -> Response {
    if !t.authorised(&headers) {
        let mut res = aviary_core::http::ApiError::unauthenticated().into_response();
        res.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return res;
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &rpc_error(&Value::Null, -32700, "parse error"),
            None,
        );
    };
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or_default();
    if method != "initialize"
        && let Err(status) = t.session(&headers)
    {
        let message = if status == StatusCode::BAD_REQUEST {
            "no Mcp-Session-Id"
        } else {
            "unknown or ended session"
        };
        return json_response(
            status,
            &rpc_error(id.as_ref().unwrap_or(&Value::Null), -32000, message),
            None,
        );
    }
    let Some(id) = id else {
        // a notification (notifications/initialized, cancelled …)
        return StatusCode::ACCEPTED.into_response();
    };
    match method {
        "initialize" => {
            let session = uuid::Uuid::new_v4().to_string();
            lock(&t.sessions).insert(session.clone());
            let result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "fake-tools", "version": env!("CARGO_PKG_VERSION")}
            });
            json_response(StatusCode::OK, &rpc_result(&id, &result), Some(&session))
        }
        "ping" => json_response(StatusCode::OK, &rpc_result(&id, &json!({})), None),
        "tools/list" => json_response(StatusCode::OK, &rpc_result(&id, &json!({"tools": tools()})), None),
        "tools/call" => match t.call_tool(&headers, &msg["params"]).await {
            Ok(r) => json_response(StatusCode::OK, &rpc_result(&id, &r), None),
            Err((code, m)) => json_response(StatusCode::OK, &rpc_error(&id, code, &m), None),
        },
        other => json_response(
            StatusCode::OK,
            &rpc_error(&id, -32601, &format!("method not found: {other}")),
            None,
        ),
    }
}

async fn no_stream() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn end_session(State(t): State<Arc<FakeTools>>, headers: HeaderMap) -> Response {
    if !t.authorised(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match t.session(&headers) {
        Ok(s) => {
            lock(&t.sessions).remove(&s);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(status) => status.into_response(),
    }
}

async fn calls(State(t): State<Arc<FakeTools>>) -> Response {
    aviary_core::http::json(StatusCode::OK, &t.calls())
}

async fn reset(State(t): State<Arc<FakeTools>>) -> Response {
    t.lock().clear();
    lock(&t.sessions).clear();
    StatusCode::NO_CONTENT.into_response()
}
