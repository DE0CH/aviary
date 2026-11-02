//! The pool's connection settings and transactions, against the CI Postgres.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use aviary_pg::Error;

#[tokio::test]
async fn connections_are_tls_with_the_service_settings() {
    let url = common::fresh_db("settings").await;
    // whatever the URL says, TLS is required
    let url = url.replace("sslmode=require", "sslmode=disable");
    let pool = aviary_pg::connect(&url, "svc:pod-1", 2, Duration::from_millis(1500)).unwrap();
    let c = pool.get().await.unwrap();
    let row = c
        .query_one(
            "select s.ssl, a.application_name, current_setting('statement_timeout'),
                    current_setting('idle_in_transaction_session_timeout')
             from pg_stat_ssl s join pg_stat_activity a using (pid) where pid = pg_backend_pid()",
            &[],
        )
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0), "not encrypted");
    assert_eq!(row.get::<_, String>(1), "svc:pod-1");
    assert_eq!(row.get::<_, String>(2), "1500ms");
    assert_eq!(row.get::<_, String>(3), "1min");
    // the statement timeout applies
    let err = c.batch_execute("select pg_sleep(3)").await.unwrap_err();
    assert_eq!(
        err.as_db_error().map(|e| e.code().clone()),
        Some(aviary_pg::reexports::tokio_postgres::error::SqlState::QUERY_CANCELED),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_bad_url_is_a_config_error_without_the_url() {
    let err = aviary_pg::connect("postgres://u:s3cret@host:notaport/db", "x", 1, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err:?}");
    assert!(!err.to_string().contains("s3cret"), "{err}");
    assert!(format!("{err:?}").find("s3cret").is_none());
}

/// A caller's own error type.
#[derive(Debug)]
enum Mine {
    Db,
    Refused,
}

impl From<Error> for Mine {
    fn from(_: Error) -> Self {
        Mine::Db
    }
}

#[tokio::test]
async fn tx_commits_on_ok_and_rolls_back_on_err() {
    let url = common::fresh_db("tx").await;
    let pool = common::pool(&url, "tx-test");
    pool.get()
        .await
        .unwrap()
        .batch_execute("create table t (id int primary key)")
        .await
        .unwrap();

    let n: i64 = aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values ($1)", &[&1_i32]).await?;
        let row = t.query_one("select count(*) from t", &[]).await?;
        Ok(row.get(0))
    })
    .await
    .unwrap();
    assert_eq!(n, 1);

    let out = aviary_pg::tx!(&pool, |t| -> Result<(), Mine> {
        t.execute("insert into t values ($1)", &[&2_i32]).await?;
        Err(Mine::Refused)
    })
    .await;
    assert!(matches!(out, Err(Mine::Refused)));

    // a failing statement is the caller's error and rolls back too
    let dup = aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values ($1)", &[&3_i32]).await?;
        t.execute("insert into t values ($1)", &[&1_i32]).await
    })
    .await;
    assert!(matches!(dup, Err(Error::Db(_))), "{dup:?}");

    let ids: Vec<i32> = aviary_pg::tx!(&pool, |t| {
        Ok(t.query("select id from t order by id", &[])
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect())
    })
    .await
    .unwrap();
    assert_eq!(ids, vec![1]);
}

#[tokio::test]
async fn tx_notify_is_delivered_on_commit_only() {
    let url = common::fresh_db("notify").await;
    let pool = common::pool(&url, "notify-test");
    let listener = aviary_pg::Listener::start(&pool, &["things"]).unwrap();
    listener.ready().await;
    let mut w = listener.waiter("things", "a");

    let _ = aviary_pg::tx!(&pool, |t| -> Result<(), Error> {
        t.notify("things", "a:1").await?;
        Err(Error::Closed)
    })
    .await;
    let woke = tokio::time::timeout(Duration::from_millis(500), w.changed()).await;
    assert!(woke.is_err(), "a rolled-back notify was delivered");

    aviary_pg::tx!(&pool, |t| t.notify("things", "a:2").await)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), w.changed())
        .await
        .expect("committed notify not delivered");
}

#[tokio::test]
async fn a_cancelled_tx_never_returns_its_connection_inside_the_transaction() {
    let url = common::fresh_db("cancel").await;
    let pool = aviary_pg::connect(&url, "cancel-test", 1, Duration::from_secs(15)).unwrap();
    pool.get()
        .await
        .unwrap()
        .batch_execute("create table t (id int)")
        .await
        .unwrap();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        aviary_pg::tx!(&pool, |t| {
            t.execute("insert into t values (1)", &[]).await?;
            std::future::pending::<()>().await;
            Ok(())
        }),
    )
    .await;
    assert!(cancelled.is_err());
    // the one pool slot is free again, on a connection outside any transaction, and the insert is gone
    let (n, in_tx): (i64, bool) = aviary_pg::tx!(&pool, |t| {
        let n = t.query_one("select count(*) from t", &[]).await?.get(0);
        let row = t
            .query_one("select count(*) > 0 from pg_stat_activity where datname = current_database() and state = 'idle in transaction'", &[])
            .await?;
        Ok((n, row.get(0)))
    })
    .await
    .unwrap();
    assert_eq!(n, 0);
    assert!(!in_tx, "a session was left idle in transaction");
}
