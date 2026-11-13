//! The Conversation object and the conversation routes: list (M1), details (M2), create (M3), creation lookup (M4),
//! rename (M5), delete/restore/purge (M6).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::Timestamp;
use crate::errors::ErrorInfo;
use crate::turns::{SendRequest, TurnOrigin, TurnOutcome, TurnStarted};

/// Rename limit, characters after trimming (magpie 17).
pub const RENAME_MAX_CHARS: usize = 200;
/// Generated titles are cut to this many characters.
pub const TITLE_MAX_CHARS: usize = 80;
/// List page: at most, and the default (magpie 17).
pub const LIST_MAX: u32 = 200;

/// The Conversation object (magpie 9.1, pigeon 14.1.0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// A `UUIDv4`.
    pub id: Uuid,
    /// Null means untitled (pigeon shows its localised placeholder).
    pub title: Option<String>,
    /// Null when untitled.
    pub title_source: Option<TitleSource>,
    /// Created.
    pub created_at: Timestamp,
    /// Last activity.
    pub updated_at: Timestamp,
    /// The last time the person sent something.
    pub last_user_message_at: Option<Timestamp>,
    /// Set while in Recently deleted.
    pub deleted_at: Option<Timestamp>,
    /// `deletedAt + 30 days`.
    pub purge_at: Option<Timestamp>,
    /// Claude Code's session id.
    pub claude_session_id: Option<String>,
    /// Non-null while a turn is admitted, preparing, starting, running or stopping.
    pub turn: Option<TurnRef>,
    /// The most recent ended turn.
    pub last_turn: Option<LastTurn>,
    /// `preparing` while acquisition shows the chip, `ready` once ready during that turn, null otherwise.
    pub sandbox: Option<SandboxPhase>,
    /// The open asks (at most one today).
    #[serde(default)]
    pub asks: Vec<Ask>,
    /// Run state and background tasks.
    pub run: RunInfo,
    /// The model of the last assistant line.
    pub model: Option<String>,
    /// The model the person picked for the conversation (M5), which its turns run on from the next one; absent while
    /// none was picked (the deployment's default model runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked_model: Option<String>,
    /// The thinking level the person picked (M5): `none` or one of the picked model's levels; absent while none was
    /// picked (the model's own level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked_thinking: Option<String>,
    /// Set while the deployment has closed the conversation to the person's messages (hermit's route: a session
    /// that is paused elsewhere): the sentence the app shows in place of its message box. M7 answers 409
    /// `conversation_closed` meanwhile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
    /// The person's messages sent while a turn ran that have not reached Claude Code's conversation yet (M7
    /// `midTurn`), oldest first: the queue the app shows. One leaves it when it shows on the timeline (taken into
    /// the running turn, or started as a turn of its own) or is taken back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_messages: Vec<PendingMessage>,
    /// Context tokens of the last assistant line.
    pub context_tokens: Option<u64>,
    /// The context window Claude Code runs the conversation's model at (the last turn end's `usage.contextWindow`):
    /// what `context_tokens` is a share of. Absent until a turn has ended under a supervisor that reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// How the conversation came to be.
    pub origin: Origin,
}

/// A message sent while a turn ran, still waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMessage {
    /// The app's message id (M7's `clientMessageId`).
    pub client_message_id: String,
    /// The text.
    pub text: String,
    /// How many images and files it carries.
    pub attachments: u32,
    /// When the person sent it.
    pub sent_at: Timestamp,
    /// It waits for the running turn to end and then starts a turn of its own (it has attachments, or the turn
    /// could not take it), so it can still be taken back (`DELETE …/pending/{clientMessageId}`). False: it is on
    /// its way into the running turn.
    pub held: bool,
}

/// `titleSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitleSource {
    /// From the first message, or Claude Code's own title record.
    Provisional,
    /// From the title model.
    Generated,
    /// A rename.
    User,
}

/// The running turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRef {
    /// The turn.
    pub id: Uuid,
    /// Admitted at.
    pub started_at: Timestamp,
    /// `person` or `agent`.
    pub origin: TurnOrigin,
    /// How long the turn's answered questions waited for the person (DECISIONS 17): working time is
    /// `now - startedAt - askWaitMs` while no question is open, and stands still while one is.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ask_wait_ms: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The most recent ended turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastTurn {
    /// The turn.
    pub id: Uuid,
    /// How it ended.
    pub outcome: TurnOutcome,
    /// When.
    pub ended_at: Timestamp,
    /// Set when failed.
    pub error: Option<ErrorInfo>,
}

/// The sandbox chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxPhase {
    /// Acquisition has taken more than 1 s.
    Preparing,
    /// Ready during this turn.
    Ready,
}

/// An open ask (`AskUserQuestion` and similar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// The ask id.
    pub id: String,
    /// The tool.
    pub tool_name: String,
    /// The tool call.
    pub tool_use_id: Option<String>,
    /// The tool input (Claude Code's own keys).
    pub input: Map<String, Value>,
    /// Optional title for the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional description for the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Opened at.
    pub created_at: Timestamp,
}

/// `run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    /// The conversation's run state (magpie 8.1).
    pub state: RunState,
    /// Background tasks the supervisor reported after the turn ended (`held`).
    #[serde(default)]
    pub background: Vec<BackgroundTask>,
}

/// The conversation's run state (magpie 8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    /// A turn was admitted.
    Admitted,
    /// The sandbox is being prepared.
    Preparing,
    /// A turn is starting or running.
    Running,
    /// A cancel is in progress.
    Stopping,
    /// Nothing runs.
    Idle,
    /// Background tasks outlive the turn.
    Held,
}

/// A background task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    /// Claude Code's task id.
    pub task_id: String,
    /// The tool call that launched it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Claude Code's task type (`local_bash`, …).
    #[serde(rename = "type")]
    pub kind: String,
    /// Its description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Started at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
}

/// `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    /// How.
    pub kind: OriginKind,
    /// `continue`: the share it was continued from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_id: Option<String>,
}

/// `origin.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OriginKind {
    /// Created by a person.
    New,
    /// Continued from a share.
    Continue,
    /// Imported at the switch.
    Import,
}

/// Which list (M1 `state`, admin list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ListState {
    /// Not deleted, by `updatedAt` desc.
    #[default]
    Active,
    /// Recently deleted, by `deletedAt` desc.
    Deleted,
}

/// M1 query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// `active` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ListState>,
    /// 1-200, default 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque, from `nextCursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Deleted only: case-insensitive title substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
}

/// M1 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResponse {
    /// The page.
    pub conversations: Vec<Conversation>,
    /// The next page, or null.
    pub next_cursor: Option<String>,
    /// The person's change feed position, read in the same snapshot.
    pub list: ListPosition,
}

/// `list` of M1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPosition {
    /// The person's change generation (never changes).
    pub generation: String,
    /// The person's change seq, read in the same snapshot as the page: the change feed from it (M12 `after`) holds
    /// exactly the changes the page does not show.
    pub head_seq: u64,
}

/// M3 `POST /v1/conversations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    /// Unique per person: a repeat returns the same conversation and turn.
    pub idempotency_key: String,
    /// The first message; admitted when its text or attachments are non-empty.
    pub message: SendRequest,
    /// The model picked in the draft (`pickedModel` from the start; the caller has checked it); absent: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The thinking level picked in the draft (`pickedThinking` from the start); absent: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// M3 → 201 (200 for a repeat), and M4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResponse {
    /// The conversation.
    pub conversation: Conversation,
    /// The first turn, if the message was admitted.
    pub turn: Option<TurnStarted>,
}

/// M5 `PATCH /v1/conversations/{id}`: a rename, a model pick, a thinking level pick, or several (400 `bad_request`
/// with none).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameRequest {
    /// The new title: 1-200 characters after trimming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The model the conversation's turns run on from the next one (`pickedModel`): an id of the deployment's model
    /// list, which the caller has checked (pigeon, against the models route); 1-200 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The thinking level of the conversation's turns from the next one (`pickedThinking`): a level of its model,
    /// which the caller has checked; at most 200 characters. `""`: none picked again (the model's own level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

impl RenameRequest {
    /// A rename.
    #[must_use]
    pub fn title(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::default()
        }
    }

    /// A model pick.
    #[must_use]
    pub fn model(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            ..Self::default()
        }
    }

    /// A thinking level pick (`""`: none picked again).
    #[must_use]
    pub fn thinking(level: impl Into<String>) -> Self {
        Self {
            thinking: Some(level.into()),
            ..Self::default()
        }
    }
}

/// Model id limit of a pick, characters.
pub const MODEL_MAX_CHARS: usize = 200;
