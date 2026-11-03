//! The display fixes of imported prompts (magpie 19.3), applied to every main-transcript line the importer writes
//! (cutover 7.4.2). They change only what an event shows (`meta.displayText`, `meta.attachments`), never the stored
//! line, so `claude --resume` reads exactly what Claude Code wrote:
//!
//! - the old stack's sandbox-reset note in front of a prompt is removed;
//! - Claude Code's `<pasted_content id="…">` wrappers are unwrapped (a wrapper whose closing tag names another id,
//!   or that is not closed, is left as it is);
//! - the old stack's `<image_uploads>` and `<files_input>` blocks (the last of each, as the old UI appended them) are
//!   removed from the text and their numbered entries become the prompt's attachments.
//!
//! Pure functions; deleted with the importer (cutover 10.5).

/// The note the old UI put in front of the first prompt after a sandbox release (before 2026-10-22).
pub const LEGACY_SANDBOX_RESET_NOTE: &str = "[System note: your sandbox was reset since the last message: /workspace and this conversation are as they were; installed packages, running processes and files outside /workspace are gone.]";

/// A file a prompt's block names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// The path as written (usually `/workspace/uploads/<name>`).
    pub path: String,
    /// The original name, when the entry gives one.
    pub name: Option<String>,
}

/// What a prompt shows after the fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixed {
    /// The text to show (equal to the input when nothing applied).
    pub text: String,
    /// The files its blocks named, images first, in block order.
    pub files: Vec<Named>,
}

impl Fixed {
    /// Whether any fix applied.
    #[must_use]
    pub fn changed(&self, original: &str) -> bool {
        self.text != original || !self.files.is_empty()
    }
}

/// Applies every fix to a prompt's text, in the old server's order (the note, pastes, then the image and file
/// blocks).
#[must_use]
pub fn fix(text: &str) -> Fixed {
    let t = strip_note(text);
    let t = unwrap_pasted(&t);
    let (t, mut files) = strip_block(&t, "image_uploads");
    let (t, more) = strip_block(&t, "files_input");
    files.extend(more);
    Fixed { text: t, files }
}

fn strip_note(text: &str) -> String {
    match text.strip_prefix(LEGACY_SANDBOX_RESET_NOTE) {
        Some(rest) => rest.trim_start_matches(['\r', '\n']).to_owned(),
        None => text.to_owned(),
    }
}

/// `<pasted_content id="ID">\n?BODY\n?</pasted_content id="ID">` (or a bare `</pasted_content>`) → `BODY`; when any
/// wrapper was unwrapped, leading and trailing newlines of the whole text go too.
fn unwrap_pasted(text: &str) -> String {
    const OPEN: &str = "<pasted_content id=\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut unwrapped = false;
    while let Some(at) = rest.find(OPEN) {
        let after_open = &rest[at + OPEN.len()..];
        let Some(q) = after_open.find('"') else { break };
        let id = &after_open[..q];
        let Some(body_and_more) = after_open[q + 1..].strip_prefix('>') else {
            out.push_str(&rest[..at + OPEN.len()]);
            rest = after_open;
            continue;
        };
        // the shortest body that ends at a closing tag naming the same id (or none)
        let close_id = format!("</pasted_content id=\"{id}\">");
        let close_bare = "</pasted_content>";
        let found = [close_id.as_str(), close_bare]
            .iter()
            .filter_map(|c| body_and_more.find(c).map(|p| (p, c.len())))
            .min_by_key(|(p, _)| *p);
        let Some((p, clen)) = found else {
            out.push_str(&rest[..at + OPEN.len()]);
            rest = after_open;
            continue;
        };
        let body = &body_and_more[..p];
        let body = body.strip_prefix('\n').unwrap_or(body);
        let body = body.strip_suffix('\n').unwrap_or(body);
        out.push_str(&rest[..at]);
        out.push_str(body);
        rest = &body_and_more[p + clen..];
        unwrapped = true;
    }
    out.push_str(rest);
    if unwrapped {
        out.trim_matches(['\r', '\n']).to_owned()
    } else {
        text.to_owned()
    }
}

/// The last `<tag>…</tag>` block with the whitespace around it: removed (the text before and after joined by a
/// newline and trimmed), and its numbered entries `N. <path> (original name: <name>)`.
fn strip_block(text: &str, tag: &str) -> (String, Vec<Named>) {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = text.rfind(&open) else {
        return (text.to_owned(), Vec::new());
    };
    let Some(end_rel) = text[start..].find(&close) else {
        return (text.to_owned(), Vec::new());
    };
    let inner = &text[start + open.len()..start + end_rel];
    let end = start + end_rel + close.len();
    let before = text[..start].trim_end();
    let after = text[end..].trim_start();
    let joined = format!("{before}\n{after}");
    let files = inner.lines().filter_map(entry).collect();
    (joined.trim().to_owned(), files)
}

fn entry(line: &str) -> Option<Named> {
    let l = line.trim();
    let digits = l.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = l[digits..].strip_prefix('.')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut body = rest.trim();
    let mut name = None;
    if body.ends_with(')')
        && let Some(p) = body.rfind("(original name: ")
    {
        let n = body[p + "(original name: ".len()..body.len() - 1].trim();
        name = (!n.is_empty()).then(|| n.to_owned());
        body = body[..p].trim();
    }
    (!body.is_empty()).then(|| Named {
        path: body.to_owned(),
        name,
    })
}

/// The workspace-relative path of a named file (`/workspace/uploads/a.png` → `uploads/a.png`), when it is one.
#[must_use]
pub fn workspace_relative(path: &str) -> Option<&str> {
    let rel = path.strip_prefix("/workspace/")?;
    let ok = !rel.is_empty() && rel.split('/').all(|c| !c.is_empty() && c != "." && c != "..") && !rel.contains('\0');
    ok.then_some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_goes_with_its_newlines() {
        let t = format!("{LEGACY_SANDBOX_RESET_NOTE}\n\nhello");
        assert_eq!(fix(&t).text, "hello");
        assert_eq!(fix("hello").text, "hello");
        assert!(!fix("hello").changed("hello"));
    }

    #[test]
    fn pastes_are_unwrapped_as_the_old_server_did() {
        let wrap =
            |id: &str, body: &str| format!("<pasted_content id=\"{id}\">\n{body}\n</pasted_content id=\"{id}\">");
        assert_eq!(
            fix(&format!("\n\n{}\n", wrap("7b82", "line one\n  line two"))).text,
            "line one\n  line two"
        );
        assert_eq!(
            fix(&format!("please look\n\n{}\n\nand tell me", wrap("26bf", "trace"))).text,
            "please look\n\ntrace\n\nand tell me"
        );
        assert_eq!(
            fix(&format!("{}\n{}", wrap("a", "first"), wrap("b", "second"))).text,
            "first\nsecond"
        );
        let body = "const x = <div>hi</div>;\n<pasted_content id=\"zz99\">not closed here";
        assert_eq!(fix(&wrap("c0de", body)).text, body);
        for t in [
            "<pasted_content id=\"aa11\">\nbody\n</pasted_content id=\"bb22\">",
            "<pasted_content id=\"aa11\">\nno closing tag",
            "no opening tag\n</pasted_content id=\"aa11\">",
            "the words <pasted_content> in prose",
            "  plain text\n",
        ] {
            assert_eq!(fix(t).text, t);
        }
    }

    #[test]
    fn blocks_become_attachments() {
        let t = "look at these\n\n<image_uploads>\nThe 1 image(s) attached to this message, which you can see, are also saved as files (the originals, full size):\n1. /workspace/uploads/IMG_0001.HEIC (original name: IMG_0001.HEIC)\nUse these paths when you need the image files themselves. Do not mention this block unless the user asks about it.\n</image_uploads>\n\n<files_input>\nThe user attached 2 file(s) to this message. Read each file listed below.\n1. /workspace/uploads/a.pdf (original name: report (final).pdf)\n2. /workspace/uploads/b.txt\n</files_input>";
        let f = fix(t);
        assert_eq!(f.text, "look at these");
        assert_eq!(
            f.files,
            vec![
                Named {
                    path: "/workspace/uploads/IMG_0001.HEIC".into(),
                    name: Some("IMG_0001.HEIC".into())
                },
                Named {
                    path: "/workspace/uploads/a.pdf".into(),
                    name: Some("report (final).pdf".into())
                },
                Named {
                    path: "/workspace/uploads/b.txt".into(),
                    name: None
                },
            ]
        );
        assert_eq!(workspace_relative("/workspace/uploads/a.pdf"), Some("uploads/a.pdf"));
        assert_eq!(workspace_relative("/home/x/a.pdf"), None);
        assert_eq!(workspace_relative("/workspace/../etc"), None);
    }
}
