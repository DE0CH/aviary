//! Conversations (magpie 9, 20): list, create (idempotent), creation lookup, rename, delete, restore and delete for
//! good.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    clippy::too_many_lines,
    clippy::unused_async_trait_impl
)]

mod common;

use magpie_api::changes::{After, ChangeBody, ChangesQuery, RemovedReason};
use magpie_api::conversations::{CreateRequest, ListQuery, ListState, RenameRequest, TitleSource};
use magpie_api::errors::ErrorCode;
use magpie_api::turns::SendRequest;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";

fn empty(key: &str) -> CreateRequest {
    CreateRequest {
        model: None,
        thinking: None,
        idempotency_key: key.to_owned(),
        message: SendRequest {
            client_message_id: format!("{key}:initial-message"),
            text: String::new(),
            images: Vec::new(),
            files: Vec::new(),
        },
    }
}

fn code(e: &magpie_api::client::ClientError) -> ErrorCode {
    e.code().cloned().unwrap_or_else(|| panic!("not an API error: {e:?}"))
}

#[tokio::test]
async fn create_is_idempotent_per_person_and_key() {
    let db = common::fresh_db("create").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let b = common::replica(&db.url, &t).await;

    let first = a
        .pigeon()
        .person(ALICE)
        .create_conversation(&empty("draft-1"))
        .await
        .unwrap();
    assert!(first.is_new());
    let c = first.get().conversation.clone();
    assert_eq!(c.title, None);
    assert_eq!(c.title_source, None);
    assert!(first.get().turn.is_none());

    // a repeat through another replica: 200, the same body
    let again = b
        .pigeon()
        .person(ALICE)
        .create_conversation(&empty("draft-1"))
        .await
        .unwrap();
    assert!(!again.is_new());
    assert_eq!(again.get(), first.get());
    // M4 answers it too
    let found = a.pigeon().person(ALICE).get_creation("draft-1").await.unwrap();
    assert_eq!(&found, first.get());
    // another person's key space is separate
    let other = a
        .pigeon()
        .person(BOB)
        .create_conversation(&empty("draft-1"))
        .await
        .unwrap();
    assert!(other.is_new());
    assert_ne!(other.get().conversation.id, c.id);
    // a key nobody used: 404
    let e = a.pigeon().person(ALICE).get_creation("nope").await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}

#[tokio::test]
async fn concurrent_creates_with_one_key_make_one_conversation() {
    let db = common::fresh_db("create_race").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let b = common::replica(&db.url, &t).await;
    let mut tasks = Vec::new();
    for i in 0..16 {
        let client = if i % 2 == 0 { a.pigeon() } else { b.pigeon() };
        tasks.push(tokio::spawn(async move {
            client.person(ALICE).create_conversation(&empty("race")).await.unwrap()
        }));
    }
    let mut ids = Vec::new();
    let mut new = 0;
    for t in tasks {
        let r = t.await.unwrap();
        if r.is_new() {
            new += 1;
        }
        ids.push(r.get().conversation.id);
    }
    assert_eq!(new, 1, "exactly one create is new");
    ids.dedup();
    assert_eq!(ids.len(), 1, "every create answers the same conversation");
    let list = a
        .pigeon()
        .person(ALICE)
        .list_conversations(&ListQuery::default())
        .await
        .unwrap();
    assert_eq!(list.conversations.len(), 1);
}

#[tokio::test]
async fn a_first_message_with_content_is_admitted_as_the_first_turn() {
    let db = common::fresh_db("create_text").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let mut req = empty("draft-2");
    req.message.text = "幫我整理今日有關港珠澳大橋嘅新聞，越詳細越好".to_owned();
    let r = a.pigeon().person(ALICE).create_conversation(&req).await.unwrap();
    assert!(r.is_new());
    assert!(
        r.get().turn.is_some(),
        "the first turn (magpie 8.2, turns.rs tests the rest)"
    );
}

#[tokio::test]
async fn create_validates_its_request() {
    let db = common::fresh_db("create_bad").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let alice = a.pigeon();
    let alice = alice.person(ALICE);
    let e = alice.create_conversation(&empty("")).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadRequest);
    let mut req = empty("k");
    req.message.text = "x".repeat((1 << 20) + 1);
    assert_eq!(
        code(&alice.create_conversation(&req).await.unwrap_err()),
        ErrorCode::TooLarge
    );
    let mut req = empty("k");
    req.message.images = (0..11)
        .map(|_| magpie_api::turns::ImageRef {
            upload_id: Uuid::new_v4(),
        })
        .collect();
    assert_eq!(
        code(&alice.create_conversation(&req).await.unwrap_err()),
        ErrorCode::TooManyAttachments
    );
}

#[tokio::test]
async fn list_pages_by_activity_and_lists_recently_deleted_with_search() {
    let db = common::fresh_db("list").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let mut ids = Vec::new();
    for i in 0..5 {
        let c = alice.create_conversation(&empty(&format!("k{i}"))).await.unwrap();
        ids.push(c.get().conversation.id);
    }
    // someone else's conversation never shows
    pigeon.person(BOB).create_conversation(&empty("b")).await.unwrap();

    // newest activity first, two per page
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = alice
            .list_conversations(&ListQuery {
                state: Some(ListState::Active),
                limit: Some(2),
                cursor: cursor.clone(),
                search: None,
            })
            .await
            .unwrap();
        assert!(page.conversations.len() <= 2);
        seen.extend(page.conversations.iter().map(|c| c.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let mut expected = ids.clone();
    expected.reverse();
    assert_eq!(seen, expected);

    // rename two and delete them: Recently deleted, newest deletion first, searchable by title
    alice
        .rename_conversation(ids[1], &RenameRequest::title("港珠澳大橋"))
        .await
        .unwrap();
    alice
        .rename_conversation(ids[3], &RenameRequest::title("Q3 report 100%"))
        .await
        .unwrap();
    alice.delete_conversation(ids[1]).await.unwrap();
    alice.delete_conversation(ids[3]).await.unwrap();
    let deleted = alice
        .list_conversations(&ListQuery {
            state: Some(ListState::Deleted),
            ..ListQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        deleted.conversations.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![ids[3], ids[1]]
    );
    assert!(
        deleted
            .conversations
            .iter()
            .all(|c| c.deleted_at.is_some() && c.purge_at.is_some())
    );
    let found = alice
        .list_conversations(&ListQuery {
            state: Some(ListState::Deleted),
            search: Some("大橋".into()),
            ..ListQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        found.conversations.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![ids[1]]
    );
    // `%` is literal
    let pct = alice
        .list_conversations(&ListQuery {
            state: Some(ListState::Deleted),
            search: Some("100%".into()),
            ..ListQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(pct.conversations.iter().map(|c| c.id).collect::<Vec<_>>(), vec![ids[3]]);
    let active = alice.list_conversations(&ListQuery::default()).await.unwrap();
    assert_eq!(active.conversations.len(), 3);

    // a cursor that does not parse, or of the other list
    let e = alice
        .list_conversations(&ListQuery {
            cursor: Some("not a cursor".into()),
            ..ListQuery::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadCursor);
    let page = alice
        .list_conversations(&ListQuery {
            limit: Some(1),
            ..ListQuery::default()
        })
        .await
        .unwrap();
    let e = alice
        .list_conversations(&ListQuery {
            state: Some(ListState::Deleted),
            cursor: page.next_cursor,
            ..ListQuery::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadCursor);

    // details: deleted ones included; someone else's is 404
    assert!(alice.get_conversation(ids[1]).await.unwrap().deleted_at.is_some());
    let e = pigeon.person(BOB).get_conversation(ids[0]).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}

#[tokio::test]
async fn rename_wins_and_is_announced() {
    let db = common::fresh_db("rename").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = alice
        .create_conversation(&empty("k"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    let head = alice
        .changes(&ChangesQuery {
            after: After::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap()
        .head_seq;
    let c = alice
        .rename_conversation(id, &RenameRequest::title("  新名  "))
        .await
        .unwrap();
    assert_eq!(c.title.as_deref(), Some("新名"));
    assert_eq!(c.title_source, Some(TitleSource::User));
    let ch = alice
        .changes(&ChangesQuery {
            after: After::Seq(head),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(ch.changes.len(), 1);
    match &ch.changes[0].body {
        ChangeBody::ConversationUpserted { conversation } => assert_eq!(conversation.title.as_deref(), Some("新名")),
        other => panic!("unexpected change {other:?}"),
    }
    // refusals
    let e = alice
        .rename_conversation(id, &RenameRequest::title("   "))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::EmptyTitle);
    let e = alice
        .rename_conversation(id, &RenameRequest::title("長".repeat(201)))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::TitleTooLong);
    assert!(
        alice
            .rename_conversation(id, &RenameRequest::title("長".repeat(200)))
            .await
            .is_ok()
    );
    let viewing = pigeon.viewing_as(ALICE, "admin@example.com", "s1");
    let e = viewing
        .rename_conversation(id, &RenameRequest::title("x"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ViewAsReadOnly);
    let e = pigeon
        .person(BOB)
        .rename_conversation(id, &RenameRequest::title("x"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}

#[tokio::test]
async fn delete_restore_and_delete_for_good() {
    let db = common::fresh_db("delete").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = alice
        .create_conversation(&empty("k"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;

    // delete for good only from Recently deleted
    let e = alice.purge_conversation(id).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotDeleted);

    let d = alice.delete_conversation(id).await.unwrap();
    let deleted_at = d.deleted_at.expect("deleted");
    // idempotent: the same deletion time
    assert_eq!(
        alice.delete_conversation(id).await.unwrap().deleted_at,
        Some(deleted_at)
    );
    let outbox: Vec<(String, serde_json::Value)> = sql
        .get()
        .await
        .unwrap()
        .query("select kind, payload from outbox order by id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(
        outbox,
        vec![(
            "release_sandbox".to_owned(),
            serde_json::json!({"conversationId": id, "reason": "deleted"})
        )],
        "one release, once"
    );

    let r = alice.restore_conversation(id).await.unwrap();
    assert_eq!(r.deleted_at, None);
    assert_eq!(r.purge_at, None);
    alice.restore_conversation(id).await.unwrap();

    // view-as may not delete
    let e = pigeon
        .viewing_as(ALICE, "admin@example.com", "s")
        .delete_conversation(id)
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ViewAsReadOnly);

    alice.delete_conversation(id).await.unwrap();
    let before = alice
        .changes(&ChangesQuery {
            after: After::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap()
        .head_seq;
    alice.purge_conversation(id).await.unwrap();
    let e = alice.get_conversation(id).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    let ch = alice
        .changes(&ChangesQuery {
            after: After::Seq(before),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(ch.changes.len(), 1);
    assert_eq!(ch.changes[0].conversation_id, id);
    assert_eq!(
        ch.changes[0].body,
        ChangeBody::ConversationRemoved {
            reason: RemovedReason::Purged
        }
    );
    // the earlier upserts of the purged conversation now read as its removal: the feed stays dense
    let all = alice
        .changes(&ChangesQuery {
            after: After::Seq(0),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    let seqs: Vec<u64> = all.changes.iter().map(|c| c.seq).collect();
    assert_eq!(seqs, (1..=all.head_seq).collect::<Vec<_>>());
    let conn = sql.get().await.unwrap();
    let kinds: Vec<String> = conn
        .query("select kind from outbox order by id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(
        kinds,
        vec!["release_sandbox", "release_sandbox", "delete_workspace"],
        "the second delete's release (the restore dropped the first's), then the purge's release and workspace deletion"
    );
    let events: i64 = conn
        .query_one("select count(*) from events where conversation_id = $1", &[&id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(events, 0);
    let purged: i64 = conn
        .query_one("select purged_conversations from persons where person = $1", &[&ALICE])
        .await
        .unwrap()
        .get(0);
    assert_eq!(purged, 1);
    // a purge of what is gone: 404
    let e = alice.purge_conversation(id).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}

#[tokio::test]
async fn the_thirty_day_purge_removes_what_was_deleted_a_month_ago() {
    let db = common::fresh_db("purge_job").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let old = alice
        .create_conversation(&empty("old"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    let recent = alice
        .create_conversation(&empty("recent"))
        .await
        .unwrap()
        .get()
        .conversation
        .id;
    alice.delete_conversation(old).await.unwrap();
    alice.delete_conversation(recent).await.unwrap();
    sql.get()
        .await
        .unwrap()
        .execute(
            "update conversations set deleted_at = now() - interval '30 days 1 minute' where id = $1",
            &[&old],
        )
        .await
        .unwrap();
    let stats = a.hermit().service(None).ops_stats().await.unwrap();
    assert!(stats.oldest_due_purge_at.is_some());
    assert_eq!(magpie::conversations::delete::purge_due(a.pool(), 20).await.unwrap(), 1);
    assert_eq!(
        code(&alice.get_conversation(old).await.unwrap_err()),
        ErrorCode::NotFound
    );
    assert!(alice.get_conversation(recent).await.is_ok());
    assert_eq!(magpie::conversations::delete::purge_due(a.pool(), 20).await.unwrap(), 0);
    let stats = a.hermit().service(None).ops_stats().await.unwrap();
    assert_eq!(stats.oldest_due_purge_at, None);
}
