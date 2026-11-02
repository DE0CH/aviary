//! Classification of main-transcript lines (magpie 6.5): which lines become `cc.assistant` / `cc.user` events, with
//! what `meta` and search `text`, and which are internal (stored for byte-exactness only).
//!
//! The input is the line's event data: the parsed stub with each extracted string replaced by its blob reference
//! (`{"$blob": {…}}`). Everything here is pure; the database effects (tool links, the echo flag, the title, the
//! model) are applied by the caller from the returned [`Class`].
//!
//! One reading beyond the table: a `type: "user"` line with `isCompactSummary` (Claude Code's compaction summary) is a
//! `cc.user` event but not a prompt (it is no message a person sent) and never the echo of one.

use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::{EventMeta, TaskNotification};
use serde_json::Value;

/// `cc.assistant`.
pub const ASSISTANT: &str = "cc.assistant";
/// `cc.user`.
pub const USER: &str = "cc.user";

/// What the turn the line is credited to says about echo matching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    /// The line's turn is a person-origin turn whose `user.sent` echo is not matched yet.
    pub echo_pending: bool,
}

/// An event a line becomes.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// `cc.assistant` or `cc.user`.
    pub kind: &'static str,
    /// `meta` (without `results[*].toolUseSeq` and `replaces`, which the caller fills).
    pub meta: EventMeta,
    /// Plain text of the text blocks, for search and previews.
    pub text: Option<String>,
    /// A prompt not sent through magpie (M10).
    pub is_prompt: bool,
    /// The line's `uuid`.
    pub uuid: Option<String>,
    /// Assistant: `(tool_use id, name)` of each call.
    pub tool_uses: Vec<(String, String)>,
    /// User: the `tool_use_id` of each result.
    pub results: Vec<String>,
    /// Assistant, not a sidechain: `message.model` and the context tokens of `message.usage`.
    pub model: Option<(String, Option<i64>)>,
}

/// What a line is.
#[derive(Debug, Clone, PartialEq)]
pub enum Class {
    /// An event.
    Event(Box<Event>),
    /// The echo of the turn's `user.sent` (internal; the turn's echo is now matched).
    Echo,
    /// A `custom-title` / `ai-title` record (internal): the title it names.
    Title(String),
    /// Anything else (internal).
    Internal,
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn truthy(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The text of a string value or of a blob reference's preview.
fn text_of(v: &Value) -> Option<&str> {
    v.as_str()
        .or_else(|| v.get("$blob").and_then(|b| b.get("preview")).and_then(Value::as_str))
}

/// The plain text of `message.content`: the string itself, or the `text` blocks joined by newlines.
fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(t) = text_of(content) {
        return Some(t.to_owned());
    }
    let parts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|b| s(b, "type") == Some("text"))
        .filter_map(|b| b.get("text").and_then(text_of))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn blocks(content: Option<&Value>) -> impl Iterator<Item = &Value> {
    content.and_then(Value::as_array).into_iter().flatten()
}

/// Classifies one main-transcript line from its event data (`None`: the line is not JSON).
#[must_use]
pub fn classify(data: Option<&Value>, cx: Context) -> Class {
    let Some(d) = data.filter(|d| d.is_object()) else {
        return Class::Internal;
    };
    match s(d, "type") {
        Some("assistant") => assistant(d),
        Some("user") if !truthy(d, "isMeta") => user(d, cx),
        Some("custom-title") => s(d, "customTitle").map_or(Class::Internal, |t| Class::Title(t.to_owned())),
        Some("ai-title") => s(d, "aiTitle").map_or(Class::Internal, |t| Class::Title(t.to_owned())),
        _ => Class::Internal,
    }
}

fn assistant(d: &Value) -> Class {
    let message = d.get("message");
    let content = message.and_then(|m| m.get("content"));
    let tool_uses: Vec<(String, String)> = blocks(content)
        .filter(|b| s(b, "type") == Some("tool_use"))
        .filter_map(|b| Some((s(b, "id")?.to_owned(), s(b, "name").unwrap_or_default().to_owned())))
        .collect();
    let text = content_text(content);
    let sidechain = truthy(d, "isSidechain");
    let error = text.as_deref().and_then(api_error);
    let model = if sidechain {
        None
    } else {
        message
            .and_then(|m| s(m, "model"))
            .filter(|m| *m != "<synthetic>")
            .map(|m| {
                (
                    m.to_owned(),
                    message.and_then(|m| m.get("usage")).and_then(context_tokens),
                )
            })
    };
    let meta = EventMeta {
        tool_uses: (!tool_uses.is_empty()).then(|| tool_uses.iter().map(|(id, _)| id.clone()).collect()),
        error,
        sidechain: Some(sidechain),
        ..EventMeta::default()
    };
    Class::Event(Box::new(Event {
        kind: ASSISTANT,
        meta,
        text,
        is_prompt: false,
        uuid: s(d, "uuid").map(str::to_owned),
        tool_uses,
        results: Vec::new(),
        model,
    }))
}

/// Input plus cache tokens of a `usage` object.
fn context_tokens(usage: &Value) -> Option<i64> {
    let n = |k: &str| usage.get(k).and_then(Value::as_i64).unwrap_or(0);
    usage.get("input_tokens")?;
    Some(n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"))
}

fn user(d: &Value, cx: Context) -> Class {
    let content = d.get("message").and_then(|m| m.get("content"));
    let results: Vec<String> = blocks(content)
        .filter(|b| s(b, "type") == Some("tool_result"))
        .filter_map(|b| s(b, "tool_use_id").map(str::to_owned))
        .collect();
    let has_results = blocks(content).any(|b| s(b, "type") == Some("tool_result"));
    let text = content_text(content);
    let uuid = s(d, "uuid").map(str::to_owned);
    let event = |meta: EventMeta, is_prompt: bool, results: Vec<String>| {
        Class::Event(Box::new(Event {
            kind: USER,
            meta,
            text: text.clone(),
            is_prompt,
            uuid: uuid.clone(),
            tool_uses: Vec::new(),
            results,
            model: None,
        }))
    };
    if has_results {
        // toolUseSeq is filled by the caller from tool_links
        let meta = EventMeta {
            results: Some(
                results
                    .iter()
                    .map(|id| magpie_api::events::ToolResultLink {
                        tool_use_id: id.clone(),
                        tool_use_seq: None,
                    })
                    .collect(),
            ),
            ..EventMeta::default()
        };
        return event(meta, false, results);
    }
    if let Some(t) = text
        .as_deref()
        .filter(|t| t.trim_start().starts_with("<task-notification>"))
    {
        let meta = EventMeta {
            task_notification: task_notification(t),
            ..EventMeta::default()
        };
        return event(meta, false, Vec::new());
    }
    if truthy(d, "isCompactSummary") {
        return event(EventMeta::default(), false, Vec::new());
    }
    if cx.echo_pending {
        return Class::Echo;
    }
    let meta = EventMeta {
        prompt: Some(true),
        ..EventMeta::default()
    };
    event(meta, true, Vec::new())
}

/// The text between `<name>` and `</name>`, trimmed.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

fn task_notification(text: &str) -> Option<TaskNotification> {
    Some(TaskNotification {
        tool_use_id: tag(text, "tool-use-id")?.to_owned(),
        task_id: tag(text, "task-id").map(str::to_owned),
        status: tag(text, "status").unwrap_or_default().to_owned(),
        summary: tag(text, "summary").map(str::to_owned),
    })
}

/// The model error tag of an `API Error: …` assistant text (magpie 8.9; today's `jarvis-errors.ts` rules).
#[must_use]
pub fn api_error(text: &str) -> Option<ErrorInfo> {
    let rest = text.trim().strip_prefix("API Error:")?.trim_start();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let boundary = rest
        .as_bytes()
        .get(digits)
        .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
    let (status, body) = if digits == 3 && boundary {
        (rest[..3].parse::<u16>().ok(), rest[3..].trim_start())
    } else {
        (None, rest)
    };
    let parsed = body
        .find('{')
        .and_then(|i| serde_json::from_str::<Value>(&body[i..]).ok())
        .filter(Value::is_object);
    let inner = parsed.as_ref().and_then(|b| b.get("error")).filter(|e| e.is_object());
    let kind = inner.and_then(|e| s(e, "type")).unwrap_or_default();
    let message = match (&parsed, inner.and_then(|e| s(e, "message"))) {
        (_, Some(m)) => m.trim().to_owned(),
        (None, None) => body.trim().to_owned(),
        (Some(_), None) => String::new(),
    };
    let code = match status.unwrap_or(0) {
        _ if kind == "budget_exceeded" => DataCode::ModelBudget,
        402 => DataCode::ModelBudget,
        429 => DataCode::ModelRateLimited,
        _ if kind == "rate_limit_error" => DataCode::ModelRateLimited,
        s if s >= 500 => DataCode::ModelUnavailable,
        _ if kind == "overloaded_error" || kind == "api_error" => DataCode::ModelUnavailable,
        401 | 403 => DataCode::ModelAccess,
        s if s >= 400 => DataCode::ModelRefused,
        _ => DataCode::ModelConnection,
    };
    let mut e = ErrorInfo::new(code, message);
    e.status = status;
    Some(e)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn event(c: Class) -> Event {
        match c {
            Class::Event(e) => *e,
            other => panic!("not an event: {other:?}"),
        }
    }

    #[test]
    fn assistant_lines() {
        let d = json!({"type": "assistant", "uuid": "u1", "isSidechain": false, "message": {"model": "m-1",
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 3},
            "content": [{"type": "text", "text": "hi"}, {"type": "tool_use", "id": "toolu_1", "name": "Bash"}]}});
        let e = event(classify(Some(&d), Context::default()));
        assert_eq!(e.kind, ASSISTANT);
        assert_eq!(e.meta.tool_uses, Some(vec!["toolu_1".to_owned()]));
        assert_eq!(e.meta.sidechain, Some(false));
        assert_eq!(e.text.as_deref(), Some("hi"));
        assert_eq!(e.model, Some(("m-1".to_owned(), Some(15))));
        assert_eq!(e.tool_uses, vec![("toolu_1".to_owned(), "Bash".to_owned())]);
        let side = json!({"type": "assistant", "isSidechain": true, "message": {"model": "m", "content": []}});
        let e = event(classify(Some(&side), Context::default()));
        assert_eq!((e.model, e.meta.sidechain), (None, Some(true)));
    }

    #[test]
    fn api_errors_map_to_model_codes() {
        let cases = [
            (
                r#"API Error: 402 {"error":{"message":"budget used up"}}"#,
                DataCode::ModelBudget,
                Some(402),
                "budget used up",
            ),
            (
                r#"API Error: 400 {"error":{"type":"budget_exceeded","message":"m"}}"#,
                DataCode::ModelBudget,
                Some(400),
                "m",
            ),
            (
                "API Error: 429 slow down",
                DataCode::ModelRateLimited,
                Some(429),
                "slow down",
            ),
            (
                r#"API Error: 529 {"error":{"type":"overloaded_error"}}"#,
                DataCode::ModelUnavailable,
                Some(529),
                "",
            ),
            (
                r#"API Error: {"error":{"type":"api_error","message":"x"}}"#,
                DataCode::ModelUnavailable,
                None,
                "x",
            ),
            (
                "API Error: 403 forbidden",
                DataCode::ModelAccess,
                Some(403),
                "forbidden",
            ),
            (
                "API Error: 404 no such model",
                DataCode::ModelRefused,
                Some(404),
                "no such model",
            ),
            (
                "API Error: Connection error.",
                DataCode::ModelConnection,
                None,
                "Connection error.",
            ),
            ("API Error: 4000 tokens", DataCode::ModelConnection, None, "4000 tokens"),
        ];
        for (text, code, status, message) in cases {
            let e = api_error(text).unwrap();
            assert_eq!(
                (e.code, e.status, e.message.as_str()),
                (code, status, message),
                "{text}"
            );
        }
        assert!(api_error("Not an error").is_none());
    }

    #[test]
    fn user_lines() {
        let results =
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]}});
        let e = event(classify(Some(&results), Context { echo_pending: true }));
        assert_eq!(e.results, vec!["toolu_1".to_owned()]);
        assert!(!e.is_prompt);

        let note = json!({"type": "user", "message": {"content":
            "<task-notification>\n<task-id>b7</task-id>\n<tool-use-id>toolu_01Bg</tool-use-id>\n<status>completed</status>\n<summary>Build finished.</summary>\n</task-notification>"}});
        let e = event(classify(Some(&note), Context { echo_pending: true }));
        assert_eq!(
            e.meta.task_notification,
            Some(TaskNotification {
                tool_use_id: "toolu_01Bg".into(),
                task_id: Some("b7".into()),
                status: "completed".into(),
                summary: Some("Build finished.".into()),
            })
        );

        let prompt = json!({"type": "user", "message": {"content": [{"type": "text", "text": "hello"}]}});
        assert_eq!(classify(Some(&prompt), Context { echo_pending: true }), Class::Echo);
        let e = event(classify(Some(&prompt), Context::default()));
        assert!(e.is_prompt);
        assert_eq!(e.meta.prompt, Some(true));
        assert_eq!(e.text.as_deref(), Some("hello"));

        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "x"}});
        assert_eq!(classify(Some(&meta), Context::default()), Class::Internal);

        let summary = json!({"type": "user", "isCompactSummary": true, "message": {"content": "summary"}});
        let e = event(classify(Some(&summary), Context { echo_pending: true }));
        assert!(!e.is_prompt);
    }

    #[test]
    fn internal_lines_and_titles() {
        for d in [
            json!({"type": "attachment"}),
            json!({"type": "system", "subtype": "compact_boundary"}),
            json!({"type": "queue-operation"}),
            json!({"type": "last-prompt"}),
            json!(["not", "an", "object"]),
        ] {
            assert_eq!(classify(Some(&d), Context::default()), Class::Internal, "{d}");
        }
        assert_eq!(classify(None, Context::default()), Class::Internal);
        let t = json!({"type": "custom-title", "customTitle": "Q3"});
        assert_eq!(classify(Some(&t), Context::default()), Class::Title("Q3".into()));
        let t = json!({"type": "ai-title", "aiTitle": "AI"});
        assert_eq!(classify(Some(&t), Context::default()), Class::Title("AI".into()));
    }

    #[test]
    fn blob_text_uses_its_preview() {
        let d = json!({"type": "user", "message": {"content": {"$blob": {"blobId": "x", "bytes": 1, "kind": "text", "preview": "long"}}}});
        let e = event(classify(Some(&d), Context::default()));
        assert_eq!(e.text.as_deref(), Some("long"));
    }
}
