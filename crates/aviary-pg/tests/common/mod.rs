//! A fresh database per test on the CI Postgres (`AVIARY_TEST_DATABASE_URL`, platform 6.1).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

#[allow(clippy::disallowed_methods)] // test-only configuration (platform 6.1)
pub fn admin_url() -> String {
    std::env::var("AVIARY_TEST_DATABASE_URL")
        .unwrap_or_else(|_| panic!("AVIARY_TEST_DATABASE_URL is not set (ci.yml's test job sets it)"))
}

/// `url` with its database name replaced.
pub fn with_db(url: &str, db: &str) -> String {
    let (before, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let slash = before.rfind('/').expect("url has a database path");
    let mut out = format!("{}/{db}", &before[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

/// Creates an empty database and returns its URL.
pub async fn fresh_db(test: &str) -> String {
    let name = format!("aviary_pg_{test}_{}", uuid::Uuid::new_v4().simple());
    let admin = aviary_pg::connect(&admin_url(), "aviary-pg-test", 1, Duration::from_secs(15)).unwrap();
    admin
        .get()
        .await
        .unwrap()
        .batch_execute(&format!("create database {name}"))
        .await
        .unwrap();
    with_db(&admin_url(), &name)
}

/// A pool on `url` with a test application name.
pub fn pool(url: &str, app: &str) -> aviary_pg::Pool {
    aviary_pg::connect(url, app, 4, Duration::from_secs(15)).unwrap()
}

/// Terminates every backend of the current database holding an advisory lock (the holder's connection "dies").
pub async fn kill_lock_holders(admin: &aviary_pg::Pool) -> u64 {
    let c = admin.get().await.unwrap();
    let rows = c
        .query(
            "select pg_terminate_backend(l.pid) from pg_locks l
             where l.locktype = 'advisory' and l.granted and l.database = (select oid from pg_database where datname = current_database())",
            &[],
        )
        .await
        .unwrap();
    rows.len() as u64
}

/// Waits until `f` is true, up to `limit`.
pub async fn eventually(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}
