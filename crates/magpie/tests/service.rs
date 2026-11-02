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
    // every method a route answers: its own, and HEAD for a GET (axum's get() serves HEAD with the GET handler)
    let mut checks: Vec<(&magpie_api::routes::Route, reqwest::Method)> = Vec::new();
    for r in ROUTES {
        checks.push((r, reqwest::Method::from_bytes(r.method.as_str().as_bytes()).unwrap()));
        if r.method == magpie_api::routes::Method::Get {
            checks.push((r, reqwest::Method::HEAD));
        }
    }
    for (r, method) in checks {
        let url = format!("{}{}", a.url, path_of(r.path));
        // no token
        let res = http.request(method.clone(), &url).send().await.unwrap();
        assert_eq!(res.status(), 401, "{method} {} without a token", r.path);
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
            assert_eq!(res.status(), 403, "{method} {} as {c}", r.id);
            if method != reqwest::Method::HEAD {
                let body: serde_json::Value = res.json().await.unwrap();
                assert_eq!(body["code"], "wrong_caller");
            }
        }
    }
    // a HEAD with a forged person and no token reaches no handler: no persons row appears
    let res = http
        .head(format!("{}/v1/conversations", a.url))
        .header("aviary-person", "mallory@example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let n: i64 = common::sql(&db.url)
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from persons", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0, "a HEAD without a token created a person");
    // methods no route has: 401 without a token, 405 with one, on every path
    for r in ROUTES {
        let url = format!("{}{}", a.url, path_of(r.path));
        for m in ["OPTIONS", "TRACE", "PROPFIND"] {
            let m = reqwest::Method::from_bytes(m.as_bytes()).unwrap();
            if ROUTES
                .iter()
                .any(|o| o.path == r.path && o.method.as_str() == m.as_str())
            {
                continue;
            }
            let res = http.request(m.clone(), &url).send().await.unwrap();
            assert_eq!(res.status(), 401, "{m} {} without a token", r.path);
            let res = http
                .request(m.clone(), &url)
                .bearer_auth(&t.pigeon)
                .header("aviary-person", ALICE)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 405, "{m} {} with a token", r.path);
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

/// Every `Aviary-*` header there is, with a value that passes as each.
const EVERY_AVIARY: [(&str, &str); 12] = [
    ("aviary-request", "req-1"),
    ("aviary-protocol", "magpie-person/1"),
    ("aviary-person", "mallory@example.com"),
    ("aviary-viewer", "mallory@example.com"),
    ("aviary-view-session", "vs-1"),
    ("aviary-admin", "mallory@example.com"),
    ("aviary-sandbox-id", "sbx-1"),
    ("aviary-sandbox-conversation", "00000000-0000-0000-0000-000000000001"),
    ("aviary-sandbox-person", "mallory@example.com"),
    ("aviary-lease", "lease-1"),
    ("aviary-upload-name", "a.txt"),
    ("aviary-upload-kind", "file"),
];

/// The route layer in front of a handler that answers with the `Aviary-*` headers it was given, on every route of
/// the table: what each caller's request carries past the layer, for every route it may call.
#[tokio::test]
async fn headers_a_caller_may_not_set_are_ignored() {
    use axum::routing::{MethodFilter, MethodRouter, on};
    async fn echo(req: axum::extract::Request) -> axum::Json<Vec<String>> {
        let mut seen: Vec<String> = req
            .headers()
            .keys()
            .map(|n| n.as_str().to_owned())
            .filter(|n| n.starts_with("aviary-"))
            .collect();
        seen.sort();
        axum::Json(seen)
    }
    let t = common::Tokens::new();
    let mut by_path: Vec<(&str, MethodRouter)> = Vec::new();
    for r in ROUTES {
        let filter = match r.method {
            routes::Method::Get => MethodFilter::GET,
            routes::Method::Post => MethodFilter::POST,
            routes::Method::Put => MethodFilter::PUT,
            routes::Method::Patch => MethodFilter::PATCH,
            routes::Method::Delete => MethodFilter::DELETE,
        };
        if let Some((_, m)) = by_path.iter_mut().find(|(p, _)| *p == r.path) {
            *m = std::mem::take(m).on(filter, echo);
        } else {
            by_path.push((r.path, on(filter, echo)));
        }
    }
    let mut router = axum::Router::new();
    for (p, m) in by_path {
        router = router.route(p, m);
    }
    let router = router.route_layer(axum::middleware::from_fn_with_state(t.callers(), magpie::auth::check));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let http = common::http();
    let mut checked = 0;
    for r in ROUTES {
        for &c in r.callers {
            let mut req = http
                .request(
                    reqwest::Method::from_bytes(r.method.as_str().as_bytes()).unwrap(),
                    format!("{base}{}", path_of(r.path)),
                )
                .bearer_auth(token(&t, c));
            for (n, v) in EVERY_AVIARY {
                req = req.header(n, v);
            }
            let res = req.send().await.unwrap();
            assert_eq!(res.status(), 200, "{} as {c}", r.id);
            let seen: Vec<String> = res.json().await.unwrap();
            let mut want: Vec<String> = aviary_core::http::headers::ALWAYS_ALLOWED
                .iter()
                .chain(magpie::auth::allowed_headers(c))
                .map(|n| n.as_str().to_owned())
                .collect();
            want.sort();
            assert_eq!(seen, want, "{} as {c}: what reached the handler", r.id);
            checked += 1;
        }
    }
    assert!(checked >= ROUTES.len());
    // the lists themselves: pigeon names the person, viewer and upload; the gateway the sandbox and lease; admin
    // itself; hermit nothing
    let names = |c| -> Vec<&str> {
        magpie::auth::allowed_headers(c)
            .iter()
            .map(axum::http::HeaderName::as_str)
            .collect()
    };
    assert!(names(Caller::Hermit).is_empty());
    assert!(!names(Caller::Gateway).contains(&"aviary-person") && !names(Caller::Gateway).contains(&"aviary-viewer"));
    assert!(
        !names(Caller::Pigeon)
            .iter()
            .any(|n| n.starts_with("aviary-sandbox") || *n == "aviary-admin")
    );
    server.abort();

    // and through the real service: a forged Aviary-Viewer from a caller that may not set it leaves no trace
    let db = common::fresh_db("strip").await;
    let a = common::replica(&db.url, &t).await;
    for c in [Caller::Gateway, Caller::Hermit, Caller::Admin] {
        let res = http
            .post(format!("{}/v1/conversations", a.url))
            .bearer_auth(token(&t, c))
            .header("aviary-person", ALICE)
            .header("aviary-viewer", "mallory@example.com")
            .header("aviary-view-session", "vs-1")
            .json(&empty("forged"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 403, "create as {c}");
    }
    let n: i64 = common::sql(&db.url)
        .get()
        .await
        .unwrap()
        .query_one(
            "select (select count(*) from view_as_log) + (select count(*) from persons)",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0, "a forged viewer reached a handler");
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
        bucket: common::bucket_config("unused"),
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

/// A workspace's deletion waits for the same conversation's release; an item that does not parse is parked for an
/// hour and holds nothing up.
#[tokio::test]
async fn the_outbox_deletes_a_workspace_only_after_its_release_and_parks_what_does_not_parse() {
    let db = common::fresh_db("outbox_order").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
    {
        let c = sql.get().await.unwrap();
        c.execute(
            "insert into outbox (kind, payload) values ('release_sandbox', '{\"nonsense\": true}')",
            &[],
        )
        .await
        .unwrap();
        for (kind, payload) in [
            (
                "release_sandbox",
                serde_json::json!({"conversationId": x, "reason": "deleted"}),
            ),
            ("delete_workspace", serde_json::json!({"workspaceId": x})),
            ("delete_workspace", serde_json::json!({"workspaceId": y})),
        ] {
            c.execute("insert into outbox (kind, payload) values ($1, $2)", &[&kind, &payload])
                .await
                .unwrap();
        }
    }
    let rec = Recorder::default();
    // the release fails: x's workspace is not touched, y's (no release queued) is deleted; the bad item is parked
    *rec.fail.lock().unwrap() = 1;
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 1);
    assert_eq!(
        rec.calls.lock().unwrap().clone(),
        vec![
            Item::ReleaseSandbox {
                conversation_id: x,
                reason: ConversationReleaseReason::Deleted
            },
            Item::DeleteWorkspace { workspace_id: y },
        ]
    );
    let r = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select attempts, last_error, next_at > now() + interval '59 minutes' from outbox
             where payload ? 'nonsense'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            r.get::<_, i32>(0),
            r.get::<_, Option<String>>(1).as_deref(),
            r.get::<_, bool>(2)
        ),
        (1, Some("does not parse"), true)
    );
    // the release is done: the workspace deletion follows
    sql.get()
        .await
        .unwrap()
        .execute(
            "update outbox set next_at = now() where payload ? 'conversationId'",
            &[],
        )
        .await
        .unwrap();
    rec.calls.lock().unwrap().clear();
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 1);
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 1);
    assert_eq!(
        rec.calls.lock().unwrap().clone(),
        vec![
            Item::ReleaseSandbox {
                conversation_id: x,
                reason: ConversationReleaseReason::Deleted
            },
            Item::DeleteWorkspace { workspace_id: x },
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
    assert_eq!(left, 1, "only the parked item is left");
}

/// Delete, then restore: the delete's queued sandbox release is never delivered (it names only the conversation, so it
/// would release the sandbox of the restored conversation's next turn). Restore drops it; a release that escaped the
/// drop (restored behind the outbox's back) is dropped by the pass, uncalled.
#[tokio::test]
async fn a_restore_drops_the_deletes_sandbox_release() {
    let db = common::fresh_db("outbox_restore").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let mk = |title: &'static str| {
        let pigeon = pigeon.clone();
        async move {
            pigeon
                .person(ALICE)
                .create_conversation(&empty(title))
                .await
                .unwrap()
                .get()
                .conversation
                .id
        }
    };
    let pending = || async {
        sql.get()
            .await
            .unwrap()
            .query_one("select count(*) from outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    // hermit was failing: the release waits in the backoff while the person restores
    let x = mk("x").await;
    pigeon.person(ALICE).delete_conversation(x).await.unwrap();
    let failing = Recorder::default();
    *failing.fail.lock().unwrap() = 1;
    assert_eq!(outbox::pass(a.pool(), &failing).await.unwrap(), 0);
    assert_eq!(pending().await, 1);
    pigeon.person(ALICE).restore_conversation(x).await.unwrap();
    assert_eq!(pending().await, 0, "restore dropped the queued release");
    sql.get()
        .await
        .unwrap()
        .execute("update outbox set next_at = now()", &[])
        .await
        .unwrap();
    let rec = Recorder::default();
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 0);
    assert!(rec.calls.lock().unwrap().is_empty(), "no release after the restore");

    // a release still queued for a conversation that is no longer deleted: the pass drops it without a call
    let y = mk("y").await;
    pigeon.person(ALICE).delete_conversation(y).await.unwrap();
    sql.get()
        .await
        .unwrap()
        .execute("update conversations set deleted_at = null where id = $1", &[&y])
        .await
        .unwrap();
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 0);
    assert!(
        rec.calls.lock().unwrap().is_empty(),
        "the stale release was not delivered"
    );
    assert_eq!(pending().await, 0, "the stale release was dropped");

    // delete again after the restore: the new release is delivered as usual
    pigeon.person(ALICE).delete_conversation(x).await.unwrap();
    assert_eq!(outbox::pass(a.pool(), &rec).await.unwrap(), 1);
    assert_eq!(
        rec.calls.lock().unwrap().clone(),
        vec![Item::ReleaseSandbox {
            conversation_id: x,
            reason: ConversationReleaseReason::Deleted
        }]
    );
}

/// A pass stops before the next item once the leader check fails: a replica that lost the lock mid-pass does not
/// keep delivering beside the new leader.
#[tokio::test]
async fn an_outbox_pass_stops_when_leadership_is_lost() {
    let db = common::fresh_db("outbox_lead").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    for title in ["a", "b", "c"] {
        let id = pigeon
            .person(ALICE)
            .create_conversation(&empty(title))
            .await
            .unwrap()
            .get()
            .conversation
            .id;
        pigeon.person(ALICE).delete_conversation(id).await.unwrap();
    }
    let rec = Recorder::default();
    let checks = Arc::new(Mutex::new(0u32));
    let c = checks.clone();
    let done = outbox::pass_while(a.pool(), &rec, move || {
        let c = c.clone();
        async move {
            let mut n = c.lock().unwrap();
            *n += 1;
            *n <= 1
        }
    })
    .await
    .unwrap();
    assert_eq!(done, 1);
    assert_eq!(rec.calls.lock().unwrap().len(), 1);
    assert_eq!(
        *checks.lock().unwrap(),
        2,
        "checked before each item, stopped at the second"
    );
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

    // events and rows are Postgres's estimates, which analyze brings up to date
    sql.get()
        .await
        .unwrap()
        .batch_execute("analyze events, rows")
        .await
        .unwrap();
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

/// The view-as audit pages by entry id with no entry lost, also when entries share a time (rows written in one
/// transaction share `now()`) or differ by less than a millisecond (the wire's `at` precision).
#[tokio::test]
async fn the_view_as_log_pages_without_losing_entries() {
    let db = common::fresh_db("viewaspage").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let c = sql.get().await.unwrap();
    // 5 entries at one instant, 4 a few microseconds apart within one millisecond, 3 older ones
    c.batch_execute(
        "insert into view_as_log (at, admin, person, session, event)
           select '2026-11-02T03:00:00.123456Z', 'admin@example.com', 'bob@example.com', 's', 'start'
           from generate_series(1, 5);
         insert into view_as_log (at, admin, person, session, event)
           select '2026-11-02T03:00:00.123000Z'::timestamptz + make_interval(secs => g * 0.0001),
                  'admin@example.com', 'bob@example.com', 's', 'start'
           from generate_series(1, 4) g;
         insert into view_as_log (at, admin, person, session, event)
           select '2026-11-01T03:00:00Z'::timestamptz + make_interval(secs => g),
                  'admin@example.com', 'bob@example.com', 's', 'start'
           from generate_series(1, 3) g;",
    )
    .await
    .unwrap();
    let all: Vec<i64> = c
        .query("select id from view_as_log order by at desc, id desc", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(all.len(), 12);
    let admin = a.admin();
    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let page = admin
            .service(None)
            .ops_view_as(&ViewAsLogQuery { limit: Some(2), before })
            .await
            .unwrap()
            .view_as;
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        seen.extend(page.iter().map(|e| i64::try_from(e.id).unwrap()));
    }
    assert_eq!(seen, all);
    // an id that names no entry: an empty page
    let none = admin
        .service(None)
        .ops_view_as(&ViewAsLogQuery {
            limit: None,
            before: Some(1_000_000),
        })
        .await
        .unwrap();
    assert!(none.view_as.is_empty());
}
