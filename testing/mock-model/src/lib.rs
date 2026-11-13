//! mock-model (platform 7.5): an Anthropic Messages API compatible enough for Claude Code, plus the models route
//! (R18), scripted by the test driver.
//!
//! - `POST /v1/messages`: streaming SSE (`message_start`, `content_block_start/delta/stop`, `message_delta`,
//!   `message_stop`) or one JSON message, in `OpenRouter`'s wire form (the default, byte-checked against recordings)
//!   or Anthropic's native one ([`sse::Wire`]: per process, [`MockModel::with_wire`] / `--wire`, or per script
//!   entry, `wire`); bodies up to [`MAX_BODY`] (the gateway's `model` route limit, platform 3.11.3);
//! - `POST /v1/messages/count_tokens`: a fixed estimate (`ceil(request bytes / 4)`);
//! - `POST /v1/chat/completions`: the `OpenAI` chat completions API as Qwen Code calls it; always the text `ok`;
//! - `GET /v1/aviary/models`: the configured [`gateway_api::models::Models`];
//! - `POST /__script`: queue a reply for requests that match (`conversation` from `Aviary-Sandbox-Conversation`,
//!   `textContains` in the last user message, `nth` request of the conversation, `model`), used `times` times
//!   (0: always);
//! - `GET /__requests?conversation=`: what each request carried (model, system hash, message count, images with
//!   their size, tool names);
//! - `POST /__reset`: forget scripts, requests and counters.
//!
//! An unmatched request answers the text `ok`; a title request answers `測試標題`. A title request is one for the
//! title model id of the models route; when that id is also the default or small model's (YYC's `egress.json` sets
//! all three to one id), only a request shaped like the supervisor's title call (supervisor 14: no tools, one user
//! message starting with magpie's title prompt, [`TITLE_PROMPT_PREFIX`]) counts, so Claude Code's own turns on that
//! model still answer `ok`.

pub mod image;
pub mod sse;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::http::headers::AVIARY_SANDBOX_CONVERSATION;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use gateway_api::models::Models;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use uuid::Uuid;

use crate::sse::{Block, Envelope, Wire};

/// The largest request body (platform 3.11.3: the gateway's `model` route passes up to 64 MiB).
pub const MAX_BODY: usize = 64 << 20;

/// The default models route answer (gateway-api's fixture).
pub const DEFAULT_MODELS: &str = include_str!("../../../crates/gateway-api/fixtures/models.json");

/// The reply to a request nobody scripted.
pub const UNMATCHED_TEXT: &str = "ok";

/// The reply to an unscripted request for the title model.
pub const TITLE_TEXT: &str = "測試標題";

/// How magpie's title prompt starts (magpie 9.4), which the supervisor sends as the only user message.
pub const TITLE_PROMPT_PREFIX: &str = "Here is the first message of the conversation:";

/// Whether `seen` is a title request under `models` (see the crate docs).
#[must_use]
pub fn is_title_request(models: &Models, seen: &Seen) -> bool {
    if seen.model != models.title {
        return false;
    }
    let shared = models.title == models.default || models.title == models.small;
    !shared || (seen.tools.is_empty() && seen.messages == 1 && seen.text.starts_with(TITLE_PROMPT_PREFIX))
}

/// Which requests a script entry answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Match {
    /// `Aviary-Sandbox-Conversation` of the request.
    #[serde(default)]
    pub conversation: Option<Uuid>,
    /// A substring of the last user message's text.
    #[serde(default)]
    pub text_contains: Option<String>,
    /// The request's number within its conversation (1 = the first since the last reset).
    #[serde(default)]
    pub nth: Option<u32>,
    /// The request's `model`.
    #[serde(default)]
    pub model: Option<String>,
}

/// A tool call in a reply.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolUse {
    /// The tool.
    pub name: String,
    /// Its input (default `{}`).
    #[serde(default)]
    pub input: Option<Value>,
    /// The block id (default `call_` + 24 hex).
    #[serde(default)]
    pub id: Option<String>,
    /// The `partial_json` deltas, concatenating to the input (default: the input in one delta).
    #[serde(default)]
    pub partial_json: Option<Vec<String>>,
}

/// An upstream error in a reply: as the HTTP answer when it comes first, else as an `error` event mid-stream.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorItem {
    /// HTTP status (529 overloaded, 500, 429 …).
    pub status: u16,
    /// `error.type`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `error.message`.
    #[serde(default)]
    pub message: Option<String>,
}

/// One step of a reply: exactly one of `text`, `thinking`, `toolUse`, `sleepMs`, `error`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Item {
    /// A text block.
    #[serde(default)]
    pub text: Option<String>,
    /// A thinking block.
    #[serde(default)]
    pub thinking: Option<String>,
    /// The thinking block's signature (default empty).
    #[serde(default)]
    pub signature: Option<String>,
    /// The deltas of a text or thinking block (they must concatenate to it).
    #[serde(default)]
    pub chunks: Option<Vec<String>>,
    /// Split a text or thinking block into this many deltas (default: one per word).
    #[serde(default)]
    pub chunk_count: Option<usize>,
    /// Pause after each delta of this block.
    #[serde(default)]
    pub chunk_ms: Option<u64>,
    /// A tool call block.
    #[serde(default)]
    pub tool_use: Option<ToolUse>,
    /// Pause the stream.
    #[serde(default)]
    pub sleep_ms: Option<u64>,
    /// Fail.
    #[serde(default)]
    pub error: Option<ErrorItem>,
}

/// Values of the reply's envelope the script fixes (to match a recording, or to test usage accounting).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvelopeOverride {
    /// `msg_…`.
    #[serde(default)]
    pub id: Option<String>,
    /// `provider`.
    #[serde(default)]
    pub provider: Option<String>,
    /// `usage.input_tokens`.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// `usage.output_tokens`.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// `usage.output_tokens_details.thinking_tokens`.
    #[serde(default)]
    pub thinking_tokens: Option<u64>,
    /// `usage.cache_read_input_tokens`.
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    /// `usage.cost`, a decimal string (`"0.00001407"`).
    #[serde(default)]
    pub cost: Option<String>,
    /// `cost_details.upstream_inference_prompt_cost`.
    #[serde(default)]
    pub prompt_cost: Option<String>,
    /// `cost_details.upstream_inference_completions_cost`.
    #[serde(default)]
    pub completions_cost: Option<String>,
}

fn one() -> u32 {
    1
}

/// `POST /__script`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptEntry {
    /// Which requests (default: any).
    #[serde(rename = "match", default)]
    pub matcher: Match,
    /// The reply.
    pub reply: Vec<Item>,
    /// How many requests it answers; 0 = every matching request.
    #[serde(default = "one")]
    pub times: u32,
    /// Envelope values.
    #[serde(default)]
    pub message: Option<EnvelopeOverride>,
    /// The framing of this reply (default: the process's).
    #[serde(default)]
    pub wire: Option<Wire>,
}

impl ScriptEntry {
    /// Refuse an entry the mock cannot play.
    ///
    /// # Errors
    /// A sentence naming the problem.
    pub fn check(&self) -> Result<(), String> {
        for (i, item) in self.reply.iter().enumerate() {
            let kinds = [
                item.text.is_some(),
                item.thinking.is_some(),
                item.tool_use.is_some(),
                item.sleep_ms.is_some(),
                item.error.is_some(),
            ];
            if kinds.iter().filter(|k| **k).count() != 1 {
                return Err(format!(
                    "reply[{i}]: exactly one of text, thinking, toolUse, sleepMs, error"
                ));
            }
            if let (Some(chunks), Some(t)) = (&item.chunks, item.text.as_ref().or(item.thinking.as_ref()))
                && chunks.concat() != *t
            {
                return Err(format!("reply[{i}]: chunks do not concatenate to the block"));
            }
            if let Some(tu) = &item.tool_use
                && let Some(p) = &tu.partial_json
            {
                let v: Value =
                    serde_json::from_str(&p.concat()).map_err(|e| format!("reply[{i}]: partialJson: {e}"))?;
                if tu.input.as_ref().is_some_and(|input| *input != v) {
                    return Err(format!("reply[{i}]: partialJson is not the input"));
                }
            }
        }
        if let Some(m) = &self.message {
            for c in [&m.cost, &m.prompt_cost, &m.completions_cost].into_iter().flatten() {
                if !sse::is_number(c) {
                    return Err(format!("message: {c:?} is not a number"));
                }
            }
        }
        Ok(())
    }
}

/// One image the request carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSeen {
    /// Pixels.
    pub width: u32,
    /// Pixels.
    pub height: u32,
    /// Decoded bytes.
    pub bytes: u64,
}

/// What one request carried (`GET /__requests`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seen {
    /// When it arrived (RFC 3339).
    pub at: String,
    /// `Aviary-Sandbox-Conversation`.
    pub conversation: Option<Uuid>,
    /// `model`.
    pub model: String,
    /// SHA-256 hex of the `system` field's JSON, if any.
    pub system: Option<String>,
    /// Number of messages.
    pub messages: usize,
    /// Image blocks with base64 data, in order.
    pub images: Vec<ImageSeen>,
    /// Tool names.
    pub tools: Vec<String>,
    /// `stream`.
    pub stream: bool,
    /// The last user message's text.
    pub text: String,
    /// The headers the mock saw that a gateway decides: `authorization` and `x-api-key` present?
    pub credential: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    scripts: Vec<ScriptEntry>,
    seen: Vec<Seen>,
    counts: HashMap<Option<Uuid>, u32>,
}

/// The mock's state.
#[derive(Debug)]
pub struct MockModel {
    models: Models,
    wire: Wire,
    inner: Mutex<Inner>,
}

impl MockModel {
    /// A mock answering `models` on the models route.
    #[must_use]
    pub fn new(models: Models) -> Arc<Self> {
        Self::with_wire(models, Wire::default())
    }

    /// A mock answering `models` whose replies use `wire` unless a script entry names another.
    #[must_use]
    pub fn with_wire(models: Models, wire: Wire) -> Arc<Self> {
        Arc::new(Self {
            models,
            wire,
            inner: Mutex::new(Inner::default()),
        })
    }

    /// A mock with the default models.
    ///
    /// # Panics
    /// Never: the default is a checked-in fixture.
    #[must_use]
    pub fn with_default_models() -> Arc<Self> {
        Self::new(serde_json::from_str(DEFAULT_MODELS).unwrap_or_else(|e| panic!("models fixture: {e}")))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queue a script entry.
    ///
    /// # Errors
    /// When the entry cannot be played ([`ScriptEntry::check`]).
    pub fn script(&self, entry: ScriptEntry) -> Result<(), String> {
        entry.check()?;
        self.lock().scripts.push(entry);
        Ok(())
    }

    /// The requests seen, of one conversation or all.
    #[must_use]
    pub fn requests(&self, conversation: Option<Uuid>) -> Vec<Seen> {
        self.lock()
            .seen
            .iter()
            .filter(|s| conversation.is_none_or(|c| s.conversation == Some(c)))
            .cloned()
            .collect()
    }

    /// Forget everything.
    pub fn reset(&self) {
        *self.lock() = Inner::default();
    }

    /// The router.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(gateway_api::models::MESSAGES, post(messages))
            .route(gateway_api::models::COUNT_TOKENS, post(count_tokens))
            .route(gateway_api::models::CHAT_COMPLETIONS, post(chat_completions))
            .route(gateway_api::models::MODELS, get(models))
            .route("/__script", post(script))
            .route("/__requests", get(requests))
            .route("/__reset", post(reset))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .with_state(self.clone())
    }

    /// Record the request, count it, and take the reply that answers it.
    fn take(&self, seen: Seen) -> Option<ScriptEntry> {
        let mut inner = self.lock();
        let n = {
            let c = inner.counts.entry(seen.conversation).or_insert(0);
            *c += 1;
            *c
        };
        let found = inner.scripts.iter().position(|e| {
            let m = &e.matcher;
            m.conversation.is_none_or(|c| seen.conversation == Some(c))
                && m.text_contains.as_ref().is_none_or(|t| seen.text.contains(t.as_str()))
                && m.nth.is_none_or(|k| k == n)
                && m.model.as_ref().is_none_or(|mm| *mm == seen.model)
        });
        inner.seen.push(seen);
        let i = found?;
        let entry = inner.scripts[i].clone();
        if entry.times == 1 {
            inner.scripts.remove(i);
        } else if entry.times > 1 {
            inner.scripts[i].times -= 1;
        }
        Some(entry)
    }
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn seen_of(headers: &HeaderMap, req: &Value) -> Seen {
    let messages = req["messages"].as_array().cloned().unwrap_or_default();
    let text = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .map(|m| text_of(&m["content"]))
        .unwrap_or_default();
    let mut images = Vec::new();
    for m in &messages {
        for b in m["content"].as_array().into_iter().flatten() {
            if b["type"] == "image" && b["source"]["type"] == "base64" {
                let data = b["source"]["data"].as_str().unwrap_or_default();
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    let (width, height) = image::dimensions(&bytes).unwrap_or((0, 0));
                    images.push(ImageSeen {
                        width,
                        height,
                        bytes: bytes.len() as u64,
                    });
                }
            }
        }
    }
    let system = req.get("system").filter(|s| !s.is_null()).map(|s| {
        let text = serde_json::to_string(s).unwrap_or_default();
        sse_hex(&sha2::Sha256::digest(text.as_bytes()))
    });
    let credential = if headers.contains_key(header::AUTHORIZATION) {
        Some("authorization".to_owned())
    } else if headers.contains_key("x-api-key") {
        Some("x-api-key".to_owned())
    } else {
        None
    };
    Seen {
        at: now(),
        conversation: headers
            .get(&AVIARY_SANDBOX_CONVERSATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok()),
        model: req["model"].as_str().unwrap_or_default().to_owned(),
        system,
        messages: messages.len(),
        images,
        tools: req["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect(),
        stream: req["stream"].as_bool().unwrap_or(false),
        text,
        credential,
    }
}

fn sse_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn hex_id(n: usize) -> String {
    let mut s = Uuid::new_v4().simple().to_string();
    s.push_str(&Uuid::new_v4().simple().to_string());
    s.truncate(n);
    s
}

/// Split a block into deltas: the script's chunks, `chunkCount` even pieces, or one per word.
fn chunks_of(text: &str, item: &Item) -> Vec<String> {
    if let Some(c) = &item.chunks {
        return c.clone();
    }
    let chars: Vec<char> = text.chars().collect();
    if let Some(n) = item.chunk_count.filter(|n| *n > 0) {
        let size = chars.len().div_ceil(n).max(1);
        return chars.chunks(size).map(|c| c.iter().collect()).collect();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in chars {
        if ch == ' ' && !cur.is_empty() && !cur.ends_with(' ') {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

enum Step {
    Bytes(String),
    Sleep(u64),
}

struct Plan {
    envelope: Envelope,
    blocks: Vec<(Block, u64)>,
    /// Items in order: a block index, a sleep, or an error.
    order: Vec<Part>,
    stop_reason: &'static str,
}

enum Part {
    Block(usize),
    Sleep(u64),
    Error(ErrorItem),
}

fn plan(wire: Wire, model: &str, input_bytes: usize, entry: &ScriptEntry) -> Plan {
    let mut blocks = Vec::new();
    let mut order = Vec::new();
    let mut out_chars = 0usize;
    let mut thinking_chars = 0usize;
    let mut tool = false;
    for item in &entry.reply {
        let pace = item.chunk_ms.unwrap_or(0);
        if let Some(t) = &item.text {
            out_chars += t.chars().count();
            blocks.push((
                Block::Text {
                    chunks: chunks_of(t, item),
                },
                pace,
            ));
            order.push(Part::Block(blocks.len() - 1));
        } else if let Some(t) = &item.thinking {
            thinking_chars += t.chars().count();
            blocks.push((
                Block::Thinking {
                    chunks: chunks_of(t, item),
                    signature: item.signature.clone().unwrap_or_default(),
                },
                pace,
            ));
            order.push(Part::Block(blocks.len() - 1));
        } else if let Some(tu) = &item.tool_use {
            tool = true;
            let input = tu.input.clone().unwrap_or_else(|| json!({}));
            let partial = tu
                .partial_json
                .clone()
                .unwrap_or_else(|| vec![serde_json::to_string(&input).unwrap_or_default()]);
            out_chars += partial.concat().len();
            blocks.push((
                Block::ToolUse {
                    id: tu.id.clone().unwrap_or_else(|| {
                        let prefix = if wire == Wire::Anthropic { "toolu_" } else { "call_" };
                        format!("{prefix}{}", hex_id(24))
                    }),
                    name: tu.name.clone(),
                    partial_json: partial,
                },
                pace,
            ));
            order.push(Part::Block(blocks.len() - 1));
        } else if let Some(ms) = item.sleep_ms {
            order.push(Part::Sleep(ms));
        } else if let Some(e) = &item.error {
            order.push(Part::Error(e.clone()));
        }
    }
    let o = entry.message.clone().unwrap_or_default();
    let thinking_tokens = (thinking_chars as u64).div_ceil(4);
    let envelope = Envelope {
        wire,
        id: o.id.unwrap_or_else(|| format!("msg_{}", hex_id(20))),
        model: model.to_owned(),
        provider: o.provider.unwrap_or_else(|| "mock-model".to_owned()),
        input_tokens: o.input_tokens.unwrap_or((input_bytes as u64).div_ceil(4)),
        output_tokens: o
            .output_tokens
            .unwrap_or((out_chars as u64).div_ceil(4) + thinking_tokens + 1),
        thinking_tokens: o.thinking_tokens.unwrap_or(thinking_tokens),
        cache_read_input_tokens: o.cache_read_input_tokens.unwrap_or(0),
        cost: o.cost.unwrap_or_else(|| "0".to_owned()),
        prompt_cost: o.prompt_cost.unwrap_or_else(|| "0".to_owned()),
        completions_cost: o.completions_cost.unwrap_or_else(|| "0".to_owned()),
    };
    Plan {
        envelope,
        blocks,
        order,
        stop_reason: if tool { "tool_use" } else { "end_turn" },
    }
}

fn error_response(e: &ErrorItem) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = sse::error_body(&e.kind, e.message.as_deref().unwrap_or("mock-model: scripted error"));
    let mut res = (status, body).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}

fn streamed(p: &Plan) -> Response {
    if let Some(Part::Error(e)) = p.order.first() {
        return error_response(e);
    }
    let mut steps = vec![Step::Bytes(sse::message_start(&p.envelope))];
    let mut index = 0;
    let mut failed = false;
    for part in &p.order {
        match part {
            Part::Block(i) => {
                let (block, pace) = &p.blocks[*i];
                let events = sse::block_events(p.envelope.wire, index, block);
                let last = events.len() - 1;
                for (k, ev) in events.into_iter().enumerate() {
                    steps.push(Step::Bytes(ev));
                    if *pace > 0 && k > 0 && k < last {
                        steps.push(Step::Sleep(*pace));
                    }
                }
                index += 1;
            }
            Part::Sleep(ms) => steps.push(Step::Sleep(*ms)),
            Part::Error(e) => {
                steps.push(Step::Bytes(sse::error_event(
                    &e.kind,
                    e.message.as_deref().unwrap_or("mock-model: scripted error"),
                )));
                failed = true;
                break;
            }
        }
    }
    if !failed {
        steps.push(Step::Bytes(sse::message_end(&p.envelope, p.stop_reason)));
    }
    let stream = futures_util::stream::unfold(steps.into_iter(), |mut it| async move {
        loop {
            match it.next()? {
                Step::Sleep(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Step::Bytes(b) => return Some((Ok::<_, Infallible>(Bytes::from(b)), it)),
            }
        }
    });
    let mut res = Response::new(Body::from_stream(stream));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

async fn whole(p: Plan) -> Response {
    let mut blocks = Vec::new();
    for part in &p.order {
        match part {
            Part::Block(i) => blocks.push(p.blocks[*i].0.clone()),
            Part::Sleep(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
            Part::Error(e) => return error_response(e),
        }
    }
    let body = sse::message_json(&p.envelope, &blocks, p.stop_reason);
    let mut res = (StatusCode::OK, body).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}

fn bad_request(message: &str) -> Response {
    let mut res = (
        StatusCode::BAD_REQUEST,
        sse::error_body("invalid_request_error", message),
    )
        .into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}

/// A body over [`MAX_BODY`] gets the Messages API's own 413 (`request_too_large`), not axum's plain text.
fn body_or_413(body: Result<Bytes, BytesRejection>) -> Result<Bytes, Box<Response>> {
    body.map_err(|e| {
        let status = e.status();
        let kind = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "request_too_large"
        } else {
            "invalid_request_error"
        };
        let mut res = (status, sse::error_body(kind, &e.body_text())).into_response();
        res.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Box::new(res)
    })
}

async fn messages(
    State(m): State<Arc<MockModel>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body_or_413(body) {
        Ok(b) => b,
        Err(res) => return *res,
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return bad_request("the body is not JSON");
    };
    let seen = seen_of(&headers, &req);
    let model = seen.model.clone();
    let stream = seen.stream;
    let title = is_title_request(&m.models, &seen);
    let entry = m.take(seen).unwrap_or_else(|| ScriptEntry {
        matcher: Match::default(),
        reply: vec![Item {
            text: Some(if title { TITLE_TEXT } else { UNMATCHED_TEXT }.to_owned()),
            ..Item::default()
        }],
        times: 1,
        message: None,
        wire: None,
    });
    let p = plan(entry.wire.unwrap_or(m.wire), &model, body.len(), &entry);
    if stream { streamed(&p) } else { whole(p).await }
}

async fn count_tokens(body: Result<Bytes, BytesRejection>) -> Response {
    let body = match body_or_413(body) {
        Ok(b) => b,
        Err(res) => return *res,
    };
    aviary_core::http::json(
        StatusCode::OK,
        &json!({"input_tokens": (body.len() as u64).div_ceil(4)}),
    )
}

/// `POST /v1/chat/completions` (Qwen Code's calls): the text `ok`, as a stream of chunks when the request asks for
/// one (`stream: true`), else as one completion. Nothing is scripted or recorded for this route.
async fn chat_completions(body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = v.get("model").and_then(Value::as_str).unwrap_or("mock").to_owned();
    let usage = json!({"prompt_tokens": 7, "completion_tokens": 1, "total_tokens": 8});
    if v.get("stream").and_then(Value::as_bool) != Some(true) {
        return aviary_core::http::json(
            StatusCode::OK,
            &json!({"id": "chatcmpl-mock", "object": "chat.completion", "created": 0, "model": model, "usage": usage,
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}]}),
        );
    }
    let chunk = |choices: Value, usage: Value| {
        format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0, "model": model,
                   "choices": choices, "usage": usage})
        )
    };
    let text = [
        chunk(
            json!([{"index": 0, "delta": {"role": "assistant", "content": "ok"}, "finish_reason": null}]),
            Value::Null,
        ),
        chunk(json!([{"index": 0, "delta": {}, "finish_reason": "stop"}]), Value::Null),
        chunk(json!([]), usage),
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat();
    ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], text).into_response()
}

async fn models(State(m): State<Arc<MockModel>>) -> Response {
    aviary_core::http::json(StatusCode::OK, &m.models)
}

async fn script(State(m): State<Arc<MockModel>>, body: Bytes) -> Response {
    let entry: ScriptEntry = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => return bad_request(&format!("script: {e}")),
    };
    match m.script(entry) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => bad_request(&e),
    }
}

#[derive(Deserialize)]
struct RequestsQuery {
    conversation: Option<Uuid>,
}

async fn requests(State(m): State<Arc<MockModel>>, Query(q): Query<RequestsQuery>) -> Response {
    aviary_core::http::json(StatusCode::OK, &m.requests(q.conversation))
}

async fn reset(State(m): State<Arc<MockModel>>) -> Response {
    m.reset();
    StatusCode::NO_CONTENT.into_response()
}
