//! Golden fixtures (`fixtures/`): every route of magpie 16 with its success and error bodies, every event kind,
//! every row kind, every change type and every command type round-trips through the contract types unchanged.
//! The client and server test (`client_server.rs`) drives the same route fixtures over HTTP.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::implicit_clone
)] // tests

mod common;

use common::*;
use magpie_api::Empty;
use magpie_api::changes::*;
use magpie_api::conversations::*;
use magpie_api::events::*;
use magpie_api::files::*;
use magpie_api::legacy::*;
use magpie_api::live::*;
use magpie_api::ops::*;
use magpie_api::rows::*;
use magpie_api::sandbox::*;
use magpie_api::schedules::*;
use magpie_api::shares::*;
use magpie_api::turns::*;
use magpie_api::uploads::*;

#[allow(clippy::too_many_lines)]
fn check_route(id: &str, f: &serde_json::Value) {
    match id {
        "list_conversations" => check::<Json<ListQuery>, NoBody, Json<ListResponse>>(id, f),
        "get_conversation" => check::<NoQuery, NoBody, Json<Conversation>>(id, f),
        "create_conversation" => check::<NoQuery, Json<CreateRequest>, Json<CreateResponse>>(id, f),
        "get_creation" => check::<NoQuery, NoBody, Json<CreateResponse>>(id, f),
        "rename_conversation" => check::<NoQuery, Json<RenameRequest>, Json<Conversation>>(id, f),
        "delete_conversation" => check::<NoQuery, Json<Empty>, Json<Conversation>>(id, f),
        "restore_conversation" => check::<NoQuery, Json<Empty>, Json<Conversation>>(id, f),
        "purge_conversation" => check::<NoQuery, NoBody, NoContent>(id, f),
        "send_turn" => check::<NoQuery, Json<SendRequest>, Json<SendResponse>>(id, f),
        "cancel_turn" => check::<NoQuery, Json<Empty>, Json<CancelResponse>>(id, f),
        "timeline" => check::<Json<TimelineQuery>, NoBody, Json<TimelinePage>>(id, f),
        "live" => check::<NoQuery, NoBody, OptionJson<LiveState>>(id, f),
        "prompts" => check::<NoQuery, NoBody, Json<PromptsResponse>>(id, f),
        "search" => check::<Json<SearchQuery>, NoBody, Json<SearchResponse>>(id, f),
        "transcript" => check::<Json<TranscriptQuery>, NoBody, Json<TranscriptPage>>(id, f),
        "answer_ask" => check::<NoQuery, Json<AgentPermissionResponse>, Json<Empty>>(id, f),
        "changes" => check::<Json<ChangesQuery>, NoBody, Json<ChangesResponse>>(id, f),
        "upload" => check::<NoQuery, RawBody, Json<UploadResponse>>(id, f),
        "files_raw" => check::<Json<RawFileQuery>, NoBody, RawBytes>(id, f),
        "files_panel" => check::<NoQuery, NoBody, Json<FilesPanel>>(id, f),
        "blob" => check::<Json<BlobQuery>, NoBody, RawBytes>(id, f),
        "list_shares" => check::<NoQuery, NoBody, Json<SharesList>>(id, f),
        "create_share" => check::<NoQuery, Json<Empty>, Json<ShareRef>>(id, f),
        "revoke_share" => check::<NoQuery, NoBody, NoContent>(id, f),
        "get_share" => check::<NoQuery, NoBody, Json<ShareView>>(id, f),
        "share_files_raw" => check::<Json<RawFileQuery>, NoBody, RawBytes>(id, f),
        "continue_share" => check::<NoQuery, Json<Empty>, Json<ContinueResponse>>(id, f),
        "view_as" => check::<NoQuery, Json<ViewAsRequest>, Json<Empty>>(id, f),
        "upload_legacy_path" => check::<NoQuery, Json<LegacyPathRequest>, NoContent>(id, f),
        "lease" => check::<NoQuery, Json<Empty>, Json<LeaseResponse>>(id, f),
        "cc_tree" => check::<NoQuery, NoBody, Json<CcTree>>(id, f),
        "cc_file" => check::<Json<CcFileQuery>, NoBody, RawBytes>(id, f),
        "cc_write" => check::<Json<CcWriteQuery>, RawBody, Json<CcSizeVersion>>(id, f),
        "cc_truncate" => check::<NoQuery, Json<CcTruncate>, Json<CcSizeVersion>>(id, f),
        "cc_rename" => check::<NoQuery, Json<CcRename>, Json<CcVersion>>(id, f),
        "cc_unlink" => check::<NoQuery, Json<CcPathOp>, NoContent>(id, f),
        "cc_mkdir" => check::<NoQuery, Json<CcMkdir>, NoContent>(id, f),
        "cc_rmdir" => check::<NoQuery, Json<CcPathOp>, NoContent>(id, f),
        "cc_setattr" => check::<NoQuery, Json<CcSetattr>, Json<CcVersion>>(id, f),
        "commands" => check::<Json<CommandsQuery>, NoBody, Json<CommandsResponse>>(id, f),
        "ack_command" => check::<NoQuery, Json<AckRequest>, NoContent>(id, f),
        "agent_turn" => check::<NoQuery, Json<AgentTurnRequest>, Json<Empty>>(id, f),
        "turn_state" => check::<NoQuery, Json<TurnStateReport>, NoContent>(id, f),
        "live_frames" => check::<NoQuery, Json<LiveFramesRequest>, NoContent>(id, f),
        "tasks" => check::<NoQuery, Json<TasksReport>, NoContent>(id, f),
        "ask_open" => check::<NoQuery, Json<AskReport>, Json<Empty>>(id, f),
        "ask_resolved" => check::<NoQuery, Json<AskResolvedReport>, NoContent>(id, f),
        "heartbeat" => check::<NoQuery, Json<Heartbeat>, NoContent>(id, f),
        "sandbox_blob_get" => check::<Json<SandboxBlobQuery>, NoBody, RawBytes>(id, f),
        "sandbox_blob_put" => check::<NoQuery, RawBody, Json<Empty>>(id, f),
        "sent_files" => check::<NoQuery, Json<SentFilesRequest>, Json<SentFilesResponse>>(id, f),
        "sent_files_commit" => check::<NoQuery, Json<Empty>, Json<SentFilesCommitted>>(id, f),
        "person_purge" => check::<NoQuery, Json<PersonPurgeRequest>, Json<PersonPurgeResponse>>(id, f),
        "hermit_create_conversation" => check::<NoQuery, Json<HermitCreateRequest>, Json<CreateResponse>>(id, f),
        "hermit_send_turn" => check::<NoQuery, Json<HermitSendRequest>, Json<SendResponse>>(id, f),
        "workspaces_exists" => check::<NoQuery, Json<WorkspacesExistsRequest>, Json<WorkspacesExistsResponse>>(id, f),
        "legacy_import" => check::<NoQuery, NoBody, Json<LegacyImportStatus>>(id, f),
        "ops_people" => check::<Json<PeopleQuery>, NoBody, Json<PeopleResponse>>(id, f),
        "ops_view_as" => check::<Json<ViewAsLogQuery>, NoBody, Json<ViewAsLog>>(id, f),
        "ops_stats" => check::<NoQuery, NoBody, Json<OpsStats>>(id, f),
        "admin_conversations" => check::<Json<AdminListQuery>, NoBody, Json<AdminConversationList>>(id, f),
        "admin_conversation" => check::<NoQuery, NoBody, Json<AdminConversation>>(id, f),
        "admin_events" => check::<Json<EventsQuery>, NoBody, Json<EventsPage>>(id, f),
        "admin_timeline" => check::<Json<TimelineQuery>, NoBody, Json<TimelinePage>>(id, f),
        "admin_transcript" => check::<Json<TranscriptQuery>, NoBody, Json<TranscriptPage>>(id, f),
        "list_schedules" => check::<NoQuery, NoBody, Json<SchedulesList>>(id, f),
        "create_schedule" => check::<NoQuery, Json<CreateSchedule>, Json<Schedule>>(id, f),
        "get_schedule" => check::<NoQuery, NoBody, Json<Schedule>>(id, f),
        "update_schedule" => check::<NoQuery, Json<UpdateSchedule>, Json<Schedule>>(id, f),
        "delete_schedule" => check::<NoQuery, NoBody, NoContent>(id, f),
        "conversation_schedules" => check::<NoQuery, NoBody, Json<SchedulesList>>(id, f),
        "admin_schedules" => check::<NoQuery, NoBody, Json<AdminSchedules>>(id, f),
        "admin_schedules_pause" => check::<NoQuery, Json<PauseAll>, Json<AdminSchedules>>(id, f),
        other => panic!("no typed check for route {other}"),
    }
}

#[test]
fn every_route_has_a_fixture_and_it_round_trips() {
    let files = fixture_files("routes");
    let ids: Vec<&str> = magpie_api::routes::ROUTES.iter().map(|r| r.id).collect();
    for (name, _) in &files {
        assert!(
            ids.contains(&name.as_str()),
            "fixtures/routes/{name}.json names no route"
        );
    }
    for id in ids {
        let f = route_fixture(id);
        assert_eq!(f["route"], id, "fixtures/routes/{id}.json: \"route\"");
        check_params(id, &f);
        check_route(id, &f);
    }
}

#[test]
fn every_event_kind_has_a_fixture() {
    let mut kinds = std::collections::BTreeSet::new();
    for (name, v) in fixture_files("events") {
        let e: Event = round_trip(&v, &name);
        kinds.insert(e.body.kind().unwrap_or_else(|| panic!("{name}: unknown kind")));
    }
    let want: std::collections::BTreeSet<&str> = EVENT_KINDS.iter().copied().collect();
    assert_eq!(kinds, want);
}

#[test]
fn every_row_kind_has_a_fixture() {
    let mut kinds = std::collections::BTreeSet::new();
    for (name, v) in fixture_files("rows") {
        let r: Row = round_trip(&v, &name);
        assert_eq!(
            v["sourceSeqRanges"],
            serde_json::to_value(seq_ranges(&r.seqs)).unwrap(),
            "{name}"
        );
        assert_eq!(r.updated, r.seq_end != r.seq_start, "{name}: updated");
        kinds.insert(r.item.kind().unwrap_or_else(|| panic!("{name}: unknown kind")));
    }
    let want: std::collections::BTreeSet<&str> = ROW_KINDS.iter().copied().collect();
    assert_eq!(kinds, want);
}

#[test]
fn every_change_type_has_a_fixture() {
    let mut types = std::collections::BTreeSet::new();
    for (name, v) in fixture_files("changes") {
        let c: Change = round_trip(&v, &name);
        types.insert(c.body.type_name().unwrap_or_else(|| panic!("{name}: unknown type")));
    }
    let want: std::collections::BTreeSet<&str> = CHANGE_TYPES.iter().copied().collect();
    assert_eq!(types, want);
}

#[test]
fn every_command_type_has_a_fixture() {
    let mut types = std::collections::BTreeSet::new();
    for (name, v) in fixture_files("commands") {
        let c: Command = round_trip(&v, &name);
        types.insert(c.body.type_name().unwrap_or_else(|| panic!("{name}: unknown type")));
    }
    let want: std::collections::BTreeSet<&str> = COMMAND_TYPES.iter().copied().collect();
    assert_eq!(types, want);
}

#[test]
fn live_state_fixtures_round_trip() {
    for (name, v) in fixture_files("live") {
        let _: LiveState = round_trip(&v, &name);
    }
}
