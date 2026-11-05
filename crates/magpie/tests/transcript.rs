//! The raw view's read (`GET /v1/conversations/{id}/transcript`): the main transcript's lines in file order, long
//! text put back whole, media as a placeholder, paged by `after`; only while an admin views as the owner. And the
//! admin route (`GET /v1/admin/conversations/{id}/transcript`, pigeon's standalone raw page): any conversation, with
//! its owner and title, audited in the view-as log.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use base64::Engine as _;
use common::cc::{Sandbox, code, main_path, set_main};
use magpie_api::errors::ErrorCode;
use magpie_api::ops::{ViewAsEvent, ViewAsLogQuery};
use magpie_api::rows::TranscriptQuery;
use serde_json::{Value, json};

const ALICE: &str = "alice@example.org";
const BOB: &str = "bob@example.org";

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

#[tokio::test]
async fn the_transcript_is_read_whole_in_order_only_while_viewing_as() {
    let db = common::fresh_db("raw_transcript").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    let long = "x".repeat(40 * 1024);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(std::iter::repeat_n(0u8, 3000));
    let png = base64::engine::general_purpose::STANDARD.encode(&png);
    let lines = [
        json!({"type": "user", "uuid": "u1", "timestamp": "2026-11-05T03:12:09.120Z",
               "message": {"role": "user", "content": "hello"}}),
        json!({"type": "assistant", "uuid": "a1", "message": {"id": "m1", "model": "m-1",
               "content": [{"type": "tool_use", "id": "c1", "name": "Bash", "input": {"command": "cat big"}}]}}),
        json!({"type": "user", "uuid": "u2", "message": {"role": "user",
               "content": [{"type": "tool_result", "tool_use_id": "c1", "content": long}]}}),
        json!({"type": "user", "uuid": "u3", "message": {"role": "user",
               "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]}}),
    ];
    let bytes: Vec<u8> = lines.iter().flat_map(line).collect();
    s.write(&main_path(), 0, &bytes).await.unwrap();

    let pigeon = r.pigeon();
    let cid = s.conversation;
    // not viewing as: 404, for the owner as for anyone
    let e = pigeon
        .person(ALICE)
        .transcript(cid, &TranscriptQuery::default())
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound.as_str());
    // viewing as someone else: 404
    let e = pigeon
        .viewing_as(BOB, "admin@example.com", "s1")
        .transcript(cid, &TranscriptQuery::default())
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound.as_str());

    let view = pigeon.viewing_as(ALICE, "admin@example.com", "s1");
    let p = view.transcript(cid, &TranscriptQuery::default()).await.unwrap();
    assert_eq!(p.path.as_deref(), Some(main_path().as_str()));
    assert_eq!(p.lines.len(), 4);
    assert!(!p.more);
    let parsed: Vec<Value> = p.lines.iter().map(|l| serde_json::from_str(&l.line).unwrap()).collect();
    assert_eq!(parsed[0], lines[0]);
    assert_eq!(parsed[1], lines[1]);
    assert_eq!(parsed[2], lines[2], "long text is put back whole");
    let data = parsed[3]["message"]["content"][0]["source"]["data"].as_str().unwrap();
    assert!(data.starts_with("[media image/png "), "{data}");
    let ords: Vec<i64> = p.lines.iter().map(|l| l.ord).collect();
    assert!(ords.windows(2).all(|w| w[0] < w[1]), "{ords:?}");
    assert_eq!(p.next, ords.last().copied());

    // paging
    let first = view
        .transcript(
            cid,
            &TranscriptQuery {
                after: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.lines.len(), 1);
    assert!(first.more);
    let rest = view
        .transcript(
            cid,
            &TranscriptQuery {
                after: first.next,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.lines.len(), 3);
    // an append comes after the last ord
    let more = line(&json!({"type": "system", "subtype": "compact_boundary", "uuid": "s1"}));
    s.write(&main_path(), u64::try_from(bytes.len()).unwrap(), &more)
        .await
        .unwrap();
    let tail = view
        .transcript(
            cid,
            &TranscriptQuery {
                after: p.next,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(tail.lines.len(), 1);
    assert!(tail.lines[0].line.contains("compact_boundary"));
    let none = view
        .transcript(
            cid,
            &TranscriptQuery {
                after: tail.next,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(none.lines.len(), 0);
    assert_eq!(none.next, tail.next);
}

#[tokio::test]
async fn an_admin_reads_any_transcript_without_view_as_and_the_read_is_audited() {
    let db = common::fresh_db("raw_transcript_admin").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    let lines = [
        json!({"type": "user", "uuid": "u1", "message": {"role": "user", "content": "hello"}}),
        json!({"type": "assistant", "uuid": "a1", "message": {"id": "m1", "model": "m-1",
               "content": [{"type": "text", "text": "hi"}]}}),
    ];
    let bytes: Vec<u8> = lines.iter().flat_map(line).collect();
    s.write(&main_path(), 0, &bytes).await.unwrap();
    let cid = s.conversation;
    let admin = "root@example.com";
    let all = TranscriptQuery::default();

    // pigeon, naming the admin: every line, the owner, the title (none yet)
    let pigeon = r.pigeon();
    let calls = pigeon.service(Some(admin));
    let p = calls.admin_transcript(cid, &all).await.unwrap();
    assert_eq!(p.lines.len(), 2);
    assert_eq!(p.person.as_deref(), Some(ALICE));
    assert_eq!(p.path.as_deref(), Some(main_path().as_str()));
    let parsed: Vec<Value> = p.lines.iter().map(|l| serde_json::from_str(&l.line).unwrap()).collect();
    assert_eq!(parsed[0], lines[0]);
    // paging as on the view-as route
    let rest = calls
        .admin_transcript(
            cid,
            &TranscriptQuery {
                after: Some(p.lines[0].ord),
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.lines.len(), 1);
    // the ops tools too
    let ops = r.admin();
    let by_ops = ops.service(Some(admin)).admin_transcript(cid, &all).await.unwrap();
    assert_eq!(by_ops.lines.len(), 2);

    // an unknown conversation: 404
    let e = calls.admin_transcript(uuid::Uuid::new_v4(), &all).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound.as_str());
    // no admin named: refused (400), nothing read
    let e = pigeon.service(None).admin_transcript(cid, &all).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadRequest.as_str());
    // hermit may not call it
    let e = r
        .hermit()
        .service(Some(admin))
        .admin_transcript(cid, &all)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "wrong_caller");

    // the audit: one admin_read row for this admin, conversation and day, however many reads
    let log = r
        .hermit()
        .service(None)
        .ops_view_as(&ViewAsLogQuery::default())
        .await
        .unwrap();
    let reads: Vec<_> = log
        .view_as
        .iter()
        .filter(|e| e.event == ViewAsEvent::AdminRead)
        .collect();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!((reads[0].admin.as_str(), reads[0].user.as_str()), (admin, ALICE));
    assert_eq!(reads[0].conversation_id, Some(cid));
}
