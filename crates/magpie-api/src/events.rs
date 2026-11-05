//! The event log (magpie 7.1-7.2): immutable, `seq` contiguous per conversation from 1. pigeon reads rows, not
//! events; events are read by the admin routes, the projection and the importer's verification.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::Empty;
use crate::Timestamp;
use crate::conversations::Origin;
use crate::errors::ErrorInfo;
use crate::rows::RowFile;
use crate::turns::{AgentPermissionResponse, TurnOrigin, TurnOutcome};
use crate::uploads::{AttachedImage, Attachment, SentFile};

/// One event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Contiguous per conversation, from 1.
    pub seq: u64,
    /// `kind` and `data`.
    #[serde(flatten)]
    pub body: EventBody,
    /// When it was appended.
    pub at: Timestamp,
    /// The turn it belongs to.
    pub turn_id: Option<Uuid>,
    /// Derived facts (all optional).
    #[serde(default)]
    pub meta: EventMeta,
    /// The admin who sent it while viewing as the person.
    #[serde(default)]
    pub viewer: Option<String>,
}

/// `kind` and `data` of an event. Readers skip [`EventBody::Unknown`] kinds; a new kind ships only after its
/// projection rule exists (magpie 7.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "data")]
pub enum EventBody {
    /// The conversation was created.
    #[serde(rename = "conversation.created")]
    ConversationCreated(ConversationCreated),
    /// pigeon's send.
    #[serde(rename = "user.sent")]
    UserSent(UserSent),
    /// A turn started.
    #[serde(rename = "turn.started")]
    TurnStarted(TurnStartedData),
    /// The driver has waited 1 s for the sandbox.
    #[serde(rename = "sandbox.preparing")]
    SandboxPreparing(Empty),
    /// The sandbox is ready.
    #[serde(rename = "sandbox.ready")]
    SandboxReady(Empty),
    /// A cancel was requested.
    #[serde(rename = "turn.stop_requested")]
    TurnStopRequested(TurnStopRequested),
    /// A turn ended.
    #[serde(rename = "turn.ended")]
    TurnEnded(TurnEnded),
    /// The title changed.
    #[serde(rename = "title.changed")]
    TitleChanged(TitleChanged),
    /// A sent-file snapshot.
    #[serde(rename = "files.sent")]
    FilesSent(FilesSent),
    /// A background task's status.
    #[serde(rename = "task.status")]
    TaskStatus(TaskStatus),
    /// An ask opened.
    #[serde(rename = "ask.pending")]
    AskPending(AskPending),
    /// An ask was answered or canceled.
    #[serde(rename = "ask.resolved")]
    AskResolved(AskResolved),
    /// Earlier events left the main transcript (magpie 6.7).
    #[serde(rename = "retract")]
    Retract(Retract),
    /// A user line of the main transcript, as Claude Code wrote it (its own keys), blobs as references.
    #[serde(rename = "cc.user")]
    CcUser(Value),
    /// An assistant line of the main transcript.
    #[serde(rename = "cc.assistant")]
    CcAssistant(Value),
    /// A kind this build does not know (its data is dropped; it cannot be written back).
    #[serde(skip_serializing)]
    Unknown,
}

impl<'de> Deserialize<'de> for EventBody {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            kind: String,
            #[serde(default)]
            data: Value,
        }
        fn data<T: serde::de::DeserializeOwned, E: serde::de::Error>(v: Value) -> Result<T, E> {
            serde_json::from_value(v).map_err(E::custom)
        }
        let raw = Raw::deserialize(d)?;
        Ok(match raw.kind.as_str() {
            "conversation.created" => EventBody::ConversationCreated(data(raw.data)?),
            "user.sent" => EventBody::UserSent(data(raw.data)?),
            "turn.started" => EventBody::TurnStarted(data(raw.data)?),
            "sandbox.preparing" => EventBody::SandboxPreparing(data(raw.data)?),
            "sandbox.ready" => EventBody::SandboxReady(data(raw.data)?),
            "turn.stop_requested" => EventBody::TurnStopRequested(data(raw.data)?),
            "turn.ended" => EventBody::TurnEnded(data(raw.data)?),
            "title.changed" => EventBody::TitleChanged(data(raw.data)?),
            "files.sent" => EventBody::FilesSent(data(raw.data)?),
            "task.status" => EventBody::TaskStatus(data(raw.data)?),
            "ask.pending" => EventBody::AskPending(data(raw.data)?),
            "ask.resolved" => EventBody::AskResolved(data(raw.data)?),
            "retract" => EventBody::Retract(data(raw.data)?),
            "cc.user" => EventBody::CcUser(raw.data),
            "cc.assistant" => EventBody::CcAssistant(raw.data),
            _ => EventBody::Unknown,
        })
    }
}

impl EventBody {
    /// The kind string, `None` for [`EventBody::Unknown`].
    #[must_use]
    pub fn kind(&self) -> Option<&'static str> {
        Some(match self {
            EventBody::ConversationCreated(_) => "conversation.created",
            EventBody::UserSent(_) => "user.sent",
            EventBody::TurnStarted(_) => "turn.started",
            EventBody::SandboxPreparing(_) => "sandbox.preparing",
            EventBody::SandboxReady(_) => "sandbox.ready",
            EventBody::TurnStopRequested(_) => "turn.stop_requested",
            EventBody::TurnEnded(_) => "turn.ended",
            EventBody::TitleChanged(_) => "title.changed",
            EventBody::FilesSent(_) => "files.sent",
            EventBody::TaskStatus(_) => "task.status",
            EventBody::AskPending(_) => "ask.pending",
            EventBody::AskResolved(_) => "ask.resolved",
            EventBody::Retract(_) => "retract",
            EventBody::CcUser(_) => "cc.user",
            EventBody::CcAssistant(_) => "cc.assistant",
            EventBody::Unknown => return None,
        })
    }
}

/// Every event kind this build knows.
pub const EVENT_KINDS: &[&str] = &[
    "conversation.created",
    "user.sent",
    "turn.started",
    "sandbox.preparing",
    "sandbox.ready",
    "turn.stop_requested",
    "turn.ended",
    "title.changed",
    "files.sent",
    "task.status",
    "ask.pending",
    "ask.resolved",
    "retract",
    "cc.user",
    "cc.assistant",
];

/// `conversation.created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCreated {
    /// The provisional title, or null.
    pub title: Option<String>,
    /// How it came to be.
    pub origin: Origin,
}

/// `user.sent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSent {
    /// The app's message id.
    pub client_message_id: String,
    /// The text.
    pub text: String,
    /// Attached files.
    #[serde(default)]
    pub files: Vec<Attachment>,
    /// Attached images.
    #[serde(default)]
    pub images: Vec<AttachedImage>,
}

/// `turn.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedData {
    /// The turn.
    pub turn_id: Uuid,
    /// Who started it.
    pub origin: TurnOrigin,
}

/// `turn.stop_requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStopRequested {
    /// The turn.
    pub turn_id: Uuid,
}

/// `turn.ended`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEnded {
    /// The turn.
    pub turn_id: Uuid,
    /// How it ended.
    pub outcome: TurnOutcome,
    /// Set when failed (and for `stop.forced`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// Where a title came from (`conversations.name_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NameSource {
    /// The title model.
    Title,
    /// A rename.
    User,
}

/// `title.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleChanged {
    /// The new title.
    pub title: String,
    /// Null for a provisional title (Claude Code's own title record).
    pub name_source: Option<NameSource>,
}

/// `files.sent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesSent {
    /// The `SendUserFile` call.
    pub tool_use_id: String,
    /// The event seq of the assistant line with that call, when already stored.
    pub tool_use_seq: Option<u64>,
    /// The caption.
    pub caption: Option<String>,
    /// The snapshot.
    pub files: Vec<SentFile>,
}

/// A task report's event (`task.status`, `POST /v1/sandbox/tasks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskEvent {
    /// Started.
    Started,
    /// Progress.
    Progress,
    /// Updated.
    Updated,
    /// Claude Code's task notification.
    Notification,
}

/// A task's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    /// Still running.
    Running,
    /// Finished.
    Completed,
    /// Failed.
    Failed,
    /// Stopped or killed.
    Stopped,
}

/// `task.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// Claude Code's task id.
    pub task_id: String,
    /// The launching tool call.
    pub tool_use_id: Option<String>,
    /// What happened.
    pub event: TaskEvent,
    /// The task's status.
    pub status: TaskState,
    /// Its description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Its output file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
}

/// `ask.pending`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskPending {
    /// The ask.
    pub ask_id: String,
    /// `AskUserQuestion` and similar.
    pub tool_name: String,
    /// The tool call.
    pub tool_use_id: Option<String>,
    /// The tool input (Claude Code's own keys).
    pub input: Map<String, Value>,
}

/// `ask.resolved`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskResolved {
    /// The ask.
    pub ask_id: String,
    /// The person's answer, or the cancel resolution.
    pub resolution: AgentPermissionResponse,
}

/// `retract`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retract {
    /// The retracted events.
    pub seqs: Vec<u64>,
}

/// `meta` of an event (magpie 7.2): every key optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMeta {
    /// A prompt line not sent through magpie (imported, or queued by Claude Code itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<bool>,
    /// A `<task-notification>` line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_notification: Option<TaskNotification>,
    /// The `tool_use` ids of an assistant line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<Vec<String>>,
    /// The tool results of a user line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<ToolResultLink>>,
    /// A model error tag (`model.*`) on an `API Error:` assistant line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// The seq of a retracted line with the same `uuid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<u64>,
    /// A sidechain line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<bool>,
    /// The person's answers to a parked turn's questions (DECISIONS 17): the line the resumed turn starts with is
    /// the result of the `results` calls (the questions), not a prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_answer: Option<bool>,
    /// Imported events only: the text shown instead of the line's (magpie 19.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// Imported events only: the attachments shown with the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<RowFile>>,
    /// A compaction's marker (Claude Code's `system` `compact_boundary` line): shown as one assistant row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Compaction>,
    /// Claude Code's compaction summary (an `isCompactSummary` user line): shown as an assistant row, not a prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_summary: Option<bool>,
}

/// `meta.compaction`: the facts of a `compact_boundary` line's `compactMetadata` (0 when absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Compaction {
    /// `manual` or `auto`.
    pub trigger: String,
    /// Context tokens before.
    pub pre_tokens: u64,
    /// Context tokens after.
    pub post_tokens: u64,
    /// How long it took.
    pub duration_ms: u64,
}

/// `meta.taskNotification`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNotification {
    /// The launching tool call.
    pub tool_use_id: String,
    /// Claude Code's task id (`<task-id>`), when the notification names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The notification's status (`completed`, `failed`, `killed`, `stopped`).
    pub status: String,
    /// The notification's summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// An entry of `meta.results`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultLink {
    /// The call.
    pub tool_use_id: String,
    /// The event seq of the assistant line with the call, when known.
    pub tool_use_seq: Option<u64>,
}

/// Admin `GET /v1/admin/conversations/{id}/events` query: exactly one of `before`, `after`, `tail`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// Events with `seq < before`, the last `limit` of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    /// Events with `seq > after`, the first `limit` of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// The last `limit` events.
    #[serde(default, with = "crate::flag01", skip_serializing_if = "crate::flag01::is_false")]
    pub tail: bool,
    /// 1-500, default 40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The events page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsPage {
    /// Ascending by seq.
    pub events: Vec<Event>,
    /// The conversation's last event seq.
    pub head_seq: u64,
    /// Events exist before the page.
    pub has_older: bool,
    /// Events exist after the page.
    pub has_newer: bool,
}
