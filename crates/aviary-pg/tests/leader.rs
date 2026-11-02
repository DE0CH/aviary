//! Leadership by advisory lock, against the CI Postgres.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use aviary_pg::{Leader, LockKey};

fn leads(l: &Leader) -> bool {
    *l.is_leader().borrow()
}

#[tokio::test]
async fn one_leader_and_failover_when_the_holders_connection_dies() {
    let url = common::fresh_db("leader").await;
    let admin = common::pool(&url, "admin");
    let key = LockKey::Pair(7300, 3);
    // A bids first and wins; B retries quickly so the failover is prompt and deterministic
    let a = Leader::start_with(&common::pool(&url, "a"), key.clone(), Duration::from_secs(5));
    assert!(
        common::eventually(Duration::from_secs(5), || leads(&a)).await,
        "a never led"
    );
    let b = Leader::start_with(&common::pool(&url, "b"), key.clone(), Duration::from_millis(200));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(leads(&a) && !leads(&b), "two leaders or none");

    // the holder's connection dies
    assert_eq!(common::kill_lock_holders(&admin).await, 1);
    assert!(
        common::eventually(Duration::from_secs(2), || !leads(&a)).await,
        "a still thinks it leads"
    );
    assert!(
        common::eventually(Duration::from_secs(3), || leads(&b)).await,
        "b did not take over"
    );
    // a (waiting out its retry, then a follower) never leads again while b holds
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(leads(&b) && !leads(&a));

    // b's connection dies too: someone leads again, never both
    assert_eq!(common::kill_lock_holders(&admin).await, 1);
    assert!(
        common::eventually(Duration::from_secs(7), || leads(&a) ^ leads(&b)).await,
        "no leader after b died"
    );
    for _ in 0..20 {
        assert!(!(leads(&a) && leads(&b)), "two leaders");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn release_hands_over_within_one_retry() {
    let url = common::fresh_db("release").await;
    let key = LockKey::name("svc.jobs");
    let a = Leader::start_with(&common::pool(&url, "a"), key.clone(), Duration::from_millis(500));
    assert!(common::eventually(Duration::from_secs(5), || leads(&a)).await);
    let b = Leader::start_with(&common::pool(&url, "b"), key.clone(), Duration::from_millis(500));
    let mut a_rx = a.is_leader();
    a.release().await;
    assert!(!*a_rx.borrow_and_update(), "released but still leading");
    assert!(
        common::eventually(Duration::from_secs(2), || leads(&b)).await,
        "b did not take over"
    );
}

#[tokio::test]
async fn a_follower_survives_the_database_being_unreachable_and_leads_when_it_returns() {
    let url = common::fresh_db("unreachable").await;
    // nothing listens on port 1: every connect fails
    let bad = {
        let (head, tail) = url.split_once('@').unwrap();
        let rest = tail.split_once('/').unwrap().1;
        format!("{head}@127.0.0.1:1/{rest}")
    };
    let l = Leader::start_with(
        &common::pool(&bad, "nowhere"),
        LockKey::Pair(1, 2),
        Duration::from_millis(100),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!leads(&l));
    l.release().await;
    let l = Leader::start_with(
        &common::pool(&url, "here"),
        LockKey::Pair(1, 2),
        Duration::from_millis(100),
    );
    assert!(common::eventually(Duration::from_secs(3), || leads(&l)).await);
}

async fn connections(admin: &aviary_pg::Pool, app: &str) -> i64 {
    admin
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from pg_stat_activity where application_name = $1 and datname = current_database()",
            &[&app],
        )
        .await
        .unwrap()
        .get(0)
}

/// hermit's janitor and scaler lead on one dedicated connection (hermit 10.3, platform 11.4).
#[tokio::test]
async fn several_keys_share_one_connection_and_lead_on_their_own() {
    let db = common::fresh_db("many").await;
    let admin = common::pool(&db, "admin");
    let janitor = LockKey::Pair(7300, 3);
    let scaler = LockKey::Pair(7300, 4);
    // another pod holds the scaler
    let other = Leader::start_with(&common::pool(&db, "other"), scaler.clone(), Duration::from_millis(200));
    assert!(common::eventually(Duration::from_secs(5), || leads(&other)).await);

    let many = Leader::start_many(
        &common::pool(&db, "many"),
        &[janitor.clone(), scaler.clone()],
        Duration::from_millis(300),
    );
    let j = many.watch(&janitor).unwrap();
    let s = many.watch(&scaler).unwrap();
    assert!(
        common::eventually(Duration::from_secs(5), || *j.borrow()).await,
        "janitor never led"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!*s.borrow(), "two scaler leaders");
    assert!(many.confirm_key(&janitor).await);
    assert!(many.confirm().await, "confirm() is the first key");
    assert!(!many.confirm_key(&scaler).await);
    assert!(!many.confirm_key(&LockKey::Pair(7300, 9)).await);
    assert!(many.watch(&LockKey::Pair(7300, 9)).is_none());
    assert_eq!(connections(&admin, "many").await, 1);

    // the other pod drains: the scaler moves over, on the same connection
    other.release().await;
    assert!(
        common::eventually(Duration::from_secs(3), || *s.borrow()).await,
        "scaler not taken over"
    );
    assert!(many.confirm_key(&scaler).await);
    assert_eq!(connections(&admin, "many").await, 1);

    // the leader connection dies: confirm says so at once, whatever the background view still shows
    admin
        .get()
        .await
        .unwrap()
        .execute(
            "select pg_terminate_backend(pid) from pg_stat_activity where application_name = 'many'",
            &[],
        )
        .await
        .unwrap();
    assert!(!many.confirm_key(&janitor).await);
    assert!(!many.confirm_key(&scaler).await);
    assert!(common::eventually(Duration::from_secs(2), || !*j.borrow() && !*s.borrow()).await);
    // and both come back on a new connection
    assert!(common::eventually(Duration::from_secs(5), || *j.borrow() && *s.borrow()).await);
    assert_eq!(connections(&admin, "many").await, 1);
    many.release().await;
}
