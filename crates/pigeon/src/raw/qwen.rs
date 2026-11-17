//! A Qwen Code transcript line as typed cells (the raw view of a conversation whose main transcript is Qwen Code's,
//! `qwen/projects/<cwd>/chats/<sid>.jsonl`). Qwen Code writes Gemini-shaped records, not Claude Code's lines:
//!
//! | Qwen Code | cells |
//! |---|---|
//! | `type: "user"`, `message.parts[]` `{text}` / `{inlineData}` | `user` (the person's: `provenance` `real_user`), else `meta` titled by its `subtype` (notification, cron…); `mid_turn_user_message` is the person's |
//! | `type: "assistant"`, parts `{text, thought: true}`, `{text}`, `{functionCall: {id, name, args}}` | `thinking`, `assistant`, `tool call`; model and tokens from `model` and `usageMetadata` |
//! | `type: "tool_result"`, parts `{functionResponse: {id, name, response: {output \| error}, parts}}` | `tool result` per response, titled by the tool, error from `toolCallResult.status` or `response.error` |
//! | `type: "system"`, `subtype: "chat_compression"` | `compaction` (the token counts) and `compaction summary` (the summary the model goes on from) |
//! | `type: "system"`, other subtypes | `system` titled by the subtype; bookkeeping (telemetry, snapshots, session settings…) quiet |

use serde_json::{Map, Value};

use super::cells::{Cell, fields};

/// Record fields that say nothing about the line in the raw view.
const BOILERPLATE: &[&str] = &[
    "uuid",
    "parentUuid",
    "sessionId",
    "cwd",
    "version",
    "gitBranch",
    "timestamp",
    "type",
    "subtype",
    "provenance",
    "message",
    "executionContext",
    "promptId",
];

/// System subtypes worth seeing without asking (the rest is bookkeeping: telemetry, snapshots, session settings).
const SHOWN_SYSTEM: &[&str] = &[
    "slash_command",
    "at_command",
    "background_task_completed",
    "rewind",
    "turn_result",
    "agent_retry",
    "tool_loop_end",
    "code_mode_tool_result",
];

/// `turn_result` fields left out: the prompt and the answer are on the page already.
const TURN_RESULT_SKIP: &[&str] = &["promptText", "promptTextTruncated", "resultText", "resultTextTruncated"];

fn parts(o: &Map<String, Value>) -> &[Value] {
    o.get("message")
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Media in a part, as a line naming it (`[video clip.mp4, media video/mp4 1234 bytes]`).
fn media(inline: &Value) -> String {
    let mime = inline.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let what = mime.split('/').next().filter(|s| !s.is_empty()).unwrap_or("file");
    let mut said = vec![what.to_owned()];
    if let Some(name) = inline
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        said.push(name.to_owned());
    }
    let label = said.join(" ");
    match inline.get("data").and_then(Value::as_str) {
        // magpie puts `[media <mime> <size> bytes]` where the base64 was
        Some(d) if d.starts_with("[media ") => {
            format!("[{label}, {}]", d.trim_start_matches('[').trim_end_matches(']'))
        }
        _ => format!("[{label}]"),
    }
}

/// A value as text: a string as is, anything else as compact JSON.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Object(m) => fields(m, &[]),
        other => other.to_string(),
    }
}

/// A plain part (`text` or `inlineData`) as text; `None` for a part of another kind.
fn plain(part: &Value) -> Option<String> {
    if let Some(t) = part.get("text") {
        return Some(text_of(t));
    }
    part.get("inlineData").map(media)
}

fn usage(o: &Map<String, Value>) -> Option<String> {
    let mut out = Vec::new();
    if let Some(m) = o.get("model").and_then(Value::as_str) {
        out.push(m.to_owned());
    }
    if let Some(u) = o.get("usageMetadata").and_then(Value::as_object) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        // the prompt count includes what was read from the cache
        let cached = n("cachedContentTokenCount");
        for (label, v) in [
            ("in", n("promptTokenCount").saturating_sub(cached)),
            ("cache read", cached),
            ("out", n("candidatesTokenCount")),
            ("thinking", n("thoughtsTokenCount")),
        ] {
            if v > 0 {
                out.push(format!("{label} {v}"));
            }
        }
    }
    (!out.is_empty()).then(|| out.join(", "))
}

fn user(ord: i64, at: Option<&str>, o: &Map<String, Value>) -> Vec<Cell> {
    let subtype = o.get("subtype").and_then(Value::as_str);
    let persons = match subtype {
        None => o
            .get("provenance")
            .and_then(Value::as_str)
            .is_none_or(|p| p == "real_user"),
        Some(s) => s == "mid_turn_user_message",
    };
    let kind = if persons { "user" } else { "meta" };
    let mut out: Vec<Cell> = parts(o)
        .iter()
        .filter_map(plain)
        .map(|t| {
            let mut c = Cell::new(ord, at, kind, t);
            c.title = subtype.map(str::to_owned);
            c
        })
        .collect();
    if out.is_empty() {
        let mut c = Cell::new(ord, at, kind, String::new());
        c.title = subtype.map(str::to_owned);
        out.push(c);
    }
    out
}

fn assistant(ord: i64, at: Option<&str>, o: &Map<String, Value>) -> Vec<Cell> {
    let mut out = Vec::new();
    for p in parts(o) {
        if let Some(call) = p.get("functionCall") {
            let args = call.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
            let mut c = Cell::new(ord, at, "tool call", fields(&args, &[]));
            c.title = call.get("name").and_then(Value::as_str).map(str::to_owned);
            c.tool_id = call.get("id").and_then(Value::as_str).map(str::to_owned);
            out.push(c);
        } else if let Some(t) = p.get("text") {
            let thought = p.get("thought").and_then(Value::as_bool) == Some(true);
            let text = text_of(t);
            let (kind, text) = match (thought, text.is_empty()) {
                (true, true) => ("thinking", "[thinking not stored]".to_owned()),
                (true, false) => ("thinking", text),
                (false, _) => ("assistant", text),
            };
            out.push(Cell::new(ord, at, kind, text));
        } else if let Some(m) = p.get("inlineData") {
            out.push(Cell::new(ord, at, "assistant", media(m)));
        } else if let Some(m) = p.as_object() {
            out.push(Cell::new(ord, at, "assistant", fields(m, &[])));
        }
    }
    if out.is_empty() {
        out.push(Cell::new(ord, at, "assistant", String::new()));
    }
    if let Some(last) = out.last_mut() {
        last.meta = usage(o);
    }
    out
}

fn tool_result(ord: i64, at: Option<&str>, o: &Map<String, Value>) -> Vec<Cell> {
    let failed = o
        .get("toolCallResult")
        .and_then(|r| r.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| s != "success");
    let mut out = Vec::new();
    for p in parts(o) {
        let Some(r) = p.get("functionResponse") else {
            if let Some(t) = plain(p) {
                out.push(Cell::new(ord, at, "tool result", t));
            }
            continue;
        };
        let body = r.get("response");
        let error = body.and_then(|b| b.get("error")).filter(|e| !e.is_null());
        let mut text: Vec<String> = Vec::new();
        match error.or_else(|| body.and_then(|b| b.get("output"))) {
            Some(v) => text.push(text_of(v)),
            // a response with neither: show what it has
            None => {
                if let Some(b) = body.and_then(Value::as_object) {
                    text.push(fields(b, &[]));
                }
            }
        }
        for q in r.get("parts").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
            if let Some(t) = plain(q) {
                text.push(t);
            }
        }
        text.retain(|t| !t.is_empty());
        let mut c = Cell::new(ord, at, "tool result", text.join("\n"));
        c.title = r.get("name").and_then(Value::as_str).map(str::to_owned);
        c.tool_id = r.get("id").and_then(Value::as_str).map(str::to_owned);
        c.error = failed || error.is_some();
        out.push(c);
    }
    if out.is_empty() {
        let mut c = Cell::new(ord, at, "tool result", fields(o, BOILERPLATE));
        c.error = failed;
        out.push(c);
    }
    out
}

fn system(ord: i64, at: Option<&str>, o: &Map<String, Value>) -> Vec<Cell> {
    let subtype = o.get("subtype").and_then(Value::as_str);
    let payload = o.get("systemPayload").and_then(Value::as_object);
    if subtype == Some("chat_compression") {
        let info = payload
            .and_then(|p| p.get("info"))
            .and_then(Value::as_object)
            .map(|i| fields(i, &[]))
            .unwrap_or_default();
        let mut out = vec![Cell::new(ord, at, "compaction", info)];
        // the summary is the first entries of the history the model goes on from
        let history = payload
            .and_then(|p| p.get("compressedHistory"))
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let n = payload
            .and_then(|p| p.get("summaryPrefixLength"))
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1)
            .min(history.len());
        let summary: Vec<String> = history[..n]
            .iter()
            .flat_map(|c| c.get("parts").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))
            .filter_map(plain)
            .filter(|t| !t.is_empty())
            .collect();
        if !summary.is_empty() {
            out.push(Cell::new(ord, at, "compaction summary", summary.join("\n\n")));
        }
        return out;
    }
    let said: Vec<String> = parts(o).iter().filter_map(plain).filter(|t| !t.is_empty()).collect();
    let text = if !said.is_empty() {
        said.join("\n")
    } else if let Some(p) = payload {
        let skip = if subtype == Some("turn_result") {
            TURN_RESULT_SKIP
        } else {
            &[]
        };
        fields(p, skip)
    } else {
        fields(o, BOILERPLATE)
    };
    let mut c = Cell::new(ord, at, "system", text);
    c.title = subtype.map(str::to_owned);
    c.quiet = !subtype.is_some_and(|s| SHOWN_SYSTEM.contains(&s));
    c.error = payload.and_then(|p| p.get("state")).and_then(Value::as_str) == Some("error");
    vec![c]
}

/// The cells of one Qwen Code transcript line.
#[must_use]
pub fn of_line(ord: i64, line: &str) -> Vec<Cell> {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(line) else {
        return super::cells::of_line(ord, line);
    };
    let at = o.get("timestamp").and_then(Value::as_str);
    match o.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" => user(ord, at, &o),
        "assistant" => assistant(ord, at, &o),
        "tool_result" => tool_result(ord, at, &o),
        "system" => system(ord, at, &o),
        ty => {
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

    // lines as Qwen Code 0.27.3 wrote them (magpie's ccfs/qwen.rs tests), media as magpie hands them back
    const USER: &str = r#"{"uuid":"3b63c5a0","parentUuid":null,"sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:48.710Z","type":"user","provenance":"real_user","cwd":"/workspace","version":"0.27.3","executionContext":{"modelId":"vendor/video","authType":"openai","approvalMode":"yolo"},"message":{"role":"user","parts":[{"text":"cut the video"}]},"promptId":"f3f05d67########0"}"#;
    const ASSISTANT: &str = r#"{"uuid":"81d6ff83","parentUuid":"a005e52f","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:50.488Z","type":"assistant","provenance":"assistant_output","cwd":"/workspace","version":"0.27.3","model":"vendor/video","message":{"role":"model","parts":[{"text":"The user wants both files read.","thought":true},{"text":"I'll read both files now."},{"functionCall":{"id":"chatcmpl-tool-99e5","name":"read_file","args":{"file_path":"/workspace/work/clip.mp4"}}}]},"usageMetadata":{"promptTokenCount":12544,"candidatesTokenCount":118,"thoughtsTokenCount":54,"totalTokenCount":12662,"cachedContentTokenCount":544},"contextWindowSize":200000}"#;
    const RESULT: &str = r#"{"uuid":"a71d0632","parentUuid":"dea6e989","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:50.567Z","type":"tool_result","provenance":"tool_result","cwd":"/workspace","version":"0.27.3","message":{"role":"user","parts":[{"functionResponse":{"id":"chatcmpl-tool-99e5","name":"read_file","response":{"output":""},"parts":[{"inlineData":{"data":"[media video/mp4 1234 bytes]","mimeType":"video/mp4","displayName":"clip.mp4"}}]}}]},"toolCallResult":{"callId":"chatcmpl-tool-99e5","status":"success","resultDisplay":"Read video file: work/clip.mp4","executionStatus":"success"}}"#;
    const FAILED: &str = r#"{"uuid":"b1","timestamp":"t","type":"tool_result","message":{"role":"user","parts":[{"functionResponse":{"id":"call_2","name":"run_shell_command","response":{"error":"Command exited with code 1\nno such file"}}}]},"toolCallResult":{"callId":"call_2","status":"error"}}"#;
    const TELEMETRY: &str = r#"{"uuid":"6d1c8c48","parentUuid":"639aa03a","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:48.853Z","type":"system","provenance":"system","subtype":"ui_telemetry","systemPayload":{"uiEvent":{"v":1}}}"#;
    const NOTIFICATION: &str = r#"{"uuid":"n1","timestamp":"t","type":"user","subtype":"notification","provenance":"system","message":{"role":"user","parts":[{"text":"<task-notification>done</task-notification>"}]},"systemPayload":{"displayText":"Background task done"}}"#;
    const COMPRESSION: &str = r#"{"uuid":"c1","timestamp":"t","type":"system","subtype":"chat_compression","systemPayload":{"info":{"originalTokenCount":150000,"newTokenCount":9000,"compressionStatus":1},"compressedHistory":[{"role":"user","parts":[{"text":"<state_snapshot>the summary</state_snapshot>"}]},{"role":"model","parts":[{"text":"Got it."}]},{"role":"user","parts":[{"text":"latest"}]}],"summaryPrefixLength":2}}"#;
    const TURN_ERROR: &str = r#"{"uuid":"r1","timestamp":"t","type":"system","subtype":"turn_result","systemPayload":{"promptId":"p","state":"error","error":{"message":"400 Could not open video stream"},"endedAt":5,"resultText":"x"}}"#;

    fn kinds(c: &[Cell]) -> Vec<&str> {
        c.iter().map(|c| c.kind.as_str()).collect()
    }

    #[test]
    fn a_persons_message_shows_its_text() {
        let c = of_line(1, USER);
        assert_eq!(kinds(&c), ["user"]);
        assert_eq!(c[0].text, "cut the video");
        assert_eq!(c[0].at.as_deref(), Some("2026-11-13T04:44:48.710Z"));
        assert!(!c[0].quiet);
    }

    #[test]
    fn a_reply_shows_its_thinking_text_tool_call_and_usage() {
        let c = of_line(2, ASSISTANT);
        assert_eq!(kinds(&c), ["thinking", "assistant", "tool call"]);
        assert_eq!(c[0].text, "The user wants both files read.");
        assert_eq!(c[1].text, "I'll read both files now.");
        assert_eq!(c[2].title.as_deref(), Some("read_file"));
        assert_eq!(c[2].tool_id.as_deref(), Some("chatcmpl-tool-99e5"));
        assert_eq!(c[2].text, "file_path: /workspace/work/clip.mp4");
        assert_eq!(
            c[2].meta.as_deref(),
            Some("vendor/video, in 12000, cache read 544, out 118, thinking 54")
        );
    }

    #[test]
    fn a_tool_result_names_its_tool_and_the_media_it_showed() {
        let c = of_line(3, RESULT);
        assert_eq!(kinds(&c), ["tool result"]);
        assert!(!c[0].quiet && !c[0].error);
        assert_eq!(c[0].title.as_deref(), Some("read_file"));
        assert_eq!(c[0].tool_id.as_deref(), Some("chatcmpl-tool-99e5"));
        assert_eq!(c[0].text, "[video clip.mp4, media video/mp4 1234 bytes]");
        let c = of_line(4, FAILED);
        assert!(c[0].error);
        assert_eq!(c[0].text, "Command exited with code 1\nno such file");
    }

    #[test]
    fn bookkeeping_is_quiet_and_notifications_are_not_the_person() {
        let c = of_line(5, TELEMETRY);
        assert!(c[0].quiet);
        assert_eq!(c[0].title.as_deref(), Some("ui_telemetry"));
        let c = of_line(6, NOTIFICATION);
        assert_eq!(kinds(&c), ["meta"]);
        assert_eq!(c[0].title.as_deref(), Some("notification"));
        assert_eq!(c[0].text, "<task-notification>done</task-notification>");
    }

    #[test]
    fn a_compression_shows_its_counts_and_summary() {
        let c = of_line(7, COMPRESSION);
        assert_eq!(kinds(&c), ["compaction", "compaction summary"]);
        assert_eq!(
            c[0].text,
            "compressionStatus: 1\nnewTokenCount: 9000\noriginalTokenCount: 150000"
        );
        assert_eq!(c[1].text, "<state_snapshot>the summary</state_snapshot>\n\nGot it.");
    }

    #[test]
    fn a_failed_turn_is_an_error_without_the_answer_repeated() {
        let c = of_line(8, TURN_ERROR);
        assert_eq!(c[0].title.as_deref(), Some("turn_result"));
        assert!(c[0].error && !c[0].quiet);
        assert!(c[0].text.contains("Could not open video stream"));
        assert!(!c[0].text.contains("resultText"));
    }
}
