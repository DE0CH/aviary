//! The reverse hand-over of one conversation (cutover 14.2): an imported conversation released, then reversed against
//! a fake old cluster (its running sets and the switch module's restart route) and the writer sidecar's export route
//! served in the test: nothing while the person's old child runs something or its restart answers `busy` (owned and
//! native, the fence lifted); a native write during the reverse fence waits in its call, past one lease, and then
//! follows the reopen to the legacy route; the reopened conversation is synced to what the export wrote (the next pass
//! imports nothing again) and is pre-verified for a later release; a conversation born on the new side gets its
//! `import_sources` row; a person without an old schema is refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{World, assistant, user};
use magpie::conversations::Person;
use magpie_api::conversations::RenameRequest;
use magpie_api::errors::{ErrorCode, RouteHint};
use magpie_import::reverse::{self, Old, Outcome};
use magpie_import::{Importer, control, pass, verify};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

/// An imported conversation of `ka`, released by force (owned by magpie).
async fn owned(w: &World, ka: &str, title: &str) -> Uuid {
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u}', '{p}', '{title}', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');"
    ))
    .await;
    w.put(
        ka,
        &format!("projects/-workspace/{p}.jsonl"),
        &[
            user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
            assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
        ]
        .concat(),
    );
    w.put(
        ka,
        &format!("projects/-workspace/{p}/tool-results/toolu_1.txt"),
        b"tool output\n",
    );
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    control::release_forced(&w.imp, u).await.unwrap();
    u
}

/// The writer sidecar's export route over `imp`; its base URL.
async fn sidecar(imp: Importer) -> String {
    let app = magpie_import::server::export_router(Arc::new(imp));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

async fn state(w: &World, id: Uuid) -> (bool, bool, Option<bool>, bool) {
    let r = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select c.migrating, c.legacy_fence_at is not null, s.released_at is not null,
                    s.verified_at is not null
             from conversations c left join import_sources s on s.conversation_id = c.id where c.id = $1",
            &[&id],
        )
        .await
        .unwrap();
    (
        r.get(0),
        r.get(1),
        r.get(2),
        r.get::<_, Option<bool>>(3).unwrap_or(false),
    )
}

fn rename(w: &World, id: Uuid, title: &str) -> tokio::task::JoinHandle<magpie::error::Result<String>> {
    let app = w.imp.magpie.clone();
    let title = title.to_owned();
    tokio::spawn(async move {
        let person = Person {
            email: ALICE.to_owned(),
            viewer: None,
        };
        let a = app.clone();
        magpie::legacy::fence::native(&app, id, || {
            let (a, p, t) = (a.clone(), person.clone(), title.clone());
            async move {
                magpie::conversations::rename::rename(&a.pool, &p, id, RenameRequest { title: t })
                    .await
                    .map(|c| c.title.unwrap_or_default())
            }
        })
        .await
    })
}

#[tokio::test]
async fn a_reverse_waits_for_the_old_child_then_reopens_into_legacy() {
    let w = World::new("reverse").await;
    let ka = w.person(ALICE).await;
    let schema = format!("cc_{ka}");
    let id = owned(&w, &ka, "Report").await;
    let other = Uuid::new_v4().to_string();
    let (fake, kube) = common::old::cluster(&["ui-a", "ui-b"]).await;
    // the export pauses after each file: the reverse fence outlives one 3 s lease
    let url = sidecar(Importer {
        pause_in_fence: Duration::from_millis(1_400),
        ..w.imp.clone()
    })
    .await;
    let old = Old {
        kube: &kube,
        ui_token: common::old::SWITCH,
        export_url: &url,
    };

    // the person's old child runs another conversation: nothing happens
    fake.lock()
        .unwrap()
        .children
        .insert("ui-a".into(), vec![(schema.clone(), true, vec![other.clone()])]);
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Waiting, "{r:?}");
    assert!(r[0].why.as_deref().unwrap().contains("runs 1"), "{r:?}");
    assert_eq!(state(&w, id).await, (false, false, Some(true), true));
    assert_eq!(fake.lock().unwrap().restarts.len(), 0);
    // a child that does not answer: the same
    fake.lock()
        .unwrap()
        .children
        .insert("ui-a".into(), vec![(schema.clone(), false, vec![])]);
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Waiting, "{r:?}");

    // idle in the read, but the restart answers busy: exported, then unfenced, still owned and native
    fake.lock()
        .unwrap()
        .children
        .insert("ui-a".into(), vec![(schema.clone(), true, vec![])]);
    fake.lock().unwrap().restart.insert("ui-a".into(), "busy".into());
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Waiting, "{r:?}");
    assert!(r[0].why.as_deref().unwrap().contains("busy"), "{r:?}");
    assert!(r[0].export.is_some(), "{r:?}");
    assert!(!state(&w, id).await.0);
    assert!(!state(&w, id).await.1, "the fence is lifted");
    assert_eq!(
        rename(&w, id, "Native still").await.unwrap().unwrap(),
        "Native still",
        "a write goes native at once"
    );

    // the restart stops it: a native write during the fence waits in its call and then follows the reopen
    fake.lock().unwrap().restart.insert("ui-a".into(), "stopped".into());
    fake.lock().unwrap().restarts.clear();
    let imp = w.imp.clone();
    let (k2, u2) = (kube.clone(), url.clone());
    let rev = tokio::spawn(async move {
        let old = Old {
            kube: &k2,
            ui_token: common::old::SWITCH,
            export_url: &u2,
        };
        reverse::reverse(&imp, &old, &[id]).await.unwrap()
    });
    let fenced = common_eventually(&w, id).await;
    assert!(fenced, "the reverse fence is taken");
    let started = Instant::now();
    let write = rename(&w, id, "During the fence");
    let r = rev.await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Reversed, "{r:?}");
    assert_eq!(r[0].stopped, vec!["ui-a".to_owned()]);
    let restarts = fake.lock().unwrap().restarts.clone();
    assert_eq!(restarts.len(), 2, "every ui pod is asked: {restarts:?}");
    assert!(restarts.iter().all(|(_, p)| p == ALICE));
    let e = write.await.unwrap().unwrap_err();
    let waited = started.elapsed();
    match &e {
        magpie::error::Error::Refused(r) => {
            assert_eq!(r.code, ErrorCode::ConversationMigrating);
            assert_eq!(r.route, Some(RouteHint::Legacy), "{r:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        waited > Duration::from_millis(500),
        "it waited in the fence: {waited:?}"
    );
    // legacy again, open for the importer, not verified, the fence gone
    assert_eq!(state(&w, id).await, (true, false, Some(false), false));
    let sessions_title: String = w
        .imp
        .cloudcli
        .get()
        .await
        .unwrap()
        .query_one(
            &format!("select custom_name from {schema}.sessions where session_id = $1"),
            &[&id.to_string()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(sessions_title, "Native still", "the export wrote the new title");
    // the next pass finds it synced (nothing imported twice), and pre-verification passes
    let head = |w: &World| {
        let p = w.magpie().clone();
        async move {
            p.get()
                .await
                .unwrap()
                .query_one("select head_seq from conversations where id = $1", &[&id])
                .await
                .unwrap()
                .get::<_, i64>(0)
        }
    };
    let before = head(&w).await;
    let p = pass::pass(&w.imp, None).await.unwrap();
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.changed + p.created, 0, "{p:?}");
    assert_eq!(head(&w).await, before);
    let v = verify::verify(&w.imp, id).await.unwrap();
    assert!(v.ok(), "{v:?}");
    // legacy now: a reverse of it is refused
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Failed, "{r:?}");
}

async fn common_eventually(w: &World, id: Uuid) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if state(w, id).await.1 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn a_conversation_born_on_the_new_side_gets_its_import_record() {
    let w = World::new("reverse_born").await;
    let ka = w.person(ALICE).await;
    let id = owned(&w, &ka, "Born").await;
    // as if magpie had made it: no import record, and no sessions row on the old side
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute("delete from import_sources where conversation_id = $1", &[&id])
        .await
        .unwrap();
    w.cloudcli(&format!("delete from cc_{ka}.sessions where session_id = '{id}'"))
        .await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let url = sidecar(w.imp.clone()).await;
    let old = Old {
        kube: &kube,
        ui_token: common::old::SWITCH,
        export_url: &url,
    };
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Reversed, "{r:?}");
    assert!(r[0].stopped.is_empty(), "no child was running");
    let row = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select source_schema, user_key, released_at is null, files <> '{}'::jsonb from import_sources
             where conversation_id = $1",
            &[&id],
        )
        .await;
    let row = row.unwrap();
    assert_eq!(row.get::<_, String>(0), format!("cc_{ka}"));
    assert_eq!(row.get::<_, String>(1), ka);
    assert!(row.get::<_, bool>(2));
    assert!(row.get::<_, bool>(3), "the exported files are recorded");
    let p = pass::pass(&w.imp, None).await.unwrap();
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.changed + p.created, 0, "{p:?}");
}

#[tokio::test]
async fn a_person_without_an_old_schema_is_refused() {
    let w = World::new("reverse_noschema").await;
    let ka = w.person(ALICE).await;
    let id = owned(&w, &ka, "Gone").await;
    w.cloudcli(&format!("drop schema cc_{ka} cascade")).await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let url = sidecar(w.imp.clone()).await;
    let old = Old {
        kube: &kube,
        ui_token: common::old::SWITCH,
        export_url: &url,
    };
    let r = reverse::reverse(&w.imp, &old, &[id]).await.unwrap();
    assert_eq!(r[0].outcome, Outcome::Failed, "{r:?}");
    assert!(!state(&w, id).await.0);
    assert!(!state(&w, id).await.1);
}
