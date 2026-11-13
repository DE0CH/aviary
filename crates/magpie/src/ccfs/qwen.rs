//! Qwen Code's transcript lines in the shape the rest of magpie reads (the one Claude Code writes): a main
//! transcript under `qwen/` is Qwen Code's (`projects/<cwd>/chats/<sid>.jsonl`, measured on 0.27.3), and each of
//! its lines is turned into that shape here before it is classified, so one classifier and one projection serve
//! both harnesses. The stored line is not touched: Qwen Code reads back what it wrote, byte for byte.
//!
//! | Qwen Code | here |
//! |---|---|
//! | `type: "user"`, `message.parts[].text` | `user`, `message.content[]` `text` blocks |
//! | `type: "assistant"`, parts `{text, thought: true}`, `{text}`, `{functionCall: {id, name, args}}` | `assistant`, `thinking`, `text` and `tool_use` blocks; `message.model` from `model`, `message.usage` from `usageMetadata` |
//! | `type: "tool_result"`, parts `{functionResponse: {id, response: {output \| error}, parts[].inlineData}}` | `user` with one `tool_result` block per response: its text, each image as an `image` block, any other media named in a `text` block; `is_error` from `toolCallResult.status` |
//! | `type: "system"` (telemetry, snapshots, checkpoints, titles) and anything else | nothing: internal |
//!
//! A `user` line that is not the person's (`provenance` other than `real_user`) is marked `isMeta`, which the
//! classifier keeps off the timeline.

use serde_json::{Map, Value, json};

/// Whether a main transcript path is Qwen Code's.
#[must_use]
pub fn is_qwen(main: Option<&str>) -> bool {
    main.is_some_and(|p| p.starts_with("qwen/"))
}

/// The line in the common shape, or `None` when it is internal.
#[must_use]
pub fn normalise(line: &Value) -> Option<Value> {
    let kind = line.get("type")?.as_str()?;
    let parts: &[Value] = line
        .get("message")
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let mut out = Map::new();
    for k in ["uuid", "parentUuid", "sessionId", "timestamp", "cwd", "version"] {
        if let Some(v) = line.get(k) {
            out.insert(k.to_owned(), v.clone());
        }
    }
    match kind {
        "user" => {
            let content: Vec<Value> = parts.iter().filter_map(input_block).collect();
            if content.is_empty() {
                return None;
            }
            out.insert("type".into(), "user".into());
            if line
                .get("provenance")
                .and_then(Value::as_str)
                .is_some_and(|p| p != "real_user")
            {
                out.insert("isMeta".into(), true.into());
            }
            out.insert("message".into(), json!({"role": "user", "content": content}));
        }
        "assistant" => {
            let content: Vec<Value> = parts.iter().filter_map(output_block).collect();
            if content.is_empty() {
                return None;
            }
            let mut message = Map::new();
            if let Some(id) = line.get("uuid") {
                message.insert("id".into(), id.clone());
            }
            message.insert("role".into(), "assistant".into());
            if let Some(model) = line.get("model") {
                message.insert("model".into(), model.clone());
            }
            message.insert("content".into(), content.into());
            if let Some(u) = line.get("usageMetadata") {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                // Qwen Code's prompt count includes what was read from the cache
                let cached = n("cachedContentTokenCount");
                message.insert(
                    "usage".into(),
                    json!({
                        "input_tokens": n("promptTokenCount").saturating_sub(cached),
                        "output_tokens": n("candidatesTokenCount"),
                        "cache_read_input_tokens": cached,
                    }),
                );
            }
            out.insert("type".into(), "assistant".into());
            out.insert("message".into(), message.into());
        }
        "tool_result" => {
            let failed = line
                .get("toolCallResult")
                .and_then(|r| r.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|s| s != "success");
            let content: Vec<Value> = parts
                .iter()
                .filter_map(|p| p.get("functionResponse"))
                .filter_map(|r| result_block(r, failed))
                .collect();
            if content.is_empty() {
                return None;
            }
            out.insert("type".into(), "user".into());
            out.insert("message".into(), json!({"role": "user", "content": content}));
        }
        _ => return None,
    }
    Some(out.into())
}

/// A part of a person's message: its text, or an image.
fn input_block(part: &Value) -> Option<Value> {
    if let Some(t) = part.get("text") {
        return Some(json!({"type": "text", "text": t}));
    }
    part.get("inlineData").map(media_block)
}

/// A part of a reply: thinking, text, or a tool call.
fn output_block(part: &Value) -> Option<Value> {
    if let Some(call) = part.get("functionCall") {
        return Some(json!({
            "type": "tool_use",
            "id": call.get("id")?,
            "name": call.get("name").cloned().unwrap_or_default(),
            "input": call.get("args").cloned().unwrap_or_else(|| json!({})),
        }));
    }
    let text = part.get("text")?;
    Some(if part.get("thought").and_then(Value::as_bool) == Some(true) {
        json!({"type": "thinking", "thinking": text})
    } else {
        json!({"type": "text", "text": text})
    })
}

/// One tool's result: what it printed, then what it showed.
fn result_block(response: &Value, failed: bool) -> Option<Value> {
    let id = response.get("id")?;
    let body = response.get("response");
    let error = body.and_then(|b| b.get("error"));
    let text = error.or_else(|| body.and_then(|b| b.get("output")));
    let mut content = Vec::new();
    match text {
        Some(Value::String(s)) if s.is_empty() => {}
        Some(t @ Value::String(_)) => content.push(json!({"type": "text", "text": t})),
        // a text large enough to be a blob stays a reference
        Some(t) if t.get("$blob").is_some() => content.push(json!({"type": "text", "text": t})),
        Some(Value::Null) | None => {}
        Some(other) => content.push(json!({"type": "text", "text": other.to_string()})),
    }
    for p in response
        .get("parts")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        if let Some(m) = p.get("inlineData") {
            content.push(media_block(m));
        } else if let Some(t) = p.get("text") {
            content.push(json!({"type": "text", "text": t}));
        }
    }
    Some(json!({
        "type": "tool_result",
        "tool_use_id": id,
        "is_error": failed || error.is_some(),
        "content": content,
    }))
}

/// Media the harness put into the conversation: an image as an image block; a sound or a video (which the
/// timeline has no block for) as a line naming it.
fn media_block(inline: &Value) -> Value {
    let mime = inline.get("mimeType").and_then(Value::as_str).unwrap_or("");
    if mime.starts_with("image/") {
        return json!({
            "type": "image",
            "source": {"type": "base64", "media_type": mime, "data": inline.get("data").cloned().unwrap_or_default()},
        });
    }
    let what = mime.split('/').next().filter(|s| !s.is_empty()).unwrap_or("file");
    let name = inline.get("displayName").and_then(Value::as_str).unwrap_or("");
    json!({"type": "text", "text": format!("[{what}: {name}]")})
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ccfs::classify::{Class, Context, classify};

    fn line(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // lines as Qwen Code 0.27.3 wrote them (shortened)
    const USER: &str = r#"{"uuid":"3b63c5a0","parentUuid":null,"sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:48.710Z","type":"user","provenance":"real_user","cwd":"/workspace","version":"0.27.3","executionContext":{"modelId":"vendor/video","authType":"openai","approvalMode":"yolo"},"message":{"role":"user","parts":[{"text":"cut the video"}]},"promptId":"f3f05d67########0"}"#;
    const ASSISTANT: &str = r#"{"uuid":"81d6ff83","parentUuid":"a005e52f","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:50.488Z","type":"assistant","provenance":"assistant_output","cwd":"/workspace","version":"0.27.3","model":"vendor/video","message":{"role":"model","parts":[{"text":"The user wants both files read.","thought":true},{"text":"I'll read both files now."},{"functionCall":{"id":"chatcmpl-tool-99e5","name":"read_file","args":{"file_path":"/workspace/work/clip.mp4"}}}]},"usageMetadata":{"promptTokenCount":12544,"candidatesTokenCount":118,"thoughtsTokenCount":54,"totalTokenCount":12662,"cachedContentTokenCount":544},"contextWindowSize":200000}"#;
    const RESULT: &str = r#"{"uuid":"a71d0632","parentUuid":"dea6e989","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:50.567Z","type":"tool_result","provenance":"tool_result","cwd":"/workspace","version":"0.27.3","message":{"role":"user","parts":[{"functionResponse":{"id":"chatcmpl-tool-99e5","name":"read_file","response":{"output":""},"parts":[{"inlineData":{"data":"AAAAIGZ0eXBpc29t","mimeType":"video/mp4","displayName":"clip.mp4"}}]}}]},"toolCallResult":{"callId":"chatcmpl-tool-99e5","status":"success","resultDisplay":"Read video file: work/clip.mp4","executionStatus":"success"}}"#;
    const TELEMETRY: &str = r#"{"uuid":"6d1c8c48","parentUuid":"639aa03a","sessionId":"f3f05d67","timestamp":"2026-11-13T04:44:48.853Z","type":"system","provenance":"system","subtype":"ui_telemetry","systemPayload":{"uiEvent":{"v":1}}}"#;
    const CATALOG: &str = r#"{"uuid":"639aa03a","parentUuid":"45b59e12","sessionId":"f3f05d67","type":"system","provenance":"system","subtype":"memory_catalog_snapshot","message":{"role":"user","parts":[{"text":"Current memory catalog","partMetadata":{"qwen-code:memory-catalog-snapshot":true}}]}}"#;

    #[test]
    fn a_persons_message_is_a_prompt() {
        let n = normalise(&line(USER)).unwrap();
        assert_eq!(n["type"], "user");
        assert_eq!(n["uuid"], "3b63c5a0");
        assert_eq!(
            n["message"]["content"],
            json!([{"type": "text", "text": "cut the video"}])
        );
        assert_eq!(n.get("isMeta"), None);
        // the turn's own message is its echo; the same line outside one is a prompt
        assert_eq!(
            classify(
                Some(&n),
                Context {
                    echo_pending: true,
                    answering: false
                }
            ),
            Class::Echo
        );
        let Class::Event(e) = classify(Some(&n), Context::default()) else {
            panic!("a prompt");
        };
        assert!(e.is_prompt);
        assert_eq!(e.text.as_deref(), Some("cut the video"));
    }

    #[test]
    fn a_reply_has_its_thinking_its_text_and_its_tool_calls() {
        let n = normalise(&line(ASSISTANT)).unwrap();
        assert_eq!(n["type"], "assistant");
        assert_eq!(
            n["message"]["content"],
            json!([
                {"type": "thinking", "thinking": "The user wants both files read."},
                {"type": "text", "text": "I'll read both files now."},
                {"type": "tool_use", "id": "chatcmpl-tool-99e5", "name": "read_file",
                 "input": {"file_path": "/workspace/work/clip.mp4"}},
            ])
        );
        assert_eq!(n["message"]["model"], "vendor/video");
        assert_eq!(
            n["message"]["usage"],
            json!({"input_tokens": 12000, "output_tokens": 118, "cache_read_input_tokens": 544})
        );
        let Class::Event(e) = classify(Some(&n), Context::default()) else {
            panic!("an event");
        };
        assert_eq!(e.kind, "cc.assistant");
        assert_eq!(
            e.tool_uses,
            vec![("chatcmpl-tool-99e5".to_owned(), "read_file".to_owned())]
        );
        assert_eq!(e.model, Some(("vendor/video".to_owned(), Some(12544))));
        assert_eq!(e.uuid.as_deref(), Some("81d6ff83"));
    }

    #[test]
    fn a_tool_result_names_its_call_and_the_media_it_showed() {
        let n = normalise(&line(RESULT)).unwrap();
        assert_eq!(n["type"], "user");
        assert_eq!(
            n["message"]["content"],
            json!([{"type": "tool_result", "tool_use_id": "chatcmpl-tool-99e5", "is_error": false,
                    "content": [{"type": "text", "text": "[video: clip.mp4]"}]}])
        );
        let Class::Event(e) = classify(
            Some(&n),
            Context {
                echo_pending: true,
                answering: false,
            },
        ) else {
            panic!("a result is never the echo");
        };
        assert_eq!(e.results, vec!["chatcmpl-tool-99e5".to_owned()]);
        assert!(!e.is_prompt);

        // an image is shown; a failed call is an error with its text
        let img = line(
            r#"{"uuid":"u","type":"tool_result","message":{"parts":[{"functionResponse":{"id":"t1","name":"read_file","response":{"output":"ok"},"parts":[{"inlineData":{"data":{"$blob":{"blobId":"b1","bytes":9}},"mimeType":"image/png","displayName":"f.png"}}]}}]},"toolCallResult":{"status":"success"}}"#,
        );
        let n = normalise(&img).unwrap();
        assert_eq!(
            n["message"]["content"][0]["content"],
            json!([{"type": "text", "text": "ok"},
                   {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                                "data": {"$blob": {"blobId": "b1", "bytes": 9}}}}])
        );
        let bad = line(
            r#"{"uuid":"u","type":"tool_result","message":{"parts":[{"functionResponse":{"id":"t2","name":"run_shell_command","response":{"error":"exit 1"}}}]},"toolCallResult":{"status":"error"}}"#,
        );
        let n = normalise(&bad).unwrap();
        assert_eq!(n["message"]["content"][0]["is_error"], true);
        assert_eq!(
            n["message"]["content"][0]["content"],
            json!([{"type": "text", "text": "exit 1"}])
        );
    }

    #[test]
    fn qwen_codes_own_records_are_internal() {
        assert_eq!(normalise(&line(TELEMETRY)), None);
        assert_eq!(
            normalise(&line(CATALOG)),
            None,
            "a system record with a message is still no message"
        );
        assert_eq!(normalise(&json!({"type": "assistant", "message": {"parts": []}})), None);
        assert_eq!(normalise(&json!("not an object")), None);
        // a user line Qwen Code wrote itself is kept off the timeline
        let own =
            line(r#"{"uuid":"u","type":"user","provenance":"system_reminder","message":{"parts":[{"text":"x"}]}}"#);
        let n = normalise(&own).unwrap();
        assert_eq!(n["isMeta"], true);
        assert_eq!(classify(Some(&n), Context::default()), Class::Internal);
    }

    #[test]
    fn a_main_transcript_under_qwens_home_is_qwen_codes() {
        assert!(is_qwen(Some("qwen/projects/-workspace/chats/s.jsonl")));
        assert!(!is_qwen(Some("projects/-workspace/s.jsonl")));
        assert!(!is_qwen(None));
    }
}
