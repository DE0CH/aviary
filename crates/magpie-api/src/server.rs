//! The `Server` trait magpie and the fakes implement, and [`router`], which serves it over HTTP with the contract's
//! paths, extractors and status codes (feature `server`).
//!
//! Every method has a default answer of 501 `not_supported`, so a fake implements only what its test needs. The
//! router does no authentication: the service wraps it in aviary-core's `standard` layers and checks callers itself
//! (aviary-core's `Callers` and [`crate::routes::Route::callers`]); [`Ctx`] carries the identity headers as sent.
//! Methods answer errors as aviary-core's [`ApiError`]; a [`crate::errors::Refusal`] converts into one.
//!
//! Method bodies and query strings are parsed into the contract types; a body or query that does not parse answers
//! 400 `bad_request`, a path parameter that does not parse 404 `not_found`, both as the flat envelope.

use std::future::Future;
use std::sync::Arc;

use aviary_core::http::headers::{
    AVIARY_ADMIN, AVIARY_LEASE, AVIARY_PERSON, AVIARY_PROTOCOL, AVIARY_REQUEST, AVIARY_SANDBOX_CONVERSATION,
    AVIARY_SANDBOX_ID, AVIARY_SANDBOX_PERSON, AVIARY_UPLOAD_KIND, AVIARY_UPLOAD_NAME, AVIARY_VIEW_SESSION,
    AVIARY_VIEWER,
};
use aviary_core::http::{ApiError, json};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::changes::{ChangesQuery, ChangesResponse};
use crate::conversations::{Conversation, CreateRequest, CreateResponse, ListQuery, ListResponse, RenameRequest};
use crate::errors::{ErrorCode, Refusal};
use crate::events::{EventsPage, EventsQuery};
use crate::files::{FilesPanel, RawFileQuery};
use crate::legacy::LegacyPathRequest;
use crate::live::{LiveFramesRequest, LiveState};
use crate::ops::{
    AdminConversation, AdminConversationList, AdminListQuery, HermitCreateRequest, HermitSendRequest,
    LegacyImportStatus, OpsStats, PeopleQuery, PeopleResponse, PersonPurgeRequest, PersonPurgeResponse, ViewAsLog,
    ViewAsLogQuery, ViewAsRequest, WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use crate::routes::{
    ADMIN_CONVERSATION, ADMIN_CONVERSATIONS, ADMIN_EVENTS, ADMIN_SCHEDULES, ADMIN_SCHEDULES_PAUSE, ADMIN_TIMELINE, ADMIN_TRANSCRIPT, ASK_ANSWER, BLOB, CANCEL, CHANGES, CONVERSATION, CONVERSATIONS, CONVERSATION_COMMANDS, CONVERSATION_DELETE, CONVERSATION_PENDING, CONVERSATION_RESTORE, CONVERSATION_SCHEDULES, CONVERSATION_SHARES, CONVERSATION_TASKS, CONVERSATION_TASK_STOP, CREATION, FILES, FILES_RAW, HERMIT_CONVERSATION, HERMIT_CONVERSATIONS, HERMIT_CONVERSATION_CLOSED, HERMIT_CONVERSATION_DELETE, HERMIT_CONVERSATION_TURNS, HERMIT_LEGACY_IMPORT, HERMIT_PERSON_PURGE, HERMIT_WORKSPACES_EXISTS, LIVE, OPS_PEOPLE, OPS_STATS, OPS_VIEW_AS, PROMPTS, SANDBOX_ASKS, SANDBOX_ASK_RESOLVED, SANDBOX_BLOB, SANDBOX_CC_FILE, SANDBOX_CC_MKDIR, SANDBOX_CC_RENAME, SANDBOX_CC_RMDIR, SANDBOX_CC_SETATTR, SANDBOX_CC_TREE, SANDBOX_CC_TRUNCATE, SANDBOX_CC_UNLINK, SANDBOX_CC_WRITE, SANDBOX_COMMANDS, SANDBOX_COMMAND_ACK, SANDBOX_HEARTBEAT, SANDBOX_LEASE, SANDBOX_SCHEDULE, SANDBOX_SCHEDULES, SANDBOX_SENT_FILES, SANDBOX_SENT_FILES_COMMIT, SANDBOX_TASKS, SANDBOX_TURNS, SANDBOX_TURN_LIVE, SANDBOX_TURN_STATE, SEARCH, SHARE, SHARE_CONTINUE, SHARE_FILES_RAW, TIMELINE, TRANSCRIPT, TURNS, UPLOADS, UPLOAD_LEGACY_PATH, VIEW_AS, self,
};
use crate::rows::{
    PromptsResponse, SearchQuery, SearchResponse, TimelinePage, TimelineQuery, TranscriptPage, TranscriptQuery,
};
use crate::sandbox::{
    AckRequest, AgentTurnRequest, AskReport, AskResolvedReport, CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr,
    CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery, CommandsQuery, CommandsResponse, Heartbeat,
    LeaseResponse, SentFilesCommitted, SentFilesRequest, SentFilesResponse, TasksReport, TurnStateReport,
};
use crate::schedules::{AdminSchedules, CreateSchedule, PauseAll, Schedule, SchedulesList, UpdateSchedule};
use crate::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use crate::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use crate::uploads::{BlobQuery, SandboxBlobQuery, UploadKind, UploadMeta, UploadResponse};
use crate::{Admitted, Empty};

/// The identity and correlation headers of a request, as sent (the router trusts nothing; the service decides).
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    /// `Aviary-Request`.
    pub request_id: Option<String>,
    /// `Aviary-Protocol`.
    pub protocol: Option<String>,
    /// The bearer token of `Authorization`.
    pub bearer: Option<String>,
    /// `Aviary-Person`, trimmed and lowercased.
    pub person: Option<String>,
    /// `Aviary-Viewer` and `Aviary-View-Session`.
    pub viewer: Option<Viewer>,
    /// `Aviary-Admin`.
    pub admin: Option<String>,
    /// `Aviary-Sandbox-*`, when all three are present.
    pub sandbox: Option<SandboxIdentity>,
    /// `Aviary-Lease`.
    pub lease: Option<String>,
    /// Every header (`Range`, `If-None-Match`, …).
    pub headers: HeaderMap,
}

/// An admin viewing as the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// The admin's email, trimmed and lowercased.
    pub email: String,
    /// The view session.
    pub session: String,
}

/// The sandbox behind a gateway call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxIdentity {
    /// hermit's sandbox id.
    pub id: String,
    /// The sandbox's conversation.
    pub conversation: String,
    /// The owner's email.
    pub person: String,
}

fn header_str(h: &HeaderMap, name: &HeaderName) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

fn email(h: &HeaderMap, name: &HeaderName) -> Option<String> {
    header_str(h, name)
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
}

impl Ctx {
    /// Read the headers.
    #[must_use]
    pub fn from_headers(h: &HeaderMap) -> Self {
        let bearer =
            header_str(h, &header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer ").map(|t| t.trim().to_owned()));
        let viewer = email(h, &AVIARY_VIEWER).map(|email| Viewer {
            email,
            session: header_str(h, &AVIARY_VIEW_SESSION).unwrap_or_default(),
        });
        let sandbox = match (
            header_str(h, &AVIARY_SANDBOX_ID),
            header_str(h, &AVIARY_SANDBOX_CONVERSATION),
            email(h, &AVIARY_SANDBOX_PERSON),
        ) {
            (Some(id), Some(conversation), Some(person)) => Some(SandboxIdentity {
                id,
                conversation,
                person,
            }),
            _ => None,
        };
        Ctx {
            request_id: header_str(h, &AVIARY_REQUEST),
            protocol: header_str(h, &AVIARY_PROTOCOL),
            bearer,
            person: email(h, &AVIARY_PERSON),
            viewer,
            admin: email(h, &AVIARY_ADMIN),
            sandbox,
            lease: header_str(h, &AVIARY_LEASE),
            headers: h.clone(),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(parts: &mut Parts, _: &S) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Ctx::from_headers(&parts.headers)))
    }
}

/// A query string parsed into a contract type (400 `bad_request` otherwise).
#[derive(Debug)]
pub struct Q<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Q<T> {
    type Rejection = ApiError;

    fn from_request_parts(parts: &mut Parts, _: &S) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            Query::<T>::try_from_uri(&parts.uri)
                .map(|q| Q(q.0))
                .map_err(|e| ApiError::bad_request(format!("query: {e}"))),
        )
    }
}

/// Path parameters (404 `not_found` when they do not parse: no such resource).
#[derive(Debug)]
pub struct P<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for P<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|p| P(p.0))
            .map_err(|_| ApiError::not_found("not found"))
    }
}

/// A JSON body parsed into a contract type, whatever its `Content-Type`; an empty body reads as `{}`.
#[derive(Debug)]
pub struct J<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for J<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(format!("body: {e}")))?;
        let bytes: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
            b"{}"
        } else {
            &bytes
        };
        serde_json::from_slice(bytes)
            .map(J)
            .map_err(|e| ApiError::bad_request(format!("body: {e}")))
    }
}

fn ok_status(id: &str) -> StatusCode {
    routes::by_id(id)
        .and_then(|r| r.ok.first())
        .and_then(|s| StatusCode::from_u16(*s).ok())
        .unwrap_or(StatusCode::OK)
}

fn reply<T: Serialize>(id: &str, r: Result<T, ApiError>) -> Response {
    match r {
        Ok(t) => json(ok_status(id), &t),
        Err(e) => e.into_response(),
    }
}

fn reply_admitted<T: Serialize>(id: &str, r: Result<Admitted<T>, ApiError>) -> Response {
    match r {
        Ok(Admitted::New(t)) => json(ok_status(id), &t),
        Ok(Admitted::Repeat(t)) => json(StatusCode::OK, &t),
        Err(e) => e.into_response(),
    }
}

fn reply_empty(r: Result<(), ApiError>) -> Response {
    match r {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

fn reply_unit(id: &str, r: Result<(), ApiError>) -> Response {
    reply(id, r.map(|()| Empty {}))
}

fn reply_option<T: Serialize>(id: &str, r: Result<Option<T>, ApiError>) -> Response {
    match r {
        Ok(Some(t)) => json(ok_status(id), &t),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

fn reply_raw(r: Result<Response, ApiError>) -> Response {
    r.unwrap_or_else(IntoResponse::into_response)
}

fn upload_meta(ctx: &Ctx) -> Result<UploadMeta, ApiError> {
    let name = header_str(&ctx.headers, &AVIARY_UPLOAD_NAME)
        .map(|n| routes::decode_percent(&n))
        .filter(|n| !n.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing Aviary-Upload-Name"))?;
    let kind = header_str(&ctx.headers, &AVIARY_UPLOAD_KIND)
        .and_then(|k| UploadKind::parse(&k))
        .ok_or_else(|| ApiError::bad_request("Aviary-Upload-Kind must be file or image"))?;
    let content_type =
        header_str(&ctx.headers, &header::CONTENT_TYPE).unwrap_or_else(|| "application/octet-stream".into());
    Ok(UploadMeta {
        name,
        kind,
        content_type,
    })
}

fn not_implemented(id: &str) -> ApiError {
    Refusal::new(ErrorCode::NotSupported, format!("{id} is not supported by this server")).into()
}

/// magpie's contract as a trait: one method per route of [`crate::routes::ROUTES`], named by the route id.
///
/// Raw byte routes take an [`axum::body::Body`] and answer an [`axum::response::Response`] (the service sets
/// `content-type`, `etag`, `content-range` and the rest of magpie 5.4 and 11.3).
#[allow(unused_variables, clippy::too_many_lines)]
pub trait Server: Send + Sync + 'static {
    /// Route `list_conversations`.
    fn list_conversations(
        &self,
        ctx: Ctx,
        query: ListQuery,
    ) -> impl Future<Output = Result<ListResponse, ApiError>> + Send {
        async move { Err(not_implemented("list_conversations")) }
    }

    /// Route `get_conversation`.
    fn get_conversation(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("get_conversation")) }
    }

    /// Route `create_conversation`.
    fn create_conversation(
        &self,
        ctx: Ctx,
        body: CreateRequest,
    ) -> impl Future<Output = Result<Admitted<CreateResponse>, ApiError>> + Send {
        async move { Err(not_implemented("create_conversation")) }
    }

    /// Route `get_creation`.
    fn get_creation(
        &self,
        ctx: Ctx,
        idempotency_key: String,
    ) -> impl Future<Output = Result<CreateResponse, ApiError>> + Send {
        async move { Err(not_implemented("get_creation")) }
    }

    /// Route `rename_conversation`.
    fn rename_conversation(
        &self,
        ctx: Ctx,
        id: Uuid,
        body: RenameRequest,
    ) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("rename_conversation")) }
    }

    /// Route `delete_conversation`.
    fn delete_conversation(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("delete_conversation")) }
    }

    /// Route `restore_conversation`.
    fn restore_conversation(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("restore_conversation")) }
    }

    /// Route `purge_conversation`.
    fn purge_conversation(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("purge_conversation")) }
    }

    /// Route `send_turn`.
    fn send_turn(
        &self,
        ctx: Ctx,
        id: Uuid,
        body: SendRequest,
    ) -> impl Future<Output = Result<Admitted<SendResponse>, ApiError>> + Send {
        async move { Err(not_implemented("send_turn")) }
    }

    /// Route `cancel_turn`.
    fn cancel_turn(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<CancelResponse, ApiError>> + Send {
        async move { Err(not_implemented("cancel_turn")) }
    }

    /// Route `timeline`.
    fn timeline(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: TimelineQuery,
    ) -> impl Future<Output = Result<TimelinePage, ApiError>> + Send {
        async move { Err(not_implemented("timeline")) }
    }

    /// Route `live`.
    fn live(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<Option<LiveState>, ApiError>> + Send {
        async move { Err(not_implemented("live")) }
    }

    /// Route `prompts`.
    fn prompts(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<PromptsResponse, ApiError>> + Send {
        async move { Err(not_implemented("prompts")) }
    }

    /// Route `search`.
    fn search(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: SearchQuery,
    ) -> impl Future<Output = Result<SearchResponse, ApiError>> + Send {
        async move { Err(not_implemented("search")) }
    }

    /// Route `transcript`.
    fn transcript(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: TranscriptQuery,
    ) -> impl Future<Output = Result<TranscriptPage, ApiError>> + Send {
        async move { Err(not_implemented("transcript")) }
    }

    /// Route `answer_ask`.
    fn answer_ask(
        &self,
        ctx: Ctx,
        id: Uuid,
        ask_id: String,
        body: AgentPermissionResponse,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("answer_ask")) }
    }

    /// Route `changes`.
    fn changes(&self, ctx: Ctx, query: ChangesQuery) -> impl Future<Output = Result<ChangesResponse, ApiError>> + Send {
        async move { Err(not_implemented("changes")) }
    }

    /// Route `upload`.
    fn upload(
        &self,
        ctx: Ctx,
        meta: UploadMeta,
        body: Body,
    ) -> impl Future<Output = Result<UploadResponse, ApiError>> + Send {
        async move { Err(not_implemented("upload")) }
    }

    /// Route `files_raw`.
    fn files_raw(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: RawFileQuery,
    ) -> impl Future<Output = Result<Response, ApiError>> + Send {
        async move { Err(not_implemented("files_raw")) }
    }

    /// Route `files_panel`.
    fn files_panel(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<FilesPanel, ApiError>> + Send {
        async move { Err(not_implemented("files_panel")) }
    }

    /// Route `blob`.
    fn blob(
        &self,
        ctx: Ctx,
        blob_id: String,
        query: BlobQuery,
    ) -> impl Future<Output = Result<Response, ApiError>> + Send {
        async move { Err(not_implemented("blob")) }
    }

    /// Route `list_shares`.
    fn list_shares(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<SharesList, ApiError>> + Send {
        async move { Err(not_implemented("list_shares")) }
    }

    /// Route `create_share`.
    fn create_share(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<ShareRef, ApiError>> + Send {
        async move { Err(not_implemented("create_share")) }
    }

    /// Route `revoke_share`.
    fn revoke_share(&self, ctx: Ctx, share_id: String) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("revoke_share")) }
    }

    /// Route `get_share`.
    fn get_share(&self, ctx: Ctx, share_id: String) -> impl Future<Output = Result<ShareView, ApiError>> + Send {
        async move { Err(not_implemented("get_share")) }
    }

    /// Route `share_files_raw`.
    fn share_files_raw(
        &self,
        ctx: Ctx,
        share_id: String,
        query: RawFileQuery,
    ) -> impl Future<Output = Result<Response, ApiError>> + Send {
        async move { Err(not_implemented("share_files_raw")) }
    }

    /// Route `continue_share`.
    fn continue_share(
        &self,
        ctx: Ctx,
        share_id: String,
    ) -> impl Future<Output = Result<ContinueResponse, ApiError>> + Send {
        async move { Err(not_implemented("continue_share")) }
    }

    /// Route `view_as`.
    fn view_as(&self, ctx: Ctx, body: ViewAsRequest) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("view_as")) }
    }

    /// Route `lease`.
    fn lease(&self, ctx: Ctx) -> impl Future<Output = Result<LeaseResponse, ApiError>> + Send {
        async move { Err(not_implemented("lease")) }
    }

    /// Route `cc_tree`.
    fn cc_tree(&self, ctx: Ctx) -> impl Future<Output = Result<CcTree, ApiError>> + Send {
        async move { Err(not_implemented("cc_tree")) }
    }

    /// Route `cc_file`.
    fn cc_file(&self, ctx: Ctx, query: CcFileQuery) -> impl Future<Output = Result<Response, ApiError>> + Send {
        async move { Err(not_implemented("cc_file")) }
    }

    /// Route `cc_write`.
    fn cc_write(
        &self,
        ctx: Ctx,
        query: CcWriteQuery,
        body: Body,
    ) -> impl Future<Output = Result<CcSizeVersion, ApiError>> + Send {
        async move { Err(not_implemented("cc_write")) }
    }

    /// Route `cc_truncate`.
    fn cc_truncate(&self, ctx: Ctx, body: CcTruncate) -> impl Future<Output = Result<CcSizeVersion, ApiError>> + Send {
        async move { Err(not_implemented("cc_truncate")) }
    }

    /// Route `cc_rename`.
    fn cc_rename(&self, ctx: Ctx, body: CcRename) -> impl Future<Output = Result<CcVersion, ApiError>> + Send {
        async move { Err(not_implemented("cc_rename")) }
    }

    /// Route `cc_unlink`.
    fn cc_unlink(&self, ctx: Ctx, body: CcPathOp) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("cc_unlink")) }
    }

    /// Route `cc_mkdir`.
    fn cc_mkdir(&self, ctx: Ctx, body: CcMkdir) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("cc_mkdir")) }
    }

    /// Route `cc_rmdir`.
    fn cc_rmdir(&self, ctx: Ctx, body: CcPathOp) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("cc_rmdir")) }
    }

    /// Route `cc_setattr`.
    fn cc_setattr(&self, ctx: Ctx, body: CcSetattr) -> impl Future<Output = Result<CcVersion, ApiError>> + Send {
        async move { Err(not_implemented("cc_setattr")) }
    }

    /// Route `commands`.
    fn commands(
        &self,
        ctx: Ctx,
        query: CommandsQuery,
    ) -> impl Future<Output = Result<CommandsResponse, ApiError>> + Send {
        async move { Err(not_implemented("commands")) }
    }

    /// Route `ack_command`.
    fn ack_command(
        &self,
        ctx: Ctx,
        cmd_seq: u64,
        body: AckRequest,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("ack_command")) }
    }

    /// Route `agent_turn`.
    fn agent_turn(
        &self,
        ctx: Ctx,
        body: AgentTurnRequest,
    ) -> impl Future<Output = Result<Admitted<Empty>, ApiError>> + Send {
        async move { Err(not_implemented("agent_turn")) }
    }

    /// Route `turn_state`.
    fn turn_state(
        &self,
        ctx: Ctx,
        turn_id: Uuid,
        body: TurnStateReport,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("turn_state")) }
    }

    /// Route `live_frames`.
    fn live_frames(
        &self,
        ctx: Ctx,
        turn_id: Uuid,
        body: LiveFramesRequest,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("live_frames")) }
    }

    /// Route `tasks`.
    fn tasks(&self, ctx: Ctx, body: TasksReport) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("tasks")) }
    }

    /// Route `ask_open`.
    fn ask_open(&self, ctx: Ctx, body: AskReport) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("ask_open")) }
    }

    /// Route `ask_resolved`.
    fn ask_resolved(
        &self,
        ctx: Ctx,
        ask_id: String,
        body: AskResolvedReport,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("ask_resolved")) }
    }

    /// Route `heartbeat`.
    fn heartbeat(&self, ctx: Ctx, body: Heartbeat) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("heartbeat")) }
    }

    /// Route `sandbox_blob_get`.
    fn sandbox_blob_get(
        &self,
        ctx: Ctx,
        sha: String,
        query: SandboxBlobQuery,
    ) -> impl Future<Output = Result<Response, ApiError>> + Send {
        async move { Err(not_implemented("sandbox_blob_get")) }
    }

    /// Route `sandbox_blob_put`.
    fn sandbox_blob_put(
        &self,
        ctx: Ctx,
        sha: String,
        body: Body,
    ) -> impl Future<Output = Result<Admitted<Empty>, ApiError>> + Send {
        async move { Err(not_implemented("sandbox_blob_put")) }
    }

    /// Route `sent_files`.
    fn sent_files(
        &self,
        ctx: Ctx,
        body: SentFilesRequest,
    ) -> impl Future<Output = Result<SentFilesResponse, ApiError>> + Send {
        async move { Err(not_implemented("sent_files")) }
    }

    /// Route `sent_files_commit`.
    fn sent_files_commit(
        &self,
        ctx: Ctx,
        tool_use_id: String,
    ) -> impl Future<Output = Result<SentFilesCommitted, ApiError>> + Send {
        async move { Err(not_implemented("sent_files_commit")) }
    }

    /// Route `hermit_create_conversation`.
    fn hermit_create_conversation(
        &self,
        ctx: Ctx,
        body: HermitCreateRequest,
    ) -> impl Future<Output = Result<Admitted<CreateResponse>, ApiError>> + Send {
        async move { Err(not_implemented("hermit_create_conversation")) }
    }

    /// Route `hermit_conversation`.
    fn hermit_conversation(&self, ctx: Ctx, id: Uuid) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("hermit_conversation")) }
    }

    /// Route `hermit_close_conversation`.
    fn hermit_close_conversation(
        &self,
        ctx: Ctx,
        id: Uuid,
        body: crate::ops::HermitCloseRequest,
    ) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("hermit_close_conversation")) }
    }

    /// Route `hermit_delete_conversation`.
    fn hermit_delete_conversation(
        &self,
        ctx: Ctx,
        id: Uuid,
    ) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("hermit_delete_conversation")) }
    }

    /// Route `hermit_send_turn`.
    fn hermit_send_turn(
        &self,
        ctx: Ctx,
        id: Uuid,
        body: HermitSendRequest,
    ) -> impl Future<Output = Result<Admitted<SendResponse>, ApiError>> + Send {
        async move { Err(not_implemented("hermit_send_turn")) }
    }

    /// Route `person_purge`.
    fn person_purge(
        &self,
        ctx: Ctx,
        person: String,
        body: PersonPurgeRequest,
    ) -> impl Future<Output = Result<PersonPurgeResponse, ApiError>> + Send {
        async move { Err(not_implemented("person_purge")) }
    }

    /// Route `workspaces_exists`.
    fn workspaces_exists(
        &self,
        ctx: Ctx,
        body: WorkspacesExistsRequest,
    ) -> impl Future<Output = Result<WorkspacesExistsResponse, ApiError>> + Send {
        async move { Err(not_implemented("workspaces_exists")) }
    }

    /// Route `legacy_import`.
    fn legacy_import(&self, ctx: Ctx) -> impl Future<Output = Result<LegacyImportStatus, ApiError>> + Send {
        async move { Err(not_implemented("legacy_import")) }
    }

    /// Route `ops_people`.
    fn ops_people(
        &self,
        ctx: Ctx,
        query: PeopleQuery,
    ) -> impl Future<Output = Result<PeopleResponse, ApiError>> + Send {
        async move { Err(not_implemented("ops_people")) }
    }

    /// Route `ops_view_as`.
    fn ops_view_as(&self, ctx: Ctx, query: ViewAsLogQuery) -> impl Future<Output = Result<ViewAsLog, ApiError>> + Send {
        async move { Err(not_implemented("ops_view_as")) }
    }

    /// Route `ops_stats`.
    fn ops_stats(&self, ctx: Ctx) -> impl Future<Output = Result<OpsStats, ApiError>> + Send {
        async move { Err(not_implemented("ops_stats")) }
    }

    /// Route `admin_conversations`.
    fn admin_conversations(
        &self,
        ctx: Ctx,
        query: AdminListQuery,
    ) -> impl Future<Output = Result<AdminConversationList, ApiError>> + Send {
        async move { Err(not_implemented("admin_conversations")) }
    }

    /// Route `admin_conversation`.
    fn admin_conversation(
        &self,
        ctx: Ctx,
        id: Uuid,
    ) -> impl Future<Output = Result<AdminConversation, ApiError>> + Send {
        async move { Err(not_implemented("admin_conversation")) }
    }

    /// Route `admin_events`.
    fn admin_events(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: EventsQuery,
    ) -> impl Future<Output = Result<EventsPage, ApiError>> + Send {
        async move { Err(not_implemented("admin_events")) }
    }

    /// Route `admin_timeline`.
    fn admin_timeline(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: TimelineQuery,
    ) -> impl Future<Output = Result<TimelinePage, ApiError>> + Send {
        async move { Err(not_implemented("admin_timeline")) }
    }

    /// Route `admin_transcript`.
    fn admin_transcript(
        &self,
        ctx: Ctx,
        id: Uuid,
        query: TranscriptQuery,
    ) -> impl Future<Output = Result<TranscriptPage, ApiError>> + Send {
        async move { Err(not_implemented("admin_transcript")) }
    }

    /// Route `list_schedules`.
    fn list_schedules(&self, ctx: Ctx) -> impl Future<Output = Result<SchedulesList, ApiError>> + Send {
        async move { Err(not_implemented("list_schedules")) }
    }

    /// Route `create_schedule`.
    fn create_schedule(
        &self,
        ctx: Ctx,
        body: CreateSchedule,
    ) -> impl Future<Output = Result<Schedule, ApiError>> + Send {
        async move { Err(not_implemented("create_schedule")) }
    }

    /// Route `get_schedule`.
    fn get_schedule(&self, ctx: Ctx, schedule_id: String) -> impl Future<Output = Result<Schedule, ApiError>> + Send {
        async move { Err(not_implemented("get_schedule")) }
    }

    /// Route `update_schedule`.
    fn update_schedule(
        &self,
        ctx: Ctx,
        schedule_id: String,
        body: UpdateSchedule,
    ) -> impl Future<Output = Result<Schedule, ApiError>> + Send {
        async move { Err(not_implemented("update_schedule")) }
    }

    /// Route `delete_schedule`.
    fn delete_schedule(&self, ctx: Ctx, schedule_id: String) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("delete_schedule")) }
    }

    /// Route `withdraw_pending`.
    fn withdraw_pending(
        &self,
        ctx: Ctx,
        id: Uuid,
        client_message_id: String,
    ) -> impl Future<Output = Result<Conversation, ApiError>> + Send {
        async move { Err(not_implemented("withdraw_pending")) }
    }

    /// Route `conversation_commands`.
    fn conversation_commands(
        &self,
        ctx: Ctx,
        id: Uuid,
    ) -> impl Future<Output = Result<crate::sandbox::CommandsList, ApiError>> + Send {
        async move { Err(not_implemented("conversation_commands")) }
    /// Route `conversation_tasks`.
    fn conversation_tasks(
        &self,
        ctx: Ctx,
        id: Uuid,
    ) -> impl Future<Output = Result<crate::tasks::TasksList, ApiError>> + Send {
        async move { Err(not_implemented("conversation_tasks")) }
    }

    /// Route `stop_task`.
    fn stop_task(&self, ctx: Ctx, id: Uuid, task_id: String) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("stop_task")) }
    }

    /// Route `conversation_schedules`.
    fn conversation_schedules(
        &self,
        ctx: Ctx,
        id: Uuid,
    ) -> impl Future<Output = Result<SchedulesList, ApiError>> + Send {
        async move { Err(not_implemented("conversation_schedules")) }
    }

    /// Route `admin_schedules`.
    fn admin_schedules(&self, ctx: Ctx) -> impl Future<Output = Result<AdminSchedules, ApiError>> + Send {
        async move { Err(not_implemented("admin_schedules")) }
    }

    /// Route `admin_schedules_pause`.
    fn admin_schedules_pause(
        &self,
        ctx: Ctx,
        body: PauseAll,
    ) -> impl Future<Output = Result<AdminSchedules, ApiError>> + Send {
        async move { Err(not_implemented("admin_schedules_pause")) }
    }

    /// Route `upload_legacy_path` (until A7).
    fn upload_legacy_path(
        &self,
        ctx: Ctx,
        upload_id: Uuid,
        body: LegacyPathRequest,
    ) -> impl Future<Output = Result<(), ApiError>> + Send {
        async move { Err(not_implemented("upload_legacy_path")) }
    }
}

async fn h_upload_legacy_path<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(upload_id): P<Uuid>,
    J(body): J<LegacyPathRequest>,
) -> Response {
    reply_empty(s.upload_legacy_path(ctx, upload_id, body).await)
}

async fn h_list_conversations<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<ListQuery>) -> Response {
    reply("list_conversations", s.list_conversations(ctx, query).await)
}

async fn h_get_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("get_conversation", s.get_conversation(ctx, id).await)
}

async fn h_create_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CreateRequest>) -> Response {
    reply_admitted("create_conversation", s.create_conversation(ctx, body).await)
}

async fn h_get_creation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(idempotency_key): P<String>) -> Response {
    reply("get_creation", s.get_creation(ctx, idempotency_key).await)
}

async fn h_rename_conversation<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    J(body): J<RenameRequest>,
) -> Response {
    reply("rename_conversation", s.rename_conversation(ctx, id, body).await)
}

async fn h_delete_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("delete_conversation", s.delete_conversation(ctx, id).await)
}

async fn h_restore_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("restore_conversation", s.restore_conversation(ctx, id).await)
}

async fn h_purge_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply_empty(s.purge_conversation(ctx, id).await)
}

async fn h_send_turn<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    J(body): J<SendRequest>,
) -> Response {
    reply_admitted("send_turn", s.send_turn(ctx, id, body).await)
}

async fn h_cancel_turn<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("cancel_turn", s.cancel_turn(ctx, id).await)
}

async fn h_timeline<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<TimelineQuery>,
) -> Response {
    reply("timeline", s.timeline(ctx, id, query).await)
}

async fn h_live<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply_option("live", s.live(ctx, id).await)
}

async fn h_prompts<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("prompts", s.prompts(ctx, id).await)
}

async fn h_search<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>, Q(query): Q<SearchQuery>) -> Response {
    reply("search", s.search(ctx, id, query).await)
}

async fn h_transcript<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<TranscriptQuery>,
) -> Response {
    reply("transcript", s.transcript(ctx, id, query).await)
}

async fn h_answer_ask<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P((id, ask_id)): P<(Uuid, String)>,
    J(body): J<AgentPermissionResponse>,
) -> Response {
    reply_unit("answer_ask", s.answer_ask(ctx, id, ask_id, body).await)
}

async fn h_changes<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<ChangesQuery>) -> Response {
    reply("changes", s.changes(ctx, query).await)
}

async fn h_upload<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, body: Body) -> Response {
    let meta = match upload_meta(&ctx) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    reply("upload", s.upload(ctx, meta, body).await)
}

async fn h_files_raw<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<RawFileQuery>,
) -> Response {
    reply_raw(s.files_raw(ctx, id, query).await)
}

async fn h_files_panel<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("files_panel", s.files_panel(ctx, id).await)
}

async fn h_blob<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(blob_id): P<String>,
    Q(query): Q<BlobQuery>,
) -> Response {
    reply_raw(s.blob(ctx, blob_id, query).await)
}

async fn h_list_shares<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("list_shares", s.list_shares(ctx, id).await)
}

async fn h_create_share<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("create_share", s.create_share(ctx, id).await)
}

async fn h_revoke_share<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(share_id): P<String>) -> Response {
    reply_empty(s.revoke_share(ctx, share_id).await)
}

async fn h_get_share<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(share_id): P<String>) -> Response {
    reply("get_share", s.get_share(ctx, share_id).await)
}

async fn h_share_files_raw<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(share_id): P<String>,
    Q(query): Q<RawFileQuery>,
) -> Response {
    reply_raw(s.share_files_raw(ctx, share_id, query).await)
}

async fn h_continue_share<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(share_id): P<String>) -> Response {
    reply("continue_share", s.continue_share(ctx, share_id).await)
}

async fn h_view_as<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<ViewAsRequest>) -> Response {
    reply_unit("view_as", s.view_as(ctx, body).await)
}

async fn h_lease<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("lease", s.lease(ctx).await)
}

async fn h_cc_tree<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("cc_tree", s.cc_tree(ctx).await)
}

async fn h_cc_file<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<CcFileQuery>) -> Response {
    reply_raw(s.cc_file(ctx, query).await)
}

async fn h_cc_write<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<CcWriteQuery>, body: Body) -> Response {
    reply("cc_write", s.cc_write(ctx, query, body).await)
}

async fn h_cc_truncate<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcTruncate>) -> Response {
    reply("cc_truncate", s.cc_truncate(ctx, body).await)
}

async fn h_cc_rename<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcRename>) -> Response {
    reply("cc_rename", s.cc_rename(ctx, body).await)
}

async fn h_cc_unlink<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcPathOp>) -> Response {
    reply_empty(s.cc_unlink(ctx, body).await)
}

async fn h_cc_mkdir<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcMkdir>) -> Response {
    reply_empty(s.cc_mkdir(ctx, body).await)
}

async fn h_cc_rmdir<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcPathOp>) -> Response {
    reply_empty(s.cc_rmdir(ctx, body).await)
}

async fn h_cc_setattr<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CcSetattr>) -> Response {
    reply("cc_setattr", s.cc_setattr(ctx, body).await)
}

async fn h_commands<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<CommandsQuery>) -> Response {
    reply("commands", s.commands(ctx, query).await)
}

async fn h_ack_command<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(cmd_seq): P<u64>,
    J(body): J<AckRequest>,
) -> Response {
    reply_empty(s.ack_command(ctx, cmd_seq, body).await)
}

async fn h_agent_turn<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<AgentTurnRequest>) -> Response {
    reply_admitted("agent_turn", s.agent_turn(ctx, body).await)
}

async fn h_turn_state<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(turn_id): P<Uuid>,
    J(body): J<TurnStateReport>,
) -> Response {
    reply_empty(s.turn_state(ctx, turn_id, body).await)
}

async fn h_live_frames<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(turn_id): P<Uuid>,
    J(body): J<LiveFramesRequest>,
) -> Response {
    reply_empty(s.live_frames(ctx, turn_id, body).await)
}

async fn h_tasks<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<TasksReport>) -> Response {
    reply_empty(s.tasks(ctx, body).await)
}

async fn h_ask_open<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<AskReport>) -> Response {
    reply_unit("ask_open", s.ask_open(ctx, body).await)
}

async fn h_ask_resolved<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(ask_id): P<String>,
    J(body): J<AskResolvedReport>,
) -> Response {
    reply_empty(s.ask_resolved(ctx, ask_id, body).await)
}

async fn h_heartbeat<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<Heartbeat>) -> Response {
    reply_empty(s.heartbeat(ctx, body).await)
}

async fn h_sandbox_blob_get<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(sha): P<String>,
    Q(query): Q<SandboxBlobQuery>,
) -> Response {
    reply_raw(s.sandbox_blob_get(ctx, sha, query).await)
}

async fn h_sandbox_blob_put<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(sha): P<String>, body: Body) -> Response {
    reply_admitted("sandbox_blob_put", s.sandbox_blob_put(ctx, sha, body).await)
}

async fn h_sent_files<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<SentFilesRequest>) -> Response {
    reply("sent_files", s.sent_files(ctx, body).await)
}

async fn h_sent_files_commit<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(tool_use_id): P<String>) -> Response {
    reply("sent_files_commit", s.sent_files_commit(ctx, tool_use_id).await)
}

async fn h_hermit_create_conversation<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    J(body): J<HermitCreateRequest>,
) -> Response {
    reply_admitted(
        "hermit_create_conversation",
        s.hermit_create_conversation(ctx, body).await,
    )
}

async fn h_hermit_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("hermit_conversation", s.hermit_conversation(ctx, id).await)
}

async fn h_hermit_close_conversation<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    J(body): J<crate::ops::HermitCloseRequest>,
) -> Response {
    reply(
        "hermit_close_conversation",
        s.hermit_close_conversation(ctx, id, body).await,
    )
}

async fn h_hermit_delete_conversation<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    J(_): J<Empty>,
) -> Response {
    reply(
        "hermit_delete_conversation",
        s.hermit_delete_conversation(ctx, id).await,
    )
}

async fn h_hermit_send_turn<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    J(body): J<HermitSendRequest>,
) -> Response {
    reply_admitted("hermit_send_turn", s.hermit_send_turn(ctx, id, body).await)
}

async fn h_person_purge<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(person): P<String>,
    J(body): J<PersonPurgeRequest>,
) -> Response {
    reply("person_purge", s.person_purge(ctx, person, body).await)
}

async fn h_workspaces_exists<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    J(body): J<WorkspacesExistsRequest>,
) -> Response {
    reply("workspaces_exists", s.workspaces_exists(ctx, body).await)
}

async fn h_legacy_import<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("legacy_import", s.legacy_import(ctx).await)
}

async fn h_ops_people<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<PeopleQuery>) -> Response {
    reply("ops_people", s.ops_people(ctx, query).await)
}

async fn h_ops_view_as<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<ViewAsLogQuery>) -> Response {
    reply("ops_view_as", s.ops_view_as(ctx, query).await)
}

async fn h_list_schedules<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("list_schedules", s.list_schedules(ctx).await)
}

async fn h_create_schedule<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<CreateSchedule>) -> Response {
    reply("create_schedule", s.create_schedule(ctx, body).await)
}

async fn h_get_schedule<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(schedule_id): P<String>) -> Response {
    reply("get_schedule", s.get_schedule(ctx, schedule_id).await)
}

async fn h_update_schedule<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(schedule_id): P<String>,
    J(body): J<UpdateSchedule>,
) -> Response {
    reply("update_schedule", s.update_schedule(ctx, schedule_id, body).await)
}

async fn h_delete_schedule<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(schedule_id): P<String>) -> Response {
    reply_empty(s.delete_schedule(ctx, schedule_id).await)
}

async fn h_withdraw_pending<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P((id, client_message_id)): P<(Uuid, String)>,
) -> Response {
    reply("withdraw_pending", s.withdraw_pending(ctx, id, client_message_id).await)
}

async fn h_conversation_commands<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("conversation_commands", s.conversation_commands(ctx, id).await)
async fn h_conversation_tasks<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("conversation_tasks", s.conversation_tasks(ctx, id).await)
}

async fn h_stop_task<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P((id, task_id)): P<(Uuid, String)>,
    J(_): J<Empty>,
) -> Response {
    reply_unit("stop_task", s.stop_task(ctx, id, task_id).await)
}

async fn h_conversation_schedules<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("conversation_schedules", s.conversation_schedules(ctx, id).await)
}

async fn h_admin_schedules<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("admin_schedules", s.admin_schedules(ctx).await)
}

async fn h_admin_schedules_pause<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, J(body): J<PauseAll>) -> Response {
    reply("admin_schedules_pause", s.admin_schedules_pause(ctx, body).await)
}

async fn h_ops_stats<S: Server>(State(s): State<Arc<S>>, ctx: Ctx) -> Response {
    reply("ops_stats", s.ops_stats(ctx).await)
}

async fn h_admin_conversations<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, Q(query): Q<AdminListQuery>) -> Response {
    reply("admin_conversations", s.admin_conversations(ctx, query).await)
}

async fn h_admin_conversation<S: Server>(State(s): State<Arc<S>>, ctx: Ctx, P(id): P<Uuid>) -> Response {
    reply("admin_conversation", s.admin_conversation(ctx, id).await)
}

async fn h_admin_events<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<EventsQuery>,
) -> Response {
    reply("admin_events", s.admin_events(ctx, id, query).await)
}

async fn h_admin_timeline<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<TimelineQuery>,
) -> Response {
    reply("admin_timeline", s.admin_timeline(ctx, id, query).await)
}

async fn h_admin_transcript<S: Server>(
    State(s): State<Arc<S>>,
    ctx: Ctx,
    P(id): P<Uuid>,
    Q(query): Q<TranscriptQuery>,
) -> Response {
    reply("admin_transcript", s.admin_transcript(ctx, id, query).await)
}

/// Serve `server` on every route of the contract (fallback: 404 `not_found` as the envelope).
pub fn router<S: Server>(server: Arc<S>) -> Router {
    Router::new()
        .route(
            CONVERSATIONS,
            get(h_list_conversations::<S>).post(h_create_conversation::<S>),
        )
        .route(
            CONVERSATION,
            get(h_get_conversation::<S>)
                .patch(h_rename_conversation::<S>)
                .delete(h_purge_conversation::<S>),
        )
        .route(CREATION, get(h_get_creation::<S>))
        .route(CONVERSATION_DELETE, post(h_delete_conversation::<S>))
        .route(CONVERSATION_RESTORE, post(h_restore_conversation::<S>))
        .route(TURNS, post(h_send_turn::<S>))
        .route(CANCEL, post(h_cancel_turn::<S>))
        .route(TIMELINE, get(h_timeline::<S>))
        .route(LIVE, get(h_live::<S>))
        .route(PROMPTS, get(h_prompts::<S>))
        .route(SEARCH, get(h_search::<S>))
        .route(TRANSCRIPT, get(h_transcript::<S>))
        .route(ASK_ANSWER, post(h_answer_ask::<S>))
        .route(CHANGES, get(h_changes::<S>))
        .route(UPLOADS, post(h_upload::<S>))
        .route(FILES_RAW, get(h_files_raw::<S>))
        .route(FILES, get(h_files_panel::<S>))
        .route(BLOB, get(h_blob::<S>))
        .route(CONVERSATION_SHARES, get(h_list_shares::<S>).post(h_create_share::<S>))
        .route(SHARE, delete(h_revoke_share::<S>).get(h_get_share::<S>))
        .route(SHARE_FILES_RAW, get(h_share_files_raw::<S>))
        .route(SHARE_CONTINUE, post(h_continue_share::<S>))
        .route(VIEW_AS, post(h_view_as::<S>))
        .route(UPLOAD_LEGACY_PATH, put(h_upload_legacy_path::<S>))
        .route(SANDBOX_LEASE, post(h_lease::<S>))
        .route(SANDBOX_CC_TREE, get(h_cc_tree::<S>))
        .route(SANDBOX_CC_FILE, get(h_cc_file::<S>))
        .route(SANDBOX_CC_WRITE, post(h_cc_write::<S>))
        .route(SANDBOX_CC_TRUNCATE, post(h_cc_truncate::<S>))
        .route(SANDBOX_CC_RENAME, post(h_cc_rename::<S>))
        .route(SANDBOX_CC_UNLINK, post(h_cc_unlink::<S>))
        .route(SANDBOX_CC_MKDIR, post(h_cc_mkdir::<S>))
        .route(SANDBOX_CC_RMDIR, post(h_cc_rmdir::<S>))
        .route(SANDBOX_CC_SETATTR, post(h_cc_setattr::<S>))
        .route(SANDBOX_COMMANDS, get(h_commands::<S>))
        .route(SANDBOX_COMMAND_ACK, post(h_ack_command::<S>))
        .route(SANDBOX_TURNS, post(h_agent_turn::<S>))
        .route(SANDBOX_TURN_STATE, post(h_turn_state::<S>))
        .route(SANDBOX_TURN_LIVE, post(h_live_frames::<S>))
        .route(SANDBOX_TASKS, post(h_tasks::<S>))
        .route(SANDBOX_ASKS, post(h_ask_open::<S>))
        .route(SANDBOX_ASK_RESOLVED, post(h_ask_resolved::<S>))
        .route(SANDBOX_HEARTBEAT, post(h_heartbeat::<S>))
        .route(SANDBOX_BLOB, get(h_sandbox_blob_get::<S>).put(h_sandbox_blob_put::<S>))
        .route(SANDBOX_SENT_FILES, post(h_sent_files::<S>))
        .route(SANDBOX_SENT_FILES_COMMIT, post(h_sent_files_commit::<S>))
        .route(
            SANDBOX_SCHEDULES,
            get(h_list_schedules::<S>).post(h_create_schedule::<S>),
        )
        .route(
            SANDBOX_SCHEDULE,
            get(h_get_schedule::<S>)
                .patch(h_update_schedule::<S>)
                .delete(h_delete_schedule::<S>),
        )
        .route(CONVERSATION_SCHEDULES, get(h_conversation_schedules::<S>))
        .route(CONVERSATION_COMMANDS, get(h_conversation_commands::<S>))
        .route(CONVERSATION_PENDING, delete(h_withdraw_pending::<S>))
        .route(CONVERSATION_TASKS, get(h_conversation_tasks::<S>))
        .route(CONVERSATION_TASK_STOP, post(h_stop_task::<S>))
        .route(ADMIN_SCHEDULES, get(h_admin_schedules::<S>))
        .route(ADMIN_SCHEDULES_PAUSE, put(h_admin_schedules_pause::<S>))
        .route(HERMIT_PERSON_PURGE, post(h_person_purge::<S>))
        .route(HERMIT_CONVERSATIONS, post(h_hermit_create_conversation::<S>))
        .route(HERMIT_CONVERSATION, get(h_hermit_conversation::<S>))
        .route(HERMIT_CONVERSATION_CLOSED, put(h_hermit_close_conversation::<S>))
        .route(HERMIT_CONVERSATION_DELETE, post(h_hermit_delete_conversation::<S>))
        .route(HERMIT_CONVERSATION_TURNS, post(h_hermit_send_turn::<S>))
        .route(HERMIT_WORKSPACES_EXISTS, post(h_workspaces_exists::<S>))
        .route(HERMIT_LEGACY_IMPORT, get(h_legacy_import::<S>))
        .route(OPS_PEOPLE, get(h_ops_people::<S>))
        .route(OPS_VIEW_AS, get(h_ops_view_as::<S>))
        .route(OPS_STATS, get(h_ops_stats::<S>))
        .route(ADMIN_CONVERSATIONS, get(h_admin_conversations::<S>))
        .route(ADMIN_CONVERSATION, get(h_admin_conversation::<S>))
        .route(ADMIN_EVENTS, get(h_admin_events::<S>))
        .route(ADMIN_TIMELINE, get(h_admin_timeline::<S>))
        .route(ADMIN_TRANSCRIPT, get(h_admin_transcript::<S>))
        .fallback(|| async { ApiError::not_found("no such route").into_response() })
        .with_state(server)
}
