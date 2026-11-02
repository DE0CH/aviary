//! hermit 24.2's integration tests, ported from jarvis3 `web.test.ts` (the parts of the core; the fabric's and the
//! ops page's are WP-19's and WP-20's): the state machine, release refusal, the admission race, pool, lifetime,
//! health, resumable tasks after a replica kill, tokens and liveness without a database, the descriptor's `ETag`,
//! secrets, and the activity clock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use ::hermit::db::{self, SandboxRow};
use ::hermit::janitor;
use aviary_core::auth::Caller;
use common::{Harness, Replica, STAFF};
use hermit_api::routes;
use hermit_api::sandbox::{ReleaseReason, SandboxPhase, SandboxState};
use hermit_api::sandbox_surface::ActivityState;
use serde_json::{Value, json};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(20);

fn sandbox_path(cid: Uuid) -> String {
    routes::fill(routes::SANDBOX, &[&cid.to_string()])
}

async fn put(h: &Harness, r: &Replica, cid: Uuid, person: &str) -> common::Answer {
    h.client(Caller::Magpie)
        .call(&r.url, "PUT", &sandbox_path(cid), Some(json!({"person": person})), &[])
        .await
}

async fn row(h: &Harness, cid: Uuid) -> SandboxRow {
    db::latest_of(&h.pool(), cid).await.unwrap().unwrap()
}

async fn wait_state(h: &Harness, cid: Uuid, want: SandboxState) -> SandboxRow {
    let ok = common::eventually(WAIT, || async {
        db::latest_of(&h.pool(), cid)
            .await
            .unwrap()
            .is_some_and(|r| r.status == want)
    })
    .await;
    let r = row(h, cid).await;
    assert!(ok, "the sandbox of {cid} is {:?}, not {want:?}: {r:?}", r.status);
    r
}

async fn ready(h: &Harness, r: &Replica, person: &str) -> (Uuid, SandboxRow) {
    let cid = Uuid::now_v7();
    let a = put(h, r, cid, person).await;
    assert!(matches!(a.status, 200 | 202), "{}: {}", a.status, a.body);
    (cid, wait_state(h, cid, SandboxState::Ready).await)
}

async fn report(h: &Harness, r: &Replica, s: &SandboxRow, state: &str, idle_since: Option<&str>) -> common::Answer {
    let body = json!({
        "state": state, "turnId": null, "tasks": 0, "idleSince": idle_since, "lastOutputAt": null,
        "bootId": "boot-1", "protocol": 1, "version": "test", "restarts": 0, "rssBytes": 1,
        "memory": {"limit": 1, "used": 1}, "events": []
    });
    h.client(Caller::Gateway)
        .call(
            &r.url,
            "POST",
            routes::SANDBOX_ACTIVITY,
            Some(body),
            &[
                ("aviary-sandbox-id", s.id.to_string()),
                ("aviary-sandbox-conversation", s.conversation_id.to_string()),
                ("aviary-sandbox-person", s.person.clone()),
            ],
        )
        .await
}

fn sup(h: &Harness, s: &SandboxRow) -> std::sync::Arc<aviary_fakes::FakeSupervisor> {
    h.fabric
        .supervisor(s.ip.unwrap())
        .expect("a fake supervisor for the sandbox")
}

/// Makes a ready sandbox look idle for an hour with no reservation.
async fn age(h: &Harness, id: Uuid) {
    h.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update sandboxes set last_active_at = now() - interval '1 hour', reserved_until = null,
               last_busy_at = null, last_heartbeat_at = now() where id = $1",
            &[&id],
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_sandbox_per_conversation_put_reserves_and_the_janitor_idles_it_out() {
    let h = Harness::new("lifecycle_basic").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    // the same one while it lives; PUT resets the idle clock and reserves it
    age(&h, s.id).await;
    let a = put(&h, &r, cid, STAFF).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["id"], json!(s.id));
    let now = row(&h, cid).await;
    assert!(now.reserved_until.is_some() && now.last_active_at > s.last_active_at);
    // reserved: the janitor leaves it alone even when the clock says idle
    h.sql(&format!(
        "update sandboxes set last_active_at = now() - interval '1 hour' where id = '{}'",
        s.id
    ))
    .await;
    let p = janitor::pass(r.core()).await.unwrap();
    assert!(p.idle.is_empty(), "{p:?}");
    // reservation passed: idled out; the supervisor was asked softly
    age(&h, s.id).await;
    let p = janitor::pass(r.core()).await.unwrap();
    assert_eq!(p.idle, vec![s.id]);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("idle"));
    assert_eq!(gone.release_code.as_deref(), Some("sandbox_released"));
    let calls = sup_calls(&h, &s);
    assert_eq!(calls[0]["force"], false);
    assert_eq!(calls[0]["reason"], "idle");
    // the next PUT makes a new one
    let a = put(&h, &r, cid, STAFF).await;
    assert_eq!(a.status, 202);
    assert_ne!(a.body["id"], json!(s.id));
    // the console tail was kept
    let logs: i64 = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from sandbox_logs", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(logs, 1);
}

/// The release bodies a destroyed sandbox's supervisor received (kept by the fabric after destroy).
fn sup_calls(h: &Harness, s: &SandboxRow) -> Vec<Value> {
    h.fabric.release_bodies(s.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_is_never_idled_activity_moves_the_clock_and_a_refused_release_restarts_it() {
    let h = Harness::new("lifecycle_busy").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    // a running report: busy, a feed event, the clock now
    age(&h, s.id).await;
    let a = report(&h, &r, &s, "running", None).await;
    assert_eq!(a.status, 200, "{}", a.body);
    let now = row(&h, cid).await;
    assert!(now.busy && now.version > s.version);
    assert!(time::OffsetDateTime::now_utc() - now.last_active_at < Duration::from_secs(5));
    h.sql(&format!(
        "update sandboxes set last_active_at = now() - interval '1 hour', reserved_until = null where id = '{}'",
        s.id
    ))
    .await;
    assert!(
        janitor::pass(r.core()).await.unwrap().idle.is_empty(),
        "busy is never idled"
    );
    // busy -> idle: the clock starts when the work ended
    let ended = hermit_api::Timestamp::from_unix_millis(hermit_api::Timestamp::now().unix_millis() - 60_000).unwrap();
    let a = report(&h, &r, &s, "idle", Some(&ended.to_string())).await;
    assert_eq!(a.status, 200);
    let now = row(&h, cid).await;
    assert!(!now.busy);
    assert_eq!(hermit_api::Timestamp::from(now.last_active_at), ended);
    // an idle release the supervisor refuses (it is busy in its own view): stays ready, the clock restarts
    sup(&h, &s).set_run(ActivityState::Held, None, 1);
    age(&h, s.id).await;
    let p = janitor::pass(r.core()).await.unwrap();
    assert_eq!(p.idle, vec![s.id]);
    let refused = common::eventually(WAIT, || async {
        let r = row(&h, cid).await;
        r.release_pending.is_none()
            && r.task.is_none()
            && time::OffsetDateTime::now_utc() - r.last_active_at < Duration::from_secs(5)
    })
    .await;
    assert!(refused);
    assert_eq!(row(&h, cid).await.status, SandboxState::Ready);
    let events: i64 = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from host_events where kind = 'release-refused'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(events, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_releases_forced_and_a_requested_release_waits_for_idle() {
    let h = Harness::new("lifecycle_delete").await;
    let r = h.replica("hermit-a").await;
    let m = h.client(Caller::Magpie);
    // requested (soft): kept while the supervisor is busy, carried in the activity reply, done at the idle report
    let (cid, s) = ready(&h, &r, STAFF).await;
    sup(&h, &s).set_run(ActivityState::Running, None, 0);
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = m
        .call(&r.url, "POST", &rel, Some(json!({"reason": "requested"})), &[])
        .await;
    assert_eq!((a.status, a.body["released"].clone()), (200, json!(1)));
    assert!(common::eventually(WAIT, || async { row(&h, cid).await.task.is_none() }).await);
    let now = row(&h, cid).await;
    assert_eq!(now.status, SandboxState::Ready);
    assert_eq!(
        now.release_pending.as_ref().map(|p| p.reason),
        Some(ReleaseReason::Requested)
    );
    let a = report(&h, &r, &s, "running", None).await;
    assert_eq!(a.body["release"]["reason"], "requested");
    assert_eq!(a.body["release"]["force"], false);
    sup(&h, &s).set_run(ActivityState::Idle, None, 0);
    report(&h, &r, &s, "idle", None).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("requested"));

    // deleted (forced): the supervisor gets force: true even while busy, and the sandbox is destroyed
    let (cid, s) = ready(&h, &r, STAFF).await;
    sup(&h, &s).set_run(ActivityState::Running, None, 0);
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = m
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert_eq!(a.status, 200);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("deleted"));
    assert!(sup_calls(&h, &s).iter().any(|c| c["force"] == true));
    assert!(h.fabric.destroyed.lock().unwrap().contains(&s.name));
    // idempotent: nothing live any more
    let a = m
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert_eq!(a.body["released"], 0);
    // the feed says releasing then gone
    let states: Vec<String> = h
        .pool()
        .get()
        .await
        .unwrap()
        .query(
            "select state from sandbox_events where sandbox_id = $1 order by seq",
            &[&s.id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(&states[states.len() - 2..], ["releasing", "gone"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_during_a_pending_soft_release_cancels_it() {
    let h = Harness::new("lifecycle_cancel").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    // a pending soft release no task has picked up yet
    h.sql(&format!(
        "update sandboxes set release_pending = '{{\"reason\":\"idle\",\"note\":null,\"force\":false,\"deadlineAt\":\"2099-01-01T00:00:00.000Z\",\"askedAt\":\"2026-01-01T00:00:00.000Z\",\"refusals\":0}}' where id = '{}'",
        s.id
    ))
    .await;
    let a = put(&h, &r, cid, STAFF).await;
    assert_eq!(a.status, 200);
    assert!(row(&h, cid).await.release_pending.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pool_frees_the_least_recently_active_idle_sandbox_and_otherwise_answers_busy() {
    let cfg = common::config(|v| {
        v["sandbox"]["maxActive"] = 2.into();
        v["sandbox"]["capacityWaitSeconds"] = 2.into();
    });
    let h = Harness::with("lifecycle_pool", cfg, common::timings()).await;
    let r = h.replica("hermit-a").await;
    let (c1, s1) = ready(&h, &r, STAFF).await;
    let (c2, s2) = ready(&h, &r, STAFF).await;
    // s1 idle longer; s2 idle but busy-refusing; a reserved sandbox is never picked
    age(&h, s1.id).await;
    age(&h, s2.id).await;
    h.sql(&format!(
        "update sandboxes set last_active_at = now() - interval '2 hours' where id = '{}'",
        s1.id
    ))
    .await;
    sup(&h, &s1).set_run(ActivityState::Held, None, 1);
    let c3 = Uuid::now_v7();
    assert_eq!(put(&h, &r, c3, STAFF).await.status, 202);
    // s1 refused (skipped), so s2 goes; s3 starts
    let gone = wait_state(&h, c2, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("pool"));
    wait_state(&h, c3, SandboxState::Ready).await;
    assert_eq!(row(&h, c1).await.status, SandboxState::Ready);
    // nothing idle (s1 busy-refusing and just touched, s3 reserved): sandbox_busy after capacityWaitSeconds
    let c4 = Uuid::now_v7();
    put(&h, &r, c4, STAFF).await;
    let gone = wait_state(&h, c4, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("pool_full"));
    assert_eq!(gone.release_code.as_deref(), Some("sandbox_busy"));
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "GET", &sandbox_path(c4), None, &[])
        .await;
    assert_eq!(a.body["release"]["code"], "sandbox_busy");
    let _ = s2;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifetime_defers_while_busy_and_the_hard_limit_forces() {
    let h = Harness::new("lifecycle_lifetime").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    report(&h, &r, &s, "running", None).await;
    h.sql(&format!(
        "update sandboxes set expires_at = now() - interval '1 minute' where id = '{}'",
        s.id
    ))
    .await;
    let p = janitor::pass(r.core()).await.unwrap();
    assert!(p.lifetime_soft.is_empty() && p.lifetime_forced.is_empty(), "{p:?}");
    sup(&h, &s).set_run(ActivityState::Running, None, 0);
    h.sql(&format!(
        "update sandboxes set hard_expires_at = now() - interval '1 minute' where id = '{}'",
        s.id
    ))
    .await;
    let p = janitor::pass(r.core()).await.unwrap();
    assert_eq!(p.lifetime_forced, vec![s.id]);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("lifetime"));
    assert!(
        sup_calls(&h, &s)
            .iter()
            .any(|c| c["force"] == true && c["reason"] == "lifetime")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silence_releases_only_after_two_unanswered_checks_and_never_while_the_gateway_settles() {
    let h = Harness::new("lifecycle_health").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    let quiet =
        |id: Uuid| format!("update sandboxes set last_heartbeat_at = now() - interval '5 minutes' where id = '{id}'");
    // alive but not ready (a supervisor restarting): never unhealthy
    sup(&h, &s).set_ready(false);
    h.sql(&quiet(s.id)).await;
    assert_eq!(janitor::pass(r.core()).await.unwrap().unhealthy, Vec::<Uuid>::new());
    // silent, but the gateway is not settled: nothing judged
    h.fabric.silence(s.ip.unwrap());
    h.fabric.settled.store(false, Ordering::SeqCst);
    assert_eq!(janitor::pass(r.core()).await.unwrap().unhealthy, Vec::<Uuid>::new());
    assert_eq!(row(&h, cid).await.status, SandboxState::Ready);
    // silent with the gateway settled: lost `unhealthy`
    h.fabric.settled.store(true, Ordering::SeqCst);
    assert_eq!(janitor::pass(r.core()).await.unwrap().unhealthy, vec![s.id]);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("unhealthy"));
    assert_eq!(gone.release_code.as_deref(), Some("sandbox_lost"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_resumes_on_another_replica_after_a_kill() {
    let h = Harness::new("lifecycle_release_takeover").await;
    let a = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &a, STAFF).await;
    // A's forced release is in flight at the supervisor when A dies
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(a.core(), s.id, ReleaseReason::Deleted, true, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    assert_eq!(row(&h, cid).await.status, SandboxState::Releasing);
    a.kill();
    hold.release();
    let _b = h.replica("hermit-b").await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("deleted"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn liveness_and_token_keys_answer_from_memory_with_the_database_closed() {
    let h = Harness::new("lifecycle_liveness").await;
    let r = h.replica("hermit-a").await;
    let (_, s) = ready(&h, &r, STAFF).await;
    r.core().pool.close();
    let g = h.client(Caller::Gateway);
    let a = g
        .call(
            &r.url,
            "GET",
            &routes::fill(routes::SANDBOX_TOKEN, &[&s.id.to_string()]),
            None,
            &[],
        )
        .await;
    assert_eq!((a.status, a.body["live"].clone()), (200, json!(true)));
    let a = g.call(&r.url, "GET", routes::SANDBOX_TOKEN_KEYS, None, &[]).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["keys"][0]["alg"], "Ed25519");
    let p = h
        .client(Caller::Pigeon)
        .call(&r.url, "GET", &routes::fill(routes::PERSON, &[STAFF]), None, &[])
        .await;
    assert_eq!((p.status, p.body["allowed"].clone()), (200, json!(true)));
    // the descriptor too
    let ident = [
        ("aviary-sandbox-id", s.id.to_string()),
        ("aviary-sandbox-conversation", s.conversation_id.to_string()),
        ("aviary-sandbox-person", s.person.clone()),
    ];
    let a = g.call(&r.url, "GET", routes::SANDBOX_SELF, None, &ident).await;
    assert_eq!(a.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_descriptor_its_etag_and_secrets() {
    let h = Harness::new("lifecycle_self").await;
    let r = h.replica("hermit-a").await;
    let (_, s) = ready(&h, &r, STAFF).await;
    let g = h.client(Caller::Gateway);
    let ident = vec![
        ("aviary-sandbox-id", s.id.to_string()),
        ("aviary-sandbox-conversation", s.conversation_id.to_string()),
        ("aviary-sandbox-person", s.person.clone()),
    ];
    let a = g.call(&r.url, "GET", routes::SANDBOX_SELF, None, &ident).await;
    assert_eq!(a.status, 200);
    let d = &a.body;
    assert_eq!(
        d["claude"]["managedClaudeMd"],
        "You are the deployment's assistant.\n\nWorkspace 1 GiB, 1,000 files; scratch 5 GiB."
    );
    assert_eq!(d["title"]["system"], "Write a short title.");
    assert_eq!(d["control"]["port"], 7070);
    let token = ::hermit::starts::sandbox_token(r.core(), &s).unwrap();
    assert_eq!(
        d["env"]["PIP_INDEX_URL"],
        format!("http://sbx:{token}@198.19.1.1:3128/mirror/pypi/simple")
    );
    assert_eq!(d["env"]["PIP_TRUSTED_HOST"], "198.19.1.1");
    assert_eq!(d["resources"]["memoryBytes"], 1u64 << 30);
    assert_eq!(d["lifecycle"]["idleMinutes"], 30);
    let control = r.core().keys.control_token(1, s.id).unwrap();
    assert_eq!(
        d["control"]["tokenSha256"],
        json!([hermit_api::token::sha256_hex(&control)])
    );
    let etag = a.headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(json!(etag), d["etag"]);
    let mut with = ident.clone();
    with.push(("if-none-match", etag.clone()));
    assert_eq!(
        g.call(&r.url, "GET", routes::SANDBOX_SELF, None, &with).await.status,
        304
    );
    // the same descriptor from another replica (a deterministic etag)
    let r2 = h.replica("hermit-b").await;
    assert!(common::eventually(WAIT, || async { r2.core().live.get(s.id).is_some() }).await);
    let b = g.call(&r2.url, "GET", routes::SANDBOX_SELF, None, &ident).await;
    assert_eq!(b.body["etag"], json!(etag));
    // secrets: none granted
    let a = g.call(&r.url, "GET", routes::SANDBOX_SECRETS, None, &ident).await;
    assert_eq!(a.body, json!({"keys": []}));
    let a = g
        .call(
            &r.url,
            "GET",
            &routes::fill(routes::SANDBOX_SECRET, &["GITHUB_TOKEN"]),
            None,
            &ident,
        )
        .await;
    assert_eq!((a.status, a.body["code"].clone()), (404, json!("not_granted")));
    // another conversation's headers: no_sandbox
    let mut wrong = ident.clone();
    wrong[1].1 = Uuid::now_v7().to_string();
    assert_eq!(
        g.call(&r.url, "GET", routes::SANDBOX_SELF, None, &wrong).await.body["code"],
        "no_sandbox"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_removed_from_the_catalogue_is_not_live_and_is_released() {
    let dir = std::env::temp_dir().join(format!("hermit-cat-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("catalogue.json");
    std::fs::write(
        &path,
        json!({"users": ["@example.org", STAFF], "admins": []}).to_string(),
    )
    .unwrap();
    let mut h = Harness::new("lifecycle_catalogue").await;
    h.catalogue = std::sync::Arc::new(::hermit::catalogue::Catalogue::new(&path));
    assert!(h.catalogue.reload());
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    std::fs::write(&path, json!({"users": ["@example.org"], "admins": []}).to_string()).unwrap();
    assert!(h.catalogue.reload());
    let g = h.client(Caller::Gateway);
    let a = g
        .call(
            &r.url,
            "GET",
            &routes::fill(routes::SANDBOX_TOKEN, &[&s.id.to_string()]),
            None,
            &[],
        )
        .await;
    assert_eq!(a.body["live"], false);
    assert_eq!(janitor::pass(r.core()).await.unwrap().not_allowed, vec![s.id]);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("not_allowed"));
    // a refused file keeps the last good catalogue
    std::fs::write(&path, "{not json").unwrap();
    assert!(h.catalogue.reload());
    assert!(h.catalogue.allowed("x@example.org"));
    assert_eq!(put(&h, &r, Uuid::now_v7(), STAFF).await.body["code"], "not_allowed");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drain_hands_its_claims_and_leadership_to_another_replica() {
    let h = Harness::new("lifecycle_drain").await;
    let a = h.replica("hermit-a").await;
    assert!(common::eventually(WAIT, || async { a.app.leads() }).await);
    let b = h.replica("hermit-b").await;
    h.fabric.hold_create.store(true, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    put(&h, &a, cid, STAFF).await;
    assert!(common::eventually(WAIT, || async { h.fabric.creates.load(Ordering::SeqCst) >= 1 }).await);
    assert_eq!(a.app.begin_drain().await, Some(1));
    assert!(!a.app.ready());
    let moved = common::eventually(WAIT, || async {
        row(&h, cid).await.task_owner.as_deref() == Some("hermit-b")
    })
    .await;
    assert!(moved, "{:?}", row(&h, cid).await);
    assert!(
        common::eventually(WAIT, || async { b.app.leads() }).await,
        "leadership moved"
    );
    h.fabric.release_create();
    wait_state(&h, cid, SandboxState::Ready).await;
    a.app.finish_drain().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tasks_complete_with_a_one_second_idle_in_transaction_limit_while_calls_are_held() {
    let h = Harness::new("lifecycle_no_long_tx").await;
    let db_name =
        h.db.url
            .rsplit('/')
            .next()
            .unwrap()
            .split('?')
            .next()
            .unwrap()
            .to_owned();
    h.sql(&format!(
        "alter database {db_name} set idle_in_transaction_session_timeout = '1s'"
    ))
    .await;
    let r = h.replica("hermit-a").await;
    h.fabric.hold_create.store(true, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    put(&h, &r, cid, STAFF).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    h.fabric.release_create();
    let s = wait_state(&h, cid, SandboxState::Ready).await;
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Operator, true, Some("test".into()))
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    hold.release();
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_note.as_deref(), Some("test"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_failures_end_gone_with_their_reason() {
    let cfg = common::config(|v| v["sandbox"]["supervisorWaitSeconds"] = 1.into());
    let h = Harness::with("lifecycle_failures", cfg, common::timings()).await;
    let r = h.replica("hermit-a").await;
    // sandboxd refuses: start_failed with the class as the note, recorded in sandbox_starts
    *h.fabric.fail_create.lock().unwrap() = Some("image_pull".into());
    let cid = Uuid::now_v7();
    put(&h, &r, cid, STAFF).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(
        (gone.release_reason.as_deref(), gone.release_note.as_deref()),
        (Some("start_failed"), Some("image_pull"))
    );
    assert_eq!(gone.release_code.as_deref(), Some("sandbox_failed"));
    *h.fabric.fail_create.lock().unwrap() = None;
    // no host and the cap reached: hosts_full / sandbox_busy, not waited out
    h.fabric.no_slot.store(true, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    put(&h, &r, cid, STAFF).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(
        (gone.release_reason.as_deref(), gone.release_code.as_deref()),
        (Some("hosts_full"), Some("sandbox_busy"))
    );
    h.fabric.no_slot.store(false, Ordering::SeqCst);
    // a supervisor that never says ready: supervisor_timeout, destroyed
    *h.fabric.ready_delay.lock().unwrap() = Duration::from_secs(3600);
    let cid = Uuid::now_v7();
    put(&h, &r, cid, STAFF).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("supervisor_timeout"));
    assert!(h.fabric.destroyed.lock().unwrap().contains(&gone.name));
    let failed: i64 = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from sandbox_starts where not ok", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(failed, 3);
    // disabled by the operator: 503 sandbox_busy with retry-after
    h.sql("update controls set sandboxes_enabled = false").await;
    let a = put(&h, &r, Uuid::now_v7(), STAFF).await;
    assert_eq!(
        (a.status, a.body["code"].clone(), a.body["reason"].clone()),
        (503, json!("sandbox_busy"), json!("disabled"))
    );
    assert_eq!(a.headers["retry-after"], "300");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callers_only_reach_their_routes() {
    let h = Harness::new("lifecycle_auth").await;
    let r = h.replica("hermit-a").await;
    let cid = Uuid::now_v7().to_string();
    let a = h
        .client(Caller::Pigeon)
        .call(
            &r.url,
            "PUT",
            &sandbox_path(cid.parse().unwrap()),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!((a.status, a.body["code"].clone()), (403, json!("wrong_caller")));
    let a = h
        .client(Caller::Tools)
        .call(&r.url, "GET", routes::SANDBOX_TOKEN_KEYS, None, &[])
        .await;
    assert_eq!(a.status, 200);
    let a = h
        .client(Caller::Monitor)
        .call(&r.url, "GET", routes::DEPLOYMENT, None, &[])
        .await;
    assert_eq!(a.status, 403);
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "GET", "/v1/nothing-here", None, &[])
        .await;
    assert_eq!(a.status, 404);
    let mut nobody = h.client(Caller::Magpie);
    nobody.token = "not-a-token".into();
    let a = nobody.call(&r.url, "GET", routes::DEPLOYMENT, None, &[]).await;
    assert_eq!(a.status, 401);
    assert!(a.headers.contains_key("www-authenticate"));
}

/// hermit 24.2 "Resumable tasks": a replica killed while its start is in `phase` (held there by the fake), another
/// replica's takeover sweep finishes the same row: one sandbox created, the token any replica derives, one start
/// recorded. `image` has no call to hold it in, so its replica dies in `placing` and the row is set to what the
/// placing step leaves (host, address, phase `image`), the state a death right after that step finds.
async fn takeover_in(phase: SandboxPhase) {
    let cfg = common::config(|v| {
        if phase == SandboxPhase::Capacity {
            v["sandbox"]["maxActive"] = 1.into();
        }
    });
    let h = Harness::with(&format!("takeover_{phase}"), cfg, common::timings()).await;
    let a = h.replica("hermit-a").await;
    // the hold for the phase; for capacity, a sandbox holding the one slot (reserved by its PUT, so no pool victim)
    let mut blocker = None;
    match phase {
        SandboxPhase::Capacity => blocker = Some(ready(&h, &a, STAFF).await.1),
        SandboxPhase::Workspace => h.workspaces.ensure_gate.shut(),
        SandboxPhase::Placing | SandboxPhase::Image => h.fabric.place_gate.shut(),
        SandboxPhase::Host => {
            h.fabric.no_slot.store(true, Ordering::SeqCst);
            *h.fabric.launch.lock().unwrap() = ::hermit::fabric::Launch::Launched;
        }
        SandboxPhase::Creating => h.fabric.hold_create.store(true, Ordering::SeqCst),
        SandboxPhase::Supervisor => *h.fabric.ready_delay.lock().unwrap() = Duration::from_secs(3),
        SandboxPhase::Unknown => unreachable!(),
    }
    let cid = Uuid::now_v7();
    assert_eq!(put(&h, &a, cid, STAFF).await.status, 202);
    let held_in = if phase == SandboxPhase::Image {
        SandboxPhase::Placing
    } else {
        phase
    };
    let in_phase = common::eventually(WAIT, || async {
        let r = row(&h, cid).await;
        let at_call = match held_in {
            SandboxPhase::Workspace => h.workspaces.ensure_gate.reached.load(Ordering::SeqCst) >= 1,
            SandboxPhase::Placing => h.fabric.place_gate.reached.load(Ordering::SeqCst) >= 1,
            SandboxPhase::Creating => h.fabric.creates.load(Ordering::SeqCst) >= 1,
            _ => true,
        };
        r.phase == Some(held_in) && at_call
    })
    .await;
    let at_kill = row(&h, cid).await;
    assert!(in_phase, "not held in {held_in}: {at_kill:?}");
    assert_eq!(at_kill.task_owner.as_deref(), Some("hermit-a"));
    a.kill();
    if phase == SandboxPhase::Image {
        h.sql(&format!(
            "update sandboxes set phase = 'image', host = 'fake-1', ip = '198.18.9.9' where id = '{}'",
            at_kill.id
        ))
        .await;
    }
    let b = h.replica("hermit-b").await;
    match phase {
        SandboxPhase::Capacity => {
            let s0 = blocker.take().unwrap();
            ::hermit::sandboxes::request(b.core(), s0.id, ReleaseReason::Operator, true, None)
                .await
                .unwrap();
        }
        SandboxPhase::Workspace => h.workspaces.ensure_gate.open(),
        SandboxPhase::Placing | SandboxPhase::Image => h.fabric.place_gate.open(),
        SandboxPhase::Host => h.fabric.no_slot.store(false, Ordering::SeqCst),
        SandboxPhase::Creating => h.fabric.release_create(),
        _ => {}
    }
    let s = wait_state(&h, cid, SandboxState::Ready).await;
    assert_eq!(s.id, at_kill.id);
    assert!(s.task.is_none());
    // created once: A died before its create, or B's create of the same name and address was sandboxd's
    // idempotent repeat
    let created = h
        .fabric
        .created
        .lock()
        .unwrap()
        .iter()
        .filter(|n| **n == s.name)
        .count();
    assert_eq!(created, 1, "{phase}");
    // the token B's sandbox runs with is the one any replica derives from the row
    let sbx = h.fabric.sandbox_by_name(&s.name).unwrap();
    let proxy = sbx.spec_env.iter().find(|(k, _)| k == "HTTPS_PROXY").unwrap().1.clone();
    let token = ::hermit::starts::sandbox_token(b.core(), &s).unwrap();
    assert!(proxy.contains(&token), "{proxy}");
    let starts = h
        .pool()
        .get()
        .await
        .unwrap()
        .query(
            "select ok, start_ms from sandbox_starts where sandbox_id = $1",
            &[&s.id],
        )
        .await
        .unwrap();
    assert_eq!(starts.len(), 1, "{phase}");
    let (ok, start_ms): (bool, Option<i32>) = (starts[0].get(0), starts[0].get(1));
    assert!(ok);
    // start_ms runs from the start of `creating` (11.2 step 6), not from the takeover: the supervisor became ready
    // 3 s after the create, whoever was waiting for it
    let start_ms = start_ms.expect("start_ms");
    if phase == SandboxPhase::Supervisor {
        assert!(start_ms >= 2500, "start_ms {start_ms}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_in_capacity_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Capacity).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_in_workspace_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Workspace).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_in_placing_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Placing).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_waiting_for_a_host_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_after_image_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Image).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_in_creating_is_finished_by_another_replica_with_the_same_token() {
    takeover_in(SandboxPhase::Creating).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_killed_waiting_for_the_supervisor_is_finished_by_another_replica() {
    takeover_in(SandboxPhase::Supervisor).await;
    // (the supervisor phase's own check: no second create, start_ms from creating, both in takeover_in)
}

/// magpie's driver (magpie 8.3 steps 1-2): `PUT` (other 5xx: retry with backoff), then on 202 long-poll until
/// `ready` or `gone`. Returns the ready sandbox and every answer it saw; a `gone` is a failed turn.
async fn drive(h: &Harness, r: &Replica, cid: Uuid) -> (Value, Vec<(u16, Value)>) {
    let mut seen = Vec::new();
    let mut backoff = Duration::from_millis(500);
    loop {
        let a = put(h, r, cid, STAFF).await;
        seen.push((a.status, a.body.clone()));
        match a.status {
            200 => return (a.body, seen),
            202 => {
                let mut v = a.body["version"].as_u64().unwrap();
                loop {
                    let g = h
                        .client(Caller::Magpie)
                        .call(
                            &r.url,
                            "GET",
                            &format!("{}?waitMs=25000&afterVersion={v}", sandbox_path(cid)),
                            None,
                            &[],
                        )
                        .await;
                    assert_eq!(g.status, 200, "{}", g.body);
                    seen.push((g.status, g.body.clone()));
                    match g.body["state"].as_str() {
                        Some("ready") => return (g.body, seen),
                        Some("gone") => panic!("the turn would fail: {seen:?}"),
                        _ => v = g.body["version"].as_u64().unwrap(),
                    }
                }
            }
            503 => {
                assert_eq!(a.body["code"], "unavailable", "{}", a.body);
                assert!(a.headers.contains_key("retry-after"));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
            other => panic!("PUT {other}: {}", a.body),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_during_a_long_forced_release_ends_with_a_new_ready_sandbox_and_no_failed_turn() {
    // a forced release whose supervisor never answers runs to its 10 s deadline
    let timings = ::hermit::Timings {
        release_deadline: Duration::from_secs(10),
        ..common::timings()
    };
    let h = Harness::with("lifecycle_put_while_releasing", common::config(|_| {}), timings).await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Operator, true, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    assert_eq!(row(&h, cid).await.status, SandboxState::Releasing);
    let t = std::time::Instant::now();
    let (sbx, seen) = tokio::time::timeout(Duration::from_secs(60), drive(&h, &r, cid))
        .await
        .expect("the driver got a sandbox");
    assert_ne!(sbx["id"], json!(s.id), "{seen:?}");
    assert_eq!(sbx["state"], "ready");
    // the PUTs during the release were retryable 503s, never the releasing sandbox
    assert!(seen.iter().any(|(st, _)| *st == 503), "{seen:?}");
    assert!(
        seen.iter().all(|(_, b)| b["id"] != json!(s.id)),
        "the releasing sandbox was handed out: {seen:?}"
    );
    assert!(t.elapsed() >= Duration::from_secs(7), "{:?}", t.elapsed());
    let old = db::load(&h.pool(), s.id).await.unwrap().unwrap();
    assert_eq!(
        (old.status, old.release_reason.as_deref()),
        (SandboxState::Gone, Some("operator"))
    );
    hold.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_release_asked_while_a_soft_one_is_refused_still_ends_the_sandbox() {
    let h = Harness::new("lifecycle_forced_over_soft").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    // busy: a soft release is refused; the call is held at the supervisor while magpie deletes the conversation
    sup(&h, &s).set_run(ActivityState::Held, None, 1);
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Idle, false, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert_eq!((a.status, a.body["released"].clone()), (200, json!(1)));
    hold.release();
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("deleted"));
    let calls = sup_calls(&h, &s);
    assert_eq!(
        calls
            .iter()
            .map(|c| (c["reason"].clone(), c["force"].clone()))
            .collect::<Vec<_>>(),
        vec![(json!("idle"), json!(false)), (json!("deleted"), json!(true))]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_while_the_supervisor_is_asked_a_soft_release_never_answers_a_dying_sandbox() {
    let h = Harness::new("lifecycle_put_vs_soft").await;
    let r = h.replica("hermit-a").await;
    // the supervisor agrees (idle): the PUT waits for that answer and gets a new sandbox, 202
    let (cid, s) = ready(&h, &r, STAFF).await;
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Idle, false, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    let (h2, url) = (h.client(Caller::Magpie), r.url.clone());
    let p = tokio::spawn(async move {
        h2.call(&url, "PUT", &sandbox_path(cid), Some(json!({"person": STAFF})), &[])
            .await
    });
    // the cancellation is on the row at once, marked (the release is out at the supervisor), not cleared
    assert!(
        common::eventually(WAIT, || async {
            db::load(&h.pool(), s.id)
                .await
                .unwrap()
                .unwrap()
                .release_pending
                .is_some_and(|p| p.cancelled && p.sent)
        })
        .await
    );
    hold.release();
    let a = p.await.unwrap();
    assert_eq!(a.status, 202, "{}", a.body);
    assert_ne!(a.body["id"], json!(s.id));
    let old = db::load(&h.pool(), s.id).await.unwrap().unwrap();
    assert_eq!(
        (old.status, old.release_reason.as_deref()),
        (SandboxState::Gone, Some("idle"))
    );
    wait_state(&h, cid, SandboxState::Ready).await;

    // the supervisor refuses (busy): the PUT answers 200 with the same sandbox, the release is gone
    let (cid, s) = ready(&h, &r, STAFF).await;
    sup(&h, &s).set_run(ActivityState::Held, None, 1);
    let hold = aviary_fakes::Hold::new();
    sup(&h, &s)
        .faults()
        .on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Idle, false, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    let (h2, url) = (h.client(Caller::Magpie), r.url.clone());
    let p = tokio::spawn(async move {
        h2.call(&url, "PUT", &sandbox_path(cid), Some(json!({"person": STAFF})), &[])
            .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    hold.release();
    let a = p.await.unwrap();
    assert_eq!((a.status, a.body["id"].clone()), (200, json!(s.id)), "{}", a.body);
    let now = row(&h, cid).await;
    assert_eq!(now.status, SandboxState::Ready);
    assert!(now.release_pending.is_none() && now.task.is_none(), "{now:?}");
    assert_eq!(sup_calls(&h, &s).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_release_run_late_still_asks_the_supervisor_to_end_the_turn() {
    let h = Harness::new("lifecycle_forced_late").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    sup(&h, &s).set_run(ActivityState::Running, None, 0);
    // a forced release asked long ago (its claim could not be taken then): its old deadline has passed
    h.sql(&format!(
        "update sandboxes set release_pending = '{{\"reason\":\"operator\",\"note\":null,\"force\":true,\"deadlineAt\":\"2020-01-01T00:00:00.000Z\",\"askedAt\":\"2020-01-01T00:00:00.000Z\",\"refusals\":0}}' where id = '{}'",
        s.id
    ))
    .await;
    let claim = ::hermit::tasks::take(r.core(), s.id, ::hermit::tasks::Kind::Release)
        .await
        .unwrap()
        .unwrap();
    ::hermit::tasks::run(r.core(), claim);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("operator"));
    assert!(
        sup_calls(&h, &s)
            .iter()
            .any(|c| c["force"] == true && c["reason"] == "operator"),
        "the running turn was cut by a bare destroy"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_drains_through_the_binarys_hooks() {
    use aviary_core::drain::{Drain, Outcome, Signal, Signals};
    use aviary_core::{ops, version};
    let h = Harness::new("lifecycle_sigterm").await;
    let a = h.replica("hermit-a").await;
    assert!(common::eventually(WAIT, || async { a.app.leads() }).await);
    let b = h.replica("hermit-b").await;
    // A's start is inside a sandboxd call when the signal comes
    h.fabric.hold_create.store(true, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    put(&h, &a, cid, STAFF).await;
    assert!(common::eventually(WAIT, || async { h.fabric.creates.load(Ordering::SeqCst) >= 1 }).await);
    // a long-poll A holds
    let v = row(&h, cid).await.version;
    let (m, url) = (h.client(Caller::Magpie), a.url.clone());
    let poll = tokio::spawn(async move {
        let t = std::time::Instant::now();
        let g = m
            .call(
                &url,
                "GET",
                &format!("{}?waitMs=25000&afterVersion={v}", sandbox_path(cid)),
                None,
                &[],
            )
            .await;
        (g.status, t)
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // the ops server and the drain as main.rs builds them
    let build = version::Build::new("hermit");
    let o = ops::Ops::new(build.clone(), ops::registry(&build));
    let addr = o.spawn("127.0.0.1:0".parse().unwrap()).await.unwrap();
    o.readiness().started();
    let readyz = format!("http://{addr}/readyz");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(http.get(&readyz).send().await.unwrap().status(), 200);
    let mut d = Drain::new(o.readiness().clone());
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let st = stopped.clone();
    ::hermit::app::drain_hooks(&mut d, a.app.clone(), move || async move {
        st.store(true, Ordering::SeqCst);
    });
    let (sig, mut signals) = Signals::channel();
    let t = std::time::Instant::now();
    let drain = tokio::spawn(async move { d.run(&mut signals).await });
    sig.send(Signal::Term).unwrap();
    // readiness 503 through the ops server; the held long-poll answered at once
    let unready = common::eventually(Duration::from_secs(2), || async {
        http.get(&readyz).send().await.unwrap().status() == 503
    })
    .await;
    assert!(unready);
    let (status, _) = tokio::time::timeout(Duration::from_secs(3), poll)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, 200);
    // the claim and the leadership move to B while A's call is still out
    assert!(
        common::eventually(WAIT, || async {
            row(&h, cid).await.task_owner.as_deref() == Some("hermit-b")
        })
        .await
    );
    assert!(common::eventually(WAIT, || async { b.app.leads() }).await);
    assert!(
        !drain.is_finished(),
        "the drain waits for the sandboxd call already sent"
    );
    // the call returns: A writes nothing (the claim is B's), its task ends, the drain completes
    h.fabric.release_create();
    let outcome = tokio::time::timeout(Duration::from_secs(90), drain)
        .await
        .expect("drained within terminationGracePeriodSeconds")
        .unwrap();
    assert_eq!(outcome, Outcome::Drained);
    assert!(t.elapsed() < Duration::from_secs(30), "{:?}", t.elapsed());
    assert!(stopped.load(Ordering::SeqCst));
    assert!(a.core().tasks.is_empty());
    let s = wait_state(&h, cid, SandboxState::Ready).await;
    assert!(s.task.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_sandboxd_is_retried_and_a_failing_placement_is_bounded() {
    let cfg = common::config(|v| v["sandbox"]["hostWaitSeconds"] = 1.into());
    let h = Harness::with("lifecycle_create_retries", cfg, common::timings()).await;
    let r = h.replica("hermit-a").await;
    // two transport failures, then sandboxd answers: ready
    h.fabric.unreachable_creates.store(2, Ordering::SeqCst);
    let (_, s) = ready(&h, &r, STAFF).await;
    assert_eq!(h.fabric.creates.load(Ordering::SeqCst), 3);
    assert_eq!(s.status, SandboxState::Ready);
    // three: start_failed, note unreachable
    h.fabric.unreachable_creates.store(3, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    put(&h, &r, cid, STAFF).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(
        (gone.release_reason.as_deref(), gone.release_note.as_deref()),
        (Some("start_failed"), Some("unreachable"))
    );
    // a fabric whose placement keeps failing: start_failed after hostWaitSeconds, not at the 8 min deadline
    h.fabric.place_error.store(true, Ordering::SeqCst);
    let cid = Uuid::now_v7();
    let t = std::time::Instant::now();
    put(&h, &r, cid, STAFF).await;
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(
        (gone.release_reason.as_deref(), gone.release_note.as_deref()),
        (Some("start_failed"), Some("placement"))
    );
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

/// hermit 20.2 step 5 and 10.3: once a drain handed the claims back, a task of this replica makes no side effect.
/// A's start is inside its create when A drains; B resumes the row and creates the same sandbox; A's create then
/// fails definitively. A must neither destroy B's sandbox nor record a failed start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_failing_after_its_claim_was_handed_back_leaves_the_new_owner_alone() {
    // the renewal that would notice the lost claim is far off: only the hand-back itself can stop A
    let timings = ::hermit::Timings {
        claim: Duration::from_secs(15),
        renew: Duration::from_secs(5),
        ..common::timings()
    };
    let h = Harness::with("lifecycle_handback_effects", common::config(|_| {}), timings).await;
    let a = h.replica("hermit-a").await;
    let b = h.replica("hermit-b").await;
    h.fabric.hold_create.store(true, Ordering::SeqCst);
    *h.fabric.fail_create_call.lock().unwrap() = Some((1, "image_pull".into()));
    let cid = Uuid::now_v7();
    assert_eq!(put(&h, &a, cid, STAFF).await.status, 202);
    assert!(common::eventually(WAIT, || async { h.fabric.creates.load(Ordering::SeqCst) >= 1 }).await);
    assert_eq!(a.app.begin_drain().await, Some(1));
    // B resumed the row and is inside its own create of the same sandbox
    let moved = common::eventually(WAIT, || async {
        row(&h, cid).await.task_owner.as_deref() == Some("hermit-b") && h.fabric.creates.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(moved, "{:?}", row(&h, cid).await);
    // A's create fails (image_pull), B's succeeds
    h.fabric.release_create();
    let s = wait_state(&h, cid, SandboxState::Ready).await;
    assert_eq!(a.app.settle_tasks(Duration::from_secs(5)).await, 0);
    assert!(
        !h.fabric.destroyed.lock().unwrap().contains(&s.name),
        "A destroyed the sandbox B started"
    );
    assert!(h.fabric.sandbox_by_name(&s.name).is_some());
    let starts: Vec<bool> = h
        .pool()
        .get()
        .await
        .unwrap()
        .query("select ok from sandbox_starts where sandbox_id = $1", &[&s.id])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(starts, vec![true], "A recorded a failed start");
    let now = row(&h, cid).await;
    assert_eq!((now.status, now.task.as_deref()), (SandboxState::Ready, None));
    a.app.finish_drain().await;
    drop(b);
}

/// A takeover of a sent soft release that a `PUT` cancelled asks the supervisor again before the sandbox is kept:
/// the supervisor may have agreed before the release replica died.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_soft_release_whose_replica_died_is_asked_again_before_the_sandbox_is_kept() {
    let h = Harness::new("lifecycle_cancel_sent").await;
    let r = h.replica("hermit-a").await;
    // a sent soft release (idle) whose replica died before it recorded the answer; its claim is still live
    let dead_release = |id: Uuid| {
        format!(
            "update sandboxes set release_pending = '{{\"reason\":\"idle\",\"note\":null,\"force\":false,\"deadlineAt\":\"2099-01-01T00:00:00.000Z\",\"askedAt\":\"2026-01-01T00:00:00.000Z\",\"refusals\":0,\"sent\":true}}',
               task = 'release', task_owner = 'hermit-dead', task_until = now() + interval '2 seconds', reserved_until = null
             where id = '{id}'"
        )
    };
    // the supervisor agrees when asked again: the sandbox goes, and the driver gets a new one
    let (cid, s) = ready(&h, &r, STAFF).await;
    h.sql(&dead_release(s.id)).await;
    let (sbx, seen) = tokio::time::timeout(Duration::from_secs(60), drive(&h, &r, cid))
        .await
        .expect("the driver got a sandbox");
    assert_ne!(sbx["id"], json!(s.id), "{seen:?}");
    assert!(
        seen.iter().all(|(_, b)| b["id"] != json!(s.id)),
        "the released sandbox was handed out: {seen:?}"
    );
    let old = db::load(&h.pool(), s.id).await.unwrap().unwrap();
    assert_eq!(
        (old.status, old.release_reason.as_deref()),
        (SandboxState::Gone, Some("idle"))
    );
    assert_eq!(sup_calls(&h, &s).len(), 1);

    // the supervisor refuses (busy) when asked again: the sandbox stays and the cancellation clears the release
    let (cid, s) = ready(&h, &r, STAFF).await;
    sup(&h, &s).set_run(ActivityState::Held, None, 1);
    h.sql(&dead_release(s.id)).await;
    let (sbx, seen) = tokio::time::timeout(Duration::from_secs(60), drive(&h, &r, cid))
        .await
        .expect("the driver got a sandbox");
    assert_eq!(sbx["id"], json!(s.id), "{seen:?}");
    let now = row(&h, cid).await;
    assert_eq!(now.status, SandboxState::Ready);
    assert!(now.release_pending.is_none() && now.task.is_none(), "{now:?}");
    assert_eq!(sup_calls(&h, &s).len(), 1);
}

/// A soft release whose call fails while a forced one replaced it runs the forced one at once (13.12), instead of
/// leaving it to the janitor while every `PUT` answers 503.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_release_asked_while_a_soft_call_fails_runs_at_once() {
    let h = Harness::new("lifecycle_forced_over_failed_soft").await;
    let r = h.replica("hermit-a").await;
    let (cid, s) = ready(&h, &r, STAFF).await;
    // the soft call's first try answers 503, the second is held until it times out
    let hold = aviary_fakes::Hold::new();
    let sv = sup(&h, &s);
    let f = sv.faults();
    f.times(
        "release",
        aviary_fakes::FaultKind::Unavailable { retry_after_secs: 1 },
        1,
    );
    f.on("release", aviary_fakes::FaultKind::Hold(hold.clone()));
    ::hermit::sandboxes::request(r.core(), s.id, ReleaseReason::Idle, false, None)
        .await
        .unwrap();
    assert!(hold.wait_held(1, WAIT).await);
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert_eq!((a.status, a.body["released"].clone()), (200, json!(1)));
    // the held soft call times out (Failed); the forced release starts with no janitor pass (an hour in tests)
    let releasing = common::eventually(WAIT, || async {
        db::load(&h.pool(), s.id).await.unwrap().unwrap().status == SandboxState::Releasing
    })
    .await;
    assert!(releasing, "{:?}", row(&h, cid).await);
    hold.release();
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert_eq!(gone.release_reason.as_deref(), Some("deleted"));
    assert!(
        sup_calls(&h, &s)
            .iter()
            .any(|c| c["force"] == true && c["reason"] == "deleted"),
        "{:?}",
        sup_calls(&h, &s)
    );
}

/// A forced release asked while a start waits for a host ends the start at its next poll, not after
/// `hostWaitSeconds`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_release_ends_a_start_waiting_for_a_host_at_once() {
    let cfg = common::config(|v| v["sandbox"]["hostWaitSeconds"] = 60.into());
    let h = Harness::with("lifecycle_release_in_host_wait", cfg, common::timings()).await;
    let r = h.replica("hermit-a").await;
    h.fabric.no_slot.store(true, Ordering::SeqCst);
    *h.fabric.launch.lock().unwrap() = ::hermit::fabric::Launch::Launched;
    let cid = Uuid::now_v7();
    assert_eq!(put(&h, &r, cid, STAFF).await.status, 202);
    assert!(common::eventually(WAIT, || async { row(&h, cid).await.phase == Some(SandboxPhase::Host) }).await);
    let t = std::time::Instant::now();
    let rel = routes::fill(routes::SANDBOX_RELEASE, &[&cid.to_string()]);
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "POST", &rel, Some(json!({"reason": "deleted"})), &[])
        .await;
    assert_eq!(a.status, 200);
    let gone = wait_state(&h, cid, SandboxState::Gone).await;
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(gone.release_reason.as_deref(), Some("deleted"));
}
