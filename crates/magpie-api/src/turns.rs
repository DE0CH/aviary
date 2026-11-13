//! Turns from pigeon's side: send (M7), cancel (M8), ask answers (M11), and the turn enums shared by every surface.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::Timestamp;

/// Who started a turn (platform R30, DESIGN 12 D4: one enum for magpie, the supervisor and pigeon).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnOrigin {
    /// A person's send through pigeon.
    Person,
    /// A turn Claude Code started by itself (task completion, Monitor events, wake-ups).
    Agent,
    /// An old-system turn written by the importer.
    Import,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnOutcome {
    /// The reply finished.
    Completed,
    /// Failed, with an error code.
    Failed,
    /// Stopped by the person (or a forced stop).
    Canceled,
}

/// A turn's phase in the change feed (`turn` change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnPhase {
    /// `turn.started`.
    Started,
    /// Ended completed.
    Completed,
    /// Ended failed.
    Failed,
    /// Ended canceled.
    Canceled,
}

/// An image attached to a send: an upload M13 returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    /// The upload.
    pub upload_id: Uuid,
}

/// A file attached to a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRef {
    /// The opaque `uploads/<uploadId>/<name>` path M13 returned (or, until A7, an old asset path, magpie 8.2).
    pub path: String,
    /// The file name.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
}

/// M7 `POST /v1/conversations/{id}/turns`, and the `message` of M3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    /// The app's message id; a repeat returns the same turn (idempotent for ever).
    pub client_message_id: String,
    /// The text (at most 1 MiB UTF-8).
    #[serde(default)]
    pub text: String,
    /// At most 10.
    #[serde(default)]
    pub images: Vec<ImageRef>,
    /// At most 10.
    #[serde(default)]
    pub files: Vec<FileRef>,
}

/// Turn text cap, bytes (magpie 17).
pub const TURN_TEXT_MAX_BYTES: usize = 1 << 20;

/// The admitted turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStarted {
    /// The turn.
    pub id: Uuid,
    /// When it was admitted.
    pub started_at: Timestamp,
}

/// M7 → 202 (200 for a repeated `clientMessageId`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResponse {
    /// The turn: the one the message started, or with `midTurn` the running one it joined.
    pub turn: TurnStarted,
    /// The message was sent while a turn ran: it started none and waits in the conversation's `pendingMessages`. It
    /// is written into the running turn (its files into the workspace first), where Claude Code takes it in at its
    /// next tool result or when the model stops (its `user` row, with this `clientMessageId` as its `messageId`,
    /// comes then); if that turn ends first or cannot take it, magpie starts it as a turn of its own once the turn
    /// has ended.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mid_turn: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_false(b: &bool) -> bool {
    !*b
}

/// M8 `POST /v1/conversations/{id}/cancel` → 202. Never an error for "nothing running".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    /// The turn being stopped, or null (a hold's `stop_tasks`, or nothing running).
    pub turn_id: Option<Uuid>,
}

/// `allow` or `deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Behavior {
    /// Allow the tool call.
    Allow,
    /// Deny it.
    Deny,
}

/// Paseo's `AgentPermissionResponse`, carried as is (M11 body, `ask.resolved`, `ask_resolved`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPermissionResponse {
    /// The decision.
    pub behavior: Behavior,
    /// The action the person picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_action_id: Option<String>,
    /// `allow`: the tool input to use instead (Claude Code's own keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Map<String, Value>>,
    /// `allow`: permission updates (opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_permissions: Option<Vec<Value>>,
    /// `deny`: the message to the model (`"canceled"` for a cancel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `deny`: interrupt the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<bool>,
}

impl AgentPermissionResponse {
    /// The resolution of a canceled ask: `{"behavior": "deny", "message": "canceled"}` (magpie 7.2).
    #[must_use]
    pub fn canceled() -> Self {
        AgentPermissionResponse {
            behavior: Behavior::Deny,
            selected_action_id: None,
            updated_input: None,
            updated_permissions: None,
            message: Some("canceled".to_owned()),
            interrupt: None,
        }
    }
}
