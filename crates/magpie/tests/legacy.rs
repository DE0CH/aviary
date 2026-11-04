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
use magpie_api::legacy::{
    LegacyPathRequest, PriorSend, TicketClaim, TicketClaimed, TicketDone, TicketKind, TicketOutcome, TicketRequest,
    TicketRoute,
};
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
        .legacy_ticket_done(t, &TicketDone { outcome, take: None })
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
    use aviary_fakes::hermit::{FakeHermit, HermitConfig};
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("lg_table").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
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
                    outcome: TicketOutcome::Started,
                    take: None,
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

    // M16 continue of a migrating conversation's share: the new conversation is born owned (not migrating)
    let src = answered(&r, &hermit, ALICE).await;
    let share = a.create_share(src).await.unwrap();
    set_migrating(&r, src).await;
    let cont = r.pigeon().person(BOB).continue_share(&share.id).await.unwrap();
    let born = r
        .pigeon()
        .person(BOB)
        .get_conversation(cont.conversation_id)
        .await
        .unwrap();
    assert!(!born.migrating);
    assert!(
        a.get_conversation(src).await.unwrap().migrating,
        "the source stays the old stack's"
    );

    // hermit's sandbox events for a migrating conversation change nothing
    let h = migrating(&r, ALICE, "h").await;
    let sandbox = Uuid::new_v4();
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "update conversations set lease_id = gen_random_uuid(), lease_sandbox = $2 where id = $1",
        &[&h, &sandbox],
    )
    .await
    .unwrap();
    let ev = |state, version| hermit_api::sandbox::SandboxEvent {
        seq: version,
        at: hermit_api::Timestamp::now(),
        sandbox_id: sandbox,
        conversation_id: h,
        person: ALICE.into(),
        state,
        phase: None,
        version,
        busy: false,
        release: None,
    };
    let (ready, gone) = (
        ev(hermit_api::sandbox::SandboxState::Ready, 1),
        ev(hermit_api::sandbox::SandboxState::Gone, 2),
    );
    let row = async |h: Uuid| {
        let x = r
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select lease_sandbox, hermit_sandbox from conversations where id = $1",
                &[&h],
            )
            .await
            .unwrap();
        (x.get::<_, Option<Uuid>>(0), x.get::<_, Option<Uuid>>(1))
    };
    let apply = async |ev: &hermit_api::sandbox::SandboxEvent| {
        aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<()> {
            magpie::hermit::feed::apply(tx, ev).await
        })
        .await
        .unwrap();
    };
    apply(&ready).await;
    apply(&gone).await;
    assert_eq!(row(h).await, (Some(sandbox), None), "ignored while migrating");
    // (the same events once it is native: the sandbox recorded, and its lease cleared when it is gone)
    sql.execute("update conversations set migrating = false where id = $1", &[&h])
        .await
        .unwrap();
    apply(&ready).await;
    apply(&gone).await;
    assert_eq!(row(h).await, (None, Some(sandbox)));

    // magpie's own jobs skip migrating conversations: the 30-day purge, and the turn sweep and its drivers
    let old = common::files::conversation(&r, ALICE, "purge").await;
    a.delete_conversation(old).await.unwrap();
    sql.execute(
        "update conversations set deleted_at = now() - interval '31 days', migrating = true where id = $1",
        &[&old],
    )
    .await
    .unwrap();
    assert_eq!(magpie::conversations::delete::purge_due(r.pool(), 20).await.unwrap(), 0);
    assert!(a.get_conversation(old).await.unwrap().migrating);
    let swept = migrating(&r, ALICE, "sweep").await;
    let turn = Uuid::new_v4();
    sql.execute(
        "insert into turns (id, conversation_id, origin, state, deadline)
         values ($1, $2, 'person', 'admitted', now() - interval '1 minute')",
        &[&turn, &swept],
    )
    .await
    .unwrap();
    let d = magpie::turns::driver::Driver::of(r.app());
    assert_eq!(magpie::turns::sweeper::pass(&d).await.unwrap(), 0);
    magpie::turns::driver::drive(&d, turn).await.unwrap();
    let t_row = sql
        .query_one(
            "select state, driver, deadline < now() from turns where id = $1",
            &[&turn],
        )
        .await
        .unwrap();
    assert_eq!(t_row.get::<_, String>(0), "admitted");
    assert_eq!(t_row.get::<_, Option<String>>(1), None, "no driver claimed it");
    assert!(t_row.get::<_, bool>(2), "the sweep did not claim its deadline");
    // (once owned, both act)
    sql.execute(
        "update conversations set migrating = false where id in ($1, $2)",
        &[&old, &swept],
    )
    .await
    .unwrap();
    assert_eq!(magpie::conversations::delete::purge_due(r.pool(), 20).await.unwrap(), 1);
    assert!(magpie::turns::sweeper::pass(&d).await.unwrap() >= 1);
}

/// A conversation of `person` with a finished turn and a workspace file: something to share.
async fn answered(r: &common::Replica, hermit: &aviary_fakes::hermit::FakeHermit, person: &str) -> Uuid {
    use common::ev::{self, assistant, text, turn_ended, turn_started, user_sent};
    let c = ev::conversation(&r.pigeon(), person).await;
    let tid = Uuid::new_v4();
    ev::append(
        r.pool(),
        person,
        c,
        vec![
            user_sent("m1", "q"),
            ev::in_turn(turn_started(tid), tid),
            ev::in_turn(assistant("msg_1", serde_json::json!([text("a")])), tid),
            ev::in_turn(turn_ended(tid, "completed"), tid),
        ],
    )
    .await;
    hermit.write_file(c, "x.txt", b"x");
    c
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
    // the tee's record is an unattached copy of its own; a repeated PUT adds none
    let teed_record = paths::lookup(&conn, ALICE, "1762054010120-teed.pdf")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(teed_record, teed.upload_id);
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: teed_path.clone(),
        },
    )
    .await
    .unwrap();
    let records: i64 = conn
        .query_one(
            "select count(*) from uploads where legacy_path = '1762054010120-teed.pdf'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(records, 1);
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
    assert!(left.contains(&teed_record) && left.contains(&imported.upload_id));
    assert!(!left.contains(&plain.upload_id));
    // and they still resolve
    a.send_turn(
        c2,
        &send("p:late", vec![file(&imp_path, &imported), file(&teed_path, &teed)]),
    )
    .await
    .unwrap();
}

/// A stand-in for the import Deployment's `POST /v1/ingest-upload`: the names it knows are ingested (their upload
/// recorded in `import_uploads`), anything else is 404.
async fn fake_import(pool: aviary_pg::Pool, known: Arc<Mutex<HashMap<String, Uuid>>>) -> (String, Arc<Mutex<u32>>) {
    fake_import_slow(pool, known, Duration::ZERO).await
}

/// [`fake_import`] answering each call only after `delay`.
async fn fake_import_slow(
    pool: aviary_pg::Pool,
    known: Arc<Mutex<HashMap<String, Uuid>>>,
    delay: Duration,
) -> (String, Arc<Mutex<u32>>) {
    use axum::Json;
    use axum::extract::State;
    use axum::http::StatusCode;
    let calls = Arc::new(Mutex::new(0u32));
    let state = (pool, known, calls.clone(), delay);
    type St = (
        aviary_pg::Pool,
        Arc<Mutex<HashMap<String, Uuid>>>,
        Arc<Mutex<u32>>,
        Duration,
    );
    async fn ingest(
        State((pool, known, calls, delay)): State<St>,
        Json(req): Json<magpie::legacy::ingest::IngestRequest>,
    ) -> StatusCode {
        *calls.lock().unwrap() += 1;
        tokio::time::sleep(delay).await;
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
    // the import Deployment knows no file: a miss is `unknown_upload`, as once the A6 release removes the ingest
    let pool = aviary_pg::connect(&db.url, "test", 2, Duration::from_secs(10)).unwrap();
    let (url, _) = fake_import(pool, Arc::new(Mutex::new(HashMap::new()))).await;
    let bucket = common::test_bucket(&db.url).await;
    let r = common::replica_import(&db.url, &t, "http://127.0.0.1:1", common::idle(), bucket, &url).await;
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
    let teed_record = paths::lookup(&r.pool().get().await.unwrap(), ALICE, "t.pdf")
        .await
        .unwrap()
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
             where table_name in ('legacy_tickets', 'import_sources', 'import_shares', 'import_control', 'import_purged')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    // the legacy-path set is still there and still resolves, with fresh copies
    assert_eq!(paths::lookup(&conn, ALICE, "t.pdf").await.unwrap(), Some(teed_record));
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
    assert_eq!(paths::lookup(&conn, ALICE, "t.pdf").await.unwrap(), Some(teed_record));
    // a send's resolution (M3/M7's step before admission) after the contraction: both old paths become fresh copies,
    // and one in neither table is `unknown_upload`, not a statement error. (M7 itself cannot run on this schema:
    // admission still reads `conversations.migrating`, which the A6 release drops together with that read.)
    let person = magpie::conversations::Person {
        email: ALICE.into(),
        viewer: None,
    };
    let mut m = send(
        "a6:1",
        vec![
            file(&old_path(ALICE, "i.pdf"), &imported),
            file(&old_path(ALICE, "t.pdf"), &teed),
        ],
    );
    paths::resolve(r.app(), &person, &mut m).await.unwrap();
    let ids: Vec<Uuid> = m
        .files
        .iter()
        .map(|f| {
            let rest = f.path.strip_prefix("uploads/").expect("a fresh copy's path");
            rest.split('/').next().unwrap().parse().unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(!ids.contains(&imported.upload_id) && !ids.contains(&teed_record));
    for id in &ids {
        let unattached: bool = conn
            .query_one("select conversation_id is null from uploads where id = $1", &[id])
            .await
            .unwrap()
            .get(0);
        assert!(unattached);
    }
    let mut m = send("a6:2", vec![file(&old_path(ALICE, "gone.pdf"), &imported)]);
    let e = paths::resolve(r.app(), &person, &mut m).await.unwrap_err();
    assert!(e.is(&ErrorCode::UnknownUpload), "{e:?}");
}

async fn set_migrating(r: &common::Replica, c: Uuid) {
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update conversations set migrating = true where id = $1", &[&c])
        .await
        .unwrap();
}

/// Marks `c` migrating and fences it, runs `call` while it is fenced, checks that it waits, releases the fence and
/// answers what `call` got: the write then ran natively.
async fn fenced_then_released<T, F, Fut>(r: &Arc<common::Replica>, c: Uuid, call: F) -> Result<T, ClientError>
where
    T: Send + 'static,
    F: FnOnce(Arc<common::Replica>) -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>> + Send + 'static,
{
    set_migrating(r, c).await;
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    let started = Instant::now();
    let h = tokio::spawn(call(r.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.is_finished(), "the write waits while fenced");
    fence::release(r.pool(), c, &Release::default(), "idle", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    let out = h.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    if let Err(e) = &out {
        assert_ne!(e.code(), Some(&ErrorCode::ConversationMigrating), "{e:?}");
    }
    out
}

#[tokio::test]
async fn every_wrapped_route_waits_in_a_fence_then_runs_natively() {
    use aviary_fakes::hermit::{FakeHermit, HermitConfig};
    use common::ev::{self, assistant, text, turn_ended, turn_started, user_sent};
    const STAFF: &str = "staff@example.org";
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("lg_routes").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica_with(&db.url, &t, &running.url(), common::idle()).await);
    let c = ev::conversation(&r.pigeon(), STAFF).await;
    let tid = Uuid::new_v4();
    ev::append(
        r.pool(),
        STAFF,
        c,
        vec![
            user_sent("m1", "q"),
            ev::in_turn(turn_started(tid), tid),
            ev::in_turn(assistant("msg_1", serde_json::json!([text("a")])), tid),
            ev::in_turn(turn_ended(tid, "completed"), tid),
        ],
    )
    .await;
    hermit.write_file(c, "x.txt", b"x");
    // a share made while native, revoked below while legacy
    let early = r.pigeon().person(STAFF).create_share(c).await.unwrap();
    // M5
    let renamed = fenced_then_released(&r, c, move |r| async move {
        r.pigeon()
            .person(STAFF)
            .rename_conversation(c, &RenameRequest { title: "新名".into() })
            .await
    })
    .await
    .unwrap();
    assert_eq!(renamed.title.as_deref(), Some("新名"));
    // M6 delete and restore
    fenced_then_released(&r, c, move |r| async move {
        r.pigeon().person(STAFF).delete_conversation(c).await
    })
    .await
    .unwrap();
    fenced_then_released(&r, c, move |r| async move {
        r.pigeon().person(STAFF).restore_conversation(c).await
    })
    .await
    .unwrap();
    // M8 and M11 (nothing running and no such ask: their own answers, natively)
    let _ = fenced_then_released(
        &r,
        c,
        move |r| async move { r.pigeon().person(STAFF).cancel_turn(c).await },
    )
    .await;
    let _ = fenced_then_released(&r, c, move |r| async move {
        let answer: magpie_api::turns::AgentPermissionResponse =
            serde_json::from_value(serde_json::json!({"behavior": "allow"})).unwrap();
        r.pigeon().person(STAFF).answer_ask(c, "ask_1", &answer).await
    })
    .await;
    // M16 create
    fenced_then_released(
        &r,
        c,
        move |r| async move { r.pigeon().person(STAFF).create_share(c).await },
    )
    .await
    .unwrap();
    // share revoke: 409 route legacy at once while unfenced legacy, waits in a fence
    set_migrating(&r, c).await;
    let started = Instant::now();
    let e = r.pigeon().person(STAFF).revoke_share(&early.id).await.unwrap_err();
    assert!(legacy_409(&e), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    let sid = early.id.clone();
    fenced_then_released(&r, c, move |r| async move {
        r.pigeon().person(STAFF).revoke_share(&sid).await
    })
    .await
    .unwrap();
    // M7 is a_send_during_a_fence_waits_and_becomes_the_first_native_turn; M6 purge last
    r.pigeon().person(STAFF).delete_conversation(c).await.unwrap();
    fenced_then_released(&r, c, move |r| async move {
        r.pigeon().person(STAFF).purge_conversation(c).await
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_stranded_ticket_expires_and_a_retried_ticket_call_reuses_the_open_one() {
    let db = common::fresh_db("lg_strand").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = migrating(&r, ALICE, "s").await;
    let take = |kind: TicketKind, cmid: Option<&str>| {
        let req = TicketRequest {
            kind,
            client_message_id: cmid.map(str::to_owned),
        };
        let p = r.pigeon();
        async move { p.person(ALICE).legacy_ticket(c, &req).await.unwrap().ticket.unwrap() }
    };
    // a retried call (the first timed out after it committed) gets the open ticket; no id: a new one each time
    let first = take(TicketKind::Send, Some("s:1")).await;
    assert_eq!(take(TicketKind::Send, Some("s:1")).await, first);
    let plain = take(TicketKind::Rename, None).await;
    assert_ne!(take(TicketKind::Rename, None).await, plain);
    // only a send's ticket carries a clientMessageId: a send's upload tickets, one per file, are each their own, so
    // the first file's `applied` closes only its own ticket
    for kind in [TicketKind::Upload, TicketKind::Stop] {
        let e = r
            .pigeon()
            .person(ALICE)
            .legacy_ticket(
                c,
                &TicketRequest {
                    kind,
                    client_message_id: Some("s:1".into()),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest), "{kind:?}");
    }
    let up1 = take(TicketKind::Upload, None).await;
    let up2 = take(TicketKind::Upload, None).await;
    assert_ne!(up1, up2);
    done(&r, up1, TicketOutcome::Applied).await.unwrap();
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
    done(&r, up2, TicketOutcome::Applied).await.unwrap();
    // every ticket answer carries its issue time; a repeat answers the first one's
    let answer = |cmid: &'static str| {
        let p = r.pigeon();
        async move {
            p.person(ALICE)
                .legacy_ticket(
                    c,
                    &TicketRequest {
                        kind: TicketKind::Send,
                        client_message_id: Some(cmid.into()),
                    },
                )
                .await
                .unwrap()
        }
    };
    let issued = answer("s:1").await.issued_at.unwrap().0;
    let row_issued: time::OffsetDateTime = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select issued_at from legacy_tickets where ticket = $1", &[&first])
        .await
        .unwrap()
        .get(0);
    assert!(
        (issued - row_issued).abs() < time::Duration::milliseconds(1),
        "{issued} {row_issued}"
    );
    // a fresh ticket's whole hold is left; a reuse answers what is left of the first one's
    let fresh = answer("s:1").await;
    assert_eq!(fresh.ticket, Some(first));
    let hold = fresh.hold_ms.unwrap();
    assert!(hold <= 60_000 && hold > 55_000, "{hold}");
    assert!(fresh.supersedes.is_none());
    assert!(fresh.reused, "a reuse says so");
    // a repeat past TICKET_REUSE_MAX (too little hold left to hand back): a new ticket, issued now, that names the old
    // one, while the old one stays open (a write started under it may still be in flight)
    let aged = magpie_api::legacy::TICKET_REUSE_MAX.as_secs_f64() + 1.0;
    r.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update legacy_tickets set issued_at = issued_at - make_interval(secs => $2) where ticket = $1",
            &[&first, &aged],
        )
        .await
        .unwrap();
    let late = answer("s:1").await;
    let late_ticket = late.ticket.unwrap();
    assert_ne!(late_ticket, first);
    assert!(!late.reused);
    let hold = late.hold_ms.unwrap();
    assert!(hold <= 60_000 && hold > 55_000, "a new ticket's whole hold: {hold}");
    let sup = late.supersedes.clone().expect("names the superseded ticket");
    assert_eq!(sup.ticket, first);
    // TICKET_SETTLE_MAX (90 s) less the first ticket's 31 s and change
    assert!(sup.settle_ms <= 59_000 && sup.settle_ms > 54_000, "{}", sup.settle_ms);
    let age = time::OffsetDateTime::now_utc() - late.issued_at.unwrap().0;
    assert!(age.abs() < time::Duration::seconds(5), "{age}");
    let again = answer("s:1").await;
    assert_eq!(again.ticket, Some(late_ticket), "the new one is the open ticket now");
    assert!(
        again.supersedes.is_none(),
        "only the superseding answer names the old ticket"
    );
    let first_open: bool = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select done_at is null and superseded_at is not null from legacy_tickets where ticket = $1",
            &[&first],
        )
        .await
        .unwrap()
        .get(0);
    assert!(first_open, "the superseded ticket stays open");
    done(&r, late_ticket, TicketOutcome::Started).await.unwrap();
    let open: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from legacy_tickets where conversation_id = $1 and done_at is null",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(open, 3, "the superseded send ticket and the two renames");
    // pigeon never reports (it died): the conversation cannot be fenced, so it stays legacy
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
    // the importer expires only tickets issued before its bound
    let before: time::OffsetDateTime = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select min(issued_at) from legacy_tickets where conversation_id = $1",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(magpie::legacy::tickets::expire(r.pool(), c, before).await.unwrap(), 0);
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
    let now: time::OffsetDateTime = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select clock_timestamp()", &[])
        .await
        .unwrap()
        .get(0);
    assert!((now - magpie::legacy::tickets::now(r.pool()).await.unwrap()).abs() < time::Duration::seconds(5));
    // the hand-over loop's bound: a read that started now settles nothing issued within TICKET_SETTLE_MAX of it (the
    // superseded ticket is 31 s old, the others fresh), however quiet the old child looked
    assert_eq!(
        magpie::legacy::tickets::expire_settled(r.pool(), c, now).await.unwrap(),
        0
    );
    assert_eq!(fence::fence(r.pool(), c).await.unwrap(), Fence::OpenTicket);
    let settled = now + time::Duration::try_from(magpie_api::legacy::TICKET_SETTLE_MAX).unwrap();
    assert_eq!(
        magpie::legacy::tickets::expire_settled(r.pool(), c, settled + time::Duration::seconds(1))
            .await
            .unwrap(),
        3
    );
    // a late report keeps the expiry; the same clientMessageId now opens a new ticket
    done(&r, first, TicketOutcome::Started).await.unwrap();
    let outcome: String = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select outcome from legacy_tickets where ticket = $1", &[&first])
        .await
        .unwrap()
        .get(0);
    assert_eq!(outcome, "expired");
    let again = take(TicketKind::Send, Some("s:1")).await;
    assert_ne!(again, first);
    done(&r, again, TicketOutcome::Refused).await.unwrap();
    // with nothing open, the fence and the release go through (as `release --force` does after an expiry)
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    fence::release(r.pool(), c, &Release::default(), "forced", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    assert!(!r.pigeon().person(ALICE).get_conversation(c).await.unwrap().migrating);
}

/// A send's ticket handed to a second request (a reuse) gets a take of its own: only the latest take may claim the
/// write, a `refused` from the earlier take changes nothing, and the reuse says how long a claimed write needs to
/// settle. A later ticket of the same send names the one the old child accepted (`prior`), and no write can be
/// claimed under it; a ticket closed `refused` is no prior.
#[tokio::test]
async fn a_reused_ticket_has_one_writer_and_a_send_names_its_prior() {
    let db = common::fresh_db("lg_take").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = migrating(&r, ALICE, "t").await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let send = |cmid: &str| TicketRequest {
        kind: TicketKind::Send,
        client_message_id: Some(cmid.to_owned()),
    };
    let claim = |ticket: Uuid, take: Uuid| {
        let p = r.pigeon();
        async move {
            p.person(ALICE)
                .legacy_ticket_claim(ticket, &TicketClaim { take })
                .await
                .unwrap()
        }
    };
    let report = |ticket: Uuid, outcome: TicketOutcome, take: Uuid| {
        let p = r.pigeon();
        async move {
            p.person(ALICE)
                .legacy_ticket_done(
                    ticket,
                    &TicketDone {
                        outcome,
                        take: Some(take),
                    },
                )
                .await
                .unwrap();
        }
    };
    let outcome = |ticket: Uuid| {
        let pool = r.pool().clone();
        async move {
            pool.get()
                .await
                .unwrap()
                .query_one("select outcome from legacy_tickets where ticket = $1", &[&ticket])
                .await
                .unwrap()
                .get::<_, Option<String>>(0)
        }
    };
    // the first request claims its write
    let first = a.legacy_ticket(c, &send("r:1")).await.unwrap();
    let (tk, take_a) = (first.ticket.unwrap(), first.take.unwrap());
    assert!(
        !first.reused && first.prior.is_none() && first.settle_ms.is_none(),
        "{first:?}"
    );
    assert_eq!(
        claim(tk, take_a).await,
        TicketClaimed {
            claimed: true,
            held_elsewhere: false
        }
    );
    // the tab's resend on another request: the same ticket under a new take, with the claimed write's settle time
    let second = a.legacy_ticket(c, &send("r:1")).await.unwrap();
    assert_eq!(second.ticket, Some(tk));
    assert!(second.reused);
    let take_b = second.take.unwrap();
    assert_ne!(take_b, take_a);
    let settle = second.settle_ms.unwrap();
    assert!(settle <= 30_000 && settle > 25_000, "{settle}");
    // the first holder can no longer write, and its giving up does not close the ticket under the second
    assert_eq!(
        claim(tk, take_a).await,
        TicketClaimed {
            claimed: false,
            held_elsewhere: true
        }
    );
    report(tk, TicketOutcome::Refused, take_a).await;
    assert_eq!(
        outcome(tk).await,
        None,
        "a refused from an earlier take changes nothing"
    );
    assert!(claim(tk, take_b).await.claimed);
    // a real outcome closes it whichever holder reports it
    report(tk, TicketOutcome::Started, take_a).await;
    assert_eq!(outcome(tk).await.as_deref(), Some("started"));
    // a later resend of the same message: a new ticket that names the accepted one, under which nothing is claimed
    let third = a.legacy_ticket(c, &send("r:1")).await.unwrap();
    let t3 = third.ticket.unwrap();
    assert_ne!(t3, tk);
    assert!(!third.reused);
    assert_eq!(
        third.prior,
        Some(PriorSend {
            ticket: tk,
            accepted: true
        })
    );
    assert_eq!(third.settle_ms, Some(0));
    assert_eq!(
        claim(t3, third.take.unwrap()).await,
        TicketClaimed {
            claimed: false,
            held_elsewhere: false
        }
    );
    report(t3, TicketOutcome::Refused, third.take.unwrap()).await;
    assert_eq!(outcome(t3).await.as_deref(), Some("refused"));
    // a reuse of a ticket nobody claimed has nothing to wait for; the latest take's refused closes it
    let d = a.legacy_ticket(c, &send("r:2")).await.unwrap();
    let e = a.legacy_ticket(c, &send("r:2")).await.unwrap();
    assert_eq!(e.ticket, d.ticket);
    assert_eq!(e.settle_ms, Some(0));
    assert!(e.prior.is_none());
    report(d.ticket.unwrap(), TicketOutcome::Refused, d.take.unwrap()).await;
    assert_eq!(outcome(d.ticket.unwrap()).await, None);
    report(e.ticket.unwrap(), TicketOutcome::Refused, e.take.unwrap()).await;
    assert_eq!(outcome(d.ticket.unwrap()).await.as_deref(), Some("refused"));
    // a ticket closed `refused` wrote nothing: no prior
    let f = a.legacy_ticket(c, &send("r:2")).await.unwrap();
    assert_ne!(f.ticket, d.ticket);
    assert!(f.prior.is_none() && f.settle_ms.is_none(), "{f:?}");
    // past the hold, a claim is refused (a write needs a new ticket)
    let hold = magpie_api::legacy::TICKET_HOLD_MAX.as_secs_f64() + 1.0;
    r.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update legacy_tickets set issued_at = issued_at - make_interval(secs => $2) where ticket = $1",
            &[&f.ticket.unwrap(), &hold],
        )
        .await
        .unwrap();
    assert!(!claim(f.ticket.unwrap(), f.take.unwrap()).await.claimed);
    // an expired ticket of the send is a prior that may have written
    magpie::legacy::tickets::expire(
        r.pool(),
        c,
        time::OffsetDateTime::now_utc() + time::Duration::seconds(5),
    )
    .await
    .unwrap();
    let g = a.legacy_ticket(c, &send("r:2")).await.unwrap();
    assert_eq!(
        g.prior,
        Some(PriorSend {
            ticket: f.ticket.unwrap(),
            accepted: false
        })
    );
    assert_eq!(g.settle_ms, Some(0), "an expired ticket has settled");
}

#[tokio::test]
async fn the_release_judges_the_lease_by_the_clock() {
    let db = common::fresh_db("lg_clock").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let pool = r.pool().clone();
    let in_tx = |c: Uuid, sleep: f64| {
        let pool = pool.clone();
        async move {
            aviary_pg::tx!(&pool, |tx| -> magpie::error::Result<
                Result<fence::Released, NotReleased>,
            > {
                // the importer's own work earlier in the release transaction
                tx.execute("select pg_sleep($1)", &[&sleep]).await?;
                fence::release_in(tx, c, &Release::default()).await
            })
            .await
            .unwrap()
        }
    };
    // a release 1.2 s into its transaction measures the fence from the clock, not the transaction's start
    let a = migrating(&r, ALICE, "c1").await;
    assert!(matches!(fence::fence(r.pool(), a).await.unwrap(), Fence::Fenced(_)));
    let held = in_tx(a, 1.2).await.unwrap().held;
    assert!(held >= 1.2, "{held}");
    // a fence that lapsed by the clock during the transaction is not released
    let b = migrating(&r, ALICE, "c2").await;
    assert!(matches!(fence::fence(r.pool(), b).await.unwrap(), Fence::Fenced(_)));
    assert_eq!(in_tx(b, 3.2).await, Err(NotReleased::NotFenced));
    assert!(r.pigeon().person(ALICE).get_conversation(b).await.unwrap().migrating);
}

#[tokio::test]
async fn a_draining_replica_still_waits_out_a_fence() {
    use magpie::conversations::Person;
    let db = common::fresh_db("lg_drain").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let c = migrating(&r, ALICE, "d").await;
    let d = migrating(&r, ALICE, "d2").await;
    let alice = Person {
        email: ALICE.to_owned(),
        viewer: None,
    };
    r.start_drain();
    // released during the wait: native, as without a drain
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    let r2 = r.clone();
    let person = alice.clone();
    let waiting = tokio::spawn(async move {
        let req = TicketRequest {
            kind: TicketKind::Send,
            client_message_id: None,
        };
        magpie::legacy::tickets::take(r2.app(), &person, c, &req).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "a draining replica still waits in the fence");
    fence::release(r.pool(), c, &Release::default(), "idle", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.await.unwrap().unwrap().route, TicketRoute::Native);
    // never released: the lease runs out, then the conversation is plainly legacy (a ticket, not a 503)
    assert!(matches!(fence::fence(r.pool(), d).await.unwrap(), Fence::Fenced(_)));
    let started = Instant::now();
    let req = TicketRequest {
        kind: TicketKind::Send,
        client_message_id: None,
    };
    let tk = magpie::legacy::tickets::take(r.app(), &alice, d, &req).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(2_500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tk.route, TicketRoute::Legacy);
    assert_eq!(
        fence::await_unfenced(r.app(), d).await.unwrap(),
        Some(fence::Owner::Legacy)
    );
}

#[tokio::test]
async fn misses_are_ingested_together_under_one_deadline() {
    let db = common::fresh_db("lg_deadline").await;
    let t = common::Tokens::new();
    let pool = aviary_pg::connect(&db.url, "test", 4, Duration::from_secs(10)).unwrap();
    let known = Arc::new(Mutex::new(HashMap::new()));
    let (url, calls) = fake_import_slow(pool, known.clone(), Duration::from_secs(2)).await;
    let bucket = common::test_bucket(&db.url).await;
    let r = common::replica_import(&db.url, &t, "http://127.0.0.1:1", common::idle(), bucket, &url).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let mut files = Vec::new();
    for i in 0..4 {
        let up = upload(&r, ALICE, &format!("f{i}.pdf"), format!("%PDF-1.4 {i}").as_bytes()).await;
        let name = format!("17620541000{i}0-f{i}.pdf");
        known.lock().unwrap().insert(name.clone(), up.upload_id);
        files.push(file(&old_path(ALICE, &name), &up));
    }
    // the same old path twice is one attachment
    files.push(files[0].clone());
    let c = common::files::conversation(&r, ALICE, "dl").await;
    let started = Instant::now();
    a.send_turn(c, &send("dl:1", files)).await.unwrap();
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(4_500),
        "four 2 s ingests run together: {took:?}"
    );
    assert_eq!(*calls.lock().unwrap(), 4);
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads where conversation_id = $1", &[&c])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 4);
    // the index the lookup uses is built (concurrently, by its own migration) and valid
    let valid: bool = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid
             where c.relname = 'uploads_legacy_path'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(valid);
}

#[tokio::test]
async fn an_ingest_past_the_deadline_is_503_and_a_miss_is_always_ingested() {
    let db = common::fresh_db("lg_slow").await;
    let t = common::Tokens::new();
    let pool = aviary_pg::connect(&db.url, "test", 2, Duration::from_secs(10)).unwrap();
    let known = Arc::new(Mutex::new(HashMap::new()));
    let (url, calls) = fake_import_slow(pool, known.clone(), Duration::from_secs(20)).await;
    let bucket = common::test_bucket(&db.url).await;
    let r = common::replica_import(&db.url, &t, "http://127.0.0.1:1", common::idle(), bucket, &url).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let up = upload(&r, ALICE, "s.pdf", b"%PDF-1.4 s").await;
    let c = common::files::conversation(&r, ALICE, "sl").await;
    let files = vec![
        file(&old_path(ALICE, "1-a.pdf"), &up),
        file(&old_path(ALICE, "2-b.pdf"), &up),
        file(&old_path(ALICE, "3-c.pdf"), &up),
    ];
    let started = Instant::now();
    let e = a.send_turn(c, &send("sl:1", files.clone())).await.unwrap_err();
    let took = started.elapsed();
    assert_eq!(e.code(), Some(&ErrorCode::Unavailable));
    assert!(
        took >= Duration::from_millis(4_500) && took < Duration::from_millis(6_500),
        "one deadline for the whole send: {took:?}"
    );
    assert_eq!(*calls.lock().unwrap(), 3);
    // a native send to an unfenced legacy conversation: 409 route legacy at once, no ingest and no copy
    let legacy = migrating(&r, ALICE, "lg").await;
    let uploads_before: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    let started = Instant::now();
    let e = a.send_turn(legacy, &send("lg:1", files.clone())).await.unwrap_err();
    assert!(legacy_409(&e), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(*calls.lock().unwrap(), 3);
    let uploads_after: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(uploads_before, uploads_after);
    // every conversation recorded owned changes nothing (a reverse hand-over may reopen one): a miss is still
    // ingested before any answer
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update import_control set all_owned_at = now()", &[])
        .await
        .unwrap();
    let e = a.send_turn(c, &send("sl:2", files)).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::Unavailable));
    assert_eq!(*calls.lock().unwrap(), 6);
}

#[tokio::test]
async fn a_teed_upload_attached_by_its_id_and_purged_keeps_its_old_path() {
    let db = common::fresh_db("lg_teepurge").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let teed = upload(&r, ALICE, "g.pdf", b"%PDF-1.4 gate").await;
    let path = old_path(ALICE, "1762054020000-g.pdf");
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: path.clone(),
        },
    )
    .await
    .unwrap();
    // pigeon attaches the upload itself by its magpie id (the gate-time path), then the conversation is purged
    let c = common::files::conversation(&r, ALICE, "g").await;
    a.send_turn(
        c,
        &send("g:1", vec![file(&format!("uploads/{}/g.pdf", teed.upload_id), &teed)]),
    )
    .await
    .unwrap();
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
    a.delete_conversation(c).await.unwrap();
    a.purge_conversation(c).await.unwrap();
    let gone: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads where id = $1", &[&teed.upload_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    // a composer still holding the old path sends it elsewhere: it resolves through the record
    let c2 = common::files::conversation(&r, ALICE, "g2").await;
    a.send_turn(c2, &send("g2:1", vec![file(&path, &teed)])).await.unwrap();
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads where conversation_id = $1", &[&c2])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}

#[tokio::test]
async fn an_importer_release_in_its_own_transaction_is_counted_after_the_commit() {
    let db = common::fresh_db("lg_fence_metric").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    // the import Deployment registers the window's metrics on its own registry (its 9090 /metrics)
    let mut registry = prometheus_client::registry::Registry::with_prefix("magpie");
    let m = magpie::legacy::Metrics::register(&mut registry);
    let count = |registry: &prometheus_client::registry::Registry| {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, registry).unwrap();
        out.lines()
            .find_map(|l| l.strip_prefix("magpie_fence_seconds_count "))
            .map(|v| v.trim().parse::<u64>().unwrap())
            .unwrap()
    };
    let c = migrating(&r, ALICE, "fm").await;
    assert!(matches!(fence::fence(r.pool(), c).await.unwrap(), Fence::Fenced(_)));
    let imported = upload(&r, ALICE, "fm.pdf", b"fm").await;
    // a release transaction that rolls back after release_in: no release, nothing counted
    let rolled: magpie::error::Result<()> = aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<()> {
        let _ = fence::release_in(tx, c, &Release::default()).await?;
        Err(magpie::error::Error::Internal("the importer's pass failed".into()))
    })
    .await;
    assert!(rolled.is_err());
    assert!(r.pigeon().person(ALICE).get_conversation(c).await.unwrap().migrating);
    assert_eq!(count(&registry), 0);
    // the importer's release: its ingestion pass and import_sources update, then release_in last, in one
    // transaction; recorded once it committed
    let released = aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<fence::Released> {
        paths::import_upload(tx, ALICE, "fm.pdf", imported.upload_id, &[4u8; 32]).await?;
        tx.execute(
            "update import_sources set released_at = now(), release_reason = 'idle' where conversation_id = $1",
            &[&c],
        )
        .await?;
        Ok(fence::release_in(tx, c, &Release::default()).await?.unwrap())
    })
    .await
    .unwrap();
    released.record(&m);
    assert!((0.0..3.0).contains(&released.held), "{}", released.held);
    assert_eq!(count(&registry), 1);
    assert!(!r.pigeon().person(ALICE).get_conversation(c).await.unwrap().migrating);
}

#[tokio::test]
async fn a_send_fenced_between_its_owner_read_and_admission_resolves_its_old_path() {
    let db = common::fresh_db("lg_fence_race").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let imported = upload(&r, ALICE, "race.pdf", b"%PDF-1.4 race").await;
    aviary_pg::tx!(r.pool(), |tx| -> magpie::error::Result<()> {
        paths::import_upload(tx, ALICE, "1-race.pdf", imported.upload_id, &[5u8; 32]).await
    })
    .await
    .unwrap();
    let c = migrating(&r, ALICE, "race").await;
    // the importer's fence transaction holds the row lock: the send reads the owner (legacy, unfenced: no
    // resolution) and then waits in admission's lock
    let (locked, go) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let holder = {
        let (pool, locked, go) = (r.pool().clone(), locked.clone(), go.clone());
        tokio::spawn(async move {
            aviary_pg::tx!(&pool, |tx| -> magpie::error::Result<()> {
                tx.execute("select 1 from conversations where id = $1 for update", &[&c])
                    .await?;
                locked.notify_one();
                go.notified().await;
                tx.execute(
                    "update conversations set legacy_fence_at = clock_timestamp() where id = $1",
                    &[&c],
                )
                .await?;
                Ok(())
            })
            .await
            .unwrap();
        })
    };
    locked.notified().await;
    let conn = r.pool().get().await.unwrap();
    let path = old_path(ALICE, "1-race.pdf");
    let r2 = r.clone();
    let req = send("race:1", vec![file(&path, &imported)]);
    let sent = tokio::spawn(async move { r2.pigeon().person(ALICE).send_turn(c, &req).await });
    let waiting = common::eventually(Duration::from_secs(5), async || {
        conn.query_one(
            "select count(*) from pg_stat_activity
             where datname = current_database() and wait_event_type = 'Lock' and pid <> pg_backend_pid()",
            &[],
        )
        .await
        .unwrap()
        .get::<_, i64>(0)
            > 0
    })
    .await;
    assert!(waiting, "the send waits on the row lock in admission");
    go.notify_one();
    holder.await.unwrap();
    // admission answers conversation_migrating (fenced): the send waits for the release, then is native
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!sent.is_finished(), "the send waits in the fence");
    fence::release(r.pool(), c, &Release::default(), "idle", &r.app().legacy)
        .await
        .unwrap()
        .unwrap();
    let turn = sent
        .await
        .unwrap()
        .expect("a fenced send proceeds natively")
        .into_inner()
        .turn;
    let now_native = r.pigeon().person(ALICE).get_conversation(c).await.unwrap();
    assert_eq!(now_native.turn.map(|x| x.id), Some(turn.id));
    // the old path was resolved on the attempt after the fence: a fresh copy attached
    let row = conn
        .query_one(
            "select count(*), bool_and(id <> $2) from uploads where conversation_id = $1 and name = 'race.pdf'",
            &[&c, &imported.upload_id],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    assert_eq!(row.get::<_, Option<bool>>(1), Some(true));
}

/// `GET /v1/legacy/window` reads the gate, the rollback mark and what is legacy on the database; `upload_raw` hands
/// pigeon the person's own upload bytes for the copy-at-send and nobody else's.
#[tokio::test]
async fn the_window_state_and_an_uploads_raw_bytes() {
    let db = common::fresh_db("lg_window").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let alice = r.pigeon();
    let alice = alice.person(ALICE);
    let w = alice.legacy_window().await.unwrap();
    assert_eq!(w, magpie_api::legacy::WindowState::closed());
    assert!(!w.tees_uploads() && !w.creates_legacy());
    let c = migrating(&r, ALICE, "w").await;
    let w = alice.legacy_window().await.unwrap();
    assert!(w.any_legacy && w.handover_allowed_at.is_none() && w.rollback_at.is_none());
    assert_eq!(w.person_legacy, 1);
    assert!(w.tees_uploads(), "T0 to the gate: uploads are teed");
    let bob = r.pigeon();
    let b = bob.person(BOB).legacy_window().await.unwrap();
    assert!(b.any_legacy);
    assert_eq!(b.person_legacy, 0);
    let db_ = r.pool().get().await.unwrap();
    db_.execute("update import_control set handover_allowed_at = now()", &[])
        .await
        .unwrap();
    let w = alice.legacy_window().await.unwrap();
    assert!(w.handover_allowed_at.is_some() && !w.tees_uploads() && !w.creates_legacy());
    db_.execute("update import_control set rollback_at = now()", &[])
        .await
        .unwrap();
    let w = alice.legacy_window().await.unwrap();
    assert!(w.tees_uploads() && w.creates_legacy());
    db_.execute("update conversations set migrating = false where id = $1", &[&c])
        .await
        .unwrap();
    let w = alice.legacy_window().await.unwrap();
    assert!(!w.any_legacy && !w.tees_uploads());

    let up = upload(&r, ALICE, "r.pdf", b"%PDF-1.4 raw bytes").await;
    let got = alice.upload_raw(up.upload_id).await.unwrap();
    assert_eq!(
        got.headers().get("content-type").unwrap().to_str().unwrap(),
        up.mime_type
    );
    assert_eq!(got.bytes().await.unwrap().as_ref(), b"%PDF-1.4 raw bytes");
    let theirs = bob.person(BOB).upload_raw(up.upload_id).await.unwrap_err();
    assert_eq!(theirs.api().unwrap().status, 404);
}

/// magpie passes the shared ticket suite (`aviary_conformance::tickets`) that pigeon's model of it passes too
/// (`crates/pigeon/tests/ticket_rules.rs`), so the LU contract rig's tickets behave as magpie's do.
#[tokio::test]
async fn the_ticket_rules_pass_the_shared_suite() {
    use aviary_conformance::tickets::{self, Hooks, Options};
    let db = common::fresh_db("lg_suite").await;
    let t = common::Tokens::new();
    let r = Arc::new(common::replica(&db.url, &t).await);
    let (a, b, c, d) = (r.clone(), r.clone(), r.clone(), r.clone());
    let n = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let hooks = Hooks {
        legacy_conversation: Box::new(move || {
            let (r, n) = (a.clone(), n.clone());
            Box::pin(async move {
                let k = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                migrating(&r, ALICE, &format!("suite-{k}")).await
            })
        }),
        age_ticket: Box::new(move |ticket, by| {
            let r = b.clone();
            Box::pin(async move {
                r.pool()
                    .get()
                    .await
                    .unwrap()
                    .execute(
                        "update legacy_tickets set issued_at = issued_at - make_interval(secs => $2) where ticket = $1",
                        &[&ticket, &by.as_secs_f64()],
                    )
                    .await
                    .unwrap();
            })
        }),
        outcome: Box::new(move |ticket| {
            let r = c.clone();
            Box::pin(async move {
                let o: Option<String> = r
                    .pool()
                    .get()
                    .await
                    .unwrap()
                    .query_one("select outcome from legacy_tickets where ticket = $1", &[&ticket])
                    .await
                    .unwrap()
                    .get(0);
                o.map(|o| serde_json::from_value(serde_json::Value::String(o)).unwrap())
            })
        }),
        release: Box::new(move |conv| {
            let r = d.clone();
            Box::pin(async move {
                r.pool()
                    .get()
                    .await
                    .unwrap()
                    .execute("update conversations set migrating = false where id = $1", &[&conv])
                    .await
                    .unwrap();
            })
        }),
    };
    let report = aviary_conformance::report::Report::new("tickets", vec![], true);
    tickets::run(
        Options {
            client: r.pigeon(),
            person: ALICE.into(),
            hooks,
        },
        &report,
    )
    .await;
    assert!(report.passed(), "failures: {:#?}", report.failures());
    assert_eq!(report.results().len(), 8);
}
