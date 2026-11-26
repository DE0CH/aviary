//! A primary that stops answering without closing anything (YYC, 2026-11-20 08:28-08:46 UTC: its storage stalled and
//! its client connections went silent; every commit waited on it, past `statement_timeout`, which the server does not
//! apply to a commit's wait). A statement or a commit sent to such a database fails within the pool's deadline
//! (`statement_timeout` plus a margin) instead of waiting for it, and the connection it was sent on is never used
//! again; once the database answers, work goes on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The pools' `statement_timeout` here.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(1);
/// Longer than any deadline the pool may use for it, far shorter than "forever".
const LIMIT: Duration = Duration::from_secs(15);

/// A TCP proxy in front of the test Postgres that can go silent: `stall` stops passing bytes either way on every
/// connection through it (they stay open), `resume` passes them again.
struct Proxy {
    port: u16,
    stalled: Arc<AtomicBool>,
}

impl Proxy {
    async fn start(target: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stalled = Arc::new(AtomicBool::new(false));
        let s = stalled.clone();
        tokio::spawn(async move {
            loop {
                let Ok((inbound, _)) = listener.accept().await else {
                    return;
                };
                let (target, s) = (target.clone(), s.clone());
                tokio::spawn(async move {
                    let Ok(outbound) = TcpStream::connect(&target).await else {
                        return;
                    };
                    let (ri, wi) = inbound.into_split();
                    let (ro, wo) = outbound.into_split();
                    tokio::join!(pump(ri, wo, s.clone()), pump(ro, wi, s));
                });
            }
        });
        Self { port, stalled }
    }

    fn stall(&self) {
        self.stalled.store(true, Ordering::SeqCst);
    }

    fn resume(&self) {
        self.stalled.store(false, Ordering::SeqCst);
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

/// Copies `from` to `to`, holding every byte read while the proxy is stalled until it resumes.
async fn pump(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf, s: Arc<AtomicBool>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let Ok(n) = from.read(&mut buf).await else {
            return;
        };
        if n == 0 {
            let _ = to.shutdown().await;
            return;
        }
        while s.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
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
        .batch_execute("create table t (id int primary key)")
        .await
        .unwrap();
    let proxy = Proxy::start(target(&db)).await;
    let pool = aviary_pg::connect(&proxy.url(&db), "through-proxy", 2, STATEMENT_TIMEOUT).unwrap();
    (db, proxy, pool)
}

/// A statement on a connection whose database stopped answering fails within the deadline, and the next statement,
/// once the database answers again, runs on a connection that works.
#[tokio::test]
async fn a_statement_the_database_never_answers_fails_within_the_deadline() {
    let (_db, proxy, pool) = setup("stall_stmt").await;
    let conn = pool.get().await.unwrap();
    conn.execute("select 1", &[]).await.unwrap();
    proxy.stall();
    let started = Instant::now();
    let out = tokio::time::timeout(LIMIT, conn.execute("insert into t (id) values (1)", &[])).await;
    let waited = started.elapsed();
    let Ok(Err(e)) = out else {
        panic!("the statement must fail within {LIMIT:?}, it gave {out:?} after {waited:?}");
    };
    assert!(matches!(e, aviary_pg::Error::Timeout(_)), "a deadline error, not {e:?}");
    assert!(
        waited >= STATEMENT_TIMEOUT,
        "it gave up before the server could have answered ({waited:?})"
    );
    drop(conn);
    proxy.resume();
    let n: i64 = tokio::time::timeout(LIMIT, async {
        pool.get().await?.query_one("select count(*) from t", &[]).await
    })
    .await
    .expect("the pool works again once the database answers")
    .unwrap()
    .get(0);
    assert!(n <= 1);
}

/// A commit the database never answers fails within the deadline and is not run again (its outcome is unknown), and
/// the pool's next transaction commits once the database answers.
#[tokio::test]
async fn a_commit_the_database_never_answers_fails_within_the_deadline() {
    let (db, proxy, pool) = setup("stall_commit").await;
    let proxy = Arc::new(proxy);
    let p = &proxy;
    let tries = std::sync::atomic::AtomicU32::new(0);
    let tries = &tries;
    let started = Instant::now();
    let out = tokio::time::timeout(
        LIMIT,
        aviary_pg::tx!(&pool, |t| {
            tries.fetch_add(1, Ordering::SeqCst);
            t.execute("insert into t (id) values (1)", &[]).await?;
            p.stall(); // the commit below is never answered
            Ok(())
        }),
    )
    .await;
    let waited = started.elapsed();
    let Ok(Err(e)) = out else {
        panic!("the commit must fail within {LIMIT:?}, it gave {out:?} after {waited:?}");
    };
    assert!(matches!(e, aviary_pg::Error::Timeout(_)), "a deadline error, not {e:?}");
    assert_eq!(
        tries.load(Ordering::SeqCst),
        1,
        "a commit whose outcome is unknown never runs again"
    );
    proxy.resume();
    tokio::time::timeout(
        LIMIT,
        aviary_pg::tx!(&pool, |t| {
            t.execute("insert into t (id) values (2)", &[]).await?;
            Ok(())
        }),
    )
    .await
    .expect("the pool works again once the database answers")
    .unwrap();
    let ids: Vec<i32> = common::pool(&db, "check")
        .get()
        .await
        .unwrap()
        .query("select id from t order by id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(ids.contains(&2), "the later transaction committed: {ids:?}");
}
