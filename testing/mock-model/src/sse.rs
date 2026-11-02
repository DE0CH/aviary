//! The wire form of a reply: Server-Sent Events and the non-streaming JSON message, written field by field in the
//! order a real upstream sends them, so a scripted reply can match a recorded response byte for byte
//! (`recorded/*.sse`, recorded from `OpenRouter`'s Messages API, the upstream YYC's gateway relays to).
//!
//! Two framings ([`Wire`]): `OpenRouter`'s (the recorded one: `provider`, `cost`, `is_byok`, `cost_details`,
//! `container`/`stop_details`, `input_tokens` 0 in `message_start`, no `ping`) and Anthropic's native one (a `ping`
//! after `message_start`, the input usage in `message_start`, no `OpenRouter` fields, `toolu_` ids). Anthropic's
//! framing follows its streaming documentation; it is not checked against a recording.
//!
//! The JSON is written by hand rather than through a map: `serde_json`'s map sorts keys (its `preserve_order` feature
//! would change every crate in the workspace), and a cost such as `0.00001407` must stay in its decimal form
//! (`serde_json` writes `1.407e-5`).

use std::fmt::Write as _;

/// A JSON string literal.
#[must_use]
pub fn s(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Whose framing a reply is written in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    /// `OpenRouter`'s Messages API (the recordings).
    #[default]
    Openrouter,
    /// Anthropic's own Messages API.
    Anthropic,
}

impl std::str::FromStr for Wire {
    type Err = String;

    fn from_str(v: &str) -> Result<Self, Self::Err> {
        match v {
            "openrouter" => Ok(Self::Openrouter),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(format!("wire {other:?}: openrouter or anthropic")),
        }
    }
}

/// The envelope of one reply: what the upstream says about the message besides its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The framing.
    pub wire: Wire,
    /// `msg_…`.
    pub id: String,
    /// The model, as the request named it.
    pub model: String,
    /// The upstream provider `OpenRouter` routed to.
    pub provider: String,
    /// `usage.input_tokens` of `message_delta`.
    pub input_tokens: u64,
    /// `usage.output_tokens`.
    pub output_tokens: u64,
    /// `usage.output_tokens_details.thinking_tokens`.
    pub thinking_tokens: u64,
    /// `usage.cache_read_input_tokens`.
    pub cache_read_input_tokens: u64,
    /// `usage.cost` (USD), a JSON number literal.
    pub cost: String,
    /// `cost_details.upstream_inference_prompt_cost`.
    pub prompt_cost: String,
    /// `cost_details.upstream_inference_completions_cost`.
    pub completions_cost: String,
}

/// One content block, with its deltas as they are streamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Extended thinking.
    Thinking {
        /// The deltas.
        chunks: Vec<String>,
        /// `signature` of the block.
        signature: String,
    },
    /// Text.
    Text {
        /// The deltas.
        chunks: Vec<String>,
    },
    /// A tool call.
    ToolUse {
        /// `call_…` / `toolu_…`.
        id: String,
        /// The tool.
        name: String,
        /// The `partial_json` deltas; they concatenate to the input object.
        partial_json: Vec<String>,
    },
}

impl Block {
    fn start(&self, wire: Wire) -> String {
        match (self, wire) {
            (Block::Thinking { .. }, _) => r#"{"type":"thinking","thinking":"","signature":""}"#.to_owned(),
            (Block::Text { .. }, Wire::Openrouter) => r#"{"type":"text","text":"","citations":[]}"#.to_owned(),
            (Block::Text { .. }, Wire::Anthropic) => r#"{"type":"text","text":""}"#.to_owned(),
            (Block::ToolUse { id, name, .. }, Wire::Openrouter) => format!(
                r#"{{"type":"tool_use","id":{},"caller":{{"type":"direct"}},"name":{},"input":{{}}}}"#,
                s(id),
                s(name)
            ),
            (Block::ToolUse { id, name, .. }, Wire::Anthropic) => {
                format!(
                    r#"{{"type":"tool_use","id":{},"name":{},"input":{{}}}}"#,
                    s(id),
                    s(name)
                )
            }
        }
    }

    fn deltas(&self) -> Vec<String> {
        match self {
            Block::Thinking { chunks, signature } => {
                let mut out: Vec<String> = chunks
                    .iter()
                    .map(|c| format!(r#"{{"type":"thinking_delta","thinking":{}}}"#, s(c)))
                    .collect();
                // the signature closes the block, as Anthropic streams it (the recordings carry none)
                if !signature.is_empty() {
                    out.push(format!(r#"{{"type":"signature_delta","signature":{}}}"#, s(signature)));
                }
                out
            }
            Block::Text { chunks } => chunks
                .iter()
                .map(|c| format!(r#"{{"type":"text_delta","text":{}}}"#, s(c)))
                .collect(),
            Block::ToolUse { partial_json, .. } => partial_json
                .iter()
                .map(|c| format!(r#"{{"type":"input_json_delta","partial_json":{}}}"#, s(c)))
                .collect(),
        }
    }

    /// The block as it appears whole in a non-streaming message.
    fn whole(&self, wire: Wire) -> String {
        let anthropic = wire == Wire::Anthropic;
        match self {
            Block::Thinking { chunks, signature } => format!(
                r#"{{"type":"thinking","thinking":{},"signature":{}}}"#,
                s(&chunks.concat()),
                s(signature)
            ),
            Block::Text { chunks } if anthropic => format!(r#"{{"type":"text","text":{}}}"#, s(&chunks.concat())),
            Block::Text { chunks } => format!(r#"{{"type":"text","text":{},"citations":[]}}"#, s(&chunks.concat())),
            Block::ToolUse { id, name, partial_json } => {
                let input = partial_json.concat();
                let input = if input.is_empty() { "{}".to_owned() } else { input };
                let caller = if anthropic {
                    ""
                } else {
                    r#""caller":{"type":"direct"},"#
                };
                format!(
                    r#"{{"type":"tool_use","id":{},{caller}"name":{},"input":{input}}}"#,
                    s(id),
                    s(name)
                )
            }
        }
    }
}

/// `event: <name>\ndata: <json>\n\n`.
#[must_use]
pub fn event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// The `message_start` event (and, in Anthropic's framing, the `ping` that follows it).
#[must_use]
pub fn message_start(e: &Envelope) -> String {
    if e.wire == Wire::Anthropic {
        let data = format!(
            concat!(
                r#"{{"type":"message_start","message":{{"id":{},"type":"message","role":"assistant","model":{},"#,
                r#""content":[],"stop_reason":null,"stop_sequence":null,"usage":{{"input_tokens":{},"#,
                r#""cache_creation_input_tokens":0,"cache_read_input_tokens":{},"output_tokens":1}}}}}}"#
            ),
            s(&e.id),
            s(&e.model),
            e.input_tokens,
            e.cache_read_input_tokens
        );
        let mut out = event("message_start", &data);
        out.push_str(&event("ping", r#"{"type": "ping"}"#));
        return out;
    }
    let data = format!(
        concat!(
            r#"{{"type":"message_start","message":{{"id":{},"type":"message","role":"assistant","container":null,"#,
            r#""content":[],"model":{},"stop_reason":null,"stop_details":null,"stop_sequence":null,"#,
            r#""usage":{{"input_tokens":0,"output_tokens":0,"output_tokens_details":null,"#,
            r#""cache_creation_input_tokens":null,"cache_read_input_tokens":null,"cache_creation":null,"#,
            r#""inference_geo":null,"server_tool_use":null,"service_tier":null,"speed":"standard"}},"#,
            r#""provider":{}}}}}"#
        ),
        s(&e.id),
        s(&e.model),
        s(&e.provider)
    );
    event("message_start", &data)
}

/// `content_block_start`, its deltas (one string each, to be paced) and `content_block_stop`.
#[must_use]
pub fn block_events(wire: Wire, index: usize, b: &Block) -> Vec<String> {
    let mut out = vec![event(
        "content_block_start",
        &format!(
            r#"{{"type":"content_block_start","index":{index},"content_block":{}}}"#,
            b.start(wire)
        ),
    )];
    for d in b.deltas() {
        out.push(event(
            "content_block_delta",
            &format!(r#"{{"type":"content_block_delta","index":{index},"delta":{d}}}"#),
        ));
    }
    out.push(event(
        "content_block_stop",
        &format!(r#"{{"type":"content_block_stop","index":{index}}}"#),
    ));
    out
}

fn usage(e: &Envelope) -> String {
    if e.wire == Wire::Anthropic {
        return format!(
            concat!(
                r#"{{"input_tokens":{},"cache_creation_input_tokens":0,"cache_read_input_tokens":{},"#,
                r#""output_tokens":{}}}"#
            ),
            e.input_tokens, e.cache_read_input_tokens, e.output_tokens
        );
    }
    format!(
        concat!(
            r#"{{"input_tokens":{},"output_tokens":{},"output_tokens_details":{{"thinking_tokens":{}}},"#,
            r#""cache_creation_input_tokens":null,"cache_read_input_tokens":{},"cache_creation":null,"#,
            r#""server_tool_use":null,"service_tier":null,"speed":"standard","cost":{},"is_byok":false,"#,
            r#""cost_details":{{"upstream_inference_cost":{},"upstream_inference_prompt_cost":{},"#,
            r#""upstream_inference_completions_cost":{}}}}}"#
        ),
        e.input_tokens,
        e.output_tokens,
        e.thinking_tokens,
        e.cache_read_input_tokens,
        e.cost,
        e.cost,
        e.prompt_cost,
        e.completions_cost
    )
}

/// `message_delta` and `message_stop`.
#[must_use]
pub fn message_end(e: &Envelope, stop_reason: &str) -> String {
    if e.wire == Wire::Anthropic {
        let delta = format!(
            concat!(
                r#"{{"type":"message_delta","delta":{{"stop_reason":{},"stop_sequence":null}},"#,
                r#""usage":{}}}"#
            ),
            s(stop_reason),
            usage(e)
        );
        let mut out = event("message_delta", &delta);
        out.push_str(&event("message_stop", r#"{"type":"message_stop"}"#));
        return out;
    }
    let delta = format!(
        concat!(
            r#"{{"type":"message_delta","delta":{{"container":null,"stop_details":null,"stop_reason":{},"#,
            r#""stop_sequence":null}},"usage":{},"context_management":null}}"#
        ),
        s(stop_reason),
        usage(e)
    );
    let mut out = event("message_delta", &delta);
    out.push_str(&event("message_stop", r#"{"type":"message_stop"}"#));
    out
}

/// A streamed error event (an upstream failure after the stream began).
#[must_use]
pub fn error_event(kind: &str, message: &str) -> String {
    event("error", &error_body(kind, message))
}

/// The Messages API's error body.
#[must_use]
pub fn error_body(kind: &str, message: &str) -> String {
    format!(
        r#"{{"type":"error","error":{{"type":{},"message":{}}}}}"#,
        s(kind),
        s(message)
    )
}

/// The non-streaming message.
#[must_use]
pub fn message_json(e: &Envelope, blocks: &[Block], stop_reason: &str) -> String {
    let mut content = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            content.push(',');
        }
        content.push_str(&b.whole(e.wire));
    }
    let mut out = String::new();
    if e.wire == Wire::Anthropic {
        let _ = write!(
            out,
            concat!(
                r#"{{"id":{},"type":"message","role":"assistant","model":{},"content":[{}],"#,
                r#""stop_reason":{},"stop_sequence":null,"usage":{}}}"#
            ),
            s(&e.id),
            s(&e.model),
            content,
            s(stop_reason),
            usage(e)
        );
        return out;
    }
    let _ = write!(
        out,
        concat!(
            r#"{{"id":{},"type":"message","role":"assistant","container":null,"content":[{}],"model":{},"#,
            r#""stop_reason":{},"stop_details":null,"stop_sequence":null,"usage":{},"provider":{}}}"#
        ),
        s(&e.id),
        content,
        s(&e.model),
        s(stop_reason),
        usage(e),
        s(&e.provider)
    );
    out
}

/// Is `v` a plain JSON number literal (what a cost may be)?
#[must_use]
pub fn is_number(v: &str) -> bool {
    !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
        && serde_json::from_str::<serde_json::Number>(v).is_ok()
}
