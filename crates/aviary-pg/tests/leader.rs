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
