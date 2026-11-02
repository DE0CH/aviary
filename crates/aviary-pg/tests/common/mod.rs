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

/// A test's own database: its URL (it derefs to it), dropped with the guard (`drop database … with (force)`, which
/// also ends the test's remaining sessions), so a long-lived Postgres does not collect them.
pub struct TestDb {
    url: String,
    name: String,
}

impl std::ops::Deref for TestDb {
    type Target = str;
    fn deref(&self) -> &str {
        &self.url
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        // Drop is sync and the test's runtime may be shutting down: a thread with its own runtime
        let dropped = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                admin()
                    .get()
                    .await?
                    .batch_execute(&format!("drop database if exists {name} with (force)"))
                    .await
            })
        })
        .join();
        if !matches!(dropped, Ok(Ok(()))) {
            eprintln!("could not drop test database {}: {dropped:?}", self.name);
        }
    }
}

fn admin() -> aviary_pg::Pool {
    aviary_pg::connect(&admin_url(), "aviary-pg-test", 1, Duration::from_secs(15)).unwrap()
}

/// Creates an empty database; the guard drops it.
pub async fn fresh_db(test: &str) -> TestDb {
    let name = format!("aviary_pg_{test}_{}", uuid::Uuid::new_v4().simple());
    admin()
        .get()
        .await
        .unwrap()
        .batch_execute(&format!("create database {name}"))
        .await
        .unwrap();
    TestDb {
        url: with_db(&admin_url(), &name),
        name,
    }
}

/// Whether a database of this name exists.
pub async fn database_exists(name: &str) -> bool {
    admin()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) > 0 from pg_database where datname = $1", &[&name])
        .await
        .unwrap()
        .get(0)
}

impl TestDb {
    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }
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
