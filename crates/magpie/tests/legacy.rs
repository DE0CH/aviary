//! The switch window (WP-51; DESIGN 9.2-9.5, cutover 6.4 and 7.8): every route of cutover 6.4's table for a
//! migrating conversation, tickets that close only on an outcome, the fence and its 3 s lease, a send during a fence
//! becoming the first native turn, the release, the running mirror in M1 and M12, the legacy-path resolution through
//! both tables any number of times with a fresh copy per send, the synchronous ingest on a miss, and the legacy-path
//! set outliving retention and the A6 contraction.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    dead_code
)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magpie::legacy::fence::{self, Fence, NotReleased, Release};
use magpie::legacy::{mirror, paths};
use magpie_api::changes::{After, ChangeBody, ChangesQuery};
use magpie_api::client::ClientError;
use magpie_api::conversations::{ListQuery, RenameRequest, RunState};
use magpie_api::errors::{ErrorCode, RouteHint};
use magpie_api::events::NameSource;
use magpie_api::legacy::{LegacyPathRequest, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketRoute};
use magpie_api::turns::{FileRef, SendRequest};
use magpie_api::uploads::{UploadKind, UploadResponse};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";

fn key(person: &str) -> String {
    aviary_core::log::person_key(person)
}

/// An old asset path of `person` for the file `name`, as the old child returns it.
fn old_path(person: &str, name: &str) -> String {
    format!("/home/{}/.claude/cloudcli-assets/{name}", key(person))
}

async fn migrating(r: &common::Replica, person: &str, k: &str) -> Uuid {
    let c = common::files::conversation(r, person, k).await;
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update conversations set migrating = true where id = $1", &[&c])
        .await
        .unwrap();
    c
}

fn send(cmid: &str, files: Vec<FileRef>) -> SendRequest {
    SendRequest {
        client_message_id: cmid.into(),
        text: "整理一下".into(),
        images: Vec::new(),
        files,
    }
}

fn file(path: &str, up: &UploadResponse) -> FileRef {
    FileRef {
        path: path.to_owned(),
        name: up.name.clone(),
        mime_type: up.mime_type.clone(),
        size: up.size,
    }
}

fn legacy_409(e: &ClientError) -> bool {
    e.api()
        .is_some_and(|r| r.code == ErrorCode::ConversationMigrating && r.route == Some(RouteHint::Legacy))
}

async fn upload(r: &common::Replica, person: &str, name: &str, bytes: &[u8]) -> UploadResponse {
    r.pigeon()
        .person(person)
        .upload(name, UploadKind::File, "application/pdf", bytes.to_vec())
        .await
        .unwrap()
}

async fn ticket(r: &common::Replica, c: Uuid, kind: TicketKind) -> magpie_api::legacy::TicketResponse {
    r.pigeon()
        .person(ALICE)
        .legacy_ticket(
            c,
            &TicketRequest {
                kind,
                client_message_id: None,
            },
        )
        .await
        .unwrap()
}

async fn done(r: &common::Replica, t: Uuid, outcome: TicketOutcome) -> Result<(), ClientError> {
    r.pigeon()
        .person(ALICE)
        .legacy_ticket_done(t, &TicketDone { outcome })
        .await
}

async fn fence_at(r: &common::Replica, c: Uuid) -> Option<time::OffsetDateTime> {
    r.pool()
        .get()
        .await
        .unwrap()
        .query_one("select legacy_fence_at from conversations where id = $1", &[&c])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn every_route_of_the_migrating_table() {
    let db = common::fresh_db("lg_table").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = migrating(&r, ALICE, "m").await;
    let p = r.pigeon();
    let a = p.person(ALICE);
    // reads are normal, and show the conversation as migrating
    assert!(a.get_conversation(c).await.unwrap().migrating);
    let list = a.list_conversations(&ListQuery::default()).await.unwrap();
    assert!(list.conversations.iter().any(|x| x.id == c && x.migrating));
    a.timeline(
        c,
        &magpie_api::rows::TimelineQuery {
            tail: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // every native mutation of an unfenced legacy conversation: 409 route legacy, at once
    let started = Instant::now();
    let rename = RenameRequest { title: "新".into() };
    assert!(legacy_409(&a.rename_conversation(c, &rename).await.unwrap_err()));
    assert!(legacy_409(&a.send_turn(c, &send("m:1", Vec::new())).await.unwrap_err()));
    assert!(legacy_409(&a.cancel_turn(c).await.unwrap_err()));
    assert!(legacy_409(&a.delete_conversation(c).await.unwrap_err()));
    assert!(legacy_409(&a.restore_conversation(c).await.unwrap_err()));
    assert!(legacy_409(&a.create_share(c).await.unwrap_err()));
    assert!(legacy_409(&a.purge_conversation(c).await.unwrap_err()));
    let answer: magpie_api::turns::AgentPermissionResponse =
        serde_json::from_value(serde_json::json!({"behavior": "allow"})).unwrap();
    assert!(legacy_409(&a.answer_ask(c, "ask_1", &answer).await.unwrap_err()));
    assert!(started.elapsed() < Duration::from_secs(2), "no wait without a fence");
    // tickets: legacy with a ticket while legacy, 404 for someone else
    let tk = ticket(&r, c, TicketKind::Send).await;
    assert_eq!(tk.route, TicketRoute::Legacy);
    let tid = tk.ticket.unwrap();
    let other = r
        .pigeon()
        .person(BOB)
        .legacy_ticket(
            c,
            &TicketRequest {
                kind: TicketKind::Send,
                client_message_id: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(other.code(), Some(&ErrorCode::NotFound));
    assert_eq!(
        r.pigeon()
            .person(BOB)
            .legacy_ticket_done(
                tid,
                &TicketDone {
                    outcome: TicketOutcome::Started
                }
            )
            .await
            .unwrap_err()
            .code(),
        Some(&ErrorCode::NotFound)
    );
    // delivery closes nothing: the fence is refused while the ticket is open
    done(&r, tid, TicketOutcome::Delivered).await.unwrap();
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
    // an outcome that does not fit a send: 400; the turn's outcome closes it, a repeat changes nothing
    assert_eq!(
        done(&r, tid, TicketOutcome::Applied).await.unwrap_err().code(),
        Some(&ErrorCode::BadRequest)
    );
    done(&r, tid, TicketOutcome::Started).await.unwrap();
    done(&r, tid, TicketOutcome::Failed).await.unwrap();
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select outcome, delivered_at is not null from legacy_tickets where ticket = $1",
            &[&tid],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "started");
    assert!(row.get::<_, bool>(1));
    // a rename ticket closes on applied
    let rt = ticket(&r, c, TicketKind::Rename).await.ticket.unwrap();
    done(&r, rt, TicketOutcome::Applied).await.unwrap();
    assert_eq!(
        done(&r, Uuid::new_v4(), TicketOutcome::Applied)
            .await
            .unwrap_err()
            .code(),
        Some(&ErrorCode::NotFound)
    );
    // once released, a ticket call answers native, and native mutations work
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::AlreadyFenced);
    let m = magpie::legacy::Metrics::default();
    let held = fence::release(r.pool(), c, &Release::default(), "idle", &m)
        .await
        .unwrap()
        .unwrap();
    assert!((0.0..3.0).contains(&held), "{held}");
    assert_eq!(ticket(&r, c, TicketKind::Send).await.route, TicketRoute::Native);
    assert!(!a.get_conversation(c).await.unwrap().migrating);
    a.rename_conversation(c, &rename).await.unwrap();
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::NotMigrating);
}

#[tokio::test]
async fn a_send_during_a_fence_waits_and_becomes_the_first_native_turn() {
    let db = common::fresh_db("lg_fenced").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let c = migrating(&r, ALICE, "f").await;
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    // a ticket call and a native send both wait in the fence
    let r2 = r.clone();
    let sent = tokio::spawn(async move {
        let started = Instant::now();
        let out = r2.pigeon().person(ALICE).send_turn(c, &send("f:1", Vec::new())).await;
        (out, started.elapsed())
    });
    let r3 = r.clone();
    let tk = tokio::spawn(async move { ticket(&r3, c, TicketKind::Send).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!sent.is_finished() && !tk.is_finished(), "both wait while fenced");
    let rel = Release {
        model: Some("old/model-1".into()),
        name_source: Some(NameSource::Title),
    };
    fence::release(r.pool(), c, &rel, "idle", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    let (out, waited) = sent.await.unwrap();
    let turn = out.unwrap().into_inner().turn;
    assert!(waited < Duration::from_secs(3), "{waited:?}");
    assert_eq!(tk.await.unwrap().route, TicketRoute::Native);
    // the first native turn, and the release-time model and name_source
    let conv = r.pigeon().person(ALICE).get_conversation(c).await.unwrap();
    assert!(!conv.migrating);
    assert_eq!(conv.turn.map(|x| x.id), Some(turn.id));
    assert_eq!(conv.model.as_deref(), Some("old/model-1"));
    let ns: Option<String> = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select name_source from conversations where id = $1", &[&c])
        .await
        .unwrap()
        .get(0);
    assert_eq!(ns.as_deref(), Some("title"));
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from turns where conversation_id = $1 and origin = 'person'",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}

#[tokio::test]
async fn a_fence_whose_lease_lapses_unfences() {
    let db = common::fresh_db("lg_lapse").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let c = migrating(&r, ALICE, "l").await;
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    // nobody releases: the send waits out the lease (at most 3 s), then answers 409 route legacy, and the fence is
    // gone
    let started = Instant::now();
    let e = r
        .pigeon()
        .person(ALICE)
        .rename_conversation(c, &RenameRequest { title: "x".into() })
        .await
        .unwrap_err();
    let waited = started.elapsed();
    assert!(legacy_409(&e), "{e:?}");
    assert!(
        waited >= Duration::from_millis(2_500) && waited <= Duration::from_millis(3_300),
        "{waited:?}"
    );
    assert_eq!(fence_at(&r, c).await, None);
    // a lapsed fence cannot be released
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    r.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update conversations set legacy_fence_at = now() - interval '4 seconds' where id = $1",
            &[&c],
        )
        .await
        .unwrap();
    let m = magpie::legacy::Metrics::default();
    assert_eq!(
        fence::release(r.pool(), c, &Release::default(), "idle", &m)
            .await
            .unwrap(),
        Err(NotReleased::NotFenced)
    );
    // a ticket call clears a lapsed fence and gets a ticket at once
    let started = Instant::now();
    let tk = ticket(&r, c, TicketKind::Send).await;
    assert_eq!(tk.route, TicketRoute::Legacy);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(fence_at(&r, c).await, None);
    // unfence lifts a live fence: a waiting ticket call then gets a ticket
    done(&r, tk.ticket.unwrap(), TicketOutcome::Refused).await.unwrap();
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    let r2 = r.clone();
    let waiting = tokio::spawn(async move { ticket(&r2, c, TicketKind::Rename).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(fence::unfence(r.pool(), c).await.unwrap());
    let tk = waiting.await.unwrap();
    assert_eq!(tk.route, TicketRoute::Legacy);
    assert!(tk.ticket.is_some());
    // with that ticket open, no release
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
}

#[tokio::test]
async fn the_running_mirror_shows_in_the_list_and_the_change_feed() {
    let db = common::fresh_db("lg_mirror").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = migrating(&r, ALICE, "r").await;
    let native = common::files::conversation(&r, ALICE, "n").await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let head = a
        .changes(&ChangesQuery {
            after: After::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap()
        .head_seq;
    assert!(mirror::run_state(r.pool(), c, RunState::Running).await.unwrap());
    let list = a.list_conversations(&ListQuery::default()).await.unwrap();
    let shown = list.conversations.iter().find(|x| x.id == c).unwrap();
    assert_eq!(shown.run.state, RunState::Running);
    let feed = a
        .changes(&ChangesQuery {
            after: After::Seq(head),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert!(feed.changes.iter().any(|ch| ch.conversation_id == c
        && matches!(&ch.body, ChangeBody::ConversationUpserted { conversation } if conversation.run.state == RunState::Running)));
    // the same state again writes no change; idle goes back
    let head = feed.head_seq;
    assert!(mirror::run_state(r.pool(), c, RunState::Running).await.unwrap());
    assert!(mirror::run_state(r.pool(), c, RunState::Idle).await.unwrap());
    let feed = a
        .changes(&ChangesQuery {
            after: After::Seq(head),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(feed.changes.len(), 1);
    // a native conversation is never written by the mirror
    assert!(!mirror::run_state(r.pool(), native, RunState::Running).await.unwrap());
    assert_eq!(a.get_conversation(native).await.unwrap().run.state, RunState::Idle);
    assert!(mirror::run_state(r.pool(), c, RunState::Stopping).await.is_err());
}

#[tokio::test]
async fn legacy_paths_resolve_through_both_tables_any_number_of_times() {
    let db = common::fresh_db("lg_paths").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    // the tee: an upload recorded under the old path the old child returned
    let teed = upload(&r, ALICE, "teed.pdf", b"%PDF-1.4 teed").await;
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: old_path(ALICE, "1762054010120-teed.pdf"),
        },
    )
    .await
    .unwrap();
    // a path that is not the person's: 400 bad_path; another person's upload: 404
    let bad = a
        .upload_legacy_path(
            teed.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path(BOB, "x.pdf"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code(), Some(&ErrorCode::BadPath));
    let bobs = upload(&r, BOB, "b.pdf", b"bob").await;
    let nf = a
        .upload_legacy_path(
            bobs.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path(ALICE, "b.pdf"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(nf.code(), Some(&ErrorCode::NotFound));
    // the importer's ingestion: an unattached upload under import_uploads
    let imported = upload(&r, ALICE, "imported.pdf", b"%PDF-1.4 imported").await;
    aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<()> {
        paths::import_upload(tx, ALICE, "1761000000000-imported.pdf", imported.upload_id, &[1u8; 32]).await
    })
    .await
    .unwrap();
    // each old path resolves in several conversations, and twice in one, each send attaching a fresh copy
    let c1 = common::files::conversation(&r, ALICE, "p1").await;
    let c2 = common::files::conversation(&r, ALICE, "p2").await;
    let teed_path = old_path(ALICE, "1762054010120-teed.pdf");
    let imp_path = format!(".claude/cloudcli-assets/{}", "1761000000000-imported.pdf");
    let mut attached = Vec::new();
    for (i, c) in [c1, c2].into_iter().enumerate() {
        a.send_turn(
            c,
            &send(
                &format!("p:{i}"),
                vec![file(&teed_path, &teed), file(&imp_path, &imported)],
            ),
        )
        .await
        .unwrap();
        // end the turn so a second send is admitted
        r.pool()
            .get()
            .await
            .unwrap()
            .execute(
                "update conversations set run_state = 'idle', active_turn = null where id = $1",
                &[&c],
            )
            .await
            .unwrap();
    }
    a.send_turn(c1, &send("p:again", vec![file(&teed_path, &teed)]))
        .await
        .unwrap();
    let conn = r.pool().get().await.unwrap();
    for row in conn
        .query(
            "select id, conversation_id, name, blob from uploads where conversation_id is not null order by created_at",
            &[],
        )
        .await
        .unwrap()
    {
        attached.push((row.get::<_, Uuid>(0), row.get::<_, Uuid>(1), row.get::<_, String>(2)));
    }
    assert_eq!(attached.len(), 5, "{attached:?}");
    let ids: std::collections::HashSet<Uuid> = attached.iter().map(|x| x.0).collect();
    assert_eq!(ids.len(), 5, "a fresh copy per send");
    assert!(!ids.contains(&teed.upload_id) && !ids.contains(&imported.upload_id));
    // the originals stay unattached, and each copy has its own blob refs
    let unattached: i64 = conn
        .query_one(
            "select count(*) from uploads where id = any($1) and conversation_id is null",
            &[&vec![teed.upload_id, imported.upload_id]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(unattached, 2);
    let refs: i64 = conn
        .query_one(
            "select count(*) from blob_refs where owner_kind = 'upload' and owner = any($1)",
            &[&ids.iter().copied().collect::<Vec<_>>()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(refs, 5);
    // a repeated send makes no copy
    let before: i64 = conn
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    a.send_turn(c1, &send("p:again", vec![file(&teed_path, &teed)]))
        .await
        .unwrap();
    let after: i64 = conn
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    // an old path in neither table, with the import unreachable: 503 (retried, not refused)
    let miss = a
        .send_turn(c2, &send("p:miss", vec![file(&old_path(ALICE, "never.pdf"), &teed)]))
        .await
        .unwrap_err();
    assert_eq!(miss.code(), Some(&ErrorCode::Unavailable));
    // another person's old path is not an old path of this person: unknown_upload
    let other = a
        .send_turn(c2, &send("p:other", vec![file(&old_path(BOB, "x.pdf"), &teed)]))
        .await
        .unwrap_err();
    assert_eq!(other.code(), Some(&ErrorCode::UnknownUpload));
    // M3's first message resolves the same way
    let created = a
        .create_conversation(&magpie_api::conversations::CreateRequest {
            idempotency_key: "p3".into(),
            message: send("p3:1", vec![file(&teed_path, &teed)]),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(created.turn.is_some());
    // retention keeps the legacy-path set past the unattached-upload age; a plain unattached upload goes
    let plain = upload(&r, ALICE, "plain.pdf", b"plain").await;
    conn.execute(
        "update uploads set created_at = now() - interval '3 days' where conversation_id is null",
        &[],
    )
    .await
    .unwrap();
    magpie::jobs::retention::pass(r.pool()).await.unwrap();
    let left: Vec<Uuid> = conn
        .query("select id from uploads where conversation_id is null", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(left.contains(&teed.upload_id) && left.contains(&imported.upload_id));
    assert!(!left.contains(&plain.upload_id));
    // and they still resolve
    a.send_turn(c2, &send("p:late", vec![file(&imp_path, &imported)]))
        .await
        .unwrap();
}

/// A stand-in for the import Deployment's `POST /v1/ingest-upload`: the names it knows are ingested (their upload
/// recorded in `import_uploads`), anything else is 404.
async fn fake_import(pool: aviary_pg::Pool, known: Arc<Mutex<HashMap<String, Uuid>>>) -> (String, Arc<Mutex<u32>>) {
    use axum::Json;
    use axum::extract::State;
    use axum::http::StatusCode;
    let calls = Arc::new(Mutex::new(0u32));
    let state = (pool, known, calls.clone());
    type St = (aviary_pg::Pool, Arc<Mutex<HashMap<String, Uuid>>>, Arc<Mutex<u32>>);
    async fn ingest(
        State((pool, known, calls)): State<St>,
        Json(req): Json<magpie::legacy::ingest::IngestRequest>,
    ) -> StatusCode {
        *calls.lock().unwrap() += 1;
        let Some(up) = known.lock().unwrap().get(&req.legacy_path).copied() else {
            return StatusCode::NOT_FOUND;
        };
        let (person, name) = (req.person.clone(), req.legacy_path.clone());
        aviary_pg::tx!(&pool, |t| -> magpie::error::Result<()> {
            paths::import_upload(t, &person, &name, up, &[2u8; 32]).await
        })
        .await
        .unwrap();
        StatusCode::OK
    }
    let app = axum::Router::new()
        .route(magpie::legacy::ingest::ROUTE, axum::routing::post(ingest))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), calls)
}

#[tokio::test]
async fn a_miss_is_ingested_synchronously_before_unknown_upload() {
    let db = common::fresh_db("lg_ingest").await;
    let t = common::Tokens::new();
    let pool = aviary_pg::connect(&db.url, "test", 2, Duration::from_secs(10)).unwrap();
    let known = Arc::new(Mutex::new(HashMap::new()));
    let (url, calls) = fake_import(pool, known.clone()).await;
    let bucket = common::test_bucket(&db.url).await;
    let r = common::replica_import(&db.url, &t, "http://127.0.0.1:1", common::idle(), bucket, &url).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    // a file attached in an old tab at move − 2 s: in the person's old asset folder, not yet ingested (the importer
    // has the bytes as an upload of its own, which the ingest records)
    let bytes = upload(&r, ALICE, "late.pdf", b"%PDF-1.4 late").await;
    known
        .lock()
        .unwrap()
        .insert("1762054099000-late.pdf".to_owned(), bytes.upload_id);
    // sent at move + 3 s to a pigeon-born conversation: resolved through the synchronous ingest
    let c = common::files::conversation(&r, ALICE, "born").await;
    let path = old_path(ALICE, "1762054099000-late.pdf");
    a.send_turn(c, &send("born:1", vec![file(&path, &bytes)]))
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from uploads where conversation_id = $1 and name = 'late.pdf'",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    // the next send of the same path resolves through import_uploads, no ingest call
    let c2 = common::files::conversation(&r, ALICE, "born2").await;
    a.send_turn(c2, &send("born2:1", vec![file(&path, &bytes)]))
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
    // in neither table and not in the folder: 400 unknown_upload after the ingest said 404
    let e = a
        .send_turn(c2, &send("born2:2", vec![file(&old_path(ALICE, "gone.pdf"), &bytes)]))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::UnknownUpload));
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test]
async fn the_legacy_path_set_outlives_the_a6_contraction() {
    let db = common::fresh_db("lg_a6").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let teed = upload(&r, ALICE, "teed.pdf", b"teed").await;
    r.pigeon()
        .person(ALICE)
        .upload_legacy_path(
            teed.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path(ALICE, "t.pdf"),
            },
        )
        .await
        .unwrap();
    let imported = upload(&r, ALICE, "imported.pdf", b"imported").await;
    aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<()> {
        paths::import_upload(tx, ALICE, "i.pdf", imported.upload_id, &[3u8; 32]).await
    })
    .await
    .unwrap();
    let c = migrating(&r, ALICE, "a6").await;
    let tk = ticket(&r, c, TicketKind::Send).await;
    assert!(tk.ticket.is_some());
    // the A6 contraction drops the window's tables and columns
    let conn = r.pool().get().await.unwrap();
    conn.batch_execute(magpie::legacy::A6_CONTRACTION).await.unwrap();
    let gone: i64 = conn
        .query_one(
            "select count(*) from information_schema.tables
             where table_name in ('legacy_tickets', 'import_sources', 'import_shares', 'import_control')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    // the legacy-path set is still there and still resolves, with fresh copies
    assert_eq!(
        paths::lookup(&conn, ALICE, "t.pdf").await.unwrap(),
        Some(teed.upload_id)
    );
    assert_eq!(
        paths::lookup(&conn, ALICE, "i.pdf").await.unwrap(),
        Some(imported.upload_id)
    );
    let copy = paths::copy(r.pool(), ALICE, imported.upload_id).await.unwrap();
    assert_ne!(copy, imported.upload_id);
    // and retention still keeps it
    conn.execute(
        "update uploads set created_at = now() - interval '3 days' where conversation_id is null",
        &[],
    )
    .await
    .unwrap();
    magpie::jobs::retention::pass(r.pool()).await.unwrap();
    assert_eq!(
        paths::lookup(&conn, ALICE, "i.pdf").await.unwrap(),
        Some(imported.upload_id)
    );
    assert_eq!(
        paths::lookup(&conn, ALICE, "t.pdf").await.unwrap(),
        Some(teed.upload_id)
    );
}
