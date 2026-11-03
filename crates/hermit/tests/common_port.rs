//! hermit 24.2's port of jarvis3 `common.test.ts`, the parts that are hermit's: who is a user (the catalogue's
//! `users` with `@domain` entries), migrations applied twice change nothing, and one janitor leader at a time with a
//! takeover when it goes. (The test's Claude Code environment, sent-file hook and old-UI schema parts are the
//! supervisor's and magpie's.)

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

mod common;

use std::time::Duration;

use common::eventually;
use hermit::catalogue::Loaded;

#[test]
fn a_user_is_a_listed_email_or_anyone_at_a_listed_domain_only_them() {
    let c = Loaded::parse(br#"{"users": ["A@Example.org", "@YYC.example"], "admins": []}"#).unwrap();
    assert!(c.allowed("a@example.org"));
    assert!(c.allowed("staff@yyc.example"), "domains are lowercased");
    assert!(!c.allowed("someone@notyyc.example"));
    assert!(!c.allowed("x@sub.yyc.example"), "no subdomains");
    assert!(!c.allowed("yyc.example@evil.test"));
    assert!(!c.allowed("@yyc.example"));
    assert!(!c.allowed(""));
    assert!(
        Loaded::parse(br#"{"users": [], "admins": []}"#).is_err(),
        "no users is refused"
    );
}

#[tokio::test]
async fn migrations_are_idempotent_and_one_replica_leads_at_a_time() {
    let db = common::fresh_db("common_port").await;
    let pool = aviary_pg::connect(&db.url, "hermit-test", 4, Duration::from_secs(15)).unwrap();
    let first = hermit::db::migrate(&pool).await.unwrap();
    assert_eq!(first.len(), 5, "every migration");
    assert!(
        hermit::db::migrate(&pool).await.unwrap().is_empty(),
        "nothing applied twice"
    );

    let retry = Duration::from_millis(50);
    let a = aviary_pg::Leader::start_with(&pool, hermit::db::LOCK_JANITOR, retry);
    let rx_a = a.is_leader();
    assert!(eventually(Duration::from_secs(5), || async { *rx_a.borrow() }).await);
    let b = aviary_pg::Leader::start_with(&pool, hermit::db::LOCK_JANITOR, retry);
    let rx_b = b.is_leader();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(*rx_a.borrow() && !*rx_b.borrow(), "one leader");
    a.release().await;
    assert!(
        eventually(Duration::from_secs(5), || async { *rx_b.borrow() }).await,
        "the second replica takes over"
    );
    b.release().await;
}
