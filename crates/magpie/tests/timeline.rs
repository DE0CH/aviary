//! The timeline (magpie 7.3-7.4): every projection rule from events to rows, the epoch (changed only by rule 9),
//! `tail`/`before`/`after` pages with `hasOlder`/`hasNewer`, the long-poll (within 50 ms of a commit on another
//! replica, no connection held while waiting, answered at once on drain), the E6-style cost test, prompts and
//! search.

#![allow(
    clippy::assert_is_empty,
    clippy::items_after_statements,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::assigning_clones,
    clippy::collapsible_if,
    clippy::many_single_char_names,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::unused_async_trait_impl
)]

mod common;

use std::time::{Duration, Instant};

use common::ev::{self, ALICE};
use magpie_api::changes::{After, ChangeBody, ChangesQuery};
use magpie_api::errors::ErrorCode;
use magpie_api::rows::{Row, RowItem, SearchQuery, TimelinePage, ToolStatus};
use serde_json::{Value, json};
use uuid::Uuid;

fn items(p: &TimelinePage) -> Vec<Value> {
    p.rows
        .iter()
        .map(|r| {
            let mut v = serde_json::to_value(&r.item).unwrap();
            v["@seq"] = json!([r.seq_start, r.seq_end]);
            v
        })
        .collect()
}

fn tool(r: &Row) -> &magpie_api::rows::ToolItem {
    match &r.item {
        RowItem::Tool(t) => t,
        other => panic!("not a tool row: {other:?}"),
    }
}

async fn epoch_and_head(sql: &aviary_pg::Pool, id: Uuid) -> (i64, i64) {
    let r = sql
        .get()
        .await
        .unwrap()
        .query_one("select epoch, row_head from conversations where id = $1", &[&id])
        .await
        .unwrap();
    (r.get(0), r.get(1))
}

#[tokio::test]
async fn rules_1_to_8_make_and_update_rows() {
    let db = common::fresh_db("rules").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let turn = Uuid::new_v4();
    // rule 10: no rows, no row seq
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::turn_started(turn),
            ev::ev("sandbox.preparing", json!({})),
            ev::ev("sandbox.ready", json!({})),
            ev::ev("title.changed", json!({"title": "t", "nameSource": null})),
        ],
    )
    .await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    assert!(p.rows.is_empty());
    assert_eq!((p.max_seq, p.min_seq, p.epoch.as_str()), (0, 0, "1"));

    // rule 1: user.sent (with an image, a file and a viewer)
    let mut sent = ev::ev(
        "user.sent",
        json!({"clientMessageId": "c1", "text": "hello", "files": [{"uploadId": Uuid::new_v4(), "name": "a.pdf",
               "mimeType": "application/pdf", "size": 5, "path": "uploads/a.pdf",
               "blob": {"$blob": {"blobId": "f1", "bytes": 5, "kind": "file"}}}],
               "images": [{"uploadId": Uuid::new_v4(), "mimeType": "image/png", "size": 9, "width": 4, "height": 3,
               "blob": {"$blob": {"blobId": "i1", "bytes": 9, "kind": "media"}},
               "modelBlob": {"$blob": {"blobId": "i2", "bytes": 9, "kind": "media"}}}]}),
    );
    sent.viewer = Some("admin@example.com".into());
    ev::append1(pool, ALICE, id, ev::in_turn(sent, turn)).await;
    // rule 4: blocks of one message over three lines (one line per block): indexes count every block, empty and
    // redacted ones make no row; a sidechain line makes none either
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::in_turn(ev::assistant("m1", json!([ev::thinking("let me see")])), turn),
            ev::in_turn(
                ev::assistant("m1", json!([ev::text(""), {"type": "redacted_thinking", "data": "x"}])),
                turn,
            ),
            ev::in_turn(
                ev::assistant("m1", json!([ev::text("Looking."), ev::tool_use("tu1", "Bash")])),
                turn,
            ),
            ev::with_meta(
                ev::assistant("side", json!([ev::text("sidechain")])),
                json!({"sidechain": true}),
            ),
        ],
    )
    .await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    assert_eq!(
        items(&p),
        vec![
            json!({"kind": "user", "messageId": "c1", "clientMessageId": "c1", "text": "hello",
                   "images": [{"blobId": "i1", "mimeType": "image/png", "width": 4, "height": 3}],
                   "files": [{"path": "uploads/a.pdf", "name": "a.pdf", "mimeType": "application/pdf", "size": 5}],
                   "viewer": "admin@example.com", "@seq": [1, 1]}),
            json!({"kind": "reasoning", "messageId": "m1", "blockIndex": 0, "text": "let me see", "@seq": [2, 2]}),
            json!({"kind": "assistant", "messageId": "m1", "blockIndex": 3, "text": "Looking.", "@seq": [3, 3]}),
            json!({"kind": "tool", "callId": "tu1", "messageId": "m1", "blockIndex": 4, "name": "Bash",
                   "input": {"command": "ls"}, "status": "running", "@seq": [4, 4]}),
        ]
    );
    assert_eq!(p.rows[0].turn_id, Some(turn));
    // rule 5: results (completed, failed), an unknown call does nothing
    ev::append(
        pool,
        ALICE,
        id,
        vec![ev::in_turn(
            ev::assistant("m2", json!([ev::tool_use("tu2", "Read")])),
            turn,
        )],
    )
    .await;
    ev::append1(
        pool,
        ALICE,
        id,
        ev::results(&[
            ("tu1", "a\nb", false),
            ("tu2", "no such file", true),
            ("nope", "x", false),
        ]),
    )
    .await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    let t1 = tool(&p.rows[3]);
    assert_eq!(
        (t1.status, t1.result.as_ref().unwrap().text.as_str()),
        (ToolStatus::Completed, "a\nb")
    );
    assert_eq!(
        (p.rows[3].seq_start, p.rows[3].seq_end, p.rows[3].seqs.clone()),
        (4, 6, vec![4, 6])
    );
    assert!(p.rows[3].updated);
    let t2 = tool(&p.rows[4]);
    assert_eq!(t2.status, ToolStatus::Failed);
    assert!(t2.result.as_ref().unwrap().is_error);
    assert_eq!(p.max_seq, 7);

    // rule 6: a snapshot before its row (picked up by rule 4) and after it
    ev::append1(pool, ALICE, id, ev::files_sent("tu3", "out/early.txt")).await;
    ev::append1(
        pool,
        ALICE,
        id,
        ev::in_turn(ev::assistant("m3", json!([ev::tool_use("tu3", "SendUserFile")])), turn),
    )
    .await;
    ev::append1(
        pool,
        ALICE,
        id,
        ev::in_turn(ev::assistant("m4", json!([ev::tool_use("tu4", "SendUserFile")])), turn),
    )
    .await;
    ev::append1(pool, ALICE, id, ev::files_sent("tu4", "out/late.txt")).await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    let early = tool(&p.rows[5]);
    assert_eq!(early.sent_files.as_ref().unwrap()[0].path, "out/early.txt");
    assert!(!p.rows[5].updated, "made with its snapshot: one version");
    let late = tool(&p.rows[6]);
    assert_eq!(late.sent_files.as_ref().unwrap()[0].path, "out/late.txt");
    assert!(p.rows[6].updated);

    // rule 3: a task notification folds onto its launch; rule 7: a task report updates the row
    ev::append1(
        pool,
        ALICE,
        id,
        ev::in_turn(ev::assistant("m5", json!([ev::tool_use("bg", "Task")])), turn),
    )
    .await;
    ev::append1(
        pool,
        ALICE,
        id,
        ev::results(&[("bg", "Async agent launched successfully", false)]),
    )
    .await;
    ev::append1(pool, ALICE, id, ev::task_status("bg", "running")).await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    let bg = tool(&p.rows[0]);
    assert_eq!(bg.task.as_ref().unwrap().summary.as_deref(), Some("half done"));
    ev::append1(
        pool,
        ALICE,
        id,
        ev::notification("bg", "completed", "All 12 files checked."),
    )
    .await;
    ev::append1(pool, ALICE, id, ev::notification("unknown", "completed", "nothing")).await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    let bg = tool(&p.rows[0]);
    assert_eq!(bg.status, ToolStatus::Completed);
    assert_eq!(bg.result.as_ref().unwrap().text, "All 12 files checked.");
    let task = bg.task.as_ref().unwrap();
    assert_eq!(
        (task.task_id.as_str(), task.summary.as_deref()),
        ("task1", Some("All 12 files checked."))
    );
    assert_eq!(p.rows[0].seqs.len(), 4, "made, launched, reported, notified");

    // rule 2: a prompt not sent through magpie (display text and attachments of an import)
    let mut imported = ev::prompt("u-imp", "raw text");
    imported.meta = Some(json!({"prompt": true, "displayText": "shown text", "attachments": [
        {"path": "uploads/x.csv", "name": "x.csv", "mimeType": "", "size": 3, "missing": true}]}));
    ev::append1(pool, ALICE, id, imported).await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    assert_eq!(
        items(&p)[0],
        json!({"kind": "user", "messageId": "u-imp", "text": "shown text", "images": [],
               "files": [{"path": "uploads/x.csv", "name": "x.csv", "mimeType": "", "size": 3, "missing": true}],
               "@seq": [p.max_seq, p.max_seq]})
    );

    // an API Error line keeps its model error tag
    ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(
            ev::in_turn(
                ev::assistant("m6", json!([ev::text("API Error: 429 rate limited")])),
                turn,
            ),
            json!({"error": {"code": "model.rate_limited", "message": "rate limited", "status": 429}}),
        ),
    )
    .await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    match &p.rows[0].item {
        RowItem::Assistant(a) => assert_eq!(a.error.as_ref().unwrap().code.as_str(), "model.rate_limited"),
        other => panic!("{other:?}"),
    }

    // rule 8: a failed turn fails its running tool rows and adds an error row; completed leaves rows alone
    let t2 = Uuid::new_v4();
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::turn_started(t2),
            ev::in_turn(ev::assistant("m7", json!([ev::tool_use("tu7", "Bash")])), t2),
            ev::turn_ended(t2, "failed"),
        ],
    )
    .await;
    let p = alice.timeline(id, &ev::tail(2)).await.unwrap();
    let t7 = tool(&p.rows[0]);
    assert_eq!(t7.status, ToolStatus::Failed);
    assert_eq!(t7.error.as_ref().unwrap().code.as_str(), "run.oom");
    assert_eq!(
        serde_json::to_value(&p.rows[1].item).unwrap(),
        json!({"kind": "error", "code": "run.oom", "message": "killed by the memory guard"})
    );
    let t3 = Uuid::new_v4();
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::in_turn(ev::assistant("m8", json!([ev::tool_use("tu8", "Bash")])), t3),
            ev::turn_ended(t3, "completed"),
        ],
    )
    .await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    assert_eq!(
        tool(&p.rows[0]).status,
        ToolStatus::Running,
        "completed leaves a running row"
    );
    assert!(!p.rows[0].updated);
    let t4 = Uuid::new_v4();
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::in_turn(ev::assistant("m9", json!([ev::tool_use("tu9", "Bash")])), t4),
            ev::turn_ended(t4, "canceled"),
        ],
    )
    .await;
    let p = alice.timeline(id, &ev::tail(1)).await.unwrap();
    assert_eq!(tool(&p.rows[0]).status, ToolStatus::Canceled);
    assert_eq!(p.rows[0].seqs.len(), 2, "canceled: no error row");
    // no rule above changed the epoch
    assert_eq!(p.epoch, "1");
}

#[tokio::test]
async fn rule_9_retract_deletes_rebuilds_and_bumps_the_epoch_once() {
    let db = common::fresh_db("retract").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let seqs = ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::user_sent("c1", "q"),
            ev::assistant("m1", json!([ev::text("first")])),
            ev::assistant("m1", json!([ev::tool_use("tu1", "Bash")])),
            ev::results(&[("tu1", "out", false)]),
            ev::ev("sandbox.ready", json!({})),
        ],
    )
    .await;
    let feed = alice
        .changes(&ChangesQuery {
            after: After::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    // a retract of an internal line (no row came from it) changes nothing
    ev::append1(pool, ALICE, id, ev::retract(&[seqs[4]])).await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 4));
    // the result's line leaves: the tool row is rebuilt without it (a new row seq), the text row's line leaves: it
    // is deleted; one epoch change and one timeline_replaced
    ev::append1(pool, ALICE, id, ev::retract(&[seqs[1], seqs[3]])).await;
    assert_eq!(epoch_and_head(&sql, id).await, (2, 5));
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    assert_eq!(p.epoch, "2");
    assert_eq!(p.rows.len(), 2);
    let tu = tool(&p.rows[1]);
    assert_eq!(
        (tu.status, tu.result.is_none(), tu.block_index),
        (ToolStatus::Running, true, 1)
    );
    assert_eq!(p.rows[1].seqs, vec![3, 4, 5]);
    let changes = alice
        .changes(&ChangesQuery {
            after: After::Seq(feed.head_seq),
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap();
    let replaced: Vec<_> = changes
        .changes
        .iter()
        .filter_map(|c| match &c.body {
            ChangeBody::TimelineReplaced { epoch } => Some(epoch.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(replaced, vec!["2".to_owned()]);
    // `after` from before the retract skips the deleted row's seq and sees the rebuild at its own seq
    let p = alice.timeline(id, &ev::after(0, 500)).await.unwrap();
    assert_eq!(
        p.rows.iter().map(|r| (r.seq_start, r.seq_end)).collect::<Vec<_>>(),
        vec![(1, 1), (3, 3), (3, 4), (3, 5)]
    );
    // the line comes back (Claude Code rewrote it): a new row at the same block index; the result re-lands
    let back = ev::with_meta(
        ev::assistant("m1", json!([ev::text("first")])),
        json!({"replaces": seqs[1]}),
    );
    ev::append1(pool, ALICE, id, back).await;
    ev::append1(pool, ALICE, id, ev::results(&[("tu1", "out", false)])).await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    assert_eq!(p.epoch, "2", "appends never change the epoch");
    match &p.rows[2].item {
        RowItem::Assistant(x) => assert_eq!(x.block_index, 0, "the retracted line no longer counts"),
        other => panic!("{other:?}"),
    }
    assert_eq!(tool(&p.rows[1]).status, ToolStatus::Completed);
    // the tool_use line itself leaves: the row goes
    ev::append1(pool, ALICE, id, ev::retract(&[seqs[2]])).await;
    let p = alice.timeline(id, &ev::tail(500)).await.unwrap();
    assert_eq!(p.epoch, "3");
    assert!(p.rows.iter().all(|r| !matches!(r.item, RowItem::Tool(_))));
}

/// No two rows share a `(messageId, blockIndex)` and no two tool rows a `callId`.
fn assert_unique(p: &TimelinePage, when: &str) {
    let mut blocks = std::collections::HashSet::new();
    let mut calls = std::collections::HashSet::new();
    for r in &p.rows {
        let key = match &r.item {
            RowItem::Assistant(x) => Some((x.message_id.clone(), x.block_index)),
            RowItem::Reasoning(x) => Some((x.message_id.clone(), x.block_index)),
            RowItem::Tool(x) => {
                assert!(
                    calls.insert(x.call_id.clone()),
                    "{when}: two rows of call {}",
                    x.call_id
                );
                Some((x.message_id.clone(), x.block_index))
            }
            _ => None,
        };
        if let Some(k) = key {
            assert!(blocks.insert(k.clone()), "{when}: two rows of {k:?}");
        }
    }
}

#[tokio::test]
async fn a_rewritten_line_takes_over_its_row_before_the_retract_lands() {
    let db = common::fresh_db("replaces").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let seqs = ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::user_sent("c1", "q"),
            ev::assistant("m1", json!([ev::text("first")])),
            ev::assistant("m1", json!([ev::tool_use("tu1", "Bash")])),
            ev::results(&[("tu1", "out", false)]),
            ev::assistant("m1", json!([ev::text("after")])),
        ],
    )
    .await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 5));
    let page = async || alice.timeline(id, &ev::tail(500)).await.unwrap();
    // an overwrite: the rewritten line, then the old line's retract, in one transaction
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::with_meta(
                ev::assistant("m1", json!([ev::text("first, edited")])),
                json!({"replaces": seqs[1]}),
            ),
            ev::retract(&[seqs[1]]),
        ],
    )
    .await;
    let p = page().await;
    assert_unique(&p, "after the overwrite");
    assert_eq!(p.epoch, "1", "an in-place update is no replacement");
    assert_eq!(p.rows.len(), 4);
    assert_eq!(
        (p.rows[1].seq_start, p.rows[1].seq_end),
        (2, 6),
        "the row keeps its identity"
    );
    match &p.rows[1].item {
        RowItem::Assistant(x) => assert_eq!((x.block_index, x.text.as_str()), (0, "first, edited")),
        other => panic!("{other:?}"),
    }
    // a truncate: the rewritten tool line lands now, its old line's retract waits in limbo (another commit)
    ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(
            ev::assistant("m1", json!([ev::tool_use("tu1", "Bash")])),
            json!({"replaces": seqs[2]}),
        ),
    )
    .await;
    let p = page().await;
    assert_unique(&p, "while the retract waits in limbo");
    let tu = tool(&p.rows[2]);
    assert_eq!(
        (tu.status, tu.result.as_ref().map(|r| r.text.as_str()), tu.block_index),
        (ToolStatus::Completed, Some("out"), 1),
        "the row keeps its result"
    );
    assert_eq!(
        epoch_and_head(&sql, id).await,
        (1, 6),
        "nothing visible changed: no row seq"
    );
    // a new line of the same message while the old line is unretracted counts it once
    ev::append1(pool, ALICE, id, ev::assistant("m1", json!([ev::text("more")]))).await;
    let p = page().await;
    assert_unique(&p, "a later line of the message");
    match &p.rows.last().unwrap().item {
        RowItem::Assistant(x) => assert_eq!(x.block_index, 3),
        other => panic!("{other:?}"),
    }
    // the limbo flush: the retract touches no row
    ev::append1(pool, ALICE, id, ev::retract(&[seqs[2]])).await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 7));
    let p = page().await;
    assert_unique(&p, "after the flush");
    assert_eq!(p.rows.len(), 5);
    assert_eq!(tool(&p.rows[2]).status, ToolStatus::Completed);
    // an `after` walk from 0 sees each identity once
    let walk = alice.timeline(id, &ev::after(0, 500)).await.unwrap();
    let mut latest = std::collections::BTreeMap::new();
    for r in &walk.rows {
        latest.insert(r.seq_start, r.clone());
    }
    let p = TimelinePage {
        rows: latest.into_values().collect(),
        ..walk
    };
    assert_unique(&p, "the after walk");
    // a rewrite that turns a block into another kind replaces the row: a new epoch
    let last = ev::append1(pool, ALICE, id, ev::assistant("m2", json!([ev::text("x")]))).await;
    ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(
            ev::assistant("m2", json!([ev::thinking("x")])),
            json!({"replaces": last}),
        ),
    )
    .await;
    let p = page().await;
    assert_unique(&p, "a changed kind");
    assert_eq!(p.epoch, "2");
    assert!(matches!(p.rows.last().unwrap().item, RowItem::Reasoning(_)));
}

#[tokio::test]
async fn a_rewritten_prompt_or_result_line_keeps_its_row_and_the_retract_changes_nothing() {
    let db = common::fresh_db("replaces_user").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let seqs = ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::prompt("p1", "queued"),
            ev::assistant("m1", json!([ev::tool_use("tu1", "Bash")])),
            ev::results(&[("tu1", "out", false)]),
        ],
    )
    .await;
    let page = async || alice.timeline(id, &ev::tail(500)).await.unwrap();
    let prompts = |p: &TimelinePage| -> Vec<(u64, String)> {
        p.rows
            .iter()
            .filter_map(|r| match &r.item {
                RowItem::User(u) if u.message_id == "p1" => Some((r.seq_start, u.text.clone())),
                _ => None,
            })
            .collect()
    };
    let start = epoch_and_head(&sql, id).await;
    let row_of_p1 = prompts(&page().await)[0].0;
    // the prompt line rewritten with new text (a truncate's rewrite: its retract lands later)
    let x = ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(
            ev::prompt("p1", "queued, edited"),
            json!({"prompt": true, "replaces": seqs[0]}),
        ),
    )
    .await;
    let p = page().await;
    assert_eq!(
        prompts(&p),
        vec![(row_of_p1, "queued, edited".to_owned())],
        "one row, the same identity"
    );
    assert_eq!(epoch_and_head(&sql, id).await, (start.0, start.1 + 1));
    // rewritten again with the same text: no new version
    let y = ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(
            ev::prompt("p1", "queued, edited"),
            json!({"prompt": true, "replaces": x}),
        ),
    )
    .await;
    assert_eq!(epoch_and_head(&sql, id).await, (start.0, start.1 + 1));
    // the result line rewritten with the same content
    let z = ev::append1(
        pool,
        ALICE,
        id,
        ev::with_meta(ev::results(&[("tu1", "out", false)]), json!({"replaces": seqs[2]})),
    )
    .await;
    assert_eq!(epoch_and_head(&sql, id).await, (start.0, start.1 + 1));
    // the old lines' retract (the limbo flush): nothing visible changes, so no row seq and no new epoch
    ev::append1(pool, ALICE, id, ev::retract(&[seqs[0], x, seqs[2]])).await;
    let p = page().await;
    assert_eq!(epoch_and_head(&sql, id).await, (start.0, start.1 + 1));
    assert_eq!(prompts(&p), vec![(row_of_p1, "queued, edited".to_owned())]);
    let tr = p.rows.iter().find(|r| matches!(r.item, RowItem::Tool(_))).unwrap();
    assert_eq!(tool(tr).result.as_ref().map(|r| r.text.as_str()), Some("out"));
    let sources: Vec<Vec<i64>> = sql
        .get()
        .await
        .unwrap()
        .query(
            "select source_seqs from rows where conversation_id = $1 and (message_id = 'p1' or tool_use_id = 'tu1')
             order by row_id",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(sources, vec![vec![y], vec![seqs[1], z]]);
    // a retract of the prompt line that counts now does delete its row (a new epoch)
    ev::append1(pool, ALICE, id, ev::retract(&[y])).await;
    let p = page().await;
    assert!(prompts(&p).is_empty());
    assert_eq!(epoch_and_head(&sql, id).await.0, start.0 + 1);
}

#[tokio::test]
async fn an_event_that_changes_nothing_takes_no_row_seq() {
    let db = common::fresh_db("noop").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let turn = Uuid::new_v4();
    ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::turn_started(turn),
            ev::in_turn(ev::assistant("m1", json!([ev::tool_use("tb", "Bash")])), turn),
            ev::task_status("tb", "running"),
        ],
    )
    .await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 2));
    // the same progress report again, a repeated snapshot and result: no new version
    ev::append1(pool, ALICE, id, ev::task_status("tb", "running")).await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 2));
    ev::append1(pool, ALICE, id, ev::files_sent("tb", "out/a.txt")).await;
    ev::append1(pool, ALICE, id, ev::files_sent("tb", "out/a.txt")).await;
    assert_eq!(epoch_and_head(&sql, id).await, (1, 3));
    // a background launch outlives its canceled turn: it stays running until its notification
    ev::append1(pool, ALICE, id, ev::turn_ended(turn, "canceled")).await;
    let p = pigeon.person(ALICE).timeline(id, &ev::tail(10)).await.unwrap();
    let tb = tool(&p.rows[0]);
    assert_eq!((tb.status, tb.error.is_none()), (ToolStatus::Running, true));
    assert_eq!(epoch_and_head(&sql, id).await, (1, 3));
    ev::append1(pool, ALICE, id, ev::notification("tb", "completed", "done")).await;
    let p = pigeon.person(ALICE).timeline(id, &ev::tail(10)).await.unwrap();
    assert_eq!(tool(&p.rows[0]).status, ToolStatus::Completed);
    // the repeated result is still a source: a retract of the first leaves the result on the row
    let r = ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::results(&[("tb", "started", false)]),
            ev::results(&[("tb", "started", false)]),
        ],
    )
    .await;
    let before = epoch_and_head(&sql, id).await;
    ev::append1(pool, ALICE, id, ev::retract(&[r[0]])).await;
    let p = pigeon.person(ALICE).timeline(id, &ev::tail(10)).await.unwrap();
    assert_eq!(
        tool(&p.rows[0]).result.as_ref().map(|x| x.text.as_str()),
        Some("started")
    );
    // the row it rebuilds is the row it shows: only its sources change (no row seq, no new epoch)
    assert_eq!(epoch_and_head(&sql, id).await, before);
    let sources: Vec<i64> = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select source_seqs from rows where conversation_id = $1 and tool_use_id = 'tb'",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!sources.contains(&r[0]) && sources.contains(&r[1]), "{sources:?}");
}

/// A conversation with `turns` turns of 4 rows (prompt, reasoning, tool call, text) and 5 row seqs each (the tool
/// row's result is its second version).
async fn preload(pool: &aviary_pg::Pool, id: Uuid, turns: usize) {
    for i in 0..turns {
        let turn = Uuid::new_v4();
        let m = format!("msg{i}");
        let tu = format!("tu{i}");
        ev::append(
            pool,
            ALICE,
            id,
            vec![
                ev::turn_started(turn),
                ev::in_turn(
                    ev::user_sent(&format!("c{i}"), &format!("question {i} about the bridge")),
                    turn,
                ),
                ev::in_turn(ev::assistant(&m, json!([ev::thinking("hmm")])), turn),
                ev::in_turn(ev::assistant(&m, json!([ev::tool_use(&tu, "Bash")])), turn),
                ev::in_turn(ev::results(&[(&tu, "ok", false)]), turn),
                ev::in_turn(
                    ev::assistant(&format!("{m}b"), json!([ev::text(&format!("answer {i}"))])),
                    turn,
                ),
                ev::turn_ended(turn, "completed"),
            ],
        )
        .await;
    }
}

#[tokio::test]
async fn pages_are_exact_with_has_older_and_has_newer() {
    let db = common::fresh_db("pages").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    preload(a.pool(), id, 10).await; // 40 rows, 50 row seqs (10 tool rows updated once)
    let tail = alice.timeline(id, &ev::tail(15)).await.unwrap();
    assert_eq!(tail.max_seq, 50);
    assert_eq!(tail.min_seq, 1);
    assert_eq!(tail.rows.len(), 15);
    assert!(tail.has_older && !tail.has_newer);
    // walking back with before= gives every row once, ascending within a page, and hasOlder false at the start
    let mut seen: Vec<u64> = tail.rows.iter().map(|r| r.seq_start).collect();
    let mut cursor = tail.rows[0].seq_start;
    loop {
        let p = alice.timeline(id, &ev::before(cursor, 15)).await.unwrap();
        assert!(p.has_newer);
        assert!(p.rows.windows(2).all(|w| w[0].seq_start < w[1].seq_start));
        let mut ids: Vec<u64> = p.rows.iter().map(|r| r.seq_start).collect();
        ids.append(&mut seen);
        seen = ids;
        if !p.has_older {
            break;
        }
        cursor = p.rows[0].seq_start;
    }
    assert_eq!(seen.len(), 40);
    let mut sorted = seen.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), 40, "no duplicate");
    // before the first row: nothing, and newer rows exist
    let p = alice.timeline(id, &ev::before(1, 10)).await.unwrap();
    assert!(p.rows.is_empty() && !p.has_older && p.has_newer);
    // after: one entry per row seq, every integer once, in order
    let mut got = Vec::new();
    let mut s = 0;
    loop {
        let p = alice.timeline(id, &ev::after(s, 7)).await.unwrap();
        if p.rows.is_empty() {
            assert!(!p.has_newer);
            break;
        }
        for r in &p.rows {
            got.push(r.seq_end);
            assert_eq!(r.updated, r.seq_end != r.seq_start);
            assert_eq!(r.seqs.last(), Some(&r.seq_end));
        }
        s = p.rows.last().unwrap().seq_end;
        assert_eq!(p.has_newer, s < 50);
        assert!(p.has_older || got.len() <= 7);
    }
    assert_eq!(got, (1..=50).collect::<Vec<_>>());
    // query checks
    for bad in [
        magpie_api::rows::TimelineQuery::default(),
        magpie_api::rows::TimelineQuery {
            tail: true,
            after: Some(1),
            ..Default::default()
        },
        magpie_api::rows::TimelineQuery {
            tail: true,
            limit: Some(501),
            ..Default::default()
        },
        magpie_api::rows::TimelineQuery {
            tail: true,
            limit: Some(0),
            ..Default::default()
        },
        magpie_api::rows::TimelineQuery {
            tail: true,
            wait_ms: Some(10),
            ..Default::default()
        },
        // a frame number means nothing without its turn
        magpie_api::rows::TimelineQuery {
            after: Some(1),
            live_after: Some(3),
            ..Default::default()
        },
        magpie_api::rows::TimelineQuery {
            after: Some(1),
            live_turn: Some(Uuid::new_v4()),
            ..Default::default()
        },
    ] {
        let e = alice.timeline(id, &bad).await.unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest), "{bad:?}");
    }
    // someone else's conversation, or none: 404; a deleted one stays readable
    let bob = pigeon.person("bob@example.com");
    assert_eq!(
        bob.timeline(id, &ev::tail(1)).await.unwrap_err().code(),
        Some(&ErrorCode::NotFound)
    );
    assert_eq!(
        alice.timeline(Uuid::new_v4(), &ev::tail(1)).await.unwrap_err().code(),
        Some(&ErrorCode::NotFound)
    );
    alice.delete_conversation(id).await.unwrap();
    assert_eq!(alice.timeline(id, &ev::tail(3)).await.unwrap().rows.len(), 3);
}

#[tokio::test]
async fn a_page_stops_at_4_mib() {
    let db = common::fresh_db("big").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let big = "x".repeat(1 << 20);
    for i in 0..6 {
        ev::append1(
            a.pool(),
            ALICE,
            id,
            ev::assistant(&format!("m{i}"), json!([ev::text(&big)])),
        )
        .await;
    }
    let p = alice.timeline(id, &ev::tail(6)).await.unwrap();
    assert_eq!(p.rows.len(), 3);
    assert!(p.has_older);
    assert_eq!(p.rows[2].seq_start, 6, "tail keeps the newest");
    let p = alice.timeline(id, &ev::after(0, 6)).await.unwrap();
    assert_eq!(p.rows.len(), 3);
    assert!(p.has_newer);
}

#[tokio::test]
async fn a_long_poll_on_one_replica_answers_within_50ms_of_a_commit_on_another() {
    let db = common::fresh_db("poll").await;
    let t = common::Tokens::new();
    // held polls re-query only every minute: the wake-up must come from the notification
    let a = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::no_requery()).await;
    let b = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let id = ev::conversation(&pigeon, ALICE).await;
    ev::append1(b.pool(), ALICE, id, ev::user_sent("c0", "hi")).await;
    // the bound is on the median of the rounds (a shared CI runner can stall one round), with every round well under
    // the 1 s re-query that would otherwise answer
    let rounds = 7u64;
    let mut lags = Vec::new();
    for round in 1..=rounds {
        let poll = {
            let pigeon = a.pigeon();
            tokio::spawn(async move {
                let p = pigeon
                    .person(ALICE)
                    .timeline(id, &ev::after_wait(round, 20_000))
                    .await
                    .unwrap();
                (Instant::now(), p)
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        ev::append1(b.pool(), ALICE, id, ev::user_sent(&format!("c{round}"), "more")).await;
        let committed = Instant::now();
        let (answered, p) = poll.await.unwrap();
        let lag = answered.saturating_duration_since(committed);
        assert!(
            lag < Duration::from_millis(900),
            "round {round}: answered {lag:?} after the commit"
        );
        lags.push(lag);
        assert_eq!(p.rows.iter().map(|r| r.seq_end).collect::<Vec<_>>(), vec![round + 1]);
    }
    lags.sort();
    let median = lags[lags.len() / 2];
    assert!(
        median < Duration::from_millis(50),
        "median wake-up {median:?} (all: {lags:?})"
    );
    // nothing new: rows [] after waitMs
    let started = Instant::now();
    let p = pigeon
        .person(ALICE)
        .timeline(id, &ev::after_wait(rounds + 1, 400))
        .await
        .unwrap();
    assert!(p.rows.is_empty());
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert_eq!(p.max_seq, rounds + 1);
}

#[tokio::test]
async fn held_polls_hold_no_connection_and_answer_at_once_on_drain() {
    let db = common::fresh_db("held").await;
    let t = common::Tokens::new();
    // no re-query within the test: between looks a held poll is only a waiter
    let a = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::no_requery()).await;
    let pigeon = a.pigeon();
    let id = ev::conversation(&pigeon, ALICE).await;
    // three times as many held polls as the pool has connections
    let n = magpie::limits::POOL_SIZE * 3;
    let mut polls = Vec::new();
    for _ in 0..n {
        let pigeon = a.pigeon();
        polls.push(tokio::spawn(async move {
            pigeon
                .person(ALICE)
                .timeline(id, &ev::after_wait(0, 25_000))
                .await
                .unwrap()
        }));
    }
    // once every poll has looked and is waiting, they hold no connection: every pooled connection is idle
    let idle = common::eventually(Duration::from_secs(20), async || {
        let s = a.pool().status();
        a.app().fanout.held() >= n as i64 && s.waiting == 0 && s.available == s.size
    })
    .await;
    assert!(idle, "{:?}, {} held", a.pool().status(), a.app().fanout.held());
    // and other requests are served at once
    for _ in 0..5 {
        let started = Instant::now();
        pigeon.person(ALICE).timeline(id, &ev::tail(10)).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }
    let started = Instant::now();
    a.start_drain();
    for p in polls {
        assert!(p.await.unwrap().rows.is_empty());
    }
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_retract_that_only_deletes_wakes_a_held_poll_with_the_new_epoch() {
    let db = common::fresh_db("pollepoch").await;
    let t = common::Tokens::new();
    let a = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::no_requery()).await;
    let pigeon = a.pigeon();
    let id = ev::conversation(&pigeon, ALICE).await;
    let seqs = ev::append(a.pool(), ALICE, id, vec![ev::assistant("m", json!([ev::text("hi")]))]).await;
    let poll = {
        let pigeon = a.pigeon();
        tokio::spawn(async move {
            pigeon
                .person(ALICE)
                .timeline(id, &ev::after_wait(1, 20_000))
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    ev::append1(a.pool(), ALICE, id, ev::retract(&seqs)).await;
    let p = poll.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!((p.epoch.as_str(), p.rows.len()), ("2", 0));
}

/// E6-style: with 120 turns preloaded, a page costs what a page costs (within 2x of the same page at 10 turns).
#[tokio::test]
async fn page_time_is_flat_over_120_turns() {
    let db = common::fresh_db("e6").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    async fn time_pages(alice: &magpie_api::client::PersonCalls<'_>, id: Uuid) -> Duration {
        // warm, then the best of 5 rounds of tail + one before page + one after page
        let mut best = Duration::MAX;
        for _ in 0..6 {
            let started = Instant::now();
            let tail = alice.timeline(id, &ev::tail(40)).await.unwrap();
            alice
                .timeline(id, &ev::before(tail.rows[0].seq_start, 40))
                .await
                .unwrap();
            alice
                .timeline(id, &ev::after(tail.max_seq.saturating_sub(48), 40))
                .await
                .unwrap();
            best = best.min(started.elapsed());
        }
        best
    }
    preload(a.pool(), id, 10).await;
    let small = time_pages(&alice, id).await;
    preload(a.pool(), id, 110).await;
    let large = time_pages(&alice, id).await;
    assert!(
        large <= small * 2 + Duration::from_millis(5),
        "10 turns: {small:?}, 120 turns: {large:?}"
    );
    // and the walk back over 120 turns gives 600 rows, no gap, no duplicate
    let mut n = 0;
    let mut cursor = u64::MAX;
    let mut last_first = u64::MAX;
    loop {
        let p = if cursor == u64::MAX {
            alice.timeline(id, &ev::tail(40)).await.unwrap()
        } else {
            alice.timeline(id, &ev::before(cursor, 40)).await.unwrap()
        };
        if let Some(r) = p.rows.last() {
            assert!(r.seq_start < last_first);
        }
        n += p.rows.len();
        if !p.has_older {
            break;
        }
        cursor = p.rows[0].seq_start;
        last_first = cursor;
    }
    assert_eq!(n, 480);
}

#[tokio::test]
async fn prompts_and_search() {
    let db = common::fresh_db("prompts").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let long = format!("{} tail", "長".repeat(300));
    ev::append(
        a.pool(),
        ALICE,
        id,
        vec![
            ev::user_sent("c1", "Where is the Bridge?"),
            ev::assistant("m1", json!([ev::text("The bridge links three cities. BRIDGE!")])),
            ev::prompt("u2", &long),
            ev::assistant("m2", json!([ev::text("100% sure-ok")])),
        ],
    )
    .await;
    let p = alice.prompts(id).await.unwrap();
    assert_eq!(p.epoch, "1");
    assert_eq!(p.prompts.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![1, 3]);
    assert_eq!(p.prompts[0].preview, "Where is the Bridge?");
    assert_eq!(p.prompts[1].preview.chars().count(), 200);
    let s = alice
        .search(
            id,
            &SearchQuery {
                q: "bridge".into(),
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        s.locations
            .iter()
            .map(|l| (l.seq, l.kind.as_str(), l.count))
            .collect::<Vec<_>>(),
        vec![(1, "user", 1), (2, "assistant", 2)]
    );
    assert!(s.next_cursor.is_none());
    // pages of one location
    let s1 = alice
        .search(
            id,
            &SearchQuery {
                q: "BRIDGE".into(),
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(s1.locations.len(), 1);
    let s2 = alice
        .search(
            id,
            &SearchQuery {
                q: "bridge".into(),
                cursor: s1.next_cursor.clone(),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(s2.locations[0].seq, 2);
    // like's wildcards are literal
    let pct = alice
        .search(
            id,
            &SearchQuery {
                q: "0%".into(),
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(pct.locations.len(), 1);
    let none = alice
        .search(
            id,
            &SearchQuery {
                q: "e_o".into(),
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert!(none.locations.is_empty());
    for bad in ["", "  "] {
        let e = alice
            .search(
                id,
                &SearchQuery {
                    q: bad.into(),
                    cursor: None,
                    limit: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(e.code(), Some(&ErrorCode::BadRequest));
    }
}
