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
}
