//! View-as (M17), the hermit routes (magpie 16.3), the ops reads (16.4, platform 15.3) and the admin reads (13).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;
use crate::conversations::{Conversation, ListState};
use crate::sandbox::{CcFacts, SupervisorFacts};

/// M17 `POST /v1/view-as` → 201 `{}`. pigeon opens the view only after the 201.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewAsRequest {
    /// The admin's email.
    pub admin: String,
    /// The person viewed.
    pub person: String,
    /// The view session (at most 64 characters).
    pub session: String,
    /// When the view ends.
    pub expires_at: Timestamp,
}

/// Why a person is purged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PurgeReason {
    /// The person left (an operator's purge through hermit).
    Offboarded,
}

/// `POST /v1/hermit/persons/{person}/purge` → 202; 409 `person_allowed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonPurgeRequest {
    /// Why.
    pub reason: PurgeReason,
}

/// Person purge response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonPurgeResponse {
    /// Conversations purged.
    pub conversations: u64,
}

/// `POST /v1/hermit/conversations` → 201 (200 for a repeat): M3 for `person`, made by the deployment itself (a
/// deployment whose people start no conversations, hermit's `conversations.peopleCreate` false). Idempotent for ever
/// by (person, `idempotencyKey`), as M3. The answer is M3's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermitCreateRequest {
    /// The conversation's person.
    pub person: String,
    /// Unique per person: a repeat returns the same conversation and turn.
    pub idempotency_key: String,
    /// The first message; admitted when its text or attachments are non-empty.
    pub message: crate::turns::SendRequest,
    /// The conversation's model from the start (its `pickedModel`, as if the person had picked it): the model the
    /// deployment started the session on. Kept on a repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// `POST /v1/hermit/conversations/{id}/turns` → 202 (200 for a repeat): M7 as the conversation's person, sent by the
/// deployment (its scheduled prompts, messages from outside the app). 404 when the conversation is not `person`'s;
/// 409 `turn_running` while a turn runs, as M7. The answer is M7's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermitSendRequest {
    /// The conversation's person.
    pub person: String,
    /// The turn's message.
    pub message: crate::turns::SendRequest,
}

/// At most this many ids per `workspaces/exists` call.
pub const WORKSPACES_EXISTS_MAX: usize = 1_000;

/// `POST /v1/hermit/workspaces/exists` (the subset magpie still references).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacesExistsRequest {
    /// Workspace ids (<= 1,000).
    pub ids: Vec<Uuid>,
}

/// `workspaces/exists` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacesExistsResponse {
    /// Conversation ids of non-purged conversations and `workspaceId`s of live shares, among `ids`.
    pub exists: Vec<Uuid>,
}

/// `GET /v1/hermit/legacy-import`: whether the switch's import is complete (cutover 10.6, hermit 22.3 step 6), from
/// the record a clean `magpie import verify --all` leaves once every imported conversation is owned by magpie. hermit's
/// `legacy-cleanup` removes the old `users/` and `sent/` trees only when `complete`. Deleted with `legacy-cleanup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyImportStatus {
    /// The record holds, and nothing the import did since makes it stale.
    pub complete: bool,
    /// When the last clean `verify --all` with every conversation owned ran (`None`: never).
    pub verified_at: Option<Timestamp>,
    /// How many imported conversations it verified.
    pub conversations: u64,
    /// Why it is not complete (`None` when it is).
    pub why: Option<String>,
}

/// `GET /v1/ops/people` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleQuery {
    /// IANA zone for `today` (an invalid zone falls back to UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

/// `GET /v1/ops/people` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleResponse {
    /// The zone used.
    pub tz: String,
    /// One per person.
    pub people: Vec<PersonStats>,
}

/// A person's counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonStats {
    /// The person's email.
    pub owner: String,
    /// Conversations, not purged.
    pub total: u64,
    /// Active today (from the zone's midnight).
    pub today: u64,
    /// Active in the last 7 days.
    pub last7d: u64,
    /// With a turn in progress.
    pub live: u64,
    /// In Recently deleted.
    pub deleted: u64,
    /// Purged so far.
    pub purged: u64,
    /// Last activity.
    pub last_active_at: Option<Timestamp>,
}

/// `GET /v1/ops/view-as` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewAsLogQuery {
    /// Default 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Entries older than the entry with this id (the last `id` of the previous page; magpie 13 `before=<id>`).
    /// Entries are ordered newest first by `(at, id)`, so a page never skips entries that share a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
}

/// `GET /v1/ops/view-as` response, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewAsLog {
    /// The entries.
    pub view_as: Vec<ViewAsEntry>,
}

/// What a view-as log entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewAsEvent {
    /// The view started (M17).
    Start,
    /// A conversation created while viewing.
    Create,
    /// A message sent while viewing.
    Send,
    /// A cancel while viewing.
    Cancel,
    /// An upload while viewing.
    Upload,
    /// An admin's first read of a conversation that day.
    AdminRead,
}

/// A view-as log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewAsEntry {
    /// The entry's id: the next page's `before`.
    pub id: u64,
    /// When.
    pub at: Timestamp,
    /// The conversation (null for `start`).
    pub conversation_id: Option<Uuid>,
    /// What.
    pub event: ViewAsEvent,
    /// The admin's email.
    pub admin: String,
    /// The person's email.
    pub user: String,
    /// The view session.
    pub session: Option<String>,
    /// `send`: characters of the text (never the text); `upload`: bytes.
    pub chars: Option<u64>,
}

/// `GET /v1/ops/stats`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsStats {
    /// Not purged.
    pub conversations: u64,
    /// In Recently deleted.
    pub deleted: u64,
    /// Events (Postgres's estimate, `pg_class.reltuples`).
    pub events: u64,
    /// Timeline rows (Postgres's estimate).
    pub rows: u64,
    /// Claude Code state bytes.
    pub cc_bytes: u64,
    /// Blobs.
    pub blobs: u64,
    /// Blob bytes.
    pub blob_bytes: u64,
    /// Blobs waiting for the bucket.
    pub staged_blobs: u64,
    /// Their bytes.
    pub staged_bytes: u64,
    /// hermit calls waiting.
    pub outbox_pending: u64,
    /// The oldest of them.
    pub oldest_outbox_at: Option<Timestamp>,
    /// The oldest conversation due for purge.
    pub oldest_due_purge_at: Option<Timestamp>,
    /// How far magpie's copy of hermit's sandbox feed lags.
    pub hermit_feed_lag_seconds: Option<f64>,
}

/// `GET /v1/admin/conversations` query (`Aviary-Admin` required).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminListQuery {
    /// The person's email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// Which list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ListState>,
}

/// `GET /v1/admin/conversations` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminConversationList {
    /// The conversations.
    pub conversations: Vec<AdminConversation>,
}

/// A conversation as an admin sees it: the Conversation plus the operational fields (magpie 13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminConversation {
    /// The Conversation object.
    #[serde(flatten)]
    pub conversation: Conversation,
    /// The owner's email.
    pub person: String,
    /// The supervisor's lease.
    pub lease: Option<AdminLease>,
    /// The last heartbeat.
    pub heartbeat: Option<AdminHeartbeat>,
    /// hermit's sandbox for the conversation, from its feed.
    pub hermit_sandbox: Option<String>,
}

/// The lease as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLease {
    /// The lease.
    pub lease_id: Uuid,
    /// The lease epoch.
    pub epoch: u64,
    /// The sandbox holding it.
    pub sandbox_id: String,
    /// Granted at.
    pub granted_at: Timestamp,
}

/// The last heartbeat as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminHeartbeat {
    /// When.
    pub at: Timestamp,
    /// Claude Code generation.
    #[serde(rename = "gen")]
    pub generation: u64,
    /// The supervisor.
    pub supervisor: SupervisorFacts,
    /// The FUSE layer.
    pub cc: CcFacts,
}
