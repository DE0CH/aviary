//! The change feed (M12, magpie 7.6) and live fan-out across replicas (magpie 14): dense head, `latest`, 410 after
//! retention, joining from M1 without loss, a write on one replica waking a long-poll on another within 50 ms, a
//! dropped LISTEN connection losing nothing, and held polls answering at once on drain.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    clippy::too_many_lines,
    clippy::unused_async_trait_impl
)]

mod common;

use std::time::{Duration, Instant};

use magpie_api::changes::{After, ChangeBody, ChangesQuery};
use magpie_api::conversations::{CreateRequest, ListQuery, RenameRequest};
use magpie_api::errors::ErrorCode;
use magpie_api::turns::SendRequest;

const ALICE: &str = "alice@example.com";

fn empty(key: &str) -> CreateRequest {
    CreateRequest {
        idempotency_key: key.to_owned(),
        message: SendRequest {
            client_message_id: format!("{key}:m"),
            text: String::new(),
            images: Vec::new(),
            files: Vec::new(),
        },
    }
}

fn q(after: After, wait_ms: Option<u64>) -> ChangesQuery {
    ChangesQuery {
        after,
        limit: None,
        wait_ms,
    }
}

#[tokio::test]
async fn the_feed_is_dense_answers_latest_at_once_and_pages() {
    let db = common::fresh_db("feed").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    // a person seen first on the feed gets a row and a stable generation
    let first = alice.changes(&q(After::Latest, None)).await.unwrap();
    assert_eq!(first.head_seq, 0);
    assert!(first.changes.is_empty());
    for i in 0..7 {
        let id = alice
            .create_conversation(&empty(&format!("k{i}")))
            .await
            .unwrap()
            .get()
            .conversation
            .id;
        alice
            .rename_conversation(id, &RenameRequest { title: format!("t{i}") })
            .await
            .unwrap();
    }
    let started = Instant::now();
    let latest = alice.changes(&q(After::Latest, Some(25_000))).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "latest answers at once");
    assert_eq!(latest.head_seq, 14);
    assert_eq!(latest.generation, first.generation, "the generation never changes");
    assert!(latest.changes.is_empty());
    // pages of 5: every seq once, in order
    let mut seqs = Vec::new();
    let mut after = 0;
    loop {
        let page = alice
            .changes(&ChangesQuery {
                after: After::Seq(after),
                limit: Some(5),
                wait_ms: None,
            })
            .await
            .unwrap();
        if page.changes.is_empty() {
            break;
        }
        assert!(page.changes.len() <= 5);
        seqs.extend(page.changes.iter().map(|c| c.seq));
        after = page.changes.last().unwrap().seq;
        assert_eq!(page.head_seq, 14);
    }
    assert_eq!(seqs, (1..=14).collect::<Vec<_>>());
    // limits
    for bad in [0, 501] {
        let e = alice
            .changes(&ChangesQuery {
                after: After::Seq(0),
                limit: Some(bad),
                wait_ms: None,
            })
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
    }
    // at the head with no wait: an empty answer at once
    let at_head = alice.changes(&q(After::Seq(14), None)).await.unwrap();
    assert!(at_head.changes.is_empty());
    assert_eq!(at_head.head_seq, 14);
}

#[tokio::test]
async fn a_cursor_older_than_retention_answers_410() {
    let db = common::fresh_db("feed410").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    for i in 0..4 {
        alice.create_conversation(&empty(&format!("k{i}"))).await.unwrap();
    }
    // the first three changes are older than 30 days; retention deletes them
    sql.get()
        .await
        .unwrap()
        .execute(
            "update changes set at = now() - interval '31 days' where person = $1 and seq <= 3",
            &[&ALICE],
        )
        .await
        .unwrap();
    assert_eq!(magpie::jobs::retention::pass(a.pool()).await.unwrap(), 3);
    // seq 3's successor (4) is kept: after=3 still works; after=2 lost seq 3
    let ok = alice.changes(&q(After::Seq(3), None)).await.unwrap();
    assert_eq!(ok.changes.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![4]);
    for after in [0, 2] {
        let e = alice.changes(&q(After::Seq(after), None)).await.unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::CursorExpired), "after={after}");
        assert_eq!(e.api().unwrap().status, 410);
    }
    // a position past the head was never this feed's
    let e = alice.changes(&q(After::Seq(99), None)).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::CursorExpired));
}

/// Retention deletes in batches until nothing is due: a backlog several batches long (a leader that never ran, the
/// import's old rows) is cleared in one pass, and what is still kept stays.
#[tokio::test]
async fn retention_clears_a_backlog_larger_than_a_batch() {
    let db = common::fresh_db("retbatch").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    alice.create_conversation(&empty("kept")).await.unwrap();
    let old = 2 * magpie::limits::RETENTION_BATCH + 17;
    sql.get()
        .await
        .unwrap()
        .execute(
            "insert into changes (person, seq, at, type, conversation_id, data)
               select $1, -g, now() - interval '31 days', 'conversation_upserted', gen_random_uuid(), '{}'
               from generate_series(1::bigint, $2::bigint) g",
            &[&ALICE, &old],
        )
        .await
        .unwrap();
    sql.get()
        .await
        .unwrap()
        .execute(
            "insert into cc_ops (scope, op_id, result, at)
               select gen_random_uuid(), gen_random_uuid(), '{}', now() - interval '2 days'
               from generate_series(1::bigint, $1::bigint) g",
            &[&old],
        )
        .await
        .unwrap();
    let deleted = magpie::jobs::retention::pass(a.pool()).await.unwrap();
    assert_eq!(deleted, u64::try_from(2 * old).unwrap());
    let left: i64 = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select (select count(*) from changes) + (select count(*) from cc_ops)",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 1, "only the recent change is kept");
    assert_eq!(magpie::jobs::retention::pass(a.pool()).await.unwrap(), 0);
}

#[tokio::test]
async fn joining_from_the_list_loses_nothing() {
    let db = common::fresh_db("join").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let b = common::replica(&db.url, &t).await;
    // a writer on B keeps creating while a reader on A lists and tails
    let writer = {
        let client = b.pigeon();
        tokio::spawn(async move {
            let mut ids = Vec::new();
            for i in 0..40 {
                let r = client
                    .person(ALICE)
                    .create_conversation(&empty(&format!("w{i}")))
                    .await
                    .unwrap();
                ids.push(r.get().conversation.id);
            }
            ids
        })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let list = alice.list_conversations(&ListQuery::default()).await.unwrap();
    let mut known: std::collections::HashSet<_> = list.conversations.iter().map(|c| c.id).collect();
    let ids = writer.await.unwrap();
    let mut after = list.list.head_seq;
    loop {
        let page = alice.changes(&q(After::Seq(after), None)).await.unwrap();
        if page.changes.is_empty() {
            break;
        }
        for c in &page.changes {
            if let ChangeBody::ConversationUpserted { conversation } = &c.body {
                known.insert(conversation.id);
            }
        }
        after = page.changes.last().unwrap().seq;
    }
    for id in ids {
        assert!(
            known.contains(&id),
            "{id} was in neither the list nor the feed after it"
        );
    }
}

/// The poller's replica re-queries a held poll only every minute, so every answer below came from a notification;
/// each wake-up is timed from the commit, as the test's own LISTEN connection on the change channel sees it (the
/// notification is sent at commit; the poller's replica gets it at the same moment and must still query and answer).
#[tokio::test]
async fn a_write_on_one_replica_wakes_a_long_poll_on_the_other_within_50_ms() {
    let db = common::fresh_db("fanout").await;
    let t = common::Tokens::new();
    let a = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::no_requery()).await;
    let b = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let commits = aviary_pg::Listener::start(&sql, &[magpie::fanout::CHANGES]).unwrap();
    commits.ready().await;
    let poller = a.pigeon();
    let writer = b.pigeon();
    // the writer's first request makes its person row and warms its connections: its own cost is not the fan-out's
    writer.person(ALICE).create_conversation(&empty("warm")).await.unwrap();
    let head = poller
        .person(ALICE)
        .changes(&q(After::Latest, None))
        .await
        .unwrap()
        .head_seq;
    let mut after = head;
    let mut lags = Vec::new();
    let mut from_write = Vec::new();
    for i in 0..7 {
        let poll = {
            let c = poller.clone();
            tokio::spawn(async move {
                let r = c
                    .person(ALICE)
                    .changes(&q(After::Seq(after), Some(10_000)))
                    .await
                    .unwrap();
                (Instant::now(), r)
            })
        };
        let committed = {
            let mut w = commits.channel_waiter(magpie::fanout::CHANGES);
            tokio::spawn(async move {
                w.changed().await;
                Instant::now()
            })
        };
        // the poll is held before the write
        tokio::time::sleep(Duration::from_millis(150)).await;
        let write_started = Instant::now();
        writer
            .person(ALICE)
            .create_conversation(&empty(&format!("f{i}")))
            .await
            .unwrap();
        let committed = committed.await.unwrap();
        let (woke, r) = poll.await.unwrap();
        assert_eq!(r.changes.len(), 1);
        after = r.head_seq;
        // from before the write was sent: the write's own request and commit, Postgres's notify queue, B's LISTEN
        // and the poll's answer all count (an upper bound on the lag from the commit)
        from_write.push(woke.duration_since(write_started));
        // a poll answered before the test's listener saw the commit woke within no time of it
        lags.push(woke.checked_duration_since(committed).unwrap_or(Duration::ZERO));
    }
    commits.stop().await;
    assert!(
        from_write.iter().all(|l| *l < Duration::from_millis(50)),
        "every wake-up within 50 ms of the write: {from_write:?}"
    );
    assert!(
        lags.iter().all(|l| *l < Duration::from_millis(50)),
        "every wake-up within 50 ms of the commit as a listener sees it: {lags:?}"
    );
}

async fn kill_listeners(sql: &aviary_pg::Pool) -> usize {
    sql.get()
        .await
        .unwrap()
        .query(
            "select pg_terminate_backend(pid) from pg_stat_activity
             where datname = current_database() and application_name = 'magpie' and query ilike 'listen%'",
            &[],
        )
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_dropped_listen_connection_loses_nothing() {
    let db = common::fresh_db("relisten").await;
    let t = common::Tokens::new();
    // held polls re-query only every minute: the write made while LISTEN is down can reach the poll only through
    // the wake-up of every waiter at the reconnect
    let a = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::no_requery()).await;
    let b = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let poller = a.pigeon();
    let head = poller
        .person(ALICE)
        .changes(&q(After::Latest, None))
        .await
        .unwrap()
        .head_seq;
    let poll = {
        let c = poller.clone();
        tokio::spawn(async move {
            c.person(ALICE)
                .changes(&q(After::Seq(head), Some(20_000)))
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    // every LISTEN connection dies; the write lands while they are down
    assert!(kill_listeners(&sql).await >= 2);
    b.pigeon()
        .person(ALICE)
        .create_conversation(&empty("gap"))
        .await
        .unwrap();
    let started = Instant::now();
    let r = tokio::time::timeout(Duration::from_secs(5), poll)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.changes.len(), 1, "the write made while LISTEN was down is delivered");
    // the reconnect backoff starts at 0.5 s; the re-query would come only after a minute
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(a.app().fanout.reconnects() >= 1, "delivered by the reconnect");
    // after the reconnect, wake-ups work again
    assert!(
        common::eventually(Duration::from_secs(10), async || a.app().fanout.reconnects() >= 1).await,
        "the listener reconnected"
    );
    let head = r.head_seq;
    let poll = {
        let c = poller.clone();
        tokio::spawn(async move {
            c.person(ALICE)
                .changes(&q(After::Seq(head), Some(20_000)))
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    b.pigeon()
        .person(ALICE)
        .create_conversation(&empty("after"))
        .await
        .unwrap();
    let r = tokio::time::timeout(Duration::from_secs(3), poll)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.changes.len(), 1);
}

#[tokio::test]
async fn held_polls_answer_at_once_when_the_replica_drains() {
    let db = common::fresh_db("drainpoll").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let head = pigeon
        .person(ALICE)
        .changes(&q(After::Latest, None))
        .await
        .unwrap()
        .head_seq;
    let poll = {
        let c = pigeon.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let r = c
                .person(ALICE)
                .changes(&q(After::Seq(head), Some(25_000)))
                .await
                .unwrap();
            (started.elapsed(), r)
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.app().fanout.held(), 1);
    a.start_drain();
    let (held_for, r) = tokio::time::timeout(Duration::from_secs(3), poll)
        .await
        .unwrap()
        .unwrap();
    assert!(r.changes.is_empty());
    assert_eq!(r.head_seq, head, "the empty answer resumes from the same seq elsewhere");
    assert!(held_for < Duration::from_secs(2), "held {held_for:?}");
    assert!(!a.readiness.is_ready());
}
