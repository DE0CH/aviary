//! The sandbox surface (magpie 16.2): the supervisor's calls through the gateway, which sets `Aviary-Sandbox-*`.
//! State changes and the command poll carry `Aviary-Lease` (magpie 6.9).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::Timestamp;
use crate::events::{TaskEvent, TaskState};
use crate::turns::{TurnOrigin, TurnOutcome};
use crate::uploads::BlobId;

/// cc write body cap, bytes (16 MiB; the supervisor splits larger writes).
pub const CC_WRITE_MAX_BYTES: usize = 16 << 20;
/// JSONL line cap (including a tail being built), bytes.
pub const CC_LINE_MAX_BYTES: u64 = 64 << 20;
/// Byte file cap (a `bytes` file; a `lines` file is bounded by the quota, magpie 17).
pub const CC_FILE_MAX_BYTES: u64 = 256 << 20;
/// Claude Code state per conversation (`ccBytes` quota).
pub const CC_QUOTA_BYTES: u64 = 2 << 30;
/// Sent files per send.
pub const SENT_FILES_MAX: usize = 100;
/// Sent file size cap.
pub const SENT_FILE_MAX_BYTES: u64 = 64 << 20;
/// Sent files per send, total.
pub const SENT_TOTAL_MAX_BYTES: u64 = 256 << 20;

/// `POST /v1/sandbox/lease` → 200 (body `{}`); 409 `not_current_sandbox` / `conversation_migrating`; 503.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseResponse {
    /// The lease, carried as `Aviary-Lease`; any earlier lease is dead.
    pub lease_id: Uuid,
    /// The conversation's lease epoch.
    pub epoch: u64,
    /// The sandbox's conversation.
    pub conversation_id: Uuid,
    /// Claude Code's session id to resume, if any.
    pub provider_session_id: Option<String>,
    /// The main transcript path, if known.
    pub main_transcript: Option<String>,
    /// Refetch the tree and re-send any suffix magpie lacks before starting Claude Code (magpie 6.12).
    pub resync: bool,
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CcKind {
    /// A `.jsonl` (or `.jsonl.`) file, stored as lines.
    Lines,
    /// Any other file, stored in pages.
    Bytes,
    /// A directory.
    Dir,
}

/// `GET /v1/sandbox/cc/tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcTree {
    /// Every path in the conversation's scope.
    pub files: Vec<CcEntry>,
    /// Bytes counted against the quota.
    pub cc_bytes: u64,
    /// The quota.
    pub quota_bytes: u64,
}

/// A path of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcEntry {
    /// Relative to `~/.claude`.
    pub path: String,
    /// Kind.
    pub kind: CcKind,
    /// Bytes.
    pub size: u64,
    /// The supervisor's mtime, nanoseconds since the epoch.
    pub mtime_ns: i64,
    /// Mode bits.
    pub mode: u32,
    /// Bumped by every operation on the path.
    pub version: u64,
}

/// `GET /v1/sandbox/cc/file` query; bytes (206 with a range).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcFileQuery {
    /// Relative to `~/.claude`.
    pub path: String,
    /// Start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
}

/// `POST /v1/sandbox/cc/write` query; the raw body (<= 16 MiB) is the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcWriteQuery {
    /// Relative to `~/.claude`.
    pub path: String,
    /// Where the bytes go (`size` for an append).
    pub offset: u64,
    /// The supervisor's mtime.
    pub mtime_ns: i64,
    /// Idempotency: a replay returns the stored result.
    pub op_id: Uuid,
    /// Create the file (`1`), with `mode`.
    #[serde(default, with = "crate::flag01")]
    pub create: bool,
    /// Mode bits for a create.
    pub mode: u32,
    /// The turn the supervisor was running when it read the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    /// An append (`1`): `offset` is the size the writer expects the file to have. After rule 0 (magpie 6.6), a file
    /// of any other size answers 409 `offset_mismatch`, so a stale retry under a new `opId` cannot write twice
    /// (magpie 6.10). Without it a write inside the file is an overwrite (`pwrite`).
    #[serde(default, with = "crate::flag01", skip_serializing_if = "crate::flag01::is_false")]
    pub append: bool,
}

/// `cc/write` and `cc/truncate` → 200.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSizeVersion {
    /// The file's size after the operation.
    pub size: u64,
    /// The path's version after the operation.
    pub version: u64,
}

/// `cc/rename` and `cc/setattr` → 200.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcVersion {
    /// The path's version after the operation.
    pub version: u64,
}

/// `POST /v1/sandbox/cc/truncate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcTruncate {
    /// Path.
    pub path: String,
    /// The new size.
    pub size: u64,
    /// The supervisor's mtime.
    pub mtime_ns: i64,
    /// Idempotency.
    pub op_id: Uuid,
}

/// `POST /v1/sandbox/cc/rename` (replaces `to`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcRename {
    /// From.
    pub from: String,
    /// To.
    pub to: String,
    /// Idempotency.
    pub op_id: Uuid,
}

/// `POST /v1/sandbox/cc/unlink` → 204; 403 `protected` for the main transcript. Also `cc/rmdir` → 204; 409
/// `not_empty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcPathOp {
    /// Path.
    pub path: String,
    /// Idempotency.
    pub op_id: Uuid,
}

/// `POST /v1/sandbox/cc/mkdir` → 204; 409 `exists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcMkdir {
    /// Path (the parent must exist).
    pub path: String,
    /// Mode bits.
    pub mode: u32,
    /// The supervisor's mtime.
    pub mtime_ns: i64,
    /// Idempotency.
    pub op_id: Uuid,
}

/// `POST /v1/sandbox/cc/setattr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSetattr {
    /// Path.
    pub path: String,
    /// New mode bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// New mtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ns: Option<i64>,
    /// Idempotency.
    pub op_id: Uuid,
}

/// `GET /v1/sandbox/commands` query (with `Aviary-Lease`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandsQuery {
    /// Commands with `cmdSeq > after`.
    pub after: u64,
    /// Hold up to 25,000 ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

/// `GET /v1/sandbox/commands` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandsResponse {
    /// Not canceled, `cmdSeq > after`, ascending.
    pub commands: Vec<Command>,
    /// The conversation's last command seq.
    pub head: u64,
}

/// One command of the supervisor's work queue (magpie 8.4). Idempotent per `cmdSeq` on the supervisor's side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    /// The command's seq.
    pub cmd_seq: u64,
    /// The turn it is for (`start_turn`, `interrupt`, `kill`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    /// `type` and `payload`.
    #[serde(flatten)]
    pub body: CommandBody,
}

/// `type` and `payload`. An unknown type is acked `{"result": {"error": "unsupported"}}` (supervisor 10.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum CommandBody {
    /// Run a turn.
    StartTurn(StartTurn),
    /// Generate the title.
    Title(TitleCommand),
    /// Interrupt the turn (a cancel).
    Interrupt(crate::Empty),
    /// Kill Claude Code.
    Kill(KillCommand),
    /// Answer an open ask.
    AnswerAsk(AnswerAsk),
    /// Stop every background task (a cancel while held).
    StopTasks(crate::Empty),
    /// A type this build does not know (its payload is dropped; it cannot be written back).
    #[serde(skip_serializing)]
    Unknown,
}

impl<'de> Deserialize<'de> for CommandBody {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            payload: Value,
        }
        fn payload<T: serde::de::DeserializeOwned, E: serde::de::Error>(v: Value) -> Result<T, E> {
            // a command without a payload reads as `{}`
            let v = if v.is_null() { Value::Object(Map::new()) } else { v };
            serde_json::from_value(v).map_err(E::custom)
        }
        let raw = Raw::deserialize(d)?;
        Ok(match raw.kind.as_str() {
            "start_turn" => CommandBody::StartTurn(payload(raw.payload)?),
            "title" => CommandBody::Title(payload(raw.payload)?),
            "interrupt" => CommandBody::Interrupt(payload(raw.payload)?),
            "kill" => CommandBody::Kill(payload(raw.payload)?),
            "answer_ask" => CommandBody::AnswerAsk(payload(raw.payload)?),
            "stop_tasks" => CommandBody::StopTasks(payload(raw.payload)?),
            _ => CommandBody::Unknown,
        })
    }
}

/// Every command type this build knows.
pub const COMMAND_TYPES: &[&str] = &["start_turn", "title", "interrupt", "kill", "answer_ask", "stop_tasks"];

impl CommandBody {
    /// The type string, `None` for [`CommandBody::Unknown`].
    #[must_use]
    pub fn type_name(&self) -> Option<&'static str> {
        Some(match self {
            CommandBody::StartTurn(_) => "start_turn",
            CommandBody::Title(_) => "title",
            CommandBody::Interrupt(_) => "interrupt",
            CommandBody::Kill(_) => "kill",
            CommandBody::AnswerAsk(_) => "answer_ask",
            CommandBody::StopTasks(_) => "stop_tasks",
            CommandBody::Unknown => return None,
        })
    }
}

/// `start_turn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTurn {
    /// The person's text.
    pub text: String,
    /// Files to write into `/workspace`.
    #[serde(default)]
    pub files: Vec<StartFile>,
    /// Images: the original into `/workspace`, the model copy as an image block.
    #[serde(default)]
    pub images: Vec<StartImage>,
    /// What to resume.
    pub resume: Resume,
    /// When the person sent it.
    pub sent_at: Timestamp,
}

/// A file of `start_turn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFile {
    /// The upload.
    pub upload_id: Uuid,
    /// File name.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
    /// Where to write it, relative to `/workspace` (`uploads/<name>`, unique in the conversation).
    pub workspace_path: String,
    /// The bytes' blob (`GET /v1/sandbox/blobs/{sha}?as=raw`).
    pub blob: BlobId,
}

/// An image of `start_turn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartImage {
    /// The upload.
    pub upload_id: Uuid,
    /// The original's MIME type.
    pub mime_type: String,
    /// Where to write the original.
    pub workspace_path: String,
    /// The original's blob.
    pub blob: BlobId,
    /// The model copy's blob.
    pub model_blob: BlobId,
    /// The model copy's MIME type.
    pub model_mime: String,
}

/// `start_turn.resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume {
    /// `--resume <id>` when set, else `--session-id <conversation id>`.
    pub provider_session_id: Option<String>,
}

/// `title`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleCommand {
    /// The user message to the title model.
    pub user_prompt: String,
    /// `max_tokens`.
    pub max_tokens: u32,
}

/// Why a kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KillReason {
    /// A cancel not confirmed in 30 s.
    Stop,
    /// magpie's watchdog.
    Watchdog,
}

/// `kill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillCommand {
    /// Why.
    pub reason: KillReason,
}

/// `answer_ask`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerAsk {
    /// The ask.
    pub ask_id: String,
    /// Allow or deny.
    pub allow: bool,
    /// The tool input to use (Claude Code's own keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Map<String, Value>>,
    /// The deny message.
    pub message: Option<String>,
}

/// `POST /v1/sandbox/commands/{cmdSeq}/ack` → 204.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckRequest {
    /// The outcome.
    pub result: AckResult,
}

/// A command's outcome: the fields depend on the command.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckResult {
    /// `start_turn`: the prompt reached Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<bool>,
    /// `title`: the Messages API response, as is (Anthropic's own keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Any: why it failed (`unsupported` for an unknown type, `no_such_ask`, `<status> <body>` for a title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `stop_tasks`: how many were stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<u32>,
}

/// `POST /v1/sandbox/turns`: a turn Claude Code started by itself → 201 (200 repeat), body `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurnRequest {
    /// A `UUIDv7` minted by the supervisor; idempotent.
    pub turn_id: Uuid,
    /// `agent`.
    pub origin: TurnOrigin,
    /// When it started.
    pub at: Timestamp,
}

/// A turn state the supervisor reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReportedState {
    /// Claude Code has the prompt.
    Running,
    /// Ended.
    Completed,
    /// Ended with an error.
    Failed,
    /// Ended by a cancel.
    Canceled,
}

/// `POST /v1/sandbox/turns/{turnId}/state` → 204; 409 `bad_transition` (with `state`) / `transcript_pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStateReport {
    /// The new state.
    pub state: ReportedState,
    /// When.
    pub at: Timestamp,
    /// `running`: Claude Code's session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// `running`: `projects/-workspace/<sid>.jsonl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Claude Code's exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// `failed`: why (`run.exited`, `run.error`, `run.oom`, `cc.write_stuck`, `sandbox.lost`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReportError>,
    /// Background tasks outlive the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<bool>,
    /// The background tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<ReportedTask>>,
    /// The uuid of the turn's last transcript message (end reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_uuid: Option<String>,
    /// Informational token counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Informational duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// An error the supervisor reports (its codes are [`crate::errors::DataCode`] values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportError {
    /// The dotted code.
    pub code: crate::errors::DataCode,
    /// English, for logs.
    pub message: String,
}

/// A background task in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedTask {
    /// Claude Code's task id.
    pub task_id: String,
    /// The launching call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Claude Code's task type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Its description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Started at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
}

/// Token counts (informational; cost stays egress's).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input.
    pub input_tokens: u64,
    /// Output.
    pub output_tokens: u64,
    /// Cache reads.
    pub cache_read_input_tokens: u64,
    /// Cache writes.
    pub cache_creation_input_tokens: u64,
}

/// `POST /v1/sandbox/tasks` → 204.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksReport {
    /// The batch.
    pub events: Vec<TaskReportEvent>,
}

/// One task event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReportEvent {
    /// `<gen>:<stdout offset>`: a repeated id is skipped.
    pub id: String,
    /// Claude Code's task id.
    pub task_id: String,
    /// The launching call.
    pub tool_use_id: Option<String>,
    /// What happened.
    pub event: TaskEvent,
    /// The status.
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
    /// When.
    pub at: Timestamp,
}

/// `POST /v1/sandbox/asks` → 201, body `{}`; idempotent per `askId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskReport {
    /// The ask.
    pub ask_id: String,
    /// The tool.
    pub tool_name: String,
    /// The call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The tool input (Claude Code's own keys).
    pub input: Map<String, Value>,
}

/// How an ask ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AskOutcome {
    /// The person answered.
    Answered,
    /// Canceled.
    Canceled,
}

/// `POST /v1/sandbox/asks/{askId}/resolved` → 204.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskResolvedReport {
    /// How.
    pub outcome: AskOutcome,
}

/// The supervisor's run state in a heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeartbeatState {
    /// A turn runs.
    Running,
    /// A turn is being stopped.
    Stopping,
    /// Background tasks outlive the turn.
    Held,
    /// Nothing runs.
    Idle,
}

/// `POST /v1/sandbox/heartbeat` every 15 s → 204.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    /// The turn the supervisor runs.
    pub turn_id: Option<Uuid>,
    /// Its run state.
    pub state: HeartbeatState,
    /// Background tasks.
    pub tasks: u32,
    /// The last turn whose end report magpie answered.
    pub last_turn: Option<HeartbeatLastTurn>,
    /// Claude Code generation.
    #[serde(rename = "gen")]
    pub generation: u64,
    /// The supervisor.
    pub supervisor: SupervisorFacts,
    /// The FUSE layer.
    pub cc: CcFacts,
}

/// `lastTurn` of a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatLastTurn {
    /// The turn.
    pub id: Uuid,
    /// How it ended.
    pub outcome: TurnOutcome,
    /// Its error.
    pub error: Option<ReportError>,
}

/// `supervisor` of a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorFacts {
    /// `supervisor-<sha12>`.
    pub version: String,
    /// Per boot.
    pub boot_id: String,
    /// Keeper restarts.
    pub restarts: u32,
}

/// `cc` of a heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcFacts {
    /// A write answered EDQUOT.
    pub quota_exceeded: bool,
    /// Writes waiting in the retry loop.
    pub blocked_writes: u32,
    /// The oldest of them.
    pub oldest_blocked_ms: u64,
}

/// `POST /v1/sandbox/sent-files` → `{missing}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFilesRequest {
    /// `^[A-Za-z0-9_-]{1,128}$`.
    pub tool_use_id: String,
    /// The caption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// 1-100 files.
    pub files: Vec<SentFileSpec>,
}

/// A file of a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFileSpec {
    /// Absolute under `/workspace` (a `file://` prefix is stripped).
    pub path: String,
    /// Bytes (<= 64 MiB).
    pub size: u64,
    /// SHA-256 hex of the bytes.
    pub sha256: BlobId,
    /// MIME type from the extension table.
    pub mime_type: String,
}

/// `POST /v1/sandbox/sent-files` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFilesResponse {
    /// The blobs to `PUT` before the commit.
    pub missing: Vec<BlobId>,
}

/// `POST /v1/sandbox/sent-files/{toolUseId}/commit` → 200.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFilesCommitted {
    /// Files kept.
    pub kept: u32,
    /// The `files.sent` event's seq.
    pub seq: u64,
}
