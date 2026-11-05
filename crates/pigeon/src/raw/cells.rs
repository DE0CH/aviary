//! One transcript line as typed cells (the raw view): each content block of a message is a cell of its own kind,
//! with its content as readable text (a tool call's input fields as `name: value` lines, a tool result's output
//! text), never the line's JSON.

use serde::Serialize;
use serde_json::{Map, Value};

/// A cell's text is cut past this many bytes (the page shows head and tail of what it gets, and all on request).
pub const MAX_TEXT: usize = 256 * 1024;

/// One cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    /// The line's order in the file.
    pub ord: i64,
    /// The line's `timestamp` (ISO 8601), when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// `user`, `assistant`, `thinking`, `tool call`, `tool result`, `system`, `compaction`, `compaction summary`,
    /// `interrupt`, `summary`, `meta`, or the line's own `type` for bookkeeping lines.
    pub kind: String,
    /// A tool call's tool name, a system line's subtype.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The content as text.
    pub text: String,
    /// An error (a tool result with `is_error`, an API error, a system line at level error).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
    /// The tool call this cell is or answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    /// Model and tokens (an assistant line's last cell).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    /// A bookkeeping line (file snapshots, queue operations, titles…): hidden unless asked for.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub quiet: bool,
}

impl Cell {
    fn new(ord: i64, at: Option<&str>, kind: &str, text: String) -> Self {
        Self {
            ord,
            at: at.map(str::to_owned),
            kind: kind.to_owned(),
            title: None,
            text: cut(text),
            error: false,
            tool_id: None,
            meta: None,
            quiet: false,
        }
    }
}

/// `text` cut to [`MAX_TEXT`] bytes (on a char boundary), with how much was left out.
fn cut(mut text: String) -> String {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let total = text.len();
    let mut at = MAX_TEXT;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    text.truncate(at);
    format!("{text}\n[{} more bytes not sent]", total - at)
}

/// Fields that every line carries and that say nothing about it.
const BOILERPLATE: &[&str] = &[
    "uuid",
    "parentUuid",
    "logicalParentUuid",
    "sessionId",
    "cwd",
    "version",
    "gitBranch",
    "userType",
    "isSidechain",
    "timestamp",
    "type",
    "requestId",
    "slug",
];

/// Tool-input fields shown first, in this order; the rest follow by name.
const FIRST: &[&str] = &[
    "description",
    "command",
    "file_path",
    "path",
    "pattern",
    "query",
    "url",
    "prompt",
    "old_string",
    "new_string",
    "content",
];

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// An object's fields as `name: value` lines (a multi-line value below its name, indented; nested values compact).
#[must_use]
pub fn fields(m: &Map<String, Value>, skip: &[&str]) -> String {
    let mut keys: Vec<&String> = m.keys().filter(|k| !skip.contains(&k.as_str())).collect();
    keys.sort_by_key(|k| (FIRST.iter().position(|f| f == k).unwrap_or(FIRST.len()), (*k).clone()));
    let mut out = Vec::new();
    for k in keys {
        let v = &m[k];
        if v.is_null() {
            continue;
        }
        let s = scalar(v).unwrap_or_else(|| serde_json::to_string(v).unwrap_or_default());
        if s.contains('\n') {
            let body: Vec<String> = s.lines().map(|l| format!("  {l}")).collect();
            out.push(format!("{k}:\n{}", body.join("\n")));
        } else {
            out.push(format!("{k}: {s}"));
        }
    }
    out.join("\n")
}

/// A tool result's content as text (a string, or text blocks; images as a placeholder).
fn result_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
                Some("image") => "[image]".to_owned(),
                Some(t) => format!("[{t}]"),
                None => scalar(b).unwrap_or_default(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(v) => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn user_text_kind(o: &Map<String, Value>, text: &str) -> &'static str {
    if o.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        "compaction summary"
    } else if text.trim_start().starts_with("[Request interrupted by user") {
        "interrupt"
    } else if o.get("isMeta").and_then(Value::as_bool) == Some(true) {
        "meta"
    } else {
        "user"
    }
}

fn usage(msg: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(m) = msg.get("model").and_then(Value::as_str) {
        parts.push(m.to_owned());
    }
    if let Some(u) = msg.get("usage").and_then(Value::as_object) {
        for (k, label) in [
            ("input_tokens", "in"),
            ("cache_read_input_tokens", "cache read"),
            ("cache_creation_input_tokens", "cache write"),
            ("output_tokens", "out"),
        ] {
            if let Some(n) = u.get(k).and_then(Value::as_u64).filter(|n| *n > 0) {
                parts.push(format!("{label} {n}"));
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// A message's content blocks as cells.
fn blocks(ord: i64, at: Option<&str>, o: &Map<String, Value>, assistant: bool) -> Vec<Cell> {
    let msg = o.get("message").and_then(Value::as_object);
    let content = msg.and_then(|m| m.get("content"));
    let mut out = Vec::new();
    let text_cell = |text: &str| {
        let kind = if assistant {
            "assistant"
        } else {
            user_text_kind(o, text)
        };
        Cell::new(ord, at, kind, text.to_owned())
    };
    match content {
        Some(Value::String(s)) => out.push(text_cell(s)),
        Some(Value::Array(items)) => {
            for b in items {
                let t = b.get("type").and_then(Value::as_str).unwrap_or("");
                match t {
                    "text" => out.push(text_cell(b.get("text").and_then(Value::as_str).unwrap_or(""))),
                    "thinking" => {
                        let thought = b.get("thinking").and_then(Value::as_str).unwrap_or("");
                        let thought = if thought.is_empty() {
                            "[thinking not stored]"
                        } else {
                            thought
                        };
                        out.push(Cell::new(ord, at, "thinking", thought.to_owned()));
                    }
                    "redacted_thinking" => out.push(Cell::new(ord, at, "thinking", "[redacted]".to_owned())),
                    "tool_use" | "server_tool_use" => {
                        let input = b.get("input").and_then(Value::as_object).cloned().unwrap_or_default();
                        let mut c = Cell::new(ord, at, "tool call", fields(&input, &[]));
                        c.title = b.get("name").and_then(Value::as_str).map(str::to_owned);
                        c.tool_id = b.get("id").and_then(Value::as_str).map(str::to_owned);
                        out.push(c);
                    }
                    "tool_result" => {
                        let mut c = Cell::new(ord, at, "tool result", result_text(b.get("content")));
                        c.error = b.get("is_error").and_then(Value::as_bool) == Some(true);
                        c.tool_id = b.get("tool_use_id").and_then(Value::as_str).map(str::to_owned);
                        out.push(c);
                    }
                    "image" => out.push(text_cell("[image]")),
                    "document" => out.push(text_cell("[document]")),
                    _ if t.ends_with("_tool_result") => {
                        let mut c = Cell::new(ord, at, "tool result", result_text(b.get("content")));
                        c.tool_id = b.get("tool_use_id").and_then(Value::as_str).map(str::to_owned);
                        out.push(c);
                    }
                    _ => {
                        let m = b.as_object().cloned().unwrap_or_default();
                        let mut c = Cell::new(
                            ord,
                            at,
                            if assistant { "assistant" } else { "user" },
                            fields(&m, &["type"]),
                        );
                        c.title = Some(t.to_owned());
                        out.push(c);
                    }
                }
            }
        }
        _ => {}
    }
    if out.is_empty() {
        out.push(text_cell(""));
    }
    if o.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
        for c in &mut out {
            c.error = true;
        }
    }
    if assistant && let (Some(m), Some(last)) = (msg, out.last_mut()) {
        last.meta = usage(m);
    }
    out
}

/// The cells of one transcript line.
#[must_use]
pub fn of_line(ord: i64, line: &str) -> Vec<Cell> {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(line) else {
        let mut c = Cell::new(ord, None, "unparsed", line.to_owned());
        c.quiet = line.trim().is_empty();
        return vec![c];
    };
    let at = o.get("timestamp").and_then(Value::as_str);
    let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "user" => blocks(ord, at, &o, false),
        "assistant" => blocks(ord, at, &o, true),
        "system" => {
            let subtype = o.get("subtype").and_then(Value::as_str);
            if subtype == Some("compact_boundary") {
                let m = o
                    .get("compactMetadata")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let mut text = fields(&m, &[]);
                if let Some(c) = o.get("content").and_then(Value::as_str) {
                    text = if text.is_empty() {
                        c.to_owned()
                    } else {
                        format!("{c}\n{text}")
                    };
                }
                return vec![Cell::new(ord, at, "compaction", text)];
            }
            let text = match o.get("content").and_then(Value::as_str) {
                Some(c) => c.to_owned(),
                None => fields(&o, &[BOILERPLATE, &["subtype", "level"]].concat()),
            };
            let mut c = Cell::new(ord, at, "system", text);
            c.title = subtype.map(str::to_owned);
            c.error = o.get("level").and_then(Value::as_str) == Some("error");
            vec![c]
        }
        "summary" => vec![Cell::new(
            ord,
            at,
            "summary",
            o.get("summary").and_then(Value::as_str).unwrap_or("").to_owned(),
        )],
        _ => {
            let mut c = Cell::new(
                ord,
                at,
                if ty.is_empty() { "line" } else { ty },
                fields(&o, BOILERPLATE),
            );
            c.quiet = true;
            vec![c]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_and_an_interrupt() {
        let c = of_line(
            1,
            r#"{"type":"user","timestamp":"2026-11-05T03:12:09.120Z","message":{"role":"user","content":"港珠澳大橋今日有冇新聞？"}}"#,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, "user");
        assert_eq!(c[0].text, "港珠澳大橋今日有冇新聞？");
        assert_eq!(c[0].at.as_deref(), Some("2026-11-05T03:12:09.120Z"));
        let c = of_line(
            2,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
        );
        assert_eq!(c[0].kind, "interrupt");
    }

    #[test]
    fn an_assistant_line_with_thinking_text_and_a_tool_call() {
        let c = of_line(
            3,
            r#"{"type":"assistant","timestamp":"t","message":{"model":"claude-opus-4-5","usage":{"input_tokens":3,"cache_read_input_tokens":2000,"output_tokens":120},"content":[{"type":"thinking","thinking":"Let me look."},{"type":"text","text":"Checking."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"timeout":1000,"command":"ls -la\nwc -l x","description":"List files"}}]}}"#,
        );
        let kinds: Vec<&str> = c.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["thinking", "assistant", "tool call"]);
        assert_eq!(c[2].title.as_deref(), Some("Bash"));
        assert_eq!(c[2].tool_id.as_deref(), Some("toolu_1"));
        assert_eq!(
            c[2].text,
            "description: List files\ncommand:\n  ls -la\n  wc -l x\ntimeout: 1000"
        );
        assert_eq!(
            c[2].meta.as_deref(),
            Some("claude-opus-4-5, in 3, cache read 2000, out 120")
        );
        assert_eq!(c[0].meta, None);
    }

    #[test]
    fn tool_results_compaction_and_bookkeeping() {
        let c = of_line(
            4,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":[{"type":"text","text":"exit 1"},{"type":"image","source":{"data":"[media image/png 9 bytes]"}}]}]},"toolUseResult":{"stdout":"x"}}"#,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, "tool result");
        assert!(c[0].error);
        assert_eq!(c[0].text, "exit 1\n[image]");
        let c = of_line(
            5,
            r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":155000}}"#,
        );
        assert_eq!(c[0].kind, "compaction");
        assert_eq!(c[0].text, "Conversation compacted\npreTokens: 155000\ntrigger: auto");
        let c = of_line(
            6,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued…"}}"#,
        );
        assert_eq!(c[0].kind, "compaction summary");
        let c = of_line(
            7,
            r#"{"type":"file-history-snapshot","messageId":"m","snapshot":{"a":1}}"#,
        );
        assert!(c[0].quiet);
        assert_eq!(c[0].kind, "file-history-snapshot");
        assert_eq!(c[0].text, "messageId: m\nsnapshot: {\"a\":1}");
        let c = of_line(8, "not json");
        assert_eq!(c[0].kind, "unparsed");
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let long = "港".repeat(MAX_TEXT);
        let c = Cell::new(1, None, "user", long.clone());
        assert!(c.text.len() < MAX_TEXT + 64);
        assert!(c.text.ends_with("more bytes not sent]"));
    }
}
