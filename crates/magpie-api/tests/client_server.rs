//! The client and the server router agree: every route fixture is sent by [`magpie_api::client::Client`] to a
//! [`magpie_api::server::Server`] that checks what it received against the fixture and answers the fixture's first
//! success response (or, in the second pass, its first error response), which the client must read back unchanged.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::implicit_clone,
    clippy::needless_borrows_for_generic_args
)] // tests

mod common;

use std::sync::Arc;

use aviary_core::http::ApiError;
use axum::body::Body;
use axum::response::Response;
use common::*;
use magpie_api::changes::*;
use magpie_api::client::{Client, ClientError};
use magpie_api::conversations::*;
use magpie_api::events::*;
use magpie_api::files::*;
use magpie_api::legacy::*;
use magpie_api::live::*;
use magpie_api::ops::*;
use magpie_api::rows::*;
use magpie_api::sandbox::*;
use magpie_api::server::{Ctx, Server, router};
use magpie_api::shares::*;
use magpie_api::turns::*;
use magpie_api::uploads::*;
use magpie_api::{Admitted, Empty};
use serde_json::{Value, json};
use uuid::Uuid;

#[allow(clippy::too_many_lines)]
impl Server for FixtureServer {
    async fn list_conversations(&self, ctx: Ctx, query: ListQuery) -> Result<ListResponse, ApiError> {
        self.received(
            "list_conversations",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("list_conversations")
    }

    async fn get_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.received("get_conversation", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_json("get_conversation")
    }

    async fn create_conversation(&self, ctx: Ctx, body: CreateRequest) -> Result<Admitted<CreateResponse>, ApiError> {
        self.received(
            "create_conversation",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_admitted("create_conversation")
    }

    async fn get_creation(&self, ctx: Ctx, idempotency_key: String) -> Result<CreateResponse, ApiError> {
        self.received(
            "get_creation",
            &ctx,
            &json!({"idempotencyKey": idempotency_key.to_string()}),
            None,
            None,
        );
        self.answer_json("get_creation")
    }

    async fn rename_conversation(&self, ctx: Ctx, id: Uuid, body: RenameRequest) -> Result<Conversation, ApiError> {
        self.received(
            "rename_conversation",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("rename_conversation")
    }

    async fn delete_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.received(
            "delete_conversation",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("delete_conversation")
    }

    async fn restore_conversation(&self, ctx: Ctx, id: Uuid) -> Result<Conversation, ApiError> {
        self.received(
            "restore_conversation",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("restore_conversation")
    }

    async fn purge_conversation(&self, ctx: Ctx, id: Uuid) -> Result<(), ApiError> {
        self.received("purge_conversation", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_empty("purge_conversation")
    }

    async fn send_turn(&self, ctx: Ctx, id: Uuid, body: SendRequest) -> Result<Admitted<SendResponse>, ApiError> {
        self.received(
            "send_turn",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_admitted("send_turn")
    }

    async fn cancel_turn(&self, ctx: Ctx, id: Uuid) -> Result<CancelResponse, ApiError> {
        self.received(
            "cancel_turn",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("cancel_turn")
    }

    async fn timeline(&self, ctx: Ctx, id: Uuid, query: TimelineQuery) -> Result<TimelinePage, ApiError> {
        self.received(
            "timeline",
            &ctx,
            &json!({"id": id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("timeline")
    }

    async fn live(&self, ctx: Ctx, id: Uuid) -> Result<Option<LiveState>, ApiError> {
        self.received("live", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_option("live")
    }

    async fn prompts(&self, ctx: Ctx, id: Uuid) -> Result<PromptsResponse, ApiError> {
        self.received("prompts", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_json("prompts")
    }

    async fn search(&self, ctx: Ctx, id: Uuid, query: SearchQuery) -> Result<SearchResponse, ApiError> {
        self.received(
            "search",
            &ctx,
            &json!({"id": id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("search")
    }

    async fn answer_ask(
        &self,
        ctx: Ctx,
        id: Uuid,
        ask_id: String,
        body: AgentPermissionResponse,
    ) -> Result<(), ApiError> {
        self.received(
            "answer_ask",
            &ctx,
            &json!({"id": id.to_string(), "askId": ask_id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_unit("answer_ask")
    }

    async fn changes(&self, ctx: Ctx, query: ChangesQuery) -> Result<ChangesResponse, ApiError> {
        self.received(
            "changes",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("changes")
    }

    async fn upload(&self, ctx: Ctx, meta: UploadMeta, body: Body) -> Result<UploadResponse, ApiError> {
        self.check_upload(&meta);
        self.check_bytes("upload", body).await;
        self.received("upload", &ctx, &json!({}), None, None);
        self.answer_json("upload")
    }

    async fn files_raw(&self, ctx: Ctx, id: Uuid, query: RawFileQuery) -> Result<Response, ApiError> {
        self.received(
            "files_raw",
            &ctx,
            &json!({"id": id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_raw("files_raw")
    }

    async fn files_panel(&self, ctx: Ctx, id: Uuid) -> Result<FilesPanel, ApiError> {
        self.received("files_panel", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_json("files_panel")
    }

    async fn blob(&self, ctx: Ctx, blob_id: String, query: BlobQuery) -> Result<Response, ApiError> {
        self.received(
            "blob",
            &ctx,
            &json!({"blobId": blob_id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_raw("blob")
    }

    async fn list_shares(&self, ctx: Ctx, id: Uuid) -> Result<SharesList, ApiError> {
        self.received("list_shares", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_json("list_shares")
    }

    async fn create_share(&self, ctx: Ctx, id: Uuid) -> Result<ShareRef, ApiError> {
        self.received(
            "create_share",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("create_share")
    }

    async fn revoke_share(&self, ctx: Ctx, share_id: String) -> Result<(), ApiError> {
        self.received(
            "revoke_share",
            &ctx,
            &json!({"shareId": share_id.to_string()}),
            None,
            None,
        );
        self.answer_empty("revoke_share")
    }

    async fn get_share(&self, ctx: Ctx, share_id: String) -> Result<ShareView, ApiError> {
        self.received("get_share", &ctx, &json!({"shareId": share_id.to_string()}), None, None);
        self.answer_json("get_share")
    }

    async fn share_files_raw(&self, ctx: Ctx, share_id: String, query: RawFileQuery) -> Result<Response, ApiError> {
        self.received(
            "share_files_raw",
            &ctx,
            &json!({"shareId": share_id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_raw("share_files_raw")
    }

    async fn continue_share(&self, ctx: Ctx, share_id: String) -> Result<ContinueResponse, ApiError> {
        self.received(
            "continue_share",
            &ctx,
            &json!({"shareId": share_id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("continue_share")
    }

    async fn view_as(&self, ctx: Ctx, body: ViewAsRequest) -> Result<(), ApiError> {
        self.received(
            "view_as",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_unit("view_as")
    }

    async fn lease(&self, ctx: Ctx) -> Result<LeaseResponse, ApiError> {
        self.received("lease", &ctx, &json!({}), None, Some(json!({})));
        self.answer_json("lease")
    }

    async fn cc_tree(&self, ctx: Ctx) -> Result<CcTree, ApiError> {
        self.received("cc_tree", &ctx, &json!({}), None, None);
        self.answer_json("cc_tree")
    }

    async fn cc_file(&self, ctx: Ctx, query: CcFileQuery) -> Result<Response, ApiError> {
        self.received(
            "cc_file",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_raw("cc_file")
    }

    async fn cc_write(&self, ctx: Ctx, query: CcWriteQuery, body: Body) -> Result<CcSizeVersion, ApiError> {
        self.check_bytes("cc_write", body).await;
        self.received(
            "cc_write",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("cc_write")
    }

    async fn cc_truncate(&self, ctx: Ctx, body: CcTruncate) -> Result<CcSizeVersion, ApiError> {
        self.received(
            "cc_truncate",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("cc_truncate")
    }

    async fn cc_rename(&self, ctx: Ctx, body: CcRename) -> Result<CcVersion, ApiError> {
        self.received(
            "cc_rename",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("cc_rename")
    }

    async fn cc_unlink(&self, ctx: Ctx, body: CcPathOp) -> Result<(), ApiError> {
        self.received(
            "cc_unlink",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("cc_unlink")
    }

    async fn cc_mkdir(&self, ctx: Ctx, body: CcMkdir) -> Result<(), ApiError> {
        self.received(
            "cc_mkdir",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("cc_mkdir")
    }

    async fn cc_rmdir(&self, ctx: Ctx, body: CcPathOp) -> Result<(), ApiError> {
        self.received(
            "cc_rmdir",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("cc_rmdir")
    }

    async fn cc_setattr(&self, ctx: Ctx, body: CcSetattr) -> Result<CcVersion, ApiError> {
        self.received(
            "cc_setattr",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("cc_setattr")
    }

    async fn commands(&self, ctx: Ctx, query: CommandsQuery) -> Result<CommandsResponse, ApiError> {
        self.received(
            "commands",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("commands")
    }

    async fn ack_command(&self, ctx: Ctx, cmd_seq: u64, body: AckRequest) -> Result<(), ApiError> {
        self.received(
            "ack_command",
            &ctx,
            &json!({"cmdSeq": cmd_seq.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("ack_command")
    }

    async fn agent_turn(&self, ctx: Ctx, body: AgentTurnRequest) -> Result<Admitted<Empty>, ApiError> {
        self.received(
            "agent_turn",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_admitted("agent_turn")
    }

    async fn turn_state(&self, ctx: Ctx, turn_id: Uuid, body: TurnStateReport) -> Result<(), ApiError> {
        self.received(
            "turn_state",
            &ctx,
            &json!({"turnId": turn_id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("turn_state")
    }

    async fn live_frames(&self, ctx: Ctx, turn_id: Uuid, body: LiveFramesRequest) -> Result<(), ApiError> {
        self.received(
            "live_frames",
            &ctx,
            &json!({"turnId": turn_id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("live_frames")
    }

    async fn tasks(&self, ctx: Ctx, body: TasksReport) -> Result<(), ApiError> {
        self.received(
            "tasks",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("tasks")
    }

    async fn ask_open(&self, ctx: Ctx, body: AskReport) -> Result<(), ApiError> {
        self.received(
            "ask_open",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_unit("ask_open")
    }

    async fn ask_resolved(&self, ctx: Ctx, ask_id: String, body: AskResolvedReport) -> Result<(), ApiError> {
        self.received(
            "ask_resolved",
            &ctx,
            &json!({"askId": ask_id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("ask_resolved")
    }

    async fn heartbeat(&self, ctx: Ctx, body: Heartbeat) -> Result<(), ApiError> {
        self.received(
            "heartbeat",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("heartbeat")
    }

    async fn sandbox_blob_get(&self, ctx: Ctx, sha: String, query: SandboxBlobQuery) -> Result<Response, ApiError> {
        self.received(
            "sandbox_blob_get",
            &ctx,
            &json!({"sha": sha.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_raw("sandbox_blob_get")
    }

    async fn sandbox_blob_put(&self, ctx: Ctx, sha: String, body: Body) -> Result<Admitted<Empty>, ApiError> {
        self.check_bytes("sandbox_blob_put", body).await;
        self.received("sandbox_blob_put", &ctx, &json!({"sha": sha.to_string()}), None, None);
        self.answer_admitted("sandbox_blob_put")
    }

    async fn sent_files(&self, ctx: Ctx, body: SentFilesRequest) -> Result<SentFilesResponse, ApiError> {
        self.received(
            "sent_files",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("sent_files")
    }

    async fn sent_files_commit(&self, ctx: Ctx, tool_use_id: String) -> Result<SentFilesCommitted, ApiError> {
        self.received(
            "sent_files_commit",
            &ctx,
            &json!({"toolUseId": tool_use_id.to_string()}),
            None,
            Some(json!({})),
        );
        self.answer_json("sent_files_commit")
    }

    async fn person_purge(
        &self,
        ctx: Ctx,
        person: String,
        body: PersonPurgeRequest,
    ) -> Result<PersonPurgeResponse, ApiError> {
        self.received(
            "person_purge",
            &ctx,
            &json!({"person": person.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("person_purge")
    }

    async fn workspaces_exists(
        &self,
        ctx: Ctx,
        body: WorkspacesExistsRequest,
    ) -> Result<WorkspacesExistsResponse, ApiError> {
        self.received(
            "workspaces_exists",
            &ctx,
            &json!({}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("workspaces_exists")
    }

    async fn legacy_import(&self, ctx: Ctx) -> Result<LegacyImportStatus, ApiError> {
        self.received("legacy_import", &ctx, &json!({}), None, None);
        self.answer_json("legacy_import")
    }

    async fn ops_people(&self, ctx: Ctx, query: PeopleQuery) -> Result<PeopleResponse, ApiError> {
        self.received(
            "ops_people",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("ops_people")
    }

    async fn ops_view_as(&self, ctx: Ctx, query: ViewAsLogQuery) -> Result<ViewAsLog, ApiError> {
        self.received(
            "ops_view_as",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("ops_view_as")
    }

    async fn ops_stats(&self, ctx: Ctx) -> Result<OpsStats, ApiError> {
        self.received("ops_stats", &ctx, &json!({}), None, None);
        self.answer_json("ops_stats")
    }

    async fn admin_conversations(&self, ctx: Ctx, query: AdminListQuery) -> Result<AdminConversationList, ApiError> {
        self.received(
            "admin_conversations",
            &ctx,
            &json!({}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("admin_conversations")
    }

    async fn admin_conversation(&self, ctx: Ctx, id: Uuid) -> Result<AdminConversation, ApiError> {
        self.received("admin_conversation", &ctx, &json!({"id": id.to_string()}), None, None);
        self.answer_json("admin_conversation")
    }

    async fn admin_events(&self, ctx: Ctx, id: Uuid, query: EventsQuery) -> Result<EventsPage, ApiError> {
        self.received(
            "admin_events",
            &ctx,
            &json!({"id": id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("admin_events")
    }

    async fn admin_timeline(&self, ctx: Ctx, id: Uuid, query: TimelineQuery) -> Result<TimelinePage, ApiError> {
        self.received(
            "admin_timeline",
            &ctx,
            &json!({"id": id.to_string()}),
            Some(serde_json::to_value(&query).unwrap()),
            None,
        );
        self.answer_json("admin_timeline")
    }

    async fn legacy_ticket(&self, ctx: Ctx, id: Uuid, body: TicketRequest) -> Result<TicketResponse, ApiError> {
        self.received(
            "legacy_ticket",
            &ctx,
            &json!({"id": id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("legacy_ticket")
    }

    async fn legacy_ticket_done(&self, ctx: Ctx, ticket: Uuid, body: TicketDone) -> Result<(), ApiError> {
        self.received(
            "legacy_ticket_done",
            &ctx,
            &json!({"ticket": ticket.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("legacy_ticket_done")
    }

    async fn legacy_ticket_claim(&self, ctx: Ctx, ticket: Uuid, body: TicketClaim) -> Result<TicketClaimed, ApiError> {
        self.received(
            "legacy_ticket_claim",
            &ctx,
            &json!({"ticket": ticket.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_json("legacy_ticket_claim")
    }

    async fn upload_legacy_path(&self, ctx: Ctx, upload_id: Uuid, body: LegacyPathRequest) -> Result<(), ApiError> {
        self.received(
            "upload_legacy_path",
            &ctx,
            &json!({"uploadId": upload_id.to_string()}),
            None,
            Some(serde_json::to_value(&body).unwrap()),
        );
        self.answer_empty("upload_legacy_path")
    }

    async fn legacy_window(&self, ctx: Ctx) -> Result<WindowState, ApiError> {
        self.received("legacy_window", &ctx, &json!({}), None, None);
        self.answer_json("legacy_window")
    }

    async fn upload_raw(&self, ctx: Ctx, upload_id: Uuid) -> Result<Response, ApiError> {
        self.received(
            "upload_raw",
            &ctx,
            &json!({"uploadId": upload_id.to_string()}),
            None,
            None,
        );
        self.answer_raw("upload_raw")
    }
}

#[allow(clippy::too_many_lines)]
async fn call(c: &Client, id: &str) -> Result<Value, ClientError> {
    let f = route_fixture(id);
    match id {
        "list_conversations" => c
            .person(PERSON)
            .list_conversations(&query::<ListQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "get_conversation" => c
            .person(PERSON)
            .get_conversation(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "create_conversation" => c
            .person(PERSON)
            .create_conversation(&request::<CreateRequest>(&f))
            .await
            .map(|a| json!({"new": a.is_new(), "body": a.into_inner()})),
        "get_creation" => c
            .person(PERSON)
            .get_creation(&param_str(&f, "idempotencyKey"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "rename_conversation" => c
            .person(PERSON)
            .rename_conversation(param_uuid(&f, "id"), &request::<RenameRequest>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "delete_conversation" => c
            .person(PERSON)
            .delete_conversation(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "restore_conversation" => c
            .person(PERSON)
            .restore_conversation(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "purge_conversation" => c
            .person(PERSON)
            .purge_conversation(param_uuid(&f, "id"))
            .await
            .map(|()| Value::Null),
        "send_turn" => c
            .person(PERSON)
            .send_turn(param_uuid(&f, "id"), &request::<SendRequest>(&f))
            .await
            .map(|a| json!({"new": a.is_new(), "body": a.into_inner()})),
        "cancel_turn" => c
            .person(PERSON)
            .cancel_turn(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "timeline" => c
            .person(PERSON)
            .timeline(param_uuid(&f, "id"), &query::<TimelineQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "live" => c
            .person(PERSON)
            .live(param_uuid(&f, "id"))
            .await
            .map(|v| v.map_or(Value::Null, |v| serde_json::to_value(v).unwrap())),
        "prompts" => c
            .person(PERSON)
            .prompts(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "search" => c
            .person(PERSON)
            .search(param_uuid(&f, "id"), &query::<SearchQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "answer_ask" => c
            .person(PERSON)
            .answer_ask(
                param_uuid(&f, "id"),
                &param_str(&f, "askId"),
                &request::<AgentPermissionResponse>(&f),
            )
            .await
            .map(|()| Value::Null),
        "changes" => c
            .person(PERSON)
            .changes(&query::<ChangesQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "upload" => c
            .person(PERSON)
            .upload(
                &header(&f, "aviary-upload-name"),
                UploadKind::parse(&header(&f, "aviary-upload-kind")).unwrap(),
                &header(&f, "content-type"),
                request_bytes(&f),
            )
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "files_raw" => {
            raw_value(
                c.person(PERSON)
                    .files_raw(param_uuid(&f, "id"), &query::<RawFileQuery>(&f), None)
                    .await,
            )
            .await
        }
        "files_panel" => c
            .person(PERSON)
            .files_panel(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "blob" => raw_value(c.blob(&param_str(&f, "blobId"), &query::<BlobQuery>(&f), None).await).await,
        "list_shares" => c
            .person(PERSON)
            .list_shares(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "create_share" => c
            .person(PERSON)
            .create_share(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "revoke_share" => c
            .person(PERSON)
            .revoke_share(&param_str(&f, "shareId"))
            .await
            .map(|()| Value::Null),
        "get_share" => c
            .person(PERSON)
            .get_share(&param_str(&f, "shareId"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "share_files_raw" => {
            raw_value(
                c.person(PERSON)
                    .share_files_raw(&param_str(&f, "shareId"), &query::<RawFileQuery>(&f), None)
                    .await,
            )
            .await
        }
        "continue_share" => c
            .person(PERSON)
            .continue_share(&param_str(&f, "shareId"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "view_as" => c.view_as(&request::<ViewAsRequest>(&f)).await.map(|()| Value::Null),
        "legacy_ticket" => c
            .person(PERSON)
            .legacy_ticket(param_uuid(&f, "id"), &request::<TicketRequest>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "legacy_ticket_done" => c
            .person(PERSON)
            .legacy_ticket_done(param_uuid(&f, "ticket"), &request::<TicketDone>(&f))
            .await
            .map(|()| Value::Null),
        "legacy_ticket_claim" => c
            .person(PERSON)
            .legacy_ticket_claim(param_uuid(&f, "ticket"), &request::<TicketClaim>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "upload_legacy_path" => c
            .person(PERSON)
            .upload_legacy_path(param_uuid(&f, "uploadId"), &request::<LegacyPathRequest>(&f))
            .await
            .map(|()| Value::Null),
        "legacy_window" => c
            .person(PERSON)
            .legacy_window()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "upload_raw" => raw_value(c.person(PERSON).upload_raw(param_uuid(&f, "uploadId")).await).await,
        "lease" => c
            .sandbox(Some(LEASE))
            .lease()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "cc_tree" => c
            .sandbox(Some(LEASE))
            .cc_tree()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "cc_file" => raw_value(c.sandbox(Some(LEASE)).cc_file(&query::<CcFileQuery>(&f)).await).await,
        "cc_write" => c
            .sandbox(Some(LEASE))
            .cc_write(&query::<CcWriteQuery>(&f), request_bytes(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "cc_truncate" => c
            .sandbox(Some(LEASE))
            .cc_truncate(&request::<CcTruncate>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "cc_rename" => c
            .sandbox(Some(LEASE))
            .cc_rename(&request::<CcRename>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "cc_unlink" => c
            .sandbox(Some(LEASE))
            .cc_unlink(&request::<CcPathOp>(&f))
            .await
            .map(|()| Value::Null),
        "cc_mkdir" => c
            .sandbox(Some(LEASE))
            .cc_mkdir(&request::<CcMkdir>(&f))
            .await
            .map(|()| Value::Null),
        "cc_rmdir" => c
            .sandbox(Some(LEASE))
            .cc_rmdir(&request::<CcPathOp>(&f))
            .await
            .map(|()| Value::Null),
        "cc_setattr" => c
            .sandbox(Some(LEASE))
            .cc_setattr(&request::<CcSetattr>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "commands" => c
            .sandbox(Some(LEASE))
            .commands(&query::<CommandsQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "ack_command" => c
            .sandbox(Some(LEASE))
            .ack_command(param_u64(&f, "cmdSeq"), &request::<AckRequest>(&f))
            .await
            .map(|()| Value::Null),
        "agent_turn" => c
            .sandbox(Some(LEASE))
            .agent_turn(&request::<AgentTurnRequest>(&f))
            .await
            .map(|a| json!({"new": a.is_new()})),
        "turn_state" => c
            .sandbox(Some(LEASE))
            .turn_state(param_uuid(&f, "turnId"), &request::<TurnStateReport>(&f))
            .await
            .map(|()| Value::Null),
        "live_frames" => c
            .sandbox(Some(LEASE))
            .live_frames(param_uuid(&f, "turnId"), &request::<LiveFramesRequest>(&f))
            .await
            .map(|()| Value::Null),
        "tasks" => c
            .sandbox(Some(LEASE))
            .tasks(&request::<TasksReport>(&f))
            .await
            .map(|()| Value::Null),
        "ask_open" => c
            .sandbox(Some(LEASE))
            .ask_open(&request::<AskReport>(&f))
            .await
            .map(|()| Value::Null),
        "ask_resolved" => c
            .sandbox(Some(LEASE))
            .ask_resolved(&param_str(&f, "askId"), &request::<AskResolvedReport>(&f))
            .await
            .map(|()| Value::Null),
        "heartbeat" => c
            .sandbox(Some(LEASE))
            .heartbeat(&request::<Heartbeat>(&f))
            .await
            .map(|()| Value::Null),
        "sandbox_blob_get" => {
            raw_value(
                c.sandbox(Some(LEASE))
                    .sandbox_blob_get(&param_str(&f, "sha"), &query::<SandboxBlobQuery>(&f))
                    .await,
            )
            .await
        }
        "sandbox_blob_put" => c
            .sandbox(Some(LEASE))
            .sandbox_blob_put(&param_str(&f, "sha"), request_bytes(&f))
            .await
            .map(|a| json!({"new": a.is_new()})),
        "sent_files" => c
            .sandbox(Some(LEASE))
            .sent_files(&request::<SentFilesRequest>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "sent_files_commit" => c
            .sandbox(Some(LEASE))
            .sent_files_commit(&param_str(&f, "toolUseId"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "person_purge" => c
            .service(Some(ADMIN))
            .person_purge(&param_str(&f, "person"), &request::<PersonPurgeRequest>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "workspaces_exists" => c
            .service(Some(ADMIN))
            .workspaces_exists(&request::<WorkspacesExistsRequest>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "legacy_import" => c
            .service(Some(ADMIN))
            .legacy_import()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "ops_people" => c
            .service(Some(ADMIN))
            .ops_people(&query::<PeopleQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "ops_view_as" => c
            .service(Some(ADMIN))
            .ops_view_as(&query::<ViewAsLogQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "ops_stats" => c
            .service(Some(ADMIN))
            .ops_stats()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_conversations" => c
            .service(Some(ADMIN))
            .admin_conversations(&query::<AdminListQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_conversation" => c
            .service(Some(ADMIN))
            .admin_conversation(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_events" => c
            .service(Some(ADMIN))
            .admin_events(param_uuid(&f, "id"), &query::<EventsQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_timeline" => c
            .service(Some(ADMIN))
            .admin_timeline(param_uuid(&f, "id"), &query::<TimelineQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        other => panic!("no client call for route {other}"),
    }
}

async fn serve(fail: bool) -> (Client, Arc<FixtureServer>) {
    let server = Arc::new(FixtureServer::new(fail));
    let app = router(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // reqwest is built without a default crypto provider (platform 3.9); the services install ring at start
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder().build().unwrap();
    (Client::new(http, format!("http://{addr}"), TOKEN), server)
}

#[tokio::test]
async fn every_route_round_trips_its_success_fixture() {
    let (client, server) = serve(false).await;
    for r in magpie_api::routes::ROUTES {
        let got = call(&client, r.id).await.unwrap_or_else(|e| panic!("{}: {e}", r.id));
        assert_eq!(
            got,
            expected_success(r.id),
            "{}: the client read back something else",
            r.id
        );
    }
    server.assert_saw_every_route();
}

#[tokio::test]
async fn every_route_round_trips_its_error_fixture() {
    let (client, _server) = serve(true).await;
    for r in magpie_api::routes::ROUTES {
        let (status, body) = first_error(r.id);
        match call(&client, r.id).await {
            Err(ClientError::Api(e)) => {
                assert_eq!(e.status, status, "{}", r.id);
                assert_eq!(serde_json::to_value(&e).unwrap(), body, "{}", r.id);
            }
            other => panic!("{}: expected the error fixture, got {other:?}", r.id),
        }
    }
}
