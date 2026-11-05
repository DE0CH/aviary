//! The user message of a `start_turn` (supervisor 12.2): the person's text followed by today's tags,
//! byte-compatible with `CloudCLI`'s `appendFilesInputTag`/`appendImageUploadsTag` and the importer's display fixes;
//! a string without images, else a text block and one base64 image block per image.

use base64::Engine as _;
use serde_json::{Value, json};

/// One attached file or image as the tags list it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    /// Absolute: `/workspace/<workspacePath>`.
    pub path: String,
    /// The original name (`None`: no suffix).
    pub name: Option<String>,
}

/// An image's model copy for an image block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlock {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

fn entries(list: &[Attached]) -> Vec<String> {
    list.iter()
        .enumerate()
        .map(|(i, a)| {
            let clean = a
                .name
                .as_deref()
                .map(|n| n.replace(['(', ')', '\r', '\n'], "").trim().to_owned())
                .filter(|n| !n.is_empty());
            match clean {
                Some(n) => format!("{}. {} (original name: {n})", i + 1, a.path),
                None => format!("{}. {}", i + 1, a.path),
            }
        })
        .collect()
}

/// The text: the person's, then `<files_input>` when there are files, then `<image_uploads>` when there are images.
#[must_use]
pub fn text(prompt: &str, files: &[Attached], images: &[Attached]) -> String {
    let mut out = prompt.to_owned();
    if !files.is_empty() {
        let mut lines = vec![
            out,
            String::new(),
            "<files_input>".to_owned(),
            format!(
                "The user attached {} file(s) to this message. Read each file listed below with your file reading tools and use its contents to answer the prompt above. Do not mention this block or the file paths unless the user asks about them.",
                files.len()
            ),
        ];
        lines.extend(entries(files));
        lines.push("</files_input>".to_owned());
        out = lines.join("\n");
    }
    if !images.is_empty() {
        let mut lines = vec![
            out,
            String::new(),
            "<image_uploads>".to_owned(),
            format!(
                "The {} image(s) attached to this message, which you can see, are also saved as files (the originals, full size):",
                images.len()
            ),
        ];
        lines.extend(entries(images));
        lines.push("Use these paths when you need the image files themselves (to crop, convert, measure, read metadata or put them in a document). Do not mention this block unless the user asks about it.".to_owned());
        lines.push("</image_uploads>".to_owned());
        out = lines.join("\n");
    }
    out
}

/// `message.content`: the text as a string, or a text block and one image block per image.
#[must_use]
pub fn content(text: String, images: &[ImageBlock]) -> Value {
    if images.is_empty() {
        return Value::String(text);
    }
    let mut blocks = vec![json!({"type": "text", "text": text})];
    for i in images {
        blocks.push(json!({"type": "image", "source": {
            "type": "base64",
            "media_type": i.media_type,
            "data": base64::engine::general_purpose::STANDARD.encode(&i.bytes),
        }}));
    }
    Value::Array(blocks)
}

/// The NDJSON line written to Claude Code's stdin (with its newline).
#[must_use]
pub fn user_line(content: &Value, sent_at: &str) -> Vec<u8> {
    let mut v = serde_json::to_vec(&json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "timestamp": sent_at,
    }))
    .unwrap_or_default();
    v.push(b'\n');
    v
}

/// The message that resumes a parked turn (DECISIONS 17): each answer worded as Claude Code 2.1.304 words the
/// tool's own result (`AskUserQuestion`'s `mapToolResultToToolResultBlockParam`, `ExitPlanMode`'s approval), so
/// the model reads the person's answer as it would have read the tool's result. The resumed Claude Code has
/// already closed the question itself ("[Tool call interrupted …]", supervisor 7.6), so this is the turn's next
/// user message.
#[must_use]
pub fn answers(list: &[magpie_api::sandbox::ParkedAnswer]) -> String {
    list.iter().map(answer).collect::<Vec<_>>().join("\n\n")
}

fn answer(a: &magpie_api::sandbox::ParkedAnswer) -> String {
    if !a.allow {
        return a
            .message
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "The person declined.".to_owned());
    }
    let input = a.updated_input.as_ref().unwrap_or(&a.input);
    match a.tool_name.as_str() {
        "AskUserQuestion" => questions(&a.input, input),
        "ExitPlanMode" => {
            let plan = input.get("plan").and_then(Value::as_str).unwrap_or("").trim();
            if plan.is_empty() {
                "User has approved exiting plan mode. You can now proceed.".to_owned()
            } else {
                format!(
                    "User has approved your plan. You can now start coding. Start with updating your todo list if applicable\n\n## Approved Plan:\n{plan}"
                )
            }
        }
        tool => format!(
            "The user allowed the {tool} call. It did not run: the session ended before the answer arrived, so call it again."
        ),
    }
}

/// `AskUserQuestion`'s result for `answers` (`updatedInput.answers`, question → answer) of `asked`'s questions.
fn questions(asked: &serde_json::Map<String, Value>, given: &serde_json::Map<String, Value>) -> String {
    let empty = Vec::new();
    let qs = asked
        .get("questions")
        .or_else(|| given.get("questions"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let answers = given.get("answers").and_then(Value::as_object);
    let answer_of = |q: &str| -> Option<String> {
        match answers?.get(q)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Array(v) if !v.is_empty() => Some(v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
            _ => None,
        }
    };
    let mut parts = Vec::new();
    let mut chosen = true;
    for q in qs {
        let Some(text) = q.get("question").and_then(Value::as_str) else {
            continue;
        };
        let Some(a) = answer_of(text) else { continue };
        let labels: Vec<&str> = q
            .get("options")
            .and_then(Value::as_array)
            .map(|o| {
                o.iter()
                    .filter_map(|x| x.get("label").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let multi = q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
        let is_label = labels.contains(&a.as_str()) || (multi && a.split(", ").all(|p| labels.contains(&p)));
        chosen &= is_label;
        parts.push(format!("\"{text}\"=\"{a}\""));
    }
    let response = given.get("response").and_then(Value::as_str).map_or("", str::trim);
    if !response.is_empty() {
        return format!("The user responded: {response}");
    }
    if parts.is_empty() {
        return "The user did not answer the questions.".to_owned();
    }
    let d = parts.join(", ");
    if chosen {
        format!("Your questions have been answered: {d}. You can now continue with these answers in mind.")
    } else {
        format!(
            "The user answered: {d}. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_a_string() {
        assert_eq!(text("你好", &[], &[]), "你好");
        let line = user_line(&content("你好".into(), &[]), "2026-11-02T03:46:10.120Z");
        let v: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["message"]["content"], "你好");
        assert_eq!(v["parent_tool_use_id"], Value::Null);
        assert_eq!(*line.last().unwrap(), b'\n');
        assert_eq!(line.iter().position(|b| *b == b'\n'), Some(line.len() - 1));
    }

    #[test]
    fn tags_match_cloudcli_byte_for_byte() {
        let files = [Attached {
            path: "/workspace/uploads/Q3 report.pdf".into(),
            name: Some("Q3 (report)\n.pdf".into()),
        }];
        let images = [Attached {
            path: "/workspace/uploads/IMG_0042.jpg".into(),
            name: Some("IMG_0042.jpg".into()),
        }];
        let t = text("Summarise", &files, &images);
        let want = "Summarise\n\n<files_input>\nThe user attached 1 file(s) to this message. Read each file listed below with your file reading tools and use its contents to answer the prompt above. Do not mention this block or the file paths unless the user asks about them.\n1. /workspace/uploads/Q3 report.pdf (original name: Q3 report.pdf)\n</files_input>\n\n<image_uploads>\nThe 1 image(s) attached to this message, which you can see, are also saved as files (the originals, full size):\n1. /workspace/uploads/IMG_0042.jpg (original name: IMG_0042.jpg)\nUse these paths when you need the image files themselves (to crop, convert, measure, read metadata or put them in a document). Do not mention this block unless the user asks about it.\n</image_uploads>";
        assert_eq!(t, want);
        let nameless = [Attached {
            path: "/workspace/uploads/x".into(),
            name: Some("()".into()),
        }];
        assert!(text("", &nameless, &[]).contains("\n1. /workspace/uploads/x\n"));
    }

    #[test]
    fn images_are_blocks() {
        let c = content(
            "t".into(),
            &[ImageBlock {
                media_type: "image/png".into(),
                bytes: vec![1, 2, 3],
            }],
        );
        assert_eq!(c[0], json!({"type": "text", "text": "t"}));
        assert_eq!(c[1]["source"]["media_type"], "image/png");
        assert_eq!(c[1]["source"]["data"], "AQID");
    }

    #[allow(clippy::needless_pass_by_value)] // json! literals
    fn parked(
        tool: &str,
        input: Value,
        allow: bool,
        updated: Option<Value>,
        message: Option<&str>,
    ) -> magpie_api::sandbox::ParkedAnswer {
        magpie_api::sandbox::ParkedAnswer {
            ask_id: "a1".into(),
            tool_name: tool.into(),
            tool_use_id: Some("toolu_1".into()),
            input: input.as_object().cloned().unwrap_or_default(),
            allow,
            updated_input: updated.and_then(|u| u.as_object().cloned()),
            message: message.map(str::to_owned),
        }
    }

    #[test]
    fn a_parked_answer_reads_as_the_tools_own_result() {
        let asked = json!({"questions": [{"question": "Which colour?", "header": "Colour", "multiSelect": false,
            "options": [{"label": "Red", "description": "r"}, {"label": "Blue", "description": "b"}]}]});
        let mut given = asked.clone();
        given["answers"] = json!({"Which colour?": "Blue"});
        assert_eq!(
            answers(&[parked(
                "AskUserQuestion",
                asked.clone(),
                true,
                Some(given.clone()),
                None
            )]),
            "Your questions have been answered: \"Which colour?\"=\"Blue\". You can now continue with these answers in mind."
        );
        given["answers"] = json!({"Which colour?": "green, please"});
        assert!(
            answers(&[parked("AskUserQuestion", asked.clone(), true, Some(given), None)])
                .starts_with("The user answered: \"Which colour?\"=\"green, please\". Read the answers carefully")
        );
        assert_eq!(
            answers(&[parked(
                "AskUserQuestion",
                asked.clone(),
                true,
                Some(asked.clone()),
                None
            )]),
            "The user did not answer the questions."
        );
        assert_eq!(
            answers(&[parked("AskUserQuestion", asked, false, None, Some("not now"))]),
            "not now"
        );
        assert!(
            answers(&[parked("ExitPlanMode", json!({"plan": "1. do it"}), true, None, None)])
                .starts_with("User has approved your plan. You can now start coding.")
        );
    }
}
