//! hermit's routes and the admin reads (magpie 9.6, 9.7, 13, 16.3, 16.4): a leaver's purge (refused while hermit
//! allows them, idempotent, shares revoked `offboarded`), the workspaces magpie still references, the admin reads and
//! their audit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::script::Reply;
use common::ev::{self, assistant, text, turn_ended, turn_started, user_sent};
use common::files::{gateway, grant_lease};
use magpie_api::errors::ErrorCode;
use magpie_api::events::EventsQuery;
use magpie_api::ops::{
    AdminListQuery, PersonPurgeRequest, PurgeReason, ViewAsEvent, ViewAsLogQuery, WorkspacesExistsRequest,
};
use magpie_api::uploads::UploadKind;
use serde_json::json;
use uuid::Uuid;

/// Allowed by fake-hermit's catalogue (`@example.org`).
const STAFF: &str = "staff@example.org";
/// Not in it: a leaver.
const LEAVER: &str = "leaver@example.com";

async fn with_reply(r: &common::Replica, person: &str) -> Uuid {
    let c = ev::conversation(&r.pigeon(), person).await;
    let t = Uuid::new_v4();
    ev::append(
        r.pool(),
        person,
        c,
        vec![
            user_sent("m1", "q"),
            ev::in_turn(turn_started(t), t),
            ev::in_turn(assistant("msg_1", json!([text("a")])), t),
            ev::in_turn(turn_ended(t, "completed"), t),
        ],
    )
    .await;
    c
}

#[tokio::test]
async fn a_leaver_is_purged_only_when_hermit_no_longer_allows_them() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("ppurge").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let purge = PersonPurgeRequest {
        reason: PurgeReason::Offboarded,
    };

    let staff = with_reply(&r, STAFF).await;
    let e = r.hermit().service(None).person_purge(STAFF, &purge).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::PersonAllowed));
    assert!(r.pigeon().person(STAFF).get_conversation(staff).await.is_ok());

    let a = with_reply(&r, LEAVER).await;
    // a 404 from hermit (a route missing during version skew) is not a "no": 503, nothing purged
    hermit
        .script()
        .once("person", Reply::error(404, "not_found", "no such route"));
    let e = r.hermit().service(None).person_purge(LEAVER, &purge).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::Unavailable));
    assert!(r.pigeon().person(LEAVER).get_conversation(a).await.is_ok());
    let b = ev::conversation(&r.pigeon(), LEAVER).await;
    r.pigeon().person(LEAVER).delete_conversation(b).await.unwrap();
    hermit.write_file(a, "x.txt", b"x");
    let share = r.pigeon().person(LEAVER).create_share(a).await.unwrap();
    r.pigeon()
        .person(LEAVER)
        .upload("n.txt", UploadKind::File, "text/plain", b"unattached".to_vec())
        .await
        .unwrap();
    let done = r.hermit().service(None).person_purge(LEAVER, &purge).await.unwrap();
    assert_eq!(done.conversations, 2);
    let sql = r.pool().get().await.unwrap();
    let left: i64 = sql
        .query_one(
            "select (select count(*) from conversations where person = $1) + (select count(*) from uploads where person = $1)",
            &[&LEAVER],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    let reason: Option<String> = sql
        .query_one("select revoke_reason from shares where id = $1", &[&share.id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason.as_deref(), Some("offboarded"));
    assert_eq!(
        r.hermit()
            .service(None)
            .person_purge(LEAVER, &purge)
            .await
            .unwrap()
            .conversations,
        0
    );
    // the staff member's conversation is untouched; hermit unreachable is 503
    assert!(r.pigeon().person(STAFF).get_conversation(staff).await.is_ok());
    drop(running);
    let r2 = common::replica(&db.url, &t).await;
    let e = r2
        .hermit()
        .service(None)
        .person_purge(LEAVER, &purge)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::Unavailable));
}

#[tokio::test]
async fn workspaces_magpie_still_references() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("wsexists").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let live = with_reply(&r, STAFF).await;
    hermit.write_file(live, "x", b"x");
    let deleted = ev::conversation(&r.pigeon(), STAFF).await;
    r.pigeon().person(STAFF).delete_conversation(deleted).await.unwrap();
    let share = r.pigeon().person(STAFF).create_share(live).await.unwrap();
    let revoked = r.pigeon().person(STAFF).create_share(live).await.unwrap();
    r.pigeon().person(STAFF).revoke_share(&revoked.id).await.unwrap();
    let unknown = Uuid::new_v4();
    let ids = vec![
        live,
        deleted,
        unknown,
        magpie::shares::workspace_of(&share.id),
        magpie::shares::workspace_of(&revoked.id),
    ];
    let got = r
        .hermit()
        .service(None)
        .workspaces_exists(&WorkspacesExistsRequest { ids })
        .await
        .unwrap();
    assert_eq!(got.exists, vec![live, deleted, magpie::shares::workspace_of(&share.id)]);
    let many = WorkspacesExistsRequest {
        ids: (0..1001).map(|_| Uuid::new_v4()).collect(),
    };
    let e = r.hermit().service(None).workspaces_exists(&many).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
}

async fn status(r: &common::Replica) -> magpie_api::ops::LegacyImportStatus {
    r.hermit().service(None).legacy_import().await.unwrap()
}

async fn sql(r: &common::Replica, q: &str) {
    r.pool().get().await.unwrap().batch_execute(q).await.unwrap();
}

/// `GET /v1/hermit/legacy-import`: incomplete until a clean `verify --all` is recorded, then the record alone (the
/// importer's tables went in A6).
#[tokio::test]
async fn the_import_is_complete_from_its_record() {
    let db = common::fresh_db("legacyimport").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = status(&r).await;
    assert!(!s.complete && s.verified_at.is_none(), "{s:?}");
    sql(
        &r,
        "insert into legacy_import_verified (verified_at, conversations) values (now(), 1)",
    )
    .await;
    let s = status(&r).await;
    assert!(s.complete && s.verified_at.is_some(), "{s:?}");
    assert_eq!((s.conversations, s.why), (1, None));
    // pigeon may not ask
    let e = r.pigeon().service(None).legacy_import().await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::WrongCaller));
}

#[tokio::test]
async fn admin_reads_are_audited_once_a_day() {
    let db = common::fresh_db("adminreads").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = with_reply(&r, STAFF).await;
    let lease = grant_lease(r.pool(), c).await;
    let hb = json!({"turnId": null, "state": "idle", "tasks": 0, "lastTurn": null, "gen": 7,
                    "supervisor": {"version": "supervisor-abc", "bootId": "b1", "restarts": 2},
                    "cc": {"quotaExceeded": false, "blockedWrites": 0, "oldestBlockedMs": 0}});
    let st = gateway(
        &r,
        reqwest::Method::POST,
        "/v1/sandbox/heartbeat",
        c,
        STAFF,
        Some(lease),
    )
    .json(&hb)
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(st, 204);

    // without Aviary-Admin: 400
    let e = r.admin().service(None).admin_conversation(c).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
    let admin = r.admin();
    let calls = admin.service(Some("root@example.com"));
    let got = calls.admin_conversation(c).await.unwrap();
    assert_eq!(got.person, STAFF);
    assert_eq!(got.lease.as_ref().unwrap().lease_id, lease);
    let heartbeat = got.heartbeat.unwrap();
    assert_eq!((heartbeat.generation, heartbeat.supervisor.restarts), (7, 2));
    let events = calls
        .admin_events(
            c,
            &EventsQuery {
                tail: true,
                limit: Some(2),
                ..EventsQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5]);
    assert!(events.has_older && !events.has_newer);
    assert_eq!(events.head_seq, 5);
    let after = calls
        .admin_events(
            c,
            &EventsQuery {
                after: Some(1),
                limit: Some(2),
                ..EventsQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(after.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
    assert!(after.has_older && after.has_newer);
    let page = calls.admin_timeline(c, &ev::tail(10)).await.unwrap();
    assert_ne!(page.rows.len(), 0);
    let listed = calls
        .admin_conversations(&AdminListQuery {
            person: Some(STAFF.into()),
            state: None,
        })
        .await
        .unwrap();
    assert_eq!(listed.conversations.len(), 1);
    // the first read today by this admin is recorded, once
    let log = r
        .hermit()
        .service(None)
        .ops_view_as(&ViewAsLogQuery::default())
        .await
        .unwrap();
    let reads: Vec<_> = log
        .view_as
        .iter()
        .filter(|e| e.event == ViewAsEvent::AdminRead)
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        (reads[0].admin.as_str(), reads[0].user.as_str()),
        ("root@example.com", STAFF)
    );
    assert_eq!(reads[0].conversation_id, Some(c));
    // concurrent first reads of another conversation by another admin: one row
    let d = with_reply(&r, STAFF).await;
    let mut reads = Vec::new();
    for _ in 0..8 {
        let a = r.admin();
        reads.push(tokio::spawn(async move {
            a.service(Some("other@example.com"))
                .admin_conversation(d)
                .await
                .unwrap();
        }));
    }
    for h in reads {
        h.await.unwrap();
    }
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from view_as_log where event = 'admin_read' and admin = 'other@example.com'
               and conversation_id = $1",
            &[&d],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    // the list shows titles: each listed conversation is audited as read, once a day, for that admin
    let admin3 = r.admin();
    for _ in 0..3 {
        let listed = admin3
            .service(Some("lister@example.com"))
            .admin_conversations(&AdminListQuery {
                person: Some(STAFF.into()),
                state: None,
            })
            .await
            .unwrap();
        assert_eq!(listed.conversations.len(), 2);
    }
    let listed: Vec<Uuid> = r
        .pool()
        .get()
        .await
        .unwrap()
        .query(
            "select conversation_id from view_as_log where event = 'admin_read' and admin = 'lister@example.com'
             order by conversation_id",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut want = vec![c, d];
    want.sort();
    assert_eq!(listed, want);
}

/// The deployment names the model a session's conversation starts on (its `pickedModel`, carried by every
/// `start_turn`), and reads the conversation back whoever its person is: the title for its own session list.
#[tokio::test]
async fn the_deployment_names_the_model_and_reads_the_conversation() {
    use magpie_api::conversations::RenameRequest;
    use magpie_api::ops::HermitCreateRequest;
    use magpie_api::turns::SendRequest;
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("hmodel").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let create = |key: &str, model: Option<&str>| HermitCreateRequest {
        person: STAFF.into(),
        idempotency_key: key.into(),
        message: SendRequest {
            client_message_id: format!("{key}:first"),
            text: "the session's first prompt".into(),
            images: vec![],
            files: vec![],
        },
        model: model.map(str::to_owned),
    };
    let made = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create("session:s1", Some("claude-opus-5-5")))
        .await
        .unwrap()
        .into_inner();
    let id = made.conversation.id;
    assert_eq!(made.conversation.picked_model.as_deref(), Some("claude-opus-5-5"));
    let payload: serde_json::Value = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select payload from commands where conversation_id = $1 and type = 'start_turn'",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(payload["model"], "claude-opus-5-5", "the first turn runs on it");
    // a repeat keeps the conversation as it is, whatever model it names
    let again = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create("session:s1", Some("claude-fable-5-2")))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.conversation.picked_model.as_deref(), Some("claude-opus-5-5"));
    // none named: none picked
    let plain = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create("session:s2", None))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(plain.conversation.picked_model, None);
    let e = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create("session:s3", Some("  ")))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::BadRequest));

    // the deployment reads it back, with the person's rename
    r.pigeon()
        .person(STAFF)
        .rename_conversation(id, &RenameRequest::title("Model picker"))
        .await
        .unwrap();
    let seen = r.hermit().service(None).hermit_conversation(id).await.unwrap();
    assert_eq!(seen.id, id);
    assert_eq!(seen.title.as_deref(), Some("Model picker"));
    assert_eq!(seen.picked_model.as_deref(), Some("claude-opus-5-5"));
    let e = r
        .hermit()
        .service(None)
        .hermit_conversation(uuid::Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::NotFound));
    // pigeon may not use hermit's route
    let e = r.pigeon().service(None).hermit_conversation(id).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::WrongCaller));
}

#[tokio::test]
async fn the_deployment_starts_a_persons_conversation_and_sends_its_turns() {
    use magpie_api::ops::{HermitCreateRequest, HermitSendRequest};
    use magpie_api::turns::SendRequest;
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("hstart").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let msg = |id: &str, text: &str| SendRequest {
        client_message_id: id.into(),
        text: text.into(),
        images: vec![],
        files: vec![],
    };
    let create = HermitCreateRequest {
        // hermit's spelling is normalised as pigeon's is
        person: "  Staff@Example.org ".into(),
        idempotency_key: "session:s1".into(),
        message: msg("first", "the session's first prompt"),
        model: None,
    };
    let made = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create)
        .await
        .unwrap();
    assert!(made.is_new());
    let c = made.into_inner();
    let id = c.conversation.id;
    assert!(c.turn.is_some(), "the first prompt is a turn");
    // the same key is the same conversation, for ever
    let again = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&create)
        .await
        .unwrap();
    assert!(!again.is_new());
    assert_eq!(again.into_inner().conversation.id, id);
    // it is the person's own: their list and their app see it
    let seen = r.pigeon().person(STAFF).get_conversation(id).await.unwrap();
    assert_eq!(seen.id, id);

    // a turn while the first one runs: 409 turn_running, as M7
    let e = r
        .hermit()
        .service(None)
        .hermit_send_turn(
            id,
            &HermitSendRequest {
                person: STAFF.into(),
                message: msg("w1", "a scheduled wakeup"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::TurnRunning));
    // another person's conversation is not found
    let e = r
        .hermit()
        .service(None)
        .hermit_send_turn(
            id,
            &HermitSendRequest {
                person: LEAVER.into(),
                message: msg("w2", "x"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::NotFound));
    // not an email
    let e = r
        .hermit()
        .service(None)
        .hermit_create_conversation(&HermitCreateRequest {
            person: "nobody".into(),
            idempotency_key: "k".into(),
            message: msg("m", "x"),
            model: None,
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
    // pigeon may not use hermit's routes
    let e = r
        .pigeon()
        .service(None)
        .hermit_create_conversation(&create)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::WrongCaller));
}
