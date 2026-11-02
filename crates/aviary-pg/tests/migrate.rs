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

fn has_index(rows: &[aviary_pg::Row]) -> Option<bool> {
    rows.first().map(|r| r.get(0))
}

const VALIDITY: &str =
    "select i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid where c.relname = 'u_x'";

/// A failed or interrupted `create index concurrently` leaves an INVALID index, which `if not exists` would accept.
#[tokio::test]
async fn an_invalid_index_from_a_failed_concurrent_build_is_dropped_and_rebuilt() {
    let db = common::fresh_db("invalid").await;
    let pool = common::pool(&db, "invalid");
    let key = LockKey::Pair(1, 1);
    let list = vec![
        m("0001_u.sql", "create table u (x int); insert into u values (1), (1);"),
        m(
            "0002_u_x.sql",
            "-- aviary-pg: no-transaction\ncreate unique index concurrently if not exists u_x on u (x); -- one; statement\n",
        ),
    ];
    // the build fails on the duplicates: the start fails, the invalid index is gone, nothing recorded
    let err = aviary_pg::migrate(&pool, &key, &list).await.unwrap_err();
    assert_eq!(
        err.code(),
        Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::UNIQUE_VIOLATION),
        "{err:?}"
    );
    let c = pool.get().await.unwrap();
    assert_eq!(has_index(&c.query(VALIDITY, &[]).await.unwrap()), None);
    let recorded: Vec<i32> = c
        .query("select version from _migrations order by 1", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(recorded, vec![1]);

    // a build interrupted earlier (here: failed outside migrate) left u_x invalid; with the data fixed, the next
    // start drops it and builds it for real instead of accepting it through `if not exists`
    assert!(
        c.batch_execute("create unique index concurrently u_x on u (x)")
            .await
            .is_err()
    );
    assert_eq!(has_index(&c.query(VALIDITY, &[]).await.unwrap()), Some(false));
    c.batch_execute("delete from u where ctid <> (select min(ctid) from u)")
        .await
        .unwrap();
    assert_eq!(aviary_pg::migrate(&pool, &key, &list).await.unwrap(), vec![2]);
    assert_eq!(has_index(&c.query(VALIDITY, &[]).await.unwrap()), Some(true));
}

/// Only the index the migration builds is ever dropped: another invalid index (an operator's failed build, a
/// partitioned table's `on only` index waiting for its partitions) stays.
#[tokio::test]
async fn only_the_migrations_own_invalid_index_is_dropped() {
    let db = common::fresh_db("ownindex").await;
    let pool = common::pool(&db, "ownindex");
    let key = LockKey::Pair(1, 1);
    let c = pool.get().await.unwrap();
    c.batch_execute(
        "create table other (x int); insert into other values (1), (1);
         create table parts (x int) partition by range (x);
         create table parts_1 partition of parts for values from (0) to (10);
         create index parts_x on only parts (x);
         create schema s; create table s.\"W\" (x int); insert into s.\"W\" values (1), (1)",
    )
    .await
    .unwrap();
    assert!(
        c.batch_execute("create unique index concurrently other_x on other (x)")
            .await
            .is_err()
    );
    // the migration's own index, invalid from an earlier failed build, in a schema-qualified quoted table
    assert!(
        c.batch_execute("create unique index concurrently \"W_x\" on s.\"W\" (x)")
            .await
            .is_err()
    );
    c.batch_execute("delete from s.\"W\" where ctid <> (select min(ctid) from s.\"W\")")
        .await
        .unwrap();
    let validity = |name: &'static str| {
        let c = &c;
        async move {
            c.query_opt(
                "select i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid where c.relname = $1",
                &[&name],
            )
            .await
            .unwrap()
            .map(|r| r.get::<_, bool>(0))
        }
    };
    assert_eq!(validity("other_x").await, Some(false));
    assert_eq!(validity("parts_x").await, Some(false));
    assert_eq!(validity("W_x").await, Some(false));
    let list = vec![m(
        "0001_w.sql",
        "-- aviary-pg: no-transaction\ncreate unique index concurrently if not exists \"W_x\" on s.\"W\" (x);\n",
    )];
    assert_eq!(aviary_pg::migrate(&pool, &key, &list).await.unwrap(), vec![1]);
    assert_eq!(
        validity("W_x").await,
        Some(true),
        "the migration's own invalid index was rebuilt"
    );
    assert_eq!(
        validity("other_x").await,
        Some(false),
        "another invalid index was dropped"
    );
    assert_eq!(
        validity("parts_x").await,
        Some(false),
        "a partitioned table's index was dropped"
    );
}

/// A transactional migration that commits or begins by itself would split its SQL from its `_migrations` record.
#[tokio::test]
async fn a_transactional_migration_may_not_control_its_transaction() {
    let db = common::fresh_db("txcontrol").await;
    let pool = common::pool(&db, "txcontrol");
    let key = LockKey::Pair(1, 1);
    for sql in [
        "create table a (x int); commit; create table b (x int);",
        "begin; create table a (x int);",
        "create table a (x int);\nEND;",
    ] {
        let list = vec![m("0001_a.sql", sql)];
        let err = aviary_pg::migrate(&pool, &key, &list).await.unwrap_err();
        assert!(matches!(err, Error::BadMigration(_)), "{sql}: {err:?}");
    }
    // savepoints and the words inside strings or bodies are fine
    let list = vec![m(
        "0001_a.sql",
        "create table a (note text default 'commit;'); savepoint s; create table b (x int); release s;
         create function f() returns int language plpgsql as $$ begin return 1; end $$;",
    )];
    assert_eq!(aviary_pg::migrate(&pool, &key, &list).await.unwrap(), vec![1]);
}

/// An `alter table` waiting behind a long transaction gives up after the lock timeout (so the queries queued
/// behind it run) and is retried until it gets the lock.
#[tokio::test]
async fn a_migration_waiting_for_a_lock_gives_way_and_is_retried() {
    let db = common::fresh_db("locktimeout").await;
    let key = LockKey::Pair(1, 1);
    let pool = common::pool(&db, "starter");
    let first = vec![m("0001_l.sql", "create table l (id int); insert into l values (1);")];
    aviary_pg::migrate(&pool, &key, &first).await.unwrap();

    // a running replica's long transaction holds the table for 6.5 s
    let holder = common::pool(&db, "holder");
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let hold = tokio::spawn(async move {
        aviary_pg::tx!(&holder, |t| {
            t.execute("select count(*) from l", &[]).await?;
            let _ = locked_tx.send(());
            tokio::time::sleep(std::time::Duration::from_millis(6500)).await;
            Ok(())
        })
        .await
    });
    locked_rx.await.unwrap();

    let mut list = first.clone();
    list.push(m("0002_l_note.sql", "alter table l add column note text;"));
    let started = std::time::Instant::now();
    let migrating = tokio::spawn({
        let pool = pool.clone();
        async move { aviary_pg::migrate(&pool, &key, &list).await }
    });
    // a reader arriving while the alter waits is served once the alter gives up, not after the long transaction
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    let reader = common::pool(&db, "reader");
    let n: i64 = reader
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from l", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(6000),
        "the reader waited {:?}",
        started.elapsed()
    );
    assert_eq!(migrating.await.unwrap().unwrap(), vec![2]);
    assert!(started.elapsed() >= std::time::Duration::from_millis(6000));
    hold.await.unwrap().unwrap();
}

#[tokio::test]
async fn no_transaction_statements_are_counted_past_comments_and_strings() {
    let db = common::fresh_db("count").await;
    let pool = common::pool(&db, "count");
    let key = LockKey::Pair(1, 1);
    let list = vec![
        m("0001_t.sql", "create table t (x int, note text);"),
        m(
            "0002_a.sql",
            "-- aviary-pg: no-transaction\n/* builds a; then b? no */\ncreate index concurrently if not exists a on t (x) where note <> ';'; -- trailing; comment\n",
        ),
        m(
            "0003_b.sql",
            "-- aviary-pg: no-transaction\ncreate index concurrently if not exists b on t (note) where note <> $$;$$\n",
        ),
    ];
    assert_eq!(aviary_pg::migrate(&pool, &key, &list).await.unwrap(), vec![1, 2, 3]);
    for two in [
        "-- aviary-pg: no-transaction\ncreate index concurrently c on t (x) /* ; */; create index concurrently d on t (x)\n",
        "-- aviary-pg: no-transaction\ncreate index concurrently c on t (x); -- x\ncreate index concurrently d on t (x);",
    ] {
        let mut bad = list.clone();
        bad.push(m("0004_two.sql", two));
        assert!(
            matches!(aviary_pg::migrate(&pool, &key, &bad).await, Err(Error::BadMigration(_))),
            "{two}"
        );
    }
}
