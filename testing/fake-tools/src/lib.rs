//! fake-tools (platform 7.6): the tools REST API (decision 15) with two tools, `echo` and `slow_echo` (`delayMs`
//! before answering), behind the gateway's tools pair token, recording every call with the `Aviary-Sandbox-Person`
//! the gateway added.
//!
//! The shape of jarvis3's tools (`chat/src/tools/server.ts`): `GET /v1/tools` lists the tools (`name`, `title`,
//! `description`, `inputSchema`); `POST /v1/tools/<name>` runs one with its JSON arguments as the body and answers the
//! tool's text as `text/plain`; bad arguments are 400 `{"code": "bad_request", "message"}`, an unknown tool 404;
//! `GET /v1/openapi.json` is the same as `OpenAPI` 3.1. Any other path is 404, a wrong method 405.
//!
//! It accepts a list of bearers (each the gateway's tools pair token), so a rotation through the gateway (the
//! current token and the `_NEXT` one both accepted, then the old one dropped) can be driven: [`FakeTools::with_tokens`],
//! [`FakeTools::set_tokens`], or one token per line of a token file ([`FakeTools::from_token_file`], `--token-file`),
//! read again on every request, so a mounted Secret that changes in place changes the accepted tokens at once (a
//! file that cannot be read or holds no token keeps the last set).
//! Assertions read `GET /__calls`; `POST /__reset` clears them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq as _;

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
}

/// The server.
#[derive(Debug)]
pub struct FakeTools {
    tokens: Mutex<Vec<String>>,
    token_file: Option<PathBuf>,
    calls: Mutex<Vec<ToolCall>>,
}

fn tools() -> Value {
    json!([
        {"name": "echo", "title": "Echo", "description": "Answer the text given.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "slow_echo", "title": "Slow echo", "description": "Answer the text given after delayMs milliseconds.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}, "delayMs": {"type": "integer"}},
                         "required": ["text", "delayMs"]}}
    ])
}

/// `GET /v1/openapi.json`: one `POST /v1/tools/<name>` per tool.
fn openapi() -> Value {
    let mut paths = serde_json::Map::new();
    for t in tools().as_array().into_iter().flatten() {
        paths.insert(
            format!("/v1/tools/{}", t["name"].as_str().unwrap_or_default()),
            json!({"post": {"operationId": t["name"], "summary": t["title"], "description": t["description"],
                "requestBody": {"required": true, "content": {"application/json": {"schema": t["inputSchema"]}}},
                "responses": {"200": {"description": "the text, text/plain"}, "400": {"description": "bad arguments"}}}}),
        );
    }
    json!({"openapi": "3.1.0", "info": {"title": "fake-tools", "version": "1"}, "paths": paths})
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
        }))
    }

    /// Replace the accepted bearers (the rotation's next step).
    pub fn set_tokens(&self, tokens: &[&str]) {
        *lock(&self.tokens) = tokens.iter().map(|t| (*t).to_owned()).collect();
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ToolCall>> {
        lock(&self.calls)
    }

    /// The tool calls so far.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolCall> {
        self.lock().clone()
    }

    /// The router.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/v1/tools", get(list))
            .route("/v1/tools/{name}", post(call))
            .route("/v1/openapi.json", get(openapi_route))
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

    async fn call_tool(&self, headers: &HeaderMap, name: &str, args: Value) -> Result<String, (StatusCode, String)> {
        let bad = |m: &str| (StatusCode::BAD_REQUEST, m.to_owned());
        let text = args["text"].as_str().map(str::to_owned);
        let delay = match name {
            "echo" => None,
            "slow_echo" => Some(
                Duration::from_millis(args["delayMs"].as_u64().ok_or_else(|| bad("delayMs: an integer"))?)
                    .min(MAX_DELAY),
            ),
            other => {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("no tool {other}: GET /v1/tools lists them"),
                ));
            }
        };
        let text = text.ok_or_else(|| bad("text: a string"))?;
        self.lock().push(ToolCall {
            tool: name.to_owned(),
            arguments: args,
            person: header_str(headers, "aviary-sandbox-person"),
            conversation: header_str(headers, "aviary-sandbox-conversation"),
        });
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        Ok(text)
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

fn unauthorised() -> Response {
    let mut res = aviary_core::http::ApiError::unauthenticated().into_response();
    res.headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    res
}

fn refuse(status: StatusCode, message: &str) -> Response {
    let code = if status == StatusCode::NOT_FOUND {
        "not_found"
    } else {
        "bad_request"
    };
    aviary_core::http::json(status, &json!({"code": code, "message": message}))
}

async fn list(State(t): State<Arc<FakeTools>>, headers: HeaderMap) -> Response {
    if !t.authorised(&headers) {
        return unauthorised();
    }
    aviary_core::http::json(StatusCode::OK, &tools())
}

async fn openapi_route(State(t): State<Arc<FakeTools>>, headers: HeaderMap) -> Response {
    if !t.authorised(&headers) {
        return unauthorised();
    }
    aviary_core::http::json(StatusCode::OK, &openapi())
}

async fn call(
    State(t): State<Arc<FakeTools>>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !t.authorised(&headers) {
        return unauthorised();
    }
    let args = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) if v.is_object() => v,
            _ => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "the body must be a JSON object of the tool's arguments",
                );
            }
        }
    };
    match t.call_tool(&headers, &name, args).await {
        Ok(text) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            text,
        )
            .into_response(),
        Err((status, m)) => refuse(status, &m),
    }
}

async fn calls(State(t): State<Arc<FakeTools>>) -> Response {
    aviary_core::http::json(StatusCode::OK, &t.calls())
}

async fn reset(State(t): State<Arc<FakeTools>>) -> Response {
    t.lock().clear();
    StatusCode::NO_CONTENT.into_response()
}
