//! The leader lock (cutover 7.12 item 8, the lock's part; the hand-over loop that yields to a waiter between passes
//! is WP-52's): two bidders, one leads; a one-shot command waits for the leader, shows as a waiter, and gets the
//! lock once the leader lets go; a leader whose connection dies hands over to the other bidder within its retry.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::many_single_char_names)]

mod common;

use std::time::Duration;

use common::World;
use magpie_import::lock;

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn one_leader_a_waiting_one_shot_and_a_handover_on_a_dead_connection() {
    let w = World::new("lock").await;
    let pool = w.magpie().clone();
    let a = lock::leader(&pool);
    let b = lock::leader(&pool);
    let (la, lb) = (a.is_leader(), b.is_leader());
    until("a leader", || *la.borrow() || *lb.borrow()).await;
    tokio::time::sleep(lock::RETRY * 2).await;
    assert!(*la.borrow() ^ *lb.borrow(), "exactly one leads");
    let (leader, follower) = if *la.borrow() { (a, b) } else { (b, a) };
    assert!(leader.confirm().await);

    // a one-shot command waits while the leader holds the lock, and is seen waiting
    let p2 = pool.clone();
    let waiting = tokio::spawn(async move { lock::OneShot::take(&p2).await.unwrap() });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !lock::waiter_present(&pool).await.unwrap() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the one-shot never showed as a waiter"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!waiting.is_finished());
    // the leader lets go (between two passes); the follower must not take it from the waiting command
    follower.release().await;
    leader.release().await;
    let one_shot = tokio::time::timeout(Duration::from_secs(15), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(!lock::waiter_present(&pool).await.unwrap());
    let c = lock::leader(&pool);
    tokio::time::sleep(lock::RETRY * 2).await;
    assert!(
        !*c.is_leader().borrow(),
        "nobody leads while the one-shot holds the lock"
    );
    one_shot.release().await;
    let lc = c.is_leader();
    until("the bidder after the one-shot", || *lc.borrow()).await;

    // the leader's connection dies (a kill -9 of its pod): the other bidder leads within its retry
    let d = lock::leader(&pool);
    let ld = d.is_leader();
    let pid: i32 = pool
        .get()
        .await
        .unwrap()
        .query_one(
            "select pid from pg_locks where locktype = 'advisory' and granted and objsubid = 1
               and ((classid::bigint << 32) | objid::bigint) = hashtext($1)::bigint",
            &[&lock::NAME],
        )
        .await
        .unwrap()
        .get(0);
    pool.get()
        .await
        .unwrap()
        .execute("select pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();
    until("the other bidder", || *ld.borrow()).await;
    assert!(!c.confirm().await);
    c.release().await;
    d.release().await;
}
