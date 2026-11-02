//! Migrations under the advisory lock, against the CI Postgres.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use aviary_pg::{Error, LockKey, Migration};

fn m(file: &str, sql: &'static str) -> Migration {
    Migration::embedded(file, sql).unwrap()
}

/// Neither migration is idempotent: applied twice, the first fails (`create table` exists) or the counter shows 2.
fn list() -> Vec<Migration> {
    vec![
        m(
            "0001_init.sql",
            "create table applied (n int not null);\ninsert into applied values (1);\nselect pg_sleep(0.5);\n",
        ),
        m(
            "0002_more.sql",
            "alter table applied add column note text;\ninsert into applied values (2);\n",
        ),
    ]
}

#[tokio::test]
async fn concurrent_starters_apply_each_migration_once() {
    let url = common::fresh_db("concurrent").await;
    let key = LockKey::Pair(7300, 1);
    let mut joins = Vec::new();
    for i in 0..6 {
        let pool = common::pool(&url, &format!("starter-{i}"));
        let key = key.clone();
        joins.push(tokio::spawn(
            async move { aviary_pg::migrate(&pool, &key, &list()).await },
        ));
    }
    let mut applied_by = Vec::new();
    for j in joins {
        applied_by.push(j.await.unwrap().unwrap());
    }
    // exactly one starter applied both; the others found them recorded
    assert_eq!(
        applied_by.iter().filter(|a| **a == vec![1, 2]).count(),
        1,
        "{applied_by:?}"
    );
    assert_eq!(applied_by.iter().filter(|a| a.is_empty()).count(), 5, "{applied_by:?}");

    let pool = common::pool(&url, "check");
    let c = pool.get().await.unwrap();
    let rows: Vec<i32> = c
        .query("select n from applied order by n", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(rows, vec![1, 2]);
    let recorded = c
        .query("select version, name, sha256 from _migrations order by version", &[])
        .await
        .unwrap();
    let recorded: Vec<(i32, String, String)> = recorded.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
    let expected: Vec<(i32, String, String)> = list().iter().map(|m| (m.version, m.name.clone(), m.sha256())).collect();
    assert_eq!(recorded, expected);
    // the lock is free again
    let free: bool = c
        .query_one("select pg_try_advisory_lock(7300, 1)", &[])
        .await
        .unwrap()
        .get(0);
    assert!(free);
}

#[tokio::test]
async fn an_edited_migration_fails_start() {
    let url = common::fresh_db("edited").await;
    let pool = common::pool(&url, "edited");
    let key = LockKey::name("svc.migrate");
    aviary_pg::migrate(&pool, &key, &list()).await.unwrap();

    let mut edited = list();
    edited[0].sql = "create table applied (n int not null);\ninsert into applied values (1);\n";
    // a later migration is not applied either: verification comes first
    edited.push(m("0003_later.sql", "create table later (x int);"));
    let err = aviary_pg::migrate(&pool, &key, &edited).await.unwrap_err();
    match &err {
        Error::MigrationEdited {
            version,
            name,
            recorded,
            embedded,
        } => {
            assert_eq!((*version, name.as_str()), (1, "init"));
            assert_eq!(*recorded, list()[0].sha256());
            assert_eq!(*embedded, edited[0].sha256());
        }
        other => panic!("{other:?}"),
    }
    let c = pool.get().await.unwrap();
    let later: Option<String> = c
        .query_one("select to_regclass('later')::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(later, None);
}

#[tokio::test]
async fn an_older_build_accepts_newer_recorded_migrations_and_reruns_nothing() {
    let url = common::fresh_db("older").await;
    let pool = common::pool(&url, "older");
    let key = LockKey::Pair(1, 1);
    assert_eq!(aviary_pg::migrate(&pool, &key, &list()).await.unwrap(), vec![1, 2]);
    assert_eq!(
        aviary_pg::migrate(&pool, &key, &list()[..1]).await.unwrap(),
        Vec::<i32>::new()
    );
    assert_eq!(
        aviary_pg::migrate(&pool, &key, &list()).await.unwrap(),
        Vec::<i32>::new()
    );
}

#[tokio::test]
async fn a_failing_migration_is_rolled_back_and_not_recorded() {
    let url = common::fresh_db("failing").await;
    let pool = common::pool(&url, "failing");
    let key = LockKey::Pair(1, 1);
    let bad = vec![m(
        "0001_bad.sql",
        "create table half (x int);\nselect * from missing_table;\n",
    )];
    assert!(matches!(aviary_pg::migrate(&pool, &key, &bad).await, Err(Error::Db(_))));
    let c = pool.get().await.unwrap();
    let half: Option<String> = c
        .query_one("select to_regclass('half')::text", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(half, None);
    let n: i64 = c
        .query_one("select count(*) from _migrations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_no_transaction_migration_creates_an_index_concurrently() {
    let url = common::fresh_db("concurrently").await;
    let key = LockKey::Pair(1, 1);
    let list = vec![
        m("0001_t.sql", "create table t (x int);"),
        m(
            "0002_idx.sql",
            "-- aviary-pg: no-transaction\ncreate index concurrently if not exists t_x on t (x);\n",
        ),
    ];
    // two starters at once: the waiter polls, so the holder's concurrent index build does not wait on it
    let a = common::pool(&url, "a");
    let b = common::pool(&url, "b");
    let (ra, rb) = tokio::join!(aviary_pg::migrate(&a, &key, &list), aviary_pg::migrate(&b, &key, &list));
    let mut all = ra.unwrap();
    all.extend(rb.unwrap());
    all.sort_unstable();
    assert_eq!(all, vec![1, 2]);
    let c = a.get().await.unwrap();
    let valid: bool = c
        .query_one(
            "select indisvalid from pg_index where indexrelid = 't_x'::regclass",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(valid);

    let two = vec![m(
        "0001_x.sql",
        "-- aviary-pg: no-transaction\ncreate index concurrently a on t (x);\ncreate index concurrently b on t (x);\n",
    )];
    assert!(matches!(
        aviary_pg::migrate(&a, &key, &two).await,
        Err(Error::BadMigration(_))
    ));
}

#[test]
fn file_names_and_order_are_checked() {
    assert_eq!(m("0042_add_people.sql", "").version, 42);
    assert_eq!(m("0042_add_people.sql", "").name, "add_people");
    for bad in [
        "0001.sql",
        "init.sql",
        "0000_zero.sql",
        "0001_x.txt",
        "x1_y.sql",
        "_x.sql",
        "0001_.sql",
    ] {
        assert!(Migration::embedded(bad, "").is_err(), "{bad}");
    }
}

#[tokio::test]
async fn versions_out_of_order_are_refused() {
    let url = common::fresh_db("order").await;
    let pool = common::pool(&url, "order");
    let list = vec![m("0002_b.sql", ""), m("0001_a.sql", "")];
    assert!(matches!(
        aviary_pg::migrate(&pool, &LockKey::Pair(1, 1), &list).await,
        Err(Error::BadMigration(_))
    ));
}

#[tokio::test]
async fn the_macro_embeds_files_in_order() {
    let list =
        aviary_pg::migrations!(dir = "tests/fixtures/migrations"; "0001_people.sql", "0002_people_lower.sql").unwrap();
    assert_eq!(list.iter().map(|m| m.version).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(list[0].sql, include_str!("fixtures/migrations/0001_people.sql"));
    let url = common::fresh_db("macro").await;
    let pool = common::pool(&url, "macro");
    assert_eq!(
        aviary_pg::migrate(&pool, &LockKey::Pair(1, 1), &list).await.unwrap(),
        vec![1, 2]
    );
}
