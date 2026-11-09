//! A planned primary move (a `CloudNativePG` switchover) as the services see it: every connection cut, new ones
//! refused for a few seconds, then a new primary at the same address. Work caught by it waits and runs again
//! (`aviary_pg::retry`), so callers see a slower answer, never an error.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A TCP proxy in front of the test Postgres that can play the switchover: `cut` ends every connection through it
/// and refuses new ones (the old primary stopping), `restore` lets them through again (the new primary serving).
struct Proxy {
    port: u16,
    down: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Proxy {
    async fn start(target: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let down = Arc::new(AtomicBool::new(false));
        let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let (d, c) = (down.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if d.load(Ordering::SeqCst) {
                    drop(inbound); // refused: the connection ends at once
                    continue;
                }
                let target = target.clone();
                let task = tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(&target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                c.lock().unwrap().push(task);
            }
        });
        Self { port, down, conns }
    }

    fn cut(&self) {
        self.down.store(true, Ordering::SeqCst);
        for t in self.conns.lock().unwrap().drain(..) {
            t.abort();
        }
    }

    fn restore(&self) {
        self.down.store(false, Ordering::SeqCst);
    }

    /// `url` with its host and port replaced by the proxy's.
    fn url(&self, url: &str) -> String {
        let (scheme, rest) = url.split_once("://").unwrap();
        let (auth, after) = rest.rsplit_once('@').map_or(("", rest), |(a, b)| (a, b));
        let path = &after[after.find('/').unwrap()..];
        let at = if auth.is_empty() {
            String::new()
        } else {
            format!("{auth}@")
        };
        format!("{scheme}://{at}127.0.0.1:{}{path}", self.port)
    }
}

/// host:port of `url`.
fn target(url: &str) -> String {
    let rest = url.split_once("://").unwrap().1;
    let after = rest.rsplit_once('@').map_or(rest, |(_, b)| b);
    let hostport = &after[..after.find('/').unwrap()];
    if hostport.contains(':') {
        hostport.to_owned()
    } else {
        format!("{hostport}:5432")
    }
}

async fn setup(test: &str) -> (common::TestDb, Proxy, aviary_pg::Pool) {
    let db = common::fresh_db(test).await;
    common::pool(&db, "setup")
        .get()
        .await
        .unwrap()
        .batch_execute("create table t (id int primary key, n int not null default 0)")
        .await
        .unwrap();
    let proxy = Proxy::start(target(&db)).await;
    let pool = common::pool(&proxy.url(&db), "through-proxy");
    (db, proxy, pool)
}

async fn count(db: &str) -> i64 {
    common::pool(db, "check")
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from t", &[])
        .await
        .unwrap()
        .get(0)
}

/// A transaction started while the database is away waits for it, and commits once.
#[tokio::test]
async fn a_transaction_during_the_gap_waits_and_commits_once() {
    let (db, proxy, pool) = setup("gap_tx").await;
    pool.get().await.unwrap().execute("select 1", &[]).await.unwrap(); // a pooled connection, cut below
    proxy.cut();
    let restore = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        proxy.restore();
        proxy
    });
    let started = Instant::now();
    aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t (id) values (1)", &[]).await?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "it ran before the database was back"
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "it waited long after the database was back"
    );
    let _proxy = restore.await.unwrap();
    assert_eq!(count(&db).await, 1);
}

/// A transaction whose connection is cut in the middle runs again from the start, and its work lands once.
#[tokio::test]
async fn a_transaction_cut_in_the_middle_runs_again_once() {
    let (db, proxy, pool) = setup("cut_tx").await;
    let proxy = Arc::new(proxy);
    let tries = AtomicU32::new(0);
    let tries = &tries;
    let p = &proxy;
    aviary_pg::tx!(&pool, |t| {
        let n = tries.fetch_add(1, Ordering::SeqCst);
        t.execute("insert into t (id) values (1)", &[]).await?;
        if n == 0 {
            p.cut(); // the old primary stops: this transaction never commits
            let p2 = Arc::clone(p);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                p2.restore();
            });
        }
        t.execute("update t set n = n + 1 where id = 1", &[]).await?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(tries.load(Ordering::SeqCst), 2);
    let n: i32 = common::pool(&db, "check")
        .get()
        .await
        .unwrap()
        .query_one("select n from t where id = 1", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1, "the cut try's update must not count");
}

/// A statement outside a transaction on a connection the server ended runs again on a new connection.
#[tokio::test]
async fn a_statement_on_a_cut_connection_runs_again() {
    let (db, proxy, pool) = setup("cut_stmt").await;
    let conn = pool.get().await.unwrap();
    conn.execute("select 1", &[]).await.unwrap();
    proxy.cut();
    let restore = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        proxy.restore();
        proxy
    });
    conn.execute("insert into t (id) values (7)", &[]).await.unwrap();
    let _proxy = restore.await.unwrap();
    assert_eq!(count(&db).await, 1);
}

/// The server ending a pooled connection (`pg_terminate_backend`, 57P01 as a switchover's fast shutdown sends) is
/// not an error either.
#[tokio::test]
async fn a_terminated_backend_is_replaced() {
    let (db, _proxy, pool) = setup("terminated").await;
    let conn = pool.get().await.unwrap();
    let pid: i32 = conn.query_one("select pg_backend_pid()", &[]).await.unwrap().get(0);
    common::pool(&db, "killer")
        .get()
        .await
        .unwrap()
        .execute("select pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let other: i32 = conn.query_one("select pg_backend_pid()", &[]).await.unwrap().get(0);
    assert_ne!(other, pid);
}

/// A real error is not retried: the caller gets it at once.
#[tokio::test]
async fn a_real_error_is_not_retried() {
    let (_db, _proxy, pool) = setup("real_error").await;
    let started = Instant::now();
    let tries = AtomicU32::new(0);
    let tries = &tries;
    let r = aviary_pg::tx!(&pool, |t| {
        tries.fetch_add(1, Ordering::SeqCst);
        t.execute("insert into t (id) values (1), (1)", &[]).await?;
        Ok(())
    })
    .await;
    assert!(r.is_err());
    assert_eq!(tries.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
}
