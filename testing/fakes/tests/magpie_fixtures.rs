//! fake-magpie passes magpie-api's golden route fixtures: every response of every route fixture (successes and
//! errors), scripted as the fake's answer, is read back unchanged by magpie-api's client, and the fake recorded the
//! fixture's path parameters, query, body and identity headers.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::needless_borrows_for_generic_args,
    clippy::implicit_clone
)] // tests

use std::collections::BTreeSet;

use aviary_fakes::script::Reply;
use aviary_fakes::{FakeMagpie, fixtures};
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
use magpie_api::schedules::*;
use magpie_api::turns::*;
use magpie_api::uploads::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

const PERSON: &str = "chan.tai.man@example.com";
const ADMIN: &str = "admin@example.com";
const LEASE: Uuid = Uuid::from_u128(0x7d2f_4e6a_8b9c_4d0e_a1f2_3b4c_5d6e_7f80);

fn route_fixture(id: &str) -> Value {
    fixtures::magpie_route(id)
}

fn param_str(f: &Value, k: &str) -> String {
    f["params"][k].as_str().unwrap().to_owned()
}

fn param_uuid(f: &Value, k: &str) -> Uuid {
    Uuid::parse_str(&param_str(f, k)).unwrap()
}

fn param_u64(f: &Value, k: &str) -> u64 {
    param_str(f, k).parse().unwrap()
}

fn query<T: DeserializeOwned>(f: &Value) -> T {
    serde_json::from_value(f["query"].clone()).unwrap()
}

fn request<T: DeserializeOwned>(f: &Value) -> T {
    serde_json::from_value(f["request"].clone()).unwrap()
}

fn header(f: &Value, k: &str) -> String {
    f["headers"][k].as_str().unwrap().to_owned()
}

fn request_bytes(f: &Value) -> Vec<u8> {
    f["requestBytes"].as_str().unwrap().as_bytes().to_vec()
}

async fn raw_value(r: Result<reqwest::Response, ClientError>) -> Result<Value, ClientError> {
    let resp = r?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.unwrap();
    Ok(json!({"status": status, "bytes": String::from_utf8(bytes.to_vec()).unwrap()}))
}

/// What the client reads back for a success entry (the same shapes as magpie-api's own client test).
fn expected(id: &str, entry: &Value) -> Value {
    let route = magpie_api::routes::by_id(id).unwrap();
    let status = entry["status"].as_u64().unwrap();
    if let Some(bytes) = entry.get("bytes") {
        return json!({"status": status, "bytes": bytes});
    }
    let Some(body) = entry.get("body") else {
        return Value::Null;
    };
    let admitted = route.ok.contains(&200) && route.ok.iter().any(|s| matches!(s, 201 | 202));
    let empty = body == &json!({});
    match (admitted, empty) {
        (true, true) => json!({"new": status != 200}),
        (true, false) => json!({"new": status != 200, "body": body}),
        (false, true) => Value::Null,
        (false, false) => body.clone(),
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
        "transcript" => c
            .person(PERSON)
            .transcript(param_uuid(&f, "id"), &query::<TranscriptQuery>(&f))
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
        "admin_transcript" => c
            .service(Some(ADMIN))
            .admin_transcript(param_uuid(&f, "id"), &query::<TranscriptQuery>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "list_schedules" => c
            .sandbox(Some(LEASE))
            .list_schedules()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "create_schedule" => c
            .sandbox(Some(LEASE))
            .create_schedule(&request::<CreateSchedule>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "get_schedule" => c
            .sandbox(Some(LEASE))
            .get_schedule(&param_str(&f, "scheduleId"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "update_schedule" => c
            .sandbox(Some(LEASE))
            .update_schedule(&param_str(&f, "scheduleId"), &request::<UpdateSchedule>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "delete_schedule" => c
            .sandbox(Some(LEASE))
            .delete_schedule(&param_str(&f, "scheduleId"))
            .await
            .map(|()| Value::Null),
        "conversation_schedules" => c
            .person(PERSON)
            .conversation_schedules(param_uuid(&f, "id"))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_schedules" => c
            .service(Some(ADMIN))
            .admin_schedules()
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        "admin_schedules_pause" => c
            .service(Some(ADMIN))
            .admin_schedules_pause(&request::<PauseAll>(&f))
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
        other => panic!("no client call for route {other}"),
    }
}

async fn serve() -> aviary_fakes::magpie::Running {
    let _ = rustls::crypto::ring::default_provider().install_default();
    FakeMagpie::new().serve().await.unwrap()
}

/// A client speaking as the route's first listed caller.
fn client_for(running: &aviary_fakes::magpie::Running, id: &str) -> Client {
    let route = magpie_api::routes::by_id(id).unwrap();
    running.client(reqwest::Client::new(), route.callers[0])
}

#[tokio::test]
async fn every_response_of_every_route_fixture_reads_back_unchanged() {
    let running = serve().await;
    let fake = &running.fake;
    let mut seen = BTreeSet::new();
    for r in magpie_api::routes::ROUTES {
        let f = route_fixture(r.id);
        for entry in f["responses"].as_array().unwrap() {
            fake.script().once(r.id, Reply::from_fixture(entry));
            let status = u16::try_from(entry["status"].as_u64().unwrap()).unwrap();
            let got = call(&client_for(&running, r.id), r.id).await;
            if r.ok.contains(&status) {
                let got = got.unwrap_or_else(|e| panic!("{} {status}: {e}", r.id));
                assert_eq!(got, expected(r.id, entry), "{} {status}", r.id);
            } else {
                match got {
                    Err(ClientError::Api(e)) => {
                        assert_eq!(e.status, status, "{}", r.id);
                        assert_eq!(serde_json::to_value(&e).unwrap(), entry["body"], "{} {status}", r.id);
                    }
                    other => panic!("{} {status}: expected the error, got {other:?}", r.id),
                }
            }
            seen.insert((r.id, status));
        }
    }
    assert!(seen.len() > magpie_api::routes::ROUTES.len() * 2);
}

#[tokio::test]
async fn the_fake_records_what_the_fixture_sent() {
    let running = serve().await;
    let fake = &running.fake;
    for r in magpie_api::routes::ROUTES {
        let f = route_fixture(r.id);
        fake.script().once(r.id, Reply::from_fixture(&f["responses"][0]));
        let _ = call(&client_for(&running, r.id), r.id).await;
        let calls = fake.script().calls_to(r.id);
        let got = calls.last().unwrap_or_else(|| panic!("{}: not recorded", r.id));
        assert_eq!(&got.params, f.get("params").unwrap_or(&json!({})), "{}: params", r.id);
        assert_eq!(got.query.as_ref(), f.get("query"), "{}: query", r.id);
        assert_eq!(got.body.as_ref(), f.get("request"), "{}: body", r.id);
        if f.get("requestBytes").is_some() {
            assert_eq!(got.bytes.as_deref().unwrap(), &request_bytes(&f)[..], "{}: bytes", r.id);
        }
        assert_eq!(
            got.header("authorization"),
            Some(format!("Bearer {}", fake.token(r.callers[0]).unwrap()).as_str()),
            "{}",
            r.id
        );
        if r.person {
            assert_eq!(got.header("aviary-person"), Some(PERSON), "{}", r.id);
        }
        if r.lease {
            assert_eq!(got.header("aviary-lease"), Some(LEASE.to_string().as_str()), "{}", r.id);
        }
        if r.admin {
            assert_eq!(got.header("aviary-admin"), Some(ADMIN), "{}", r.id);
        }
    }
}

#[tokio::test]
async fn an_unscripted_route_without_a_model_answers_not_supported() {
    let running = serve().await;
    let c = client_for(&running, "list_conversations");
    let e = c
        .person(PERSON)
        .list_conversations(&ListQuery::default())
        .await
        .unwrap_err();
    match e {
        ClientError::Api(e) => {
            assert_eq!(e.status, 501);
            assert_eq!(serde_json::to_value(&e).unwrap()["code"], "not_supported");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_fake_refuses_an_unknown_token_and_a_caller_the_route_does_not_list() {
    use aviary_core::auth::Caller;
    let running = serve().await;
    let fake = &running.fake;
    for r in magpie_api::routes::ROUTES {
        let f = route_fixture(r.id);
        // a scripted answer is served only to an admitted caller
        fake.script().always(r.id, Reply::from_fixture(&f["responses"][0]));
        let wrong = Client::new(reqwest::Client::new(), running.url(), "not-a-pair-token");
        match call(&wrong, r.id).await {
            Err(ClientError::Api(e)) => {
                assert_eq!(e.status, 401, "{}", r.id);
                assert_eq!(serde_json::to_value(&e).unwrap()["code"], "unauthenticated", "{}", r.id);
            }
            other => panic!("{}: a wrong token got {other:?}", r.id),
        }
        let other = Caller::ALL
            .iter()
            .find(|c| !r.callers.contains(c) && fake.token(**c).is_some())
            .unwrap();
        match call(&running.client(reqwest::Client::new(), *other), r.id).await {
            Err(ClientError::Api(e)) => {
                assert_eq!(e.status, 403, "{} as {other}", r.id);
                assert_eq!(serde_json::to_value(&e).unwrap()["code"], "wrong_caller", "{}", r.id);
            }
            other_r => panic!("{} as {other}: got {other_r:?}", r.id),
        }
        assert!(
            fake.script().calls_to(r.id).is_empty(),
            "{}: a refused call is recorded",
            r.id
        );
        for caller in r.callers {
            // every listed caller is admitted and gets the scripted answer
            if let Err(ClientError::Api(e)) = call(&running.client(reqwest::Client::new(), *caller), r.id).await {
                assert!(e.status != 401 && e.status != 403, "{} as {caller}: {}", r.id, e.status);
            }
        }
        assert_eq!(fake.script().calls_to(r.id).len(), r.callers.len(), "{}", r.id);
    }
}

#[tokio::test]
async fn the_unguarded_fake_accepts_any_token() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let running = FakeMagpie::unguarded().serve().await.unwrap();
    let f = route_fixture("list_conversations");
    running
        .fake
        .script()
        .once("list_conversations", Reply::from_fixture(&f["responses"][0]));
    let c = Client::new(reqwest::Client::new(), running.url(), "anything");
    call(&c, "list_conversations").await.unwrap();
}
