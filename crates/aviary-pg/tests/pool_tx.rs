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
        err.code().cloned(),
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

async fn ids(pool: &aviary_pg::Pool, table: &str) -> Vec<i32> {
    pool.get()
        .await
        .unwrap()
        .query(&format!("select id from {table} order by id"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

/// Postgres answers COMMIT of an aborted transaction with ROLLBACK and no error; tx! must not report a commit.
#[tokio::test]
async fn a_swallowed_statement_error_makes_tx_fail_and_nothing_is_committed() {
    let db = common::fresh_db("swallowed").await;
    let pool = aviary_pg::connect(&db, "swallowed", 1, Duration::from_secs(15)).unwrap();
    pool.get()
        .await
        .unwrap()
        .batch_execute("create table t (id int primary key); insert into t values (1)")
        .await
        .unwrap();

    // a unique violation ignored, then Ok
    let out = aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values (2)", &[]).await?;
        let _ = t.execute("insert into t values (1)", &[]).await;
        Ok(())
    })
    .await;
    assert!(matches!(out, Err(Error::RolledBack)), "{out:?}");
    assert_eq!(ids(&pool, "t").await, vec![1], "the earlier insert was committed");

    // `.is_err()` used as "already exists"
    let out = aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values (3)", &[]).await?;
        if t.execute("insert into t values (1)", &[]).await.is_err() {
            return Ok("exists");
        }
        Ok("inserted")
    })
    .await;
    assert!(matches!(out, Err(Error::RolledBack)), "{out:?}");
    assert_eq!(ids(&pool, "t").await, vec![1]);

    // a failure the closure recovered from with a savepoint commits
    aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values (4)", &[]).await?;
        t.batch_execute("savepoint s").await?;
        let _ = t.execute("insert into t values (1)", &[]).await;
        t.batch_execute("rollback to savepoint s").await?;
        t.execute("insert into t values (5)", &[]).await?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(ids(&pool, "t").await, vec![1, 4, 5]);

    // a client-side error (row count) aborts nothing: the commit goes ahead
    aviary_pg::tx!(&pool, |t| {
        t.execute("insert into t values (6)", &[]).await?;
        assert!(t.query_one("select 1 where false", &[]).await.is_err());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(ids(&pool, "t").await, vec![1, 4, 5, 6]);
}

#[tokio::test]
async fn transaction_control_is_refused_outside_tx_and_inside_it() {
    let db = common::fresh_db("control").await;
    let pool = aviary_pg::connect(&db, "control", 1, Duration::from_secs(15)).unwrap();
    let c = pool.get().await.unwrap();
    for sql in [
        "begin",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "start transaction",
        "/* x */ commit",
        "savepoint a",
        "rollback",
    ] {
        let e = c.execute(sql, &[]).await.unwrap_err();
        assert!(matches!(e, Error::TransactionControl(_)), "{sql}: {e:?}");
    }
    for sql in ["select 1; begin", "create table x (id int); commit"] {
        let e = c.batch_execute(sql).await.unwrap_err();
        assert!(matches!(e, Error::TransactionControl(_)), "{sql}: {e:?}");
    }
    // nothing of a refused batch ran
    let x: Option<String> = c.query_one("select to_regclass('x')::text", &[]).await.unwrap().get(0);
    assert_eq!(x, None);
    drop(c);

    let out = aviary_pg::tx!(&pool, |t| t.batch_execute("commit; begin").await).await;
    assert!(matches!(out, Err(Error::TransactionControl(_))), "{out:?}");
    let out = aviary_pg::tx!(&pool, |t| t.execute("end", &[]).await).await;
    assert!(matches!(out, Err(Error::TransactionControl(_))), "{out:?}");
    // savepoints are fine inside
    aviary_pg::tx!(&pool, |t| {
        t.execute("savepoint a", &[]).await?;
        t.execute("release savepoint a", &[]).await
    })
    .await
    .unwrap();
}

/// A replica's cached `select *` meets a column that another replica's migration added.
#[tokio::test]
async fn a_stale_cached_statement_recovers() {
    let db = common::fresh_db("stale").await;
    // one connection, so every statement below reuses its cache
    let a = aviary_pg::connect(&db, "old-replica", 1, Duration::from_secs(15)).unwrap();
    let b = common::pool(&db, "new-replica");
    let migrate = |sql: &'static str| {
        let b = b.clone();
        async move { b.get().await.unwrap().batch_execute(sql).await.unwrap() }
    };
    migrate("create table s (id int); insert into s values (1)").await;
    let cols = |rows: Vec<aviary_pg::Row>| rows[0].len();

    let rows = aviary_pg::tx!(&a, |t| t.query("select * from s", &[]).await)
        .await
        .unwrap();
    assert_eq!(cols(rows), 1);
    migrate("alter table s add column note text").await;
    // the transaction's first statement: restarted and retried
    let rows = aviary_pg::tx!(&a, |t| t.query("select * from s", &[]).await)
        .await
        .unwrap();
    assert_eq!(cols(rows), 2);

    // later in a transaction it cannot be retried (the transaction is aborted): an error, and the next one works
    migrate("alter table s add column more int").await;
    let out = aviary_pg::tx!(&a, |t| {
        t.execute("insert into s (id) values (2)", &[]).await?;
        t.query("select * from s", &[]).await
    })
    .await;
    assert_eq!(
        out.as_ref().map(|_| ()).unwrap_err().code(),
        Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::FEATURE_NOT_SUPPORTED),
        "{out:?}"
    );
    let rows = aviary_pg::tx!(&a, |t| {
        t.execute("insert into s (id) values (3)", &[]).await?;
        t.query("select * from s", &[]).await
    })
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "ids 1 and 3");
    assert_eq!(cols(rows), 3);

    // outside a transaction: retried
    let c = a.get().await.unwrap();
    assert_eq!(c.query("select * from s", &[]).await.unwrap()[0].len(), 3);
    migrate("alter table s add column last int").await;
    assert_eq!(c.query("select * from s", &[]).await.unwrap()[0].len(), 4);
    drop(c);

    // the server lost the prepared statements (`prepared statement … does not exist`)
    a.get().await.unwrap().batch_execute("deallocate all").await.unwrap();
    let rows = aviary_pg::tx!(&a, |t| t.query("select * from s", &[]).await)
        .await
        .unwrap();
    assert_eq!(cols(rows), 4);
    a.get().await.unwrap().batch_execute("deallocate all").await.unwrap();
    assert_eq!(
        a.get().await.unwrap().query("select * from s", &[]).await.unwrap()[0].len(),
        4
    );
}

#[tokio::test]
async fn a_test_database_is_dropped_with_its_guard() {
    let db = common::fresh_db("guard").await;
    let name = db.name().to_owned();
    let pool = common::pool(&db, "guard");
    pool.get().await.unwrap().execute("select 1", &[]).await.unwrap();
    assert!(common::database_exists(&name).await);
    // an open session does not stop the drop
    drop(db);
    assert!(!common::database_exists(&name).await);
}
