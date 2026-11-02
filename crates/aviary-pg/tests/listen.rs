//! The LISTEN helper, against the CI Postgres.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use aviary_pg::{Listener, Pool};

async fn notify(pool: &Pool, channel: &str, payload: &str) {
    pool.get()
        .await
        .unwrap()
        .execute("select pg_notify($1, $2)", &[&channel, &payload])
        .await
        .unwrap();
}

async fn kill_listener(admin: &Pool, app: &str) -> usize {
    admin
        .get()
        .await
        .unwrap()
        .query(
            "select pg_terminate_backend(pid) from pg_stat_activity where application_name = $1 and query ilike 'listen%'",
            &[&app],
        )
        .await
        .unwrap()
        .len()
}

const SOON: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

#[tokio::test]
async fn notifications_wake_the_matching_waiters_only() {
    let url = common::fresh_db("listen").await;
    let pool = common::pool(&url, "listen-a");
    let listener = Listener::start(&pool, &["rows", "changes"]).unwrap();
    listener.ready().await;
    let mut a = listener.waiter("rows", "conv-a");
    let mut b = listener.waiter("rows", "conv-b");
    let mut any = listener.channel_waiter("rows");
    let mut other = listener.channel_waiter("changes");

    notify(&pool, "rows", "conv-a:7").await;
    tokio::time::timeout(SOON, a.changed()).await.expect("a not woken");
    tokio::time::timeout(SOON, any.changed())
        .await
        .expect("channel waiter not woken");
    assert!(
        tokio::time::timeout(QUIET, b.changed()).await.is_err(),
        "b woken by a's key"
    );
    assert!(
        tokio::time::timeout(QUIET, other.changed()).await.is_err(),
        "other channel woken"
    );

    // a payload without ':' is its own key
    let mut h = listener.waiter("changes", "sandbox-1");
    notify(&pool, "changes", "sandbox-1").await;
    tokio::time::timeout(SOON, h.changed())
        .await
        .expect("whole-payload key not woken");
    assert_eq!(listener.notifications(), 2);
}

#[tokio::test]
async fn a_reconnect_wakes_every_waiter_and_notifications_flow_again() {
    let url = common::fresh_db("reconnect").await;
    let admin = common::pool(&url, "admin");
    let pool = common::pool(&url, "listen-b");
    let listener = Listener::start(&pool, &["rows", "live"]).unwrap();
    listener.ready().await;
    let mut waiters: Vec<_> = (0..20)
        .map(|i| listener.waiter(if i % 2 == 0 { "rows" } else { "live" }, &format!("k{i}")))
        .collect();
    waiters.push(listener.channel_waiter("rows"));
    waiters.push(listener.channel_waiter("live"));
    let mut connected = listener.connected();

    assert_eq!(kill_listener(&admin, "listen-b").await, 1);
    tokio::time::timeout(SOON, connected.wait_for(|up| !*up))
        .await
        .unwrap()
        .unwrap();
    for (i, w) in waiters.iter_mut().enumerate() {
        tokio::time::timeout(SOON, w.changed())
            .await
            .unwrap_or_else(|_| panic!("waiter {i} not woken by the reconnect"));
    }
    assert!(*listener.connected().borrow());
    assert_eq!(listener.reconnects(), 1);

    // the new connection listens on every channel
    notify(&pool, "live", "k1:1").await;
    tokio::time::timeout(SOON, waiters[1].changed())
        .await
        .expect("no notification after reconnect");
    assert!(tokio::time::timeout(QUIET, waiters[0].changed()).await.is_err());
}

#[tokio::test]
async fn waiters_are_dropped_with_their_last_holder() {
    let url = common::fresh_db("drop").await;
    let pool = common::pool(&url, "listen-c");
    let listener = Listener::start(&pool, &["rows"]).unwrap();
    listener.ready().await;
    let w1 = listener.waiter("rows", "x");
    let mut w2 = listener.waiter("rows", "x");
    drop(w1);
    notify(&pool, "rows", "x:1").await;
    tokio::time::timeout(SOON, w2.changed())
        .await
        .expect("the remaining waiter lost its slot");
    listener.stop().await;
}

#[test]
fn bad_channel_names_are_refused() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = common::pool("postgres://u@127.0.0.1:1/db", "x");
        for bad in [&["Rows"][..], &["a;b"], &["1a"], &[], &["a", "a"], &["x\""]] {
            assert!(Listener::start(&pool, bad).is_err(), "{bad:?}");
        }
        assert!(Listener::start(&pool, &["magpie_rows", "_x1"]).is_ok());
    });
}
