//! Classification of main-transcript lines (magpie 6.5): which lines become `cc.assistant` / `cc.user` events, with
//! what `meta` and search `text`, and which are internal (stored for byte-exactness only).
//!
//! The input is the line's event data: the parsed stub with each extracted string replaced by its blob reference
//! (`{"$blob": {…}}`). Everything here is pure; the database effects (tool links, the echo flag, the title, the
//! model) are applied by the caller from the returned [`Class`].
//!
//! One reading beyond the table: a `type: "user"` line with `isCompactSummary` (Claude Code's compaction summary) is a
//! `cc.user` event but not a prompt (it is no message a person sent) and never the echo of one.
//!
//! A resumed parked turn (DECISIONS 17, [`Context::answering`]): Claude Code 2.1.304 closes the questions its
//! predecessor left open with a synthetic result ("[Tool call interrupted: the session ended …]") and a synthetic
//! assistant line ("No response requested.", model `<synthetic>`) before the supervisor's message; both are internal,
//! and the message, the person's answers, is [`Class::Answer`]: the questions' result, not a prompt.
//!
//! What the old stack's history never showed is internal here too (cutover 10.2 R6, the old side's
//! `claude-sessions.provider.ts`): Claude Code's "No response requested." placeholder reply (model `<synthetic>`) in
//! any turn, and user text lines that start with one of [`INTERNAL_PREFIXES`] (an interruption banner, a reminder,
//! a caveat, a skill body). A compaction is shown as the old stack showed it: its `compact_boundary` line is a
//! `cc.user` event with `meta.compaction` (one assistant row, magpie 7.3 rule 2a), and its summary line has
//! `meta.compactSummary` (an assistant row with the summary).

use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::{Compaction, EventMeta, TaskNotification};
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
    /// The line's turn is a resumed parked turn whose answer line is not seen yet.
    pub answering: bool,
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
    /// A resumed parked turn's answer line: a `cc.user` event (`meta.askAnswer`) whose results the caller names.
    Answer(Box<Event>),
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

/// User text that is not a person's message: the old stack's history left these lines out (its
/// `INTERNAL_CONTENT_PREFIXES`), so they are internal.
pub const INTERNAL_PREFIXES: [&str; 4] = [
    "<system-reminder>",
    "Caveat:",
    "[Request interrupted",
    "Base directory for this skill:",
];

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
        Some("assistant") if (cx.answering && synthetic(d)) || no_response(d) => Class::Internal,
        Some("assistant") => assistant(d),
        Some("user") if !truthy(d, "isMeta") => user(d, cx),
        Some("system") if s(d, "subtype") == Some("compact_boundary") => compaction(d),
        Some("attachment") => queued(d),
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
    if has_results && cx.answering && interrupted(content) {
        // the resumed Claude Code closing the parked question itself: the person's answer is the result
        return Class::Internal;
    }
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
        let meta = EventMeta {
            compact_summary: Some(true),
            ..EventMeta::default()
        };
        return event(meta, false, Vec::new());
    }
    if cx.answering {
        let meta = EventMeta {
            ask_answer: Some(true),
            ..EventMeta::default()
        };
        if let Class::Event(e) = event(meta, false, Vec::new()) {
            return Class::Answer(e);
        }
    }
    if cx.echo_pending {
        return Class::Echo;
    }
    // after the echo: a person's own message is matched whatever it starts with
    if text
        .as_deref()
        .is_some_and(|t| INTERNAL_PREFIXES.iter().any(|p| t.starts_with(p)))
    {
        return Class::Internal;
    }
    let meta = EventMeta {
        prompt: Some(true),
        ..EventMeta::default()
    };
    event(meta, true, Vec::new())
}

/// A message Claude Code took in while a turn ran (its `queued_command` attachment, `commandMode` `prompt`): a
/// schedule's message delivered mid-turn (`src/schedules/fire.rs`). Claude Code adds it to the next tool result or
/// the next request rather than writing a `user` line, so this line is where the message shows on the timeline: a
/// prompt of the turn, like a `user` line after the echo. Any other attachment is internal.
fn queued(d: &Value) -> Class {
    let Some(a) = d.get("attachment").filter(|a| s(a, "type") == Some("queued_command")) else {
        return Class::Internal;
    };
    if s(a, "commandMode").is_some_and(|m| m != "prompt") {
        return Class::Internal;
    }
    let Some(text) = a.get("prompt").and_then(text_of).map(str::to_owned) else {
        return Class::Internal;
    };
    if text.trim().is_empty() || INTERNAL_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return Class::Internal;
    }
    Class::Event(Box::new(Event {
        kind: USER,
        meta: EventMeta {
            prompt: Some(true),
            ..EventMeta::default()
        },
        text: Some(text),
        is_prompt: true,
        uuid: s(d, "uuid").map(str::to_owned),
        tool_uses: Vec::new(),
        results: Vec::new(),
        model: None,
    }))
}

/// A `compact_boundary` line: a `cc.user` event with its facts (`compactMetadata` on disk, `compact_metadata` live).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the counts are finite and positive
fn compaction(d: &Value) -> Class {
    let m = d.get("compactMetadata").or_else(|| d.get("compact_metadata"));
    let n = |a: &str, b: &str| {
        m.and_then(|m| m.get(a).or_else(|| m.get(b)))
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite() && *f > 0.0)
            .map_or(0, |f| f as u64)
    };
    let trigger = if m.and_then(|m| s(m, "trigger")) == Some("manual") {
        "manual"
    } else {
        "auto"
    };
    let meta = EventMeta {
        compaction: Some(Compaction {
            trigger: trigger.to_owned(),
            pre_tokens: n("preTokens", "pre_tokens"),
            post_tokens: n("postTokens", "post_tokens"),
            duration_ms: n("durationMs", "duration_ms"),
        }),
        ..EventMeta::default()
    };
    Class::Event(Box::new(Event {
        kind: USER,
        meta,
        text: None,
        is_prompt: false,
        uuid: s(d, "uuid").map(str::to_owned),
        tool_uses: Vec::new(),
        results: Vec::new(),
        model: None,
    }))
}

/// Claude Code's "No response requested." placeholder (its `NO_RESPONSE_REQUESTED`, model `<synthetic>`): the reply
/// it writes when it resumes a conversation whose last turn ended without one. Nobody wrote it; the old stack never
/// showed it.
fn no_response(d: &Value) -> bool {
    let message = d.get("message");
    if message.and_then(|m| s(m, "model")) != Some("<synthetic>") {
        return false;
    }
    let content = message.and_then(|m| m.get("content"));
    let text = content.and_then(Value::as_str).or_else(|| {
        let a = content?.as_array()?;
        match a.as_slice() {
            [b] if s(b, "type") == Some("text") => s(b, "text"),
            _ => None,
        }
    });
    text == Some("No response requested.")
}

/// Claude Code's own assistant line with no model call behind it (`message.model` `<synthetic>`, no tool call).
fn synthetic(d: &Value) -> bool {
    let message = d.get("message");
    message.and_then(|m| s(m, "model")) == Some("<synthetic>")
        && !blocks(message.and_then(|m| m.get("content"))).any(|b| s(b, "type") == Some("tool_use"))
}

/// Every `tool_result` of the line is Claude Code's "[Tool call interrupted …]" for a call a session left open.
fn interrupted(content: Option<&Value>) -> bool {
    blocks(content)
        .filter(|b| s(b, "type") == Some("tool_result"))
        .all(|b| {
            let c = b.get("content");
            let text = c.and_then(text_of).map(str::to_owned).or_else(|| content_text(c));
            text.is_some_and(|t| t.trim_start().starts_with("[Tool call interrupted"))
        })
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

    /// The lines Claude Code 2.1.304 wrote when a session whose `AskUserQuestion` was open is resumed and given the
    /// person's answer (crates/supervisor/spikes/ask-resume): the synthetic close and reply are internal, the
    /// answer is the question's result.
    #[test]
    fn a_resumed_parked_turns_lines() {
        let answering = Context {
            echo_pending: false,
            answering: true,
        };
        let closed = json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result",
            "content": "[Tool call interrupted: the session ended before this call's result was recorded, so its outcome is unknown. Check whether it took effect before relying on it or running it again.]",
            "is_error": true, "tool_use_id": "toolu_ask17"}]}});
        assert_eq!(classify(Some(&closed), answering), Class::Internal);
        assert!(
            matches!(classify(Some(&closed), Context::default()), Class::Event(_)),
            "only while answering"
        );
        let reply = json!({"type": "assistant", "message": {"model": "<synthetic>", "role": "assistant",
            "content": [{"type": "text", "text": "No response requested."}]}});
        assert_eq!(classify(Some(&reply), answering), Class::Internal);
        let answer = json!({"type": "user", "message": {"role": "user",
            "content": "Your questions have been answered: \"Which colour?\"=\"Blue\". You can now continue with these answers in mind."}});
        let Class::Answer(e) = classify(Some(&answer), answering) else {
            panic!("not an answer")
        };
        assert_eq!(e.meta.ask_answer, Some(true));
        assert!(!e.is_prompt && e.meta.prompt.is_none());
        // a real model reply and a real tool result of the resumed turn are events as ever
        let real = json!({"type": "assistant", "message": {"model": "claude-sonnet-4-5", "content": [{"type": "text", "text": "done"}]}});
        assert!(matches!(classify(Some(&real), answering), Class::Event(_)));
        let result = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok"}]}});
        assert!(matches!(classify(Some(&result), answering), Class::Event(_)));
    }

    /// A person's turn whose echo is not matched yet.
    const ECHO: Context = Context {
        echo_pending: true,
        answering: false,
    };

    #[test]
    fn user_lines() {
        let results =
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]}});
        let e = event(classify(Some(&results), ECHO));
        assert_eq!(e.results, vec!["toolu_1".to_owned()]);
        assert!(!e.is_prompt);

        let note = json!({"type": "user", "message": {"content":
            "<task-notification>\n<task-id>b7</task-id>\n<tool-use-id>toolu_01Bg</tool-use-id>\n<status>completed</status>\n<summary>Build finished.</summary>\n</task-notification>"}});
        let e = event(classify(Some(&note), ECHO));
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
        assert_eq!(classify(Some(&prompt), ECHO), Class::Echo);
        let e = event(classify(Some(&prompt), Context::default()));
        assert!(e.is_prompt);
        assert_eq!(e.meta.prompt, Some(true));
        assert_eq!(e.text.as_deref(), Some("hello"));

        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "x"}});
        assert_eq!(classify(Some(&meta), Context::default()), Class::Internal);

        let summary = json!({"type": "user", "isCompactSummary": true, "message": {"content": "summary"}});
        let e = event(classify(Some(&summary), ECHO));
        assert!(!e.is_prompt);
        assert_eq!(e.meta.compact_summary, Some(true));
    }

    /// What the old stack's history left out is internal (cutover 10.2 R6).
    #[test]
    fn lines_the_old_history_never_showed() {
        for text in [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
            "<system-reminder>\nx\n</system-reminder>",
            "Caveat: The messages below were generated by the user while running local commands.",
            "Base directory for this skill: /etc/claude-code/skills/pdf",
        ] {
            let line = json!({"type": "user", "message": {"content": [{"type": "text", "text": text}]}});
            assert_eq!(classify(Some(&line), Context::default()), Class::Internal, "{text}");
            let line = json!({"type": "user", "message": {"content": text}});
            assert_eq!(classify(Some(&line), Context::default()), Class::Internal, "{text}");
        }
        // only at the start, as the old side reads it
        let line = json!({"type": "user", "message": {"content": "see [Request interrupted by user]"}});
        assert!(matches!(classify(Some(&line), Context::default()), Class::Event(_)));
        // the placeholder in any turn, as a string or one text block; another synthetic reply stays
        for content in [
            json!("No response requested."),
            json!([{"type": "text", "text": "No response requested."}]),
        ] {
            let reply = json!({"type": "assistant", "message": {"model": "<synthetic>", "content": content}});
            assert_eq!(classify(Some(&reply), Context::default()), Class::Internal);
        }
        let limit = json!({"type": "assistant", "message": {"model": "<synthetic>",
            "content": [{"type": "text", "text": "Claude AI usage limit reached"}]}});
        assert!(matches!(classify(Some(&limit), Context::default()), Class::Event(_)));
        let real = json!({"type": "assistant", "message": {"model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "No response requested."}]}});
        assert!(matches!(classify(Some(&real), Context::default()), Class::Event(_)));
    }

    #[test]
    fn a_compaction_boundary_is_an_event() {
        let d = json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb1",
            "compactMetadata": {"trigger": "auto", "preTokens": 155_234, "postTokens": 12_100, "durationMs": 63_400}});
        let e = event(classify(Some(&d), Context::default()));
        assert_eq!(e.kind, USER);
        assert!(!e.is_prompt);
        assert_eq!(e.uuid.as_deref(), Some("cb1"));
        assert_eq!(
            e.meta.compaction,
            Some(Compaction {
                trigger: "auto".into(),
                pre_tokens: 155_234,
                post_tokens: 12_100,
                duration_ms: 63_400
            })
        );
        let bare = json!({"type": "system", "subtype": "compact_boundary", "compact_metadata": {"trigger": "manual"}});
        let e = event(classify(Some(&bare), Context::default()));
        assert_eq!(
            e.meta.compaction,
            Some(Compaction {
                trigger: "manual".into(),
                pre_tokens: 0,
                post_tokens: 0,
                duration_ms: 0
            })
        );
    }

    #[test]
    fn internal_lines_and_titles() {
        for d in [
            json!({"type": "attachment"}),
            json!({"type": "system", "subtype": "status"}),
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
    fn a_message_taken_in_mid_turn_is_a_prompt() {
        let d = json!({"type": "attachment", "uuid": "u9", "attachment": {"type": "queued_command",
            "prompt": "<scheduled-message rule=\"每天 09:00\">x</scheduled-message>", "commandMode": "prompt"}});
        let e = event(classify(Some(&d), Context::default()));
        assert_eq!(e.kind, USER);
        assert!(e.is_prompt);
        assert_eq!(e.uuid.as_deref(), Some("u9"));
        assert!(e.text.as_deref().is_some_and(|t| t.starts_with("<scheduled-message")));
        for other in [
            json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "x",
                "commandMode": "task-notification"}}),
            json!({"type": "attachment", "attachment": {"type": "date", "date": "2026-11-06"}}),
        ] {
            assert_eq!(classify(Some(&other), Context::default()), Class::Internal, "{other}");
        }
    }

    #[test]
    fn blob_text_uses_its_preview() {
        let d = json!({"type": "user", "message": {"content": {"$blob": {"blobId": "x", "bytes": 1, "kind": "text", "preview": "long"}}}});
        let e = event(classify(Some(&d), Context::default()));
        assert_eq!(e.text.as_deref(), Some("long"));
    }
}
