//! The service itself (magpie 3, 15, 16, 18): callers, readiness, migrations, the outbox and its hermit calls, the
//! job leader, the ops reads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    clippy::too_many_lines,
    clippy::unused_async_trait_impl
)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::Caller;
use hermit_api::sandbox::ConversationReleaseReason;
use magpie::jobs::outbox::{self, Deliver, Item, Outcome};
use magpie_api::conversations::{CreateRequest, ListQuery};
use magpie_api::errors::ErrorCode;
use magpie_api::ops::{PeopleQuery, ViewAsLogQuery};
use magpie_api::routes::{self, ROUTES};
use magpie_api::turns::SendRequest;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

fn empty(key: &str) -> CreateRequest {
    CreateRequest {
        idempotency_key: key.to_owned(),
        message: SendRequest {
            client_message_id: format!("{key}:m"),
            text: String::new(),
            images: Vec::new(),
            files: Vec::new(),
        },
    }
}

fn path_of(template: &str) -> String {
    let values: Vec<String> = routes::params(template)
        .iter()
        .map(|p| match *p {
            "id" | "turnId" => Uuid::new_v4().to_string(),
            "cmdSeq" => "1".to_owned(),
            "person" => ALICE.to_owned(),
            _ => "x".to_owned(),
        })
        .collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    routes::fill(template, &refs)
}

fn token(t: &common::Tokens, c: Caller) -> &str {
    match c {
        Caller::Pigeon => &t.pigeon,
        Caller::Gateway => &t.gateway,
        Caller::Hermit => &t.hermit,
        Caller::Admin => &t.admin,
        other => panic!("magpie has no {other} caller"),
    }
}

#[tokio::test]
async fn every_route_needs_its_caller() {
    let db = common::fresh_db("auth").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let http = common::http();
    for r in ROUTES {
        let url = format!("{}{}", a.url, path_of(r.path));
        let method = reqwest::Method::from_bytes(r.method.as_str().as_bytes()).unwrap();
        // no token
        let res = http.request(method.clone(), &url).send().await.unwrap();
        assert_eq!(res.status(), 401, "{} {} without a token", r.method.as_str(), r.path);
        // an unknown token
        let res = http
            .request(method.clone(), &url)
            .bearer_auth("nope")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "{} with an unknown token", r.id);
        // every caller the route does not list
        for c in magpie::auth::CALLERS {
            if r.callers.contains(&c) {
                continue;
            }
            let res = http
                .request(method.clone(), &url)
                .bearer_auth(token(&t, c))
                .header("aviary-person", ALICE)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 403, "{} as {c}", r.id);
            let body: serde_json::Value = res.json().await.unwrap();
            assert_eq!(body["code"], "wrong_caller");
        }
    }
    // a person route without its person
    let res = http
        .get(format!("{}/v1/conversations", a.url))
        .bearer_auth(&t.pigeon)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    // an unmatched path is 404, not 401
    let res = http.get(format!("{}/v1/nothing-here", a.url)).send().await.unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn headers_a_caller_may_not_set_are_ignored() {
    let db = common::fresh_db("strip").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    // hermit may not name a person: its Aviary-Person is removed, the ops read still works
    let res = common::http()
        .get(format!("{}/v1/ops/stats", a.url))
        .bearer_auth(&t.hermit)
        .header("aviary-person", ALICE)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    // pigeon may not claim a sandbox identity: the person route sees only the person
    let c = a.pigeon().person(ALICE).create_conversation(&empty("k")).await.unwrap();
    assert!(c.is_new());
}

#[tokio::test]
async fn postgres_down_after_start_keeps_readiness_and_answers_503() {
    let db = common::fresh_db("pgdown").await;
    let proxy = common::CutProxy::start(common::pg_addr(&db.url)).await;
    let t = common::Tokens::new();
    let a = common::replica(&common::with_addr(&db.url, proxy.addr), &t).await;
    assert!(a.readiness.is_ready());
    let pigeon = a.pigeon();
    pigeon.person(ALICE).create_conversation(&empty("k")).await.unwrap();
    proxy.cut();
    let e = pigeon
        .person(ALICE)
        .list_conversations(&ListQuery::default())
        .await
        .unwrap_err();
    let r = e.api().expect("an envelope");
    assert_eq!((r.status, &r.code), (503, &ErrorCode::Unavailable));
    assert!(r.retry_after.is_some(), "retry-after is set");
    assert!(a.readiness.is_ready(), "readiness stays 200 (platform 3.2)");
    proxy.restore();
    assert!(
        common::eventually(Duration::from_secs(10), async || pigeon
            .person(ALICE)
            .list_conversations(&ListQuery::default())
            .await
            .is_ok())
        .await,
        "requests work again once Postgres is back"
    );
}

#[tokio::test]
async fn readiness_waits_for_migrations_and_listen() {
    let db = common::fresh_db("ready").await;
    let proxy = common::CutProxy::start(common::pg_addr(&db.url)).await;
    proxy.cut();
    let t = common::Tokens::new();
    let url = common::with_addr(&db.url, proxy.addr);
    let readiness = aviary_core::ops::Readiness::detached();
    let drain = aviary_core::drain::Drain::new(readiness.clone());
    let cfg = magpie::app::Config {
        database_url: url,
        callers: t.callers(),
        hermit_url: "http://127.0.0.1:1".into(),
        hermit_token: "x".into(),
        pod: "p".into(),
    };
    let start = {
        let readiness = readiness.clone();
        let draining = drain.draining();
        tokio::spawn(async move { magpie::app::Service::start(cfg, &readiness, draining).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        readiness.why().contains(&"migrations".to_owned()),
        "{:?}",
        readiness.why()
    );
    assert!(readiness.why().contains(&"listen".to_owned()));
    proxy.restore();
    let service = tokio::time::timeout(Duration::from_secs(20), start)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        readiness.why(),
        vec!["start".to_owned()],
        "only the caller's own start is left"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn replicas_starting_together_apply_the_migrations_once() {
    let db = common::fresh_db("migrate").await;
    let t = common::Tokens::new();
    let (a, b, c) = tokio::join!(
        common::replica(&db.url, &t),
        common::replica(&db.url, &t),
        common::replica(&db.url, &t)
    );
    let n: i64 = common::sql(&db.url)
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from _migrations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, i64::try_from(magpie::db::migrations().unwrap().len()).unwrap());
    for r in [&a, &b, &c] {
        assert!(r.readiness.is_ready());
    }
    // the importer's control row and tables are there (cutover 7.3)
    let ctl: i64 = common::sql(&db.url)
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from import_control", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(ctl, 1);
}

#[tokio::test]
async fn one_replica_leads_the_jobs_and_hands_over_on_shutdown() {
    let db = common::fresh_db("leader").await;
    let t = common::Tokens::new();
    let mut a = common::replica(&db.url, &t).await;
    let mut b = common::replica(&db.url, &t).await;
    let leads = |r: &common::Replica| r.service.as_ref().is_some_and(magpie::app::Service::is_leader);
    assert!(
        common::eventually(Duration::from_secs(5), async || leads(&a) ^ leads(&b)).await,
        "exactly one leader"
    );
    let (leader, follower) = if leads(&a) { (&mut a, &b) } else { (&mut b, &a) };
    leader.service.take().unwrap().shutdown().await;
    assert!(
        common::eventually(Duration::from_secs(3), async || leads(follower)).await,
        "the other replica took the jobs over"
    );
}

/// Records every call, failing the first `fail` ones.
#[derive(Clone, Default)]
struct Recorder {
    calls: Arc<Mutex<Vec<Item>>>,
    fail: Arc<Mutex<u32>>,
}

impl Deliver for Recorder {
    async fn deliver(&self, item: &Item) -> Outcome {
        self.calls.lock().unwrap().push(item.clone());
        let mut f = self.fail.lock().unwrap();
        if *f > 0 {
            *f -= 1;
            return Outcome::Retry("scripted failure".into());
        }
        Outcome::Done
    }
}

#[tokio::test]
async fn the_outbox_retries_with_backoff_and_deletes_what_is_done() {
    let db = common::fresh_db("outbox").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let id = pigeon
        .person(ALICE)
        .create_conversation(&empty("k"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    pigeon.person(ALICE).delete_conversation(id).await.unwrap();
    let rec = Recorder::default();
    *rec.fail.lock().unwrap() = 1;
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 0);
    let (attempts, err): (i32, Option<String>) = {
        let r = sql
            .get()
            .await
            .unwrap()
            .query_one("select attempts, last_error from outbox", &[])
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!((attempts, err.as_deref()), (1, Some("scripted failure")));
    // not due yet: nothing is tried
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 0);
    assert_eq!(rec.calls.lock().unwrap().len(), 1);
    sql.get()
        .await
        .unwrap()
        .execute("update outbox set next_at = now()", &[])
        .await
        .unwrap();
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 1);
    assert_eq!(
        rec.calls.lock().unwrap().clone(),
        vec![
            Item::ReleaseSandbox {
                conversation_id: id,
                reason: ConversationReleaseReason::Deleted
            };
            2
        ]
    );
    let left: i64 = sql
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from outbox", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    // the backoff: 5 s doubling to 1 h
    assert_eq!(outbox::backoff(1), Duration::from_secs(5));
    assert_eq!(outbox::backoff(2), Duration::from_secs(10));
    assert_eq!(outbox::backoff(40), Duration::from_secs(3600));
}

#[tokio::test]
async fn the_outbox_calls_hermit_and_takes_404_as_done() {
    let db = common::fresh_db("outbox_hermit").await;
    let hermit = aviary_fakes::hermit::FakeHermit::new();
    let running = hermit.serve().await.unwrap();
    let token = hermit.token(Caller::Magpie).expect("fake-hermit has a magpie token");
    let client = hermit_api::client::Client::new(common::http(), running.url()).bearer(token);
    let deliver = outbox::Hermit(client);
    // a release for a conversation hermit has no sandbox for, a workspace it never had (404)
    assert_eq!(
        deliver
            .deliver(&Item::ReleaseSandbox {
                conversation_id: Uuid::new_v4(),
                reason: ConversationReleaseReason::Deleted
            })
            .await,
        Outcome::Done
    );
    assert_eq!(
        deliver
            .deliver(&Item::DeleteWorkspace {
                workspace_id: Uuid::new_v4()
            })
            .await,
        Outcome::Done
    );
    // hermit unreachable: retried
    let dead = outbox::Hermit(hermit_api::client::Client::new(common::http(), "http://127.0.0.1:1").bearer("x"));
    assert!(matches!(
        dead.deliver(&Item::DeleteWorkspace {
            workspace_id: Uuid::new_v4()
        })
        .await,
        Outcome::Retry(_)
    ));
    drop(db);
}

#[tokio::test]
async fn the_leader_runs_the_outbox_by_itself() {
    let db = common::fresh_db("outbox_job").await;
    let hermit = aviary_fakes::hermit::FakeHermit::new();
    let running = hermit.serve().await.unwrap();
    let t = common::Tokens::new();
    let a = common::replica_with(&db.url, &t, &running.url(), common::fast()).await;
    let pigeon = a.pigeon();
    let id = pigeon
        .person(ALICE)
        .create_conversation(&empty("k"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    pigeon.person(ALICE).delete_conversation(id).await.unwrap();
    let sql = common::sql(&db.url);
    // the fake checks magpie's token: the test replica presents a token fake-hermit does not know, so the calls
    // fail and stay queued with their error, retried with backoff
    assert!(
        common::eventually(Duration::from_secs(5), async || {
            let r = sql
                .get()
                .await
                .unwrap()
                .query_one("select count(*), max(attempts) from outbox", &[])
                .await
                .unwrap();
            r.get::<_, i64>(0) == 1 && r.get::<_, Option<i32>>(1).unwrap_or(0) >= 1
        })
        .await,
        "the leader tried the call"
    );
}

#[tokio::test]
async fn the_ops_reads() {
    let db = common::fresh_db("ops").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let x = pigeon
        .person(ALICE)
        .create_conversation(&empty("a"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    pigeon.person(ALICE).create_conversation(&empty("b")).await.unwrap();
    pigeon.person(ALICE).delete_conversation(x).await.unwrap();
    // seen once, nothing else
    pigeon
        .person("new.staff@example.com")
        .list_conversations(&ListQuery::default())
        .await
        .unwrap();
    // viewed as: a create row in the audit
    pigeon
        .viewing_as("bob@example.com", "admin@example.com", "sess-1")
        .create_conversation(&empty("v"))
        .await
        .unwrap();

    for svc in [a.hermit(), a.admin()] {
        let people = svc
            .service(None)
            .ops_people(&PeopleQuery {
                tz: Some("Asia/Hong_Kong".into()),
            })
            .await
            .unwrap();
        assert_eq!(people.tz, "Asia/Hong_Kong");
        let alice = people.people.iter().find(|p| p.owner == ALICE).unwrap();
        assert_eq!(
            (alice.total, alice.today, alice.last7d, alice.deleted, alice.live),
            (2, 2, 2, 1, 0)
        );
        assert!(alice.last_active_at.is_some());
        let new = people
            .people
            .iter()
            .find(|p| p.owner == "new.staff@example.com")
            .unwrap();
        assert_eq!((new.total, new.last_active_at.is_none()), (0, true));
    }
    let bad = a
        .hermit()
        .service(None)
        .ops_people(&PeopleQuery {
            tz: Some("Mars/Base".into()),
        })
        .await
        .unwrap();
    assert_eq!(bad.tz, "UTC");

    let stats = a.hermit().service(None).ops_stats().await.unwrap();
    assert_eq!((stats.conversations, stats.deleted), (2, 1));
    assert_eq!(stats.outbox_pending, 1);
    assert!(stats.oldest_outbox_at.is_some());
    assert!(stats.events >= 3);
    assert_eq!(stats.hermit_feed_lag_seconds, None);

    let log = a
        .admin()
        .service(None)
        .ops_view_as(&ViewAsLogQuery::default())
        .await
        .unwrap();
    assert_eq!(log.view_as.len(), 1);
    let e = &log.view_as[0];
    assert_eq!(
        (e.admin.as_str(), e.user.as_str(), e.session.as_deref()),
        ("admin@example.com", "bob@example.com", Some("sess-1"))
    );
    // the viewed create's event names the viewer
    let viewer: Option<String> = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select viewer from events where kind = 'conversation.created' and conversation_id = $1",
            &[&e.conversation_id.unwrap()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(viewer.as_deref(), Some("admin@example.com"));
}
