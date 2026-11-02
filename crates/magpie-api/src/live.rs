//! Live frames (magpie 7.5, supervisor 8.3): partial assistant output of a running turn, never events, never a row
//! seq, superseded by the durable row with the same `(messageId, blockIndex)`. In this release the supervisor sends
//! none and pigeon renders none (platform R36, DESIGN 12 D3); magpie serves the receiving side.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;

/// A frame's operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameOp {
    /// A content block starts (`block`, `name` for `tool_use`).
    Start,
    /// Text added at `offset`.
    Delta,
    /// The block is complete.
    Stop,
    /// A status of the run (`api_retry` …).
    Status,
    /// Frames were dropped; the next poll answers `reset: true`.
    Gap,
}

/// The type of a live block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    /// Reply text.
    Text,
    /// Thinking.
    Thinking,
    /// A tool call (only its start, with the name).
    ToolUse,
}

/// One live frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveFrame {
    /// Per turn, contiguous from 1; `(turnId, n)` makes a re-sent batch idempotent.
    pub n: u64,
    /// Claude Code's `message.id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The block's index within the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// What the frame does.
    pub op: FrameOp,
    /// `start`: the block type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockType>,
    /// `start` of a `tool_use`: the tool's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `delta`: byte offset of `text` within the block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// `delta`: the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `start`: the sidechain's parent tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
    /// `status`: Claude Code's status (`api_retry` …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `status`: the retry attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// `status`: when the retry happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    /// When the supervisor read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
}

/// `POST /v1/sandbox/turns/{turnId}/live` → 204; 409 `turn_not_active` (drop, never retry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveFramesRequest {
    /// The batch.
    pub frames: Vec<LiveFrame>,
}

/// The live text of the running turn, in an M9 response and from `GET /v1/conversations/{id}/live`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    /// The running turn.
    pub turn_id: Uuid,
    /// The last frame included.
    pub n: u64,
    /// Every open block is sent whole from offset 0 (no `liveAfter`, a `liveTurn` that is not this turn, after a gap
    /// or a truncation).
    pub reset: bool,
    /// The open blocks.
    pub blocks: Vec<LiveBlock>,
    /// The run's status, if any.
    pub status: Option<LiveStatus>,
}

/// An in-progress block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBlock {
    /// Claude Code's `message.id`.
    pub message_id: String,
    /// The block's index within the message.
    pub block_index: u32,
    /// The block type.
    pub block: BlockType,
    /// `tool_use`: the tool's name.
    pub name: Option<String>,
    /// Where `text` starts within the block.
    pub offset: u64,
    /// The text from `offset`.
    pub text: String,
    /// The block has stopped (its durable row is about to land).
    pub done: bool,
    /// `row_head` when the block started: pigeon places the in-progress row after that row.
    pub after_row_seq: u64,
}

/// The run's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStatus {
    /// Claude Code's status (`api_retry` …).
    pub status: String,
    /// The retry attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// When the retry happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
}
