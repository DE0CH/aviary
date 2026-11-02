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
}
