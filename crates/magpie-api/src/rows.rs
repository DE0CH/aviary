//! Timeline rows (magpie 7.3-7.4, pigeon 14.1.18): magpie's protocol-neutral projection of events, with a dense row
//! seq per conversation and a stable epoch. pigeon maps one Row to one Paseo row with a pure function.
//!
//! Row seqs: every new row and every row update takes the next integer, and nothing else takes one. A row carries
//! every seq it took twice on the wire: `seqs` (pigeon 14.1.0) and the same list as `sourceSeqRanges` (magpie 7.3,
//! the Paseo field), so both readers find what they expect; [`Row::seqs`] is the one value in memory.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::Timestamp;
use crate::errors::ErrorInfo;
use crate::live::LiveState;
use crate::uploads::BlobId;

/// Timeline page: at most, and the default (magpie 17).
pub const TIMELINE_MAX: u32 = 500;
/// See [`TIMELINE_MAX`].
pub const TIMELINE_DEFAULT: u32 = 40;
/// A timeline page stops early at this many bytes of JSON.
pub const TIMELINE_MAX_JSON_BYTES: usize = 4 << 20;
/// Long-polls (timeline, changes, commands): the longest `waitMs`.
pub const WAIT_MS_MAX: u64 = 25_000;
/// A tool result's preview, characters.
pub const RESULT_PREVIEW_CHARS: usize = 4_000;

/// One timeline row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The row seq of the row's first version (its identity).
    pub seq_start: u64,
    /// The row seq of this version.
    pub seq_end: u64,
    /// Every row seq this row took, ascending.
    pub seqs: Vec<u64>,
    /// `seqEnd != seqStart`.
    pub updated: bool,
    /// When the row's first event was written.
    pub timestamp: Timestamp,
    /// The turn.
    pub turn_id: Option<Uuid>,
    /// What the row shows.
    pub item: RowItem,
}

/// A contiguous run of row seqs (`sourceSeqRanges`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeqRange {
    /// First.
    pub start_seq: u64,
    /// Last (inclusive).
    pub end_seq: u64,
}

/// `seqs` as ranges: `[10, 11, 15]` → `[{10, 11}, {15, 15}]`.
#[must_use]
pub fn seq_ranges(seqs: &[u64]) -> Vec<SeqRange> {
    let mut out: Vec<SeqRange> = Vec::new();
    for &s in seqs {
        match out.last_mut() {
            Some(r) if r.end_seq.checked_add(1) == Some(s) => r.end_seq = s,
            _ => out.push(SeqRange {
                start_seq: s,
                end_seq: s,
            }),
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowWire {
    seq_start: u64,
    seq_end: u64,
    #[serde(default)]
    seqs: Option<Vec<u64>>,
    #[serde(default)]
    source_seq_ranges: Option<Vec<SeqRange>>,
    #[serde(default)]
    updated: Option<bool>,
    timestamp: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_id: Option<Uuid>,
    item: RowItem,
}

impl Serialize for Row {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RowWire {
            seq_start: self.seq_start,
            seq_end: self.seq_end,
            source_seq_ranges: Some(seq_ranges(&self.seqs)),
            seqs: Some(self.seqs.clone()),
            updated: Some(self.updated),
            timestamp: self.timestamp,
            turn_id: self.turn_id,
            item: self.item.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Row {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = RowWire::deserialize(d)?;
        let seqs = match (w.seqs, w.source_seq_ranges) {
            (Some(seqs), _) => seqs,
            (None, Some(ranges)) => {
                let mut seqs = Vec::new();
                for r in ranges {
                    if r.end_seq < r.start_seq || r.end_seq - r.start_seq > 10_000 {
                        return Err(serde::de::Error::custom("bad sourceSeqRanges entry"));
                    }
                    seqs.extend(r.start_seq..=r.end_seq);
                }
                seqs
            }
            (None, None) => vec![w.seq_start],
        };
        Ok(Row {
            updated: w.updated.unwrap_or(w.seq_end != w.seq_start),
            seq_start: w.seq_start,
            seq_end: w.seq_end,
            seqs,
            timestamp: w.timestamp,
            turn_id: w.turn_id,
            item: w.item,
        })
    }
}

/// What a row shows. Readers skip [`RowItem::Unknown`]; magpie ships a new kind only after pigeon handles it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)] // a wire type: rows are built once and serialised, never moved in bulk
pub enum RowItem {
    /// A person's message (or an imported prompt).
    User(UserItem),
    /// A text block of a reply.
    Assistant(AssistantItem),
    /// A thinking block.
    Reasoning(ReasoningItem),
    /// A tool call, with its result once it lands.
    Tool(ToolItem),
    /// A failed turn's error.
    Error(ErrorItem),
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// Every row kind this build knows.
pub const ROW_KINDS: &[&str] = &["user", "assistant", "reasoning", "tool", "error"];

impl RowItem {
    /// The kind string, `None` for [`RowItem::Unknown`].
    #[must_use]
    pub fn kind(&self) -> Option<&'static str> {
        Some(match self {
            RowItem::User(_) => "user",
            RowItem::Assistant(_) => "assistant",
            RowItem::Reasoning(_) => "reasoning",
            RowItem::Tool(_) => "tool",
            RowItem::Error(_) => "error",
            RowItem::Unknown => return None,
        })
    }
}

/// `user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserItem {
    /// `clientMessageId` for a send, the line's `uuid` for an imported prompt.
    pub message_id: String,
    /// The app's message id (sends only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    /// The text.
    pub text: String,
    /// Images by blob id (pigeon signs `display`/`thumb` URLs).
    #[serde(default)]
    pub images: Vec<RowImage>,
    /// Attached files by workspace path.
    #[serde(default)]
    pub files: Vec<RowFile>,
    /// The admin who sent it while viewing as (not shown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
}

/// An image of a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowImage {
    /// The original's blob.
    pub blob_id: BlobId,
    /// MIME type.
    pub mime_type: String,
    /// Width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// A file of a user row (relative to `/workspace`: never absolute, no `..`, no NUL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowFile {
    /// Workspace path.
    pub path: String,
    /// File name.
    pub name: String,
    /// MIME type (may be empty for imported attachments).
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
    /// The file is no longer available (imported attachments only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<bool>,
}

/// `assistant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantItem {
    /// Claude Code's `message.id`.
    pub message_id: String,
    /// The block's index within the message.
    pub block_index: u32,
    /// The text (never empty).
    pub text: String,
    /// A model error tag (`model.*`) when the text is Claude Code's `API Error: …` line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// `reasoning`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningItem {
    /// Claude Code's `message.id`.
    pub message_id: String,
    /// The block's index within the message.
    pub block_index: u32,
    /// The thinking text.
    pub text: String,
}

/// A tool row's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    /// No result yet (or a background launch).
    Running,
    /// Result landed.
    Completed,
    /// `is_error`, or the turn failed.
    Failed,
    /// The turn was canceled, or the task stopped.
    Canceled,
}

/// `tool`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolItem {
    /// The `tool_use` id.
    pub call_id: String,
    /// Claude Code's `message.id`.
    pub message_id: String,
    /// The block's index within the message.
    pub block_index: u32,
    /// The tool's name.
    pub name: String,
    /// The tool input, as Claude Code wrote it (its own keys).
    pub input: Value,
    /// Status.
    pub status: ToolStatus,
    /// The result, once it lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResult>,
    /// `SendUserFile`'s snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_files: Option<Vec<RowSentFile>>,
    /// A background task launched by the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskInfo>,
    /// Why it failed (a failed turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// When the person answered the question this call asked (`AskUserQuestion`, `ExitPlanMode`): its
    /// `ask.resolved`. The time from the call to this is waiting, not working (DECISIONS 17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<crate::Timestamp>,
}

/// A tool result's preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// The first 4,000 characters.
    pub text: String,
    /// The full text is longer.
    pub truncated: bool,
    /// Bytes of the full text.
    pub bytes: u64,
    /// The full text, when it was extracted to a blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<BlobId>,
    /// `is_error` of the result.
    pub is_error: bool,
    /// Image blocks of the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ResultImage>>,
}

/// An image block of a tool result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultImage {
    /// The blob.
    pub blob_id: BlobId,
    /// MIME type.
    pub mime_type: String,
}

/// A sent file on a tool row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSentFile {
    /// Relative to `/workspace`.
    pub path: String,
    /// File name.
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// MIME type.
    pub mime_type: String,
    /// Shown as an image.
    pub is_image: bool,
}

/// A background task on its launching tool row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    /// Claude Code's task id.
    pub task_id: String,
    /// `running`, `completed`, `failed`, `stopped`.
    pub status: crate::events::TaskState,
    /// The summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// `error`: a failed turn's row (codes in magpie 8.9; pigeon localises by code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorItem {
    /// The dotted data code.
    pub code: crate::errors::DataCode,
    /// English, for logs.
    pub message: String,
    /// Extra detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// M9 query: exactly one of `tail=1`, `before`, `after`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    /// The last `limit` rows by `seqStart`.
    #[serde(default, with = "crate::flag01", skip_serializing_if = "crate::flag01::is_false")]
    pub tail: bool,
    /// Rows with `seqStart < before`, the last `limit` of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    /// One entry per row seq `s > after`, ascending, up to `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// 1-500, default 40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// `after` only: hold until something is past `after` or live frames change, up to 25,000 ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// `after` only, with `liveTurn`: send live text added after frame `n` of turn `liveTurn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_after: Option<u64>,
    /// `after` only, with `liveAfter`: the turn `liveAfter` counts in (the last `LiveState.turnId` seen). Frames are
    /// numbered per turn, so a `liveAfter` of another turn than the active one answers `reset: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_turn: Option<Uuid>,
}

/// M9 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    /// Decimal string; changes only when a retract removes or rebuilds a visible row.
    pub epoch: String,
    /// The smallest `seqStart` among the conversation's rows.
    pub min_seq: u64,
    /// `row_head` (`maxSeq + 1` is the next live seq).
    pub max_seq: u64,
    /// Rows exist before the page.
    pub has_older: bool,
    /// Rows exist after the page.
    pub has_newer: bool,
    /// The page.
    pub rows: Vec<Row>,
    /// The running turn's live text.
    pub live: Option<LiveState>,
}

/// M10 response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsResponse {
    /// The epoch.
    pub epoch: String,
    /// Every prompt row.
    pub prompts: Vec<Prompt>,
}

/// A prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// The user row's `seqStart`.
    pub seq: u64,
    /// When.
    pub timestamp: Timestamp,
    /// The first 200 characters.
    pub preview: String,
}

/// `GET /v1/conversations/{id}/search` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    /// Case-insensitive substring.
    pub q: String,
    /// From `nextCursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Default 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `GET /v1/conversations/{id}/transcript` query: the raw view's page of the main transcript (view-as only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptQuery {
    /// Lines after this `ord` (from `next`); absent: from the first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// Default 200, at most 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// A page of the main transcript's lines, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPage {
    /// The main transcript's path (`projects/-workspace/<sid>.jsonl`); `None` when the conversation has none yet.
    pub path: Option<String>,
    /// The lines, each reconstructed (long text put back; base64 media replaced by `[media <mime> <size> bytes]`).
    pub lines: Vec<TranscriptLine>,
    /// The `ord` to ask `after` next (the last line's, or the query's `after` when no line came).
    pub next: Option<i64>,
    /// More lines are stored past this page.
    pub more: bool,
    /// A turn is running.
    pub running: bool,
    /// The conversation's owner (the admin route only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// The conversation's title, `None` when untitled (the admin route only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One transcript line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLine {
    /// Its order in the file.
    pub ord: i64,
    /// The line without its final newline (a JSON object, as Claude Code wrote it).
    pub line: String,
}

/// Search response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// The epoch.
    pub epoch: String,
    /// Matching rows.
    pub locations: Vec<SearchLocation>,
    /// The next page.
    pub next_cursor: Option<String>,
}

/// A search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchLocation {
    /// The row's `seqStart`.
    pub seq: u64,
    /// The row's kind.
    pub kind: String,
    /// Matches in the row.
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_group_contiguous_seqs() {
        assert_eq!(
            seq_ranges(&[10, 11, 15]),
            vec![
                SeqRange {
                    start_seq: 10,
                    end_seq: 11
                },
                SeqRange {
                    start_seq: 15,
                    end_seq: 15
                }
            ]
        );
        assert_eq!(seq_ranges(&[]), Vec::new());
    }

    #[test]
    fn a_row_with_only_ranges_reads_its_seqs() {
        let row: Row = serde_json::from_value(serde_json::json!({
            "seqStart": 10, "seqEnd": 15, "sourceSeqRanges": [{"startSeq": 10, "endSeq": 10},
            {"startSeq": 15, "endSeq": 15}], "timestamp": "2026-11-02T03:46:12.345Z",
            "item": {"kind": "error", "code": "run.oom", "message": "oom"}}))
        .unwrap();
        assert_eq!(row.seqs, vec![10, 15]);
        assert!(row.updated);
    }
}
