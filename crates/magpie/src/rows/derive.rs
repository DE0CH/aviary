//! What each event makes of the rows (magpie 7.3 rules 1-8), as pure functions over the event and the row it touches.
//! [`super::project`] does the lookups and the bookkeeping (row seqs, sources, the epoch) around them; a tool row
//! rebuilt after a retract (rule 9) is made again by the same functions from its surviving events.

use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::{
    EventBody, EventMeta, FilesSent, TaskNotification, TaskState, TaskStatus, TurnEnded, UserSent,
};
use magpie_api::rows::{
    AssistantItem, ErrorItem, RESULT_PREVIEW_CHARS, ReasoningItem, ResultImage, RowFile, RowImage, RowItem,
    RowSentFile, TaskInfo, ToolItem, ToolResult, ToolStatus, UserItem,
};
use magpie_api::turns::TurnOutcome;
use serde_json::Value;

/// The `$blob` reference of a value, if it is one: `(blobId, bytes, mime, width, height, preview)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobView {
    /// `blobId`.
    pub id: String,
    /// Stored bytes.
    pub bytes: u64,
    /// `mime`.
    pub mime: Option<String>,
    /// `width`.
    pub width: Option<u32>,
    /// `height`.
    pub height: Option<u32>,
    /// Text blobs: `preview`.
    pub preview: Option<String>,
}

/// Reads `{"$blob": {…}}`.
#[must_use]
pub fn blob(v: &Value) -> Option<BlobView> {
    let b = v.get("$blob")?;
    let u32_of = |k: &str| b.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Some(BlobView {
        id: b.get("blobId")?.as_str()?.to_owned(),
        bytes: b.get("bytes").and_then(Value::as_u64).unwrap_or(0),
        mime: b.get("mime").and_then(Value::as_str).map(str::to_owned),
        width: u32_of("width"),
        height: u32_of("height"),
        preview: b.get("preview").and_then(Value::as_str).map(str::to_owned),
    })
}

/// The first `n` characters of `s`.
#[must_use]
pub fn chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The content blocks of a transcript line's `message.content` (a string reads as one text block).
#[must_use]
pub fn content(line: &Value) -> Vec<Value> {
    match line.get("message").and_then(|m| m.get("content")) {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => vec![serde_json::json!({"type": "text", "text": s})],
        _ => Vec::new(),
    }
}

/// How many content blocks a line has (for the block index of later lines of the same message).
#[must_use]
pub fn block_count(line: &Value) -> u32 {
    u32::try_from(content(line).len()).unwrap_or(u32::MAX)
}

/// `message.id` of an assistant line.
#[must_use]
pub fn message_id(line: &Value) -> Option<&str> {
    line.get("message")?.get("id")?.as_str()
}

/// A text value: inline, or a text blob's preview.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        other => blob(other).and_then(|b| b.preview),
    }
}

fn block_type(b: &Value) -> &str {
    b.get("type").and_then(Value::as_str).unwrap_or("")
}

/// Whether a line is a sidechain line (`meta.sidechain`, or Claude Code's `isSidechain`).
#[must_use]
pub fn sidechain(line: &Value, meta: &EventMeta) -> bool {
    meta.sidechain == Some(true) || line.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

/// Rule 1: `user.sent` → a `user` row.
#[must_use]
pub fn user_sent(u: &UserSent, viewer: Option<&str>) -> RowItem {
    RowItem::User(UserItem {
        message_id: u.client_message_id.clone(),
        client_message_id: Some(u.client_message_id.clone()),
        text: u.text.clone(),
        images: u
            .images
            .iter()
            .map(|i| RowImage {
                blob_id: i.blob.blob.blob_id.clone(),
                mime_type: i.mime_type.clone(),
                width: i.width,
                height: i.height,
            })
            .collect(),
        files: u
            .files
            .iter()
            .map(|f| RowFile {
                path: f.path.clone(),
                name: f.name.clone(),
                mime_type: f.mime_type.clone(),
                size: f.size,
                missing: None,
            })
            .collect(),
        viewer: viewer.map(str::to_owned),
    })
}

/// Rule 2: a prompt line (`meta.prompt`) → a `user` row. `text` is the event's plain text, when it has one (a long
/// prompt's text blocks may be blobs: their previews are not the whole text).
#[must_use]
pub fn prompt(line: &Value, meta: &EventMeta, text: Option<&str>) -> RowItem {
    let blocks = content(line);
    let text = meta.display_text.clone().unwrap_or_else(|| {
        text.map_or_else(
            || {
                blocks
                    .iter()
                    .filter(|b| block_type(b) == "text")
                    .filter_map(|b| b.get("text").and_then(text_of))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            str::to_owned,
        )
    });
    let images = blocks
        .iter()
        .filter(|b| block_type(b) == "image")
        .filter_map(|b| {
            let source = b.get("source")?;
            let r = blob(source.get("data")?)?;
            Some(RowImage {
                blob_id: r.id,
                mime_type: source
                    .get("media_type")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(r.mime)
                    .unwrap_or_default(),
                width: r.width,
                height: r.height,
            })
        })
        .collect();
    RowItem::User(UserItem {
        message_id: line.get("uuid").and_then(Value::as_str).unwrap_or_default().to_owned(),
        client_message_id: None,
        text,
        images,
        files: meta.attachments.clone().unwrap_or_default(),
        viewer: None,
    })
}

/// Tokens as Claude Code writes them (`725k`, `1.2M`), as the old stack's `formatTokenCount` does in JavaScript:
/// `(t / 1e6).toFixed(1)` (one decimal of the double's exact value, a tie rounded up) and `Math.round(t / 1000)`.
fn token_count(t: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
    let f = t as f64;
    if t >= 1_000_000 {
        // the double's exact decimal digits (at most 52 after the point): round up iff the second is 5 or more
        let exact = format!("{:.60}", f / 1_000_000.0);
        let (int, frac) = exact.split_once('.').unwrap_or((exact.as_str(), "0"));
        let digit = |i: usize| u64::from(frac.as_bytes().get(i).map_or(0, |b| b - b'0'));
        let tenths = int.parse::<u64>().unwrap_or(0) * 10 + digit(0) + u64::from(digit(1) >= 5);
        format!("{}.{}M", tenths / 10, tenths % 10)
    } else if t >= 1_000 {
        format!("{}k", (f / 1_000.0).round())
    } else {
        t.to_string()
    }
}

/// A duration as Claude Code writes one (`2m 22s`), as the old stack's `formatDuration`.
fn duration(ms: u64) -> String {
    let s = (ms + 500) / 1_000;
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

/// Rule 2a: a compaction's marker (`meta.compaction`) → an `assistant` row with the old stack's sentence
/// (`Compacted auto · 155k → 12k tokens · 1m 3s`); `messageId` = the line's `uuid`.
#[must_use]
pub fn compaction(line: &Value, c: &magpie_api::events::Compaction) -> RowItem {
    let mut said = vec!["Compacted".to_owned(), c.trigger.clone()];
    if c.pre_tokens > 0 {
        said.push(if c.post_tokens > 0 {
            format!("{} → {} tokens", token_count(c.pre_tokens), token_count(c.post_tokens))
        } else {
            format!("{} tokens", token_count(c.pre_tokens))
        });
    }
    if c.duration_ms > 0 {
        said.push(duration(c.duration_ms));
    }
    RowItem::Assistant(AssistantItem {
        message_id: line.get("uuid").and_then(Value::as_str).unwrap_or_default().to_owned(),
        block_index: 0,
        error: None,
        text: said.join(" · "),
    })
}

/// Rule 2a: the row of a compaction's lines. Its marker is one ([`compaction`]); Claude Code's summary
/// (`meta.compactSummary`) is none: it is written for the model, pages of it, and staff read the marker (Deyao,
/// 2026-11-12: with compactions at a turn's end it stood after their answer).
#[must_use]
pub fn compaction_row(line: &Value, meta: &magpie_api::events::EventMeta) -> Option<RowItem> {
    meta.compaction.as_ref().map(|c| compaction(line, c))
}

/// One row of an assistant line's block (rule 4).
#[derive(Debug, Clone, PartialEq)]
pub struct BlockRow {
    /// The block's index within the message.
    pub block_index: u32,
    /// The row.
    pub item: RowItem,
    /// `tool_use` id of a tool row.
    pub tool_use_id: Option<String>,
}

/// Rule 4: the rows of an assistant line whose first block has index `first` within its message. Text and thinking
/// blocks that are empty or only whitespace (Claude Code's `"\n\n"` before a tool call; the old stack's history left
/// them out), `redacted_thinking` and unknown block types make no row.
#[must_use]
pub fn assistant(line: &Value, meta: &EventMeta, first: u32) -> Vec<BlockRow> {
    let Some(mid) = message_id(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, b) in content(line).iter().enumerate() {
        let block_index = first.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
        match block_type(b) {
            "text" => {
                let text = b.get("text").and_then(text_of).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                out.push(BlockRow {
                    block_index,
                    item: RowItem::Assistant(AssistantItem {
                        message_id: mid.to_owned(),
                        block_index,
                        error: meta.error.clone(),
                        text,
                    }),
                    tool_use_id: None,
                });
            }
            "thinking" => {
                let text = b.get("thinking").and_then(text_of).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                out.push(BlockRow {
                    block_index,
                    item: RowItem::Reasoning(ReasoningItem {
                        message_id: mid.to_owned(),
                        block_index,
                        text,
                    }),
                    tool_use_id: None,
                });
            }
            // a `server_tool_use` block's result comes back inside the same message, never as a `tool_result` line
            // (rule 5), so only `tool_use` makes a row (rule 4 lists only it)
            "tool_use" => {
                let Some(id) = b.get("id").and_then(Value::as_str) else {
                    continue;
                };
                out.push(BlockRow {
                    block_index,
                    item: RowItem::Tool(ToolItem {
                        call_id: id.to_owned(),
                        message_id: mid.to_owned(),
                        block_index,
                        name: b.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        input: b.get("input").cloned().unwrap_or(Value::Object(serde_json::Map::new())),
                        status: ToolStatus::Running,
                        result: None,
                        sent_files: None,
                        task: None,
                        error: None,
                        answered_at: None,
                    }),
                    tool_use_id: Some(id.to_owned()),
                });
            }
            _ => {}
        }
    }
    out
}

/// The `tool_result` blocks of a user line, by `tool_use_id`.
#[must_use]
pub fn tool_results(line: &Value) -> Vec<(String, Value)> {
    content(line)
        .into_iter()
        .filter(|b| block_type(b) == "tool_result")
        .filter_map(|b| Some((b.get("tool_use_id")?.as_str()?.to_owned(), b)))
        .collect()
}

/// Rule 5's result preview of one `tool_result` block.
#[must_use]
pub fn result_of(block: &Value) -> ToolResult {
    let mut parts: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    let mut blob_id = None;
    let mut extracted = false;
    let mut images = Vec::new();
    let mut add_text = |v: &Value, parts: &mut Vec<String>| match v {
        Value::String(s) => {
            bytes += u64::try_from(s.len()).unwrap_or(u64::MAX);
            parts.push(s.clone());
        }
        other => {
            if let Some(b) = blob(other) {
                bytes += b.bytes;
                extracted = true;
                if blob_id.is_none() {
                    blob_id = Some(b.id.clone());
                }
                parts.push(b.preview.unwrap_or_default());
            }
        }
    };
    match block.get("content") {
        Some(Value::Array(a)) => {
            for b in a {
                match block_type(b) {
                    "text" => {
                        if let Some(t) = b.get("text") {
                            add_text(t, &mut parts);
                        }
                    }
                    "image" => {
                        if let Some(source) = b.get("source")
                            && let Some(r) = source.get("data").and_then(blob)
                        {
                            images.push(ResultImage {
                                blob_id: r.id,
                                mime_type: source
                                    .get("media_type")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .or(r.mime)
                                    .unwrap_or_default(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(v) => add_text(v, &mut parts),
        None => {}
    }
    let full = parts.join("\n");
    let text = chars(&full, RESULT_PREVIEW_CHARS).to_owned();
    ToolResult {
        truncated: extracted || text.len() < full.len(),
        text,
        bytes,
        blob_id,
        is_error: block.get("is_error").and_then(Value::as_bool) == Some(true),
        images: if images.is_empty() { None } else { Some(images) },
    }
}

/// Rule 5: a result lands on its tool row.
pub fn apply_result(t: &mut ToolItem, block: &Value) {
    let r = result_of(block);
    t.status = if r.is_error {
        ToolStatus::Failed
    } else {
        ToolStatus::Completed
    };
    t.result = Some(r);
}

fn notification_state(status: &str) -> (TaskState, ToolStatus) {
    match status {
        "completed" => (TaskState::Completed, ToolStatus::Completed),
        "failed" | "error" => (TaskState::Failed, ToolStatus::Failed),
        _ => (TaskState::Stopped, ToolStatus::Canceled),
    }
}

/// Rule 3: a task notification folds onto its launching tool row: `task`, `status`, and the summary in place of
/// the launch's own result text (today's history folding, done once at write time). A launch whose output was
/// extracted keeps its `blobId` and `bytes` (the link to that full output), with `truncated` set.
pub fn apply_notification(t: &mut ToolItem, n: &TaskNotification) {
    let (task_state, status) = notification_state(&n.status);
    let task_id = n
        .task_id
        .clone()
        .or_else(|| t.task.as_ref().map(|k| k.task_id.clone()))
        .unwrap_or_default();
    t.task = Some(TaskInfo {
        task_id,
        status: task_state,
        summary: n
            .summary
            .clone()
            .or_else(|| t.task.as_ref().and_then(|k| k.summary.clone())),
    });
    t.status = status;
    if let Some(summary) = &n.summary {
        let text = chars(summary, RESULT_PREVIEW_CHARS).to_owned();
        let bytes = u64::try_from(summary.len()).unwrap_or(u64::MAX);
        let cut = text.len() < summary.len();
        match &mut t.result {
            Some(r) => {
                r.text = text;
                if r.blob_id.is_some() {
                    r.truncated = true;
                } else {
                    r.truncated = cut;
                    r.bytes = bytes;
                }
            }
            None => {
                t.result = Some(ToolResult {
                    text,
                    truncated: cut,
                    bytes,
                    blob_id: None,
                    is_error: false,
                    images: None,
                });
            }
        }
    }
}

/// Rule 6: a sent-file snapshot on the `SendUserFile` row.
pub fn apply_sent(t: &mut ToolItem, f: &FilesSent) {
    t.sent_files = Some(
        f.files
            .iter()
            .map(|s| RowSentFile {
                path: s.path.clone(),
                name: s.name.clone(),
                size: s.size,
                mime_type: s.mime_type.clone(),
                is_image: s.is_image,
            })
            .collect(),
    );
}

/// Rule 7: a supervisor task report on its launching row; a terminal status ends the row.
pub fn apply_task(t: &mut ToolItem, s: &TaskStatus) {
    t.task = Some(TaskInfo {
        task_id: s.task_id.clone(),
        status: s.status,
        summary: s
            .summary
            .clone()
            .or_else(|| t.task.as_ref().and_then(|k| k.summary.clone())),
    });
    match s.status {
        TaskState::Running => {
            // a launch the end of its turn canceled or failed before the supervisor's report of the task landed: the
            // task runs, so the row does too (it ends on the notification, a terminal report, or the stop magpie
            // reports when the supervisor no longer tracks the task)
            if matches!(t.status, ToolStatus::Canceled | ToolStatus::Failed) && t.result.is_none() {
                t.status = ToolStatus::Running;
                t.error = None;
            }
        }
        TaskState::Completed => t.status = ToolStatus::Completed,
        TaskState::Failed => t.status = ToolStatus::Failed,
        TaskState::Stopped => t.status = ToolStatus::Canceled,
    }
}

/// Rule 8 for one running tool row of the ended turn; `false` when the row stays as it is (a completed turn, or a
/// background launch whose task still runs: it stays running until its notification or task report).
pub fn apply_turn_end(t: &mut ToolItem, e: &TurnEnded) -> bool {
    if t.status != ToolStatus::Running || t.task.as_ref().is_some_and(|k| k.status == TaskState::Running) {
        return false;
    }
    match e.outcome {
        TurnOutcome::Completed => false,
        TurnOutcome::Canceled => {
            t.status = ToolStatus::Canceled;
            true
        }
        TurnOutcome::Failed => {
            t.status = ToolStatus::Failed;
            t.error = Some(turn_error(e));
            true
        }
    }
}

fn turn_error(e: &TurnEnded) -> ErrorInfo {
    e.error
        .clone()
        .unwrap_or_else(|| ErrorInfo::new(DataCode::RunError, "the turn failed"))
}

/// Rule 8: the `error` row of a failed turn.
#[must_use]
pub fn turn_error_row(e: &TurnEnded) -> Option<RowItem> {
    if e.outcome != TurnOutcome::Failed {
        return None;
    }
    let err = turn_error(e);
    Some(RowItem::Error(ErrorItem {
        code: err.code,
        message: err.message,
        detail: err.detail,
    }))
}

/// Applies one later event of a tool row to it (rules 3, 5, 6, 7, 8): what the incremental projection does for an
/// event that touches the row, and what a rebuild does with each surviving event. `false` when the event does not
/// concern the row.
pub fn apply(t: &mut ToolItem, body: &EventBody, meta: &EventMeta) -> bool {
    match body {
        EventBody::CcUser(line) => {
            if let Some(n) = &meta.task_notification {
                if n.tool_use_id == t.call_id {
                    apply_notification(t, n);
                    return true;
                }
                return false;
            }
            if meta.ask_answer == Some(true) {
                // a parked turn's answers: the line's text is the questions' result (DECISIONS 17)
                if meta.results.iter().flatten().any(|r| r.tool_use_id == t.call_id) {
                    apply_result(t, &serde_json::json!({"type": "tool_result", "content": content(line)}));
                    return true;
                }
                return false;
            }
            let mut hit = false;
            for (id, block) in tool_results(line) {
                if id == t.call_id {
                    apply_result(t, &block);
                    hit = true;
                }
            }
            hit
        }
        EventBody::FilesSent(f) if f.tool_use_id == t.call_id => {
            apply_sent(t, f);
            true
        }
        EventBody::TaskStatus(s) if s.tool_use_id.as_deref() == Some(t.call_id.as_str()) => {
            apply_task(t, s);
            true
        }
        EventBody::TurnEnded(e) => apply_turn_end(t, e),
        _ => false,
    }
}

/// The tool row a rebuild starts from: the `tool_use` block with `call_id` in an assistant line.
#[must_use]
pub fn tool_from_line(line: &Value, call_id: &str, block_index: u32) -> Option<ToolItem> {
    let mid = message_id(line)?;
    let b = content(line)
        .into_iter()
        .find(|b| b.get("id").and_then(Value::as_str) == Some(call_id))?;
    Some(ToolItem {
        call_id: call_id.to_owned(),
        message_id: mid.to_owned(),
        block_index,
        name: b.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        input: b.get("input").cloned().unwrap_or(Value::Object(serde_json::Map::new())),
        status: ToolStatus::Running,
        result: None,
        sent_files: None,
        task: None,
        error: None,
        answered_at: None,
    })
}

/// The text a search or a prompt preview reads from a row.
#[must_use]
pub fn row_text(item: &RowItem) -> Option<&str> {
    match item {
        RowItem::User(u) => Some(&u.text),
        RowItem::Assistant(a) => Some(&a.text),
        RowItem::Reasoning(r) => Some(&r.text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn a_long_result_is_cut_to_the_preview() {
        let long = "字".repeat(RESULT_PREVIEW_CHARS + 10);
        let r = result_of(&json!({"type": "tool_result", "tool_use_id": "t", "content": long}));
        assert_eq!(r.text.chars().count(), RESULT_PREVIEW_CHARS);
        assert!(r.truncated);
        assert_eq!(r.bytes, u64::try_from(long.len()).unwrap());
    }

    #[test]
    fn an_extracted_result_points_at_its_blob() {
        let r = result_of(
            &json!({"type": "tool_result", "tool_use_id": "t", "is_error": true, "content": [
            {"type": "text", "text": {"$blob": {"blobId": "c1", "bytes": 48211, "kind": "text", "preview": "abc"}}},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                "data": {"$blob": {"blobId": "9f", "bytes": 10, "kind": "media"}}}}]}),
        );
        assert_eq!(r.text, "abc");
        assert!(r.truncated);
        assert!(r.is_error);
        assert_eq!(r.bytes, 48211);
        assert_eq!(r.blob_id.as_deref(), Some("c1"));
        assert_eq!(r.images.unwrap()[0].blob_id, "9f");
    }

    #[test]
    fn empty_blocks_make_no_row_and_indexes_count_every_block() {
        let line = json!({"type": "assistant", "message": {"id": "m", "content": [
            {"type": "text", "text": ""}, {"type": "redacted_thinking", "data": "x"},
            {"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "hi"}]}});
        let rows = assistant(&line, &EventMeta::default(), 3);
        assert_eq!(rows.iter().map(|r| r.block_index).collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(block_count(&line), 4);
    }

    /// Claude Code's `"\n\n"` text block before a tool call, and a blank thinking block, make no row: the old
    /// stack's history left them out (cutover 10.2 R6).
    #[test]
    fn whitespace_only_blocks_make_no_row() {
        let line = json!({"type": "assistant", "message": {"id": "m", "content": [
            {"type": "text", "text": "\n\n"}, {"type": "thinking", "thinking": " \n"},
            {"type": "tool_use", "id": "t", "name": "Bash"}, {"type": "text", "text": "\nok"}]}});
        let rows = assistant(&line, &EventMeta::default(), 0);
        assert_eq!(rows.iter().map(|r| r.block_index).collect::<Vec<_>>(), vec![2, 3]);
        let RowItem::Assistant(a) = &rows[1].item else {
            panic!("not an assistant row")
        };
        assert_eq!(a.text, "\nok");
    }

    /// The old stack's compaction rows (`claude-sessions.provider.ts`: `formatTokenCount`, `formatDuration`).
    #[test]
    fn a_compaction_reads_as_the_old_stack_wrote_it() {
        let c = |trigger: &str, pre: u64, post: u64, ms: u64| magpie_api::events::Compaction {
            trigger: trigger.into(),
            pre_tokens: pre,
            post_tokens: post,
            duration_ms: ms,
        };
        let line = json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb1"});
        let text = |c| match compaction(&line, &c) {
            RowItem::Assistant(a) => {
                assert_eq!((a.message_id.as_str(), a.block_index), ("cb1", 0));
                a.text
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            text(c("auto", 155_234, 12_100, 63_400)),
            "Compacted · auto · 155k → 12k tokens · 1m 3s"
        );
        assert_eq!(
            text(c("manual", 1_250_000, 0, 4_499)),
            "Compacted · manual · 1.3M tokens · 4s"
        );
        assert_eq!(text(c("auto", 999, 0, 0)), "Compacted · auto · 999 tokens");
        assert_eq!(text(c("auto", 0, 0, 120_000)), "Compacted · auto · 2m 0s");
        assert_eq!(text(c("auto", 0, 0, 0)), "Compacted · auto");
        // JavaScript's toFixed on the double: 1.15 is 1.1499…, 1.25 a true tie (up), 999,999,999 is 1000.0
        for (t, want) in [
            (1_150_000, "1.1M"),
            (1_050_000, "1.1M"),
            (2_350_000, "2.4M"),
            (1_000_000, "1.0M"),
            (999_999_999, "1000.0M"),
            (1_500, "2k"),
            (1_499, "1k"),
        ] {
            assert_eq!(token_count(t), want, "{t}");
        }
        // the marker is the compaction's one row; the summary Claude Code writes for the model is none
        let boundary = json!({"type": "system", "subtype": "compact_boundary", "uuid": "b1"});
        let marker = magpie_api::events::EventMeta {
            compaction: Some(magpie_api::events::Compaction {
                trigger: "manual".into(),
                pre_tokens: 21_676,
                post_tokens: 3_799,
                duration_ms: 11_112,
            }),
            ..Default::default()
        };
        let Some(RowItem::Assistant(a)) = compaction_row(&boundary, &marker) else {
            panic!("no row")
        };
        assert_eq!(
            (a.message_id.as_str(), a.text.as_str()),
            ("b1", "Compacted manual · 22k → 4k tokens · 11s")
        );
        let summary = json!({"type": "user", "uuid": "s1", "isCompactSummary": true,
            "message": {"content": "This session is being continued…"}});
        let of_summary = magpie_api::events::EventMeta {
            compact_summary: Some(true),
            ..Default::default()
        };
        assert!(compaction_row(&summary, &of_summary).is_none());
    }

    fn launch() -> ToolItem {
        tool_from_line(
            &json!({"message": {"id": "m", "content": [{"type": "tool_use", "id": "t", "name": "Bash"}]}}),
            "t",
            0,
        )
        .unwrap()
    }

    #[test]
    fn a_server_tool_use_block_makes_no_row() {
        let line = json!({"type": "assistant", "message": {"id": "m", "content": [
            {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}},
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []},
            {"type": "text", "text": "found"}]}});
        let rows = assistant(&line, &EventMeta::default(), 0);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].item, RowItem::Assistant(_)));
        assert_eq!(rows[0].block_index, 2);
    }

    #[test]
    fn a_notification_keeps_the_link_to_an_extracted_launch_output() {
        let mut t = launch();
        apply_result(
            &mut t,
            &json!({"type": "tool_result", "tool_use_id": "t", "content":
                {"$blob": {"blobId": "c1", "bytes": 48211, "kind": "text", "preview": "abc"}}}),
        );
        let n: TaskNotification =
            serde_json::from_value(json!({"toolUseId": "t", "taskId": "k", "status": "completed", "summary": "done"}))
                .unwrap();
        apply_notification(&mut t, &n);
        let r = t.result.unwrap();
        assert_eq!(
            (r.text.as_str(), r.blob_id.as_deref(), r.bytes, r.truncated),
            ("done", Some("c1"), 48211, true)
        );
        // a short stub launch result: the summary takes its place whole
        let mut t = launch();
        apply_result(
            &mut t,
            &json!({"type": "tool_result", "tool_use_id": "t", "content": "started"}),
        );
        apply_notification(&mut t, &n);
        let r = t.result.unwrap();
        assert_eq!(
            (r.text.as_str(), r.blob_id, r.bytes, r.truncated),
            ("done", None, 4, false)
        );
    }

    #[test]
    fn a_running_background_launch_outlives_a_canceled_or_failed_turn() {
        let ended = |outcome: &str| -> TurnEnded {
            serde_json::from_value(json!({"turnId": uuid::Uuid::nil(), "outcome": outcome})).unwrap()
        };
        let mut t = launch();
        let s: TaskStatus =
            serde_json::from_value(json!({"taskId": "k", "toolUseId": "t", "event": "updated", "status": "running"}))
                .unwrap();
        apply_task(&mut t, &s);
        for outcome in ["canceled", "failed"] {
            assert!(!apply_turn_end(&mut t, &ended(outcome)));
            assert_eq!(t.status, ToolStatus::Running);
        }
        // a plain running call does end with the turn
        let mut t = launch();
        assert!(apply_turn_end(&mut t, &ended("canceled")));
        assert_eq!(t.status, ToolStatus::Canceled);
        // the task's start report landed after the turn's end: the launch runs again, and a stop ends it
        apply_task(&mut t, &s);
        assert_eq!(t.status, ToolStatus::Running);
        assert_eq!(t.error, None);
        let stopped: TaskStatus =
            serde_json::from_value(json!({"taskId": "k", "toolUseId": "t", "event": "updated", "status": "stopped"}))
                .unwrap();
        apply_task(&mut t, &stopped);
        assert_eq!(t.status, ToolStatus::Canceled);
        // a call that has its result does not come back
        let mut t = launch();
        apply_result(
            &mut t,
            &json!({"type": "tool_result", "tool_use_id": "t", "content": "started"}),
        );
        t.status = ToolStatus::Canceled;
        apply_task(&mut t, &s);
        assert_eq!(t.status, ToolStatus::Canceled);
    }
}
