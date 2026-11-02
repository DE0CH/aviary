//! Shares (M16, magpie 12): the freeze leaves out a running turn and its Claude Code lines, the view equals the
//! conversation's rows at the freeze, the frozen workspace is a clone (later changes do not reach it), soft delete
//! hides a share and restore brings it back, continue makes an independent conversation, revoke and purge give 404,
//! view-as is read-only, a failed clone stores nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names
)]

mod common;

use aviary_fakes::fault::FaultKind;
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use common::ev::{self, assistant, results, retract, text, tool_use, turn_ended, turn_started, user_sent};
use common::files::{gateway, grant_lease};
use magpie::blobs;
use magpie_api::conversations::ListQuery;
use magpie_api::errors::ErrorCode;
use magpie_api::files::RawFileQuery;
use serde_json::json;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";

fn code(e: &magpie_api::client::ClientError) -> Option<ErrorCode> {
    e.code().cloned()
}

/// A finished turn (a reply and a sent file), then a turn still running, with the main transcript's lines of both.
async fn conversation_with_turns(r: &common::Replica, hermit: &FakeHermit) -> (Uuid, Uuid) {
    let c = ev::conversation(&r.pigeon(), ALICE).await;
    hermit.write_file(c, "report.txt", b"v1");
    let t1 = Uuid::new_v4();
    ev::append(
        r.pool(),
        ALICE,
        c,
        vec![
            user_sent("m1", "first question"),
            ev::in_turn(turn_started(t1), t1),
            ev::in_turn(assistant("msg_1", json!([text("first answer")])), t1),
            ev::in_turn(turn_ended(t1, "completed"), t1),
        ],
    )
    .await;
    // a sent file within the finished turn
    let lease = grant_lease(r.pool(), c).await;
    let bytes = b"sent snapshot".to_vec();
    let hex = blobs::to_hex(&blobs::sha256(&bytes));
    let st = gateway(r, reqwest::Method::POST, "/v1/sandbox/sent-files", c, ALICE, Some(lease))
        .json(&json!({"toolUseId": "call_1", "files": [{"path": "/workspace/out.txt", "size": bytes.len(), "sha256": hex, "mimeType": "text/plain"}]}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st, 200);
    let st = gateway(
        r,
        reqwest::Method::PUT,
        &format!("/v1/sandbox/blobs/{hex}"),
        c,
        ALICE,
        Some(lease),
    )
    .body(bytes)
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(st, 201);
    let st = gateway(
        r,
        reqwest::Method::POST,
        "/v1/sandbox/sent-files/call_1/commit",
        c,
        ALICE,
        Some(lease),
    )
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(st, 200);
    // the second turn is running
    let t2 = Uuid::new_v4();
    let seqs = ev::append(
        r.pool(),
        ALICE,
        c,
        vec![
            user_sent("m2", "second question"),
            ev::in_turn(turn_started(t2), t2),
            ev::in_turn(assistant("msg_2", json!([text("partial")])), t2),
        ],
    )
    .await;
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "insert into turns (id, conversation_id, origin, state, user_seq, start_seq) values ($1, $2, 'person', 'running', $3, $4)",
        &[&t2, &c, &seqs[0], &seqs[1]],
    )
    .await
    .unwrap();
    sql.execute(
        "update conversations set active_turn = $2, run_state = 'running',
                main_transcript = 'projects/-workspace/s1.jsonl', provider_session_id = 's1' where id = $1",
        &[&c, &t2],
    )
    .await
    .unwrap();
    // the main transcript: a line of each turn (the cut leaves the running turn's out)
    let f: i64 = sql
        .query_one(
            "insert into cc_files (scope, path, kind, mode, mtime_ns, size, next_ord)
             values ($1, 'projects/-workspace/s1.jsonl', 'lines', 420, 1, 20, 2) returning id",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    for (ord, off, turn) in [(0i64, 0i64, t1), (1, 10, t2)] {
        sql.execute(
            "insert into cc_lines (file_id, ord, start_off, len, sha256, stub, turn_id)
             values ($1, $2, $3, 10, $5, $6, $4)",
            &[&f, &ord, &off, &turn, &vec![0u8; 32], &b"{\"a\":1}\n".to_vec()],
        )
        .await
        .unwrap();
    }
    (c, t2)
}

#[tokio::test]
async fn freeze_view_continue_revoke_and_purge() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("shares").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let (c, _) = conversation_with_turns(&r, &hermit).await;
    let alice = r.pigeon();
    let bob = r.pigeon();

    // nothing to share without a finished reply
    let empty = ev::conversation(&r.pigeon(), ALICE).await;
    assert_eq!(
        code(&alice.person(ALICE).create_share(empty).await.unwrap_err()),
        Some(ErrorCode::NothingToShare)
    );
    // read-only while viewing as
    let viewing = r.pigeon();
    let e = viewing
        .viewing_as(ALICE, "root@example.com", "s")
        .create_share(c)
        .await
        .unwrap_err();
    assert_eq!(code(&e), Some(ErrorCode::ViewAsReadOnly));

    let share = alice.person(ALICE).create_share(c).await.unwrap();
    assert_eq!(share.id.len(), 22);
    let listed = alice.person(ALICE).list_shares(c).await.unwrap();
    assert_eq!(listed.shares, vec![share.clone()]);

    // the view: the finished turn's rows exactly as the timeline has them; the running turn's are left out
    let page = alice.person(ALICE).timeline(c, &ev::tail(100)).await.unwrap();
    let upto: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select upto_seq from shares where id = $1", &[&share.id])
        .await
        .unwrap()
        .get(0);
    let user2: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select user_seq from turns where conversation_id = $1 and state = 'running'",
            &[&c],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(upto, user2 - 1);
    let view = bob.person(BOB).get_share(&share.id).await.unwrap();
    assert!(!view.is_owner);
    assert_eq!(view.owner_name, "alice");
    assert!(view.rows.len() >= 2);
    let texts: Vec<String> = view
        .rows
        .iter()
        .map(|r| serde_json::to_value(&r.item).unwrap()["text"].to_string())
        .collect();
    assert!(texts.iter().any(|t| t.contains("first answer")), "{texts:?}");
    assert!(
        !texts
            .iter()
            .any(|t| t.contains("second question") || t.contains("partial")),
        "{texts:?}"
    );
    let first_rows: Vec<_> = page
        .rows
        .iter()
        .filter(|row| row.seq_start <= view.rows.last().unwrap().seq_start)
        .cloned()
        .collect();
    assert_eq!(view.rows, first_rows);
    assert!(alice.person(ALICE).get_share(&share.id).await.unwrap().is_owner);

    // the frozen Claude Code state: the main transcript ends before the running turn's line
    let snap = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select f.size, (select count(*) from cc_lines l where l.file_id = f.id)
             from shares s join cc_files f on f.scope = s.snapshot where s.id = $1",
            &[&share.id],
        )
        .await
        .unwrap();
    assert_eq!((snap.get::<_, i64>(0), snap.get::<_, i64>(1)), (10, 1));

    // downloads: the send's snapshot; the frozen workspace, not the live one
    hermit.write_file(c, "report.txt", b"v2");
    let raw = |path: &str, sent: Option<&str>| RawFileQuery {
        path: path.into(),
        sent: sent.map(Into::into),
        inline: false,
    };
    let d = bob
        .person(BOB)
        .share_files_raw(&share.id, &raw("out.txt", Some("call_1")), None)
        .await
        .unwrap();
    assert_eq!(d.bytes().await.unwrap().as_ref(), b"sent snapshot");
    let d = bob
        .person(BOB)
        .share_files_raw(&share.id, &raw("report.txt", None), None)
        .await
        .unwrap();
    assert_eq!(d.bytes().await.unwrap().as_ref(), b"v1");
    let live = alice
        .person(ALICE)
        .files_raw(c, &raw("report.txt", None), None)
        .await
        .unwrap();
    assert_eq!(live.bytes().await.unwrap().as_ref(), b"v2");

    // Recently deleted hides the link; restore brings it back
    alice.person(ALICE).delete_conversation(c).await.unwrap();
    assert_eq!(
        code(&bob.person(BOB).get_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    alice.person(ALICE).restore_conversation(c).await.unwrap();
    bob.person(BOB).get_share(&share.id).await.unwrap();

    // continue: bob's own conversation with the share's rows, events and state, and a clone of the frozen workspace
    let cont = bob.person(BOB).continue_share(&share.id).await.unwrap();
    let mine = bob.person(BOB).list_conversations(&ListQuery::default()).await.unwrap();
    assert!(mine.conversations.iter().any(|x| x.id == cont.conversation_id));
    let conv = bob.person(BOB).get_conversation(cont.conversation_id).await.unwrap();
    assert_eq!(
        serde_json::to_value(&conv.origin).unwrap(),
        json!({"kind": "continue", "shareId": share.id})
    );
    assert_eq!(conv.claude_session_id.as_deref(), Some("s1"));
    let cpage = bob
        .person(BOB)
        .timeline(cont.conversation_id, &ev::tail(100))
        .await
        .unwrap();
    assert_eq!(cpage.rows, view.rows);
    let head: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select head_seq from conversations where id = $1",
            &[&cont.conversation_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(head, upto);
    assert_eq!(
        hermit.read_file(cont.conversation_id, "report.txt").as_deref(),
        Some(&b"v1"[..])
    );
    let panel = bob.person(BOB).files_panel(cont.conversation_id).await.unwrap();
    assert_eq!(panel.files.len(), 1);

    // revoke: only the owner; then 404 everywhere; the continued conversation is untouched
    assert_eq!(
        code(&bob.person(BOB).revoke_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    alice.person(ALICE).revoke_share(&share.id).await.unwrap();
    assert_eq!(
        code(&bob.person(BOB).get_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(
        code(&bob.person(BOB).continue_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(
        code(&alice.person(ALICE).revoke_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(alice.person(ALICE).list_shares(c).await.unwrap().shares.len(), 0);
    let left: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select (select count(*) from share_rows where share_id = $1)
                  + (select count(*) from blob_refs b join shares s on s.snapshot = b.owner where s.id = $1)
                  + (select count(*) from cc_files f join shares s on s.snapshot = f.scope where s.id = $1)",
            &[&share.id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);

    // a second share, then the source is purged: 404, and the continued conversation still reads
    let again = alice.person(ALICE).create_share(c).await.unwrap();
    alice.person(ALICE).delete_conversation(c).await.unwrap();
    alice.person(ALICE).purge_conversation(c).await.unwrap();
    assert_eq!(
        code(&bob.person(BOB).get_share(&again.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(
        bob.person(BOB)
            .timeline(cont.conversation_id, &ev::tail(100))
            .await
            .unwrap()
            .rows,
        view.rows
    );
    // the source's frozen workspaces and its own are queued for deletion
    let queued: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from outbox where kind = 'delete_workspace'", &[])
        .await
        .unwrap()
        .get(0);
    assert!(queued >= 3, "{queued}");
}

#[tokio::test]
async fn a_failed_clone_stores_nothing() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("sharefail").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let (c, _) = conversation_with_turns(&r, &hermit).await;
    hermit
        .faults()
        .times("clone_workspace", FaultKind::Unavailable { retry_after_secs: 1 }, 1);
    let e = r.pigeon().person(ALICE).create_share(c).await.unwrap_err();
    assert_eq!(code(&e), Some(ErrorCode::ShareFailed));
    let shares: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from shares", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(shares, 0);
    let share = r.pigeon().person(ALICE).create_share(c).await.unwrap();
    hermit
        .faults()
        .times("clone_workspace", FaultKind::Unavailable { retry_after_secs: 1 }, 1);
    let e = r.pigeon().person(BOB).continue_share(&share.id).await.unwrap_err();
    assert_eq!(code(&e), Some(ErrorCode::ContinueFailed));
    let theirs = r
        .pigeon()
        .person(BOB)
        .list_conversations(&ListQuery::default())
        .await
        .unwrap();
    assert_eq!(theirs.conversations.len(), 0);
}

#[tokio::test]
async fn a_share_freezes_only_rows_of_finished_events_and_continue_takes_the_frozen_state() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("sharefrozen").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let c = ev::conversation(&r.pigeon(), ALICE).await;
    hermit.write_file(c, "notes.txt", b"n");
    // a finished turn: an answer, a background tool call, a line the running turn will retract
    let t1 = Uuid::new_v4();
    let s1 = ev::append(
        r.pool(),
        ALICE,
        c,
        vec![
            user_sent("m1", "first question"),
            ev::in_turn(turn_started(t1), t1),
            ev::in_turn(assistant("msg_1", json!([text("first answer")])), t1),
            ev::in_turn(assistant("msg_2", json!([tool_use("call_bg", "Bash")])), t1),
            ev::in_turn(assistant("msg_3", json!([text("soon retracted")])), t1),
            ev::in_turn(turn_ended(t1, "completed"), t1),
        ],
    )
    .await;
    // the running turn: the background call's result lands in it, and it retracts a line of the finished turn
    let t2 = Uuid::new_v4();
    let s2 = ev::append(
        r.pool(),
        ALICE,
        c,
        vec![
            user_sent("m2", "second question"),
            ev::in_turn(turn_started(t2), t2),
            ev::in_turn(results(&[("call_bg", "background output", false)]), t2),
            ev::in_turn(retract(&[s1[4]]), t2),
        ],
    )
    .await;
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "insert into turns (id, conversation_id, origin, state, user_seq, start_seq) values ($1, $2, 'person', 'running', $3, $4)",
        &[&t2, &c, &s2[0], &s2[1]],
    )
    .await
    .unwrap();
    sql.execute(
        "update conversations set active_turn = $2, run_state = 'running', model = 'model-a' where id = $1",
        &[&c, &t2],
    )
    .await
    .unwrap();
    let share = r.pigeon().person(ALICE).create_share(c).await.unwrap();
    let upto: i64 = sql
        .query_one("select upto_seq from shares where id = $1", &[&share.id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(upto, s2[0] - 1);
    // in the source, the finished turn's tool row took the result, and the retraction is recorded
    let src = sql
        .query_one(
            "select (select count(*) from rows where conversation_id = $1 and row_id <= $2
                       and exists (select 1 from unnest(source_seqs) x where x > $2)),
                    (select count(*) from retracted where conversation_id = $1 and seq = $3)",
            &[&c, &upto, &s1[4]],
        )
        .await
        .unwrap();
    assert!(
        src.get::<_, i64>(0) >= 1,
        "no finished-turn row was changed by the running turn"
    );
    assert_eq!(src.get::<_, i64>(1), 1);
    // no frozen row has a source event of the running turn: the tool row the result updated is left out
    let late: i64 = sql
        .query_one(
            "select count(*) from share_rows where share_id = $1 and exists (select 1 from unnest(source_seqs) x where x > $2)",
            &[&share.id, &upto],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(late, 0);
    let view = r.pigeon().person(BOB).get_share(&share.id).await.unwrap();
    let items: Vec<String> = view
        .rows
        .iter()
        .map(|r| serde_json::to_value(&r.item).unwrap().to_string())
        .collect();
    assert!(items.iter().any(|t| t.contains("first answer")), "{items:?}");
    assert!(!items.iter().any(|t| t.contains("background output")), "{items:?}");
    // the source moves on: a new model; the continue takes the share's
    sql.execute("update conversations set model = 'model-b' where id = $1", &[&c])
        .await
        .unwrap();
    let cont = r.pigeon().person(BOB).continue_share(&share.id).await.unwrap();
    let row = sql
        .query_one(
            "select model, (select count(*) from retracted where conversation_id = $1),
                    (select count(*) from events where conversation_id = $1 and seq > $2),
                    (select count(*) from pending_workspaces)
             from conversations where id = $1",
            &[&cont.conversation_id, &upto],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("model-a"));
    assert_eq!(
        row.get::<_, i64>(1),
        0,
        "the running turn's retraction reached the copy"
    );
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(row.get::<_, i64>(3), 0, "a committed clone stays pending");
    let cpage = r
        .pigeon()
        .person(BOB)
        .timeline(cont.conversation_id, &ev::tail(100))
        .await
        .unwrap();
    assert_eq!(cpage.rows, view.rows);
    // the source in Recently deleted: continue answers 404 as the view does
    r.pigeon().person(ALICE).delete_conversation(c).await.unwrap();
    assert_eq!(
        code(&r.pigeon().person(BOB).continue_share(&share.id).await.unwrap_err()),
        Some(ErrorCode::NotFound)
    );
}

#[tokio::test]
async fn clones_not_yet_committed_count_as_referenced_and_expire() {
    let db = common::fresh_db("pendingws").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let (pending, stale, used) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let sql = r.pool().get().await.unwrap();
    for w in [pending, stale, used] {
        sql.execute("insert into pending_workspaces (workspace_id) values ($1)", &[&w])
            .await
            .unwrap();
    }
    let got = r
        .hermit()
        .service(None)
        .workspaces_exists(&magpie_api::ops::WorkspacesExistsRequest {
            ids: vec![pending, Uuid::new_v4()],
        })
        .await
        .unwrap();
    assert_eq!(got.exists, vec![pending]);
    // a replica that died after its clone: given up after an hour, its clone's deletion queued unless referenced
    let c = ev::conversation(&r.pigeon(), ALICE).await;
    sql.execute(
        "update pending_workspaces set workspace_id = $2, created_at = now() - interval '2 hours' where workspace_id = $1",
        &[&used, &c],
    )
    .await
    .unwrap();
    sql.execute(
        "update pending_workspaces set created_at = now() - interval '2 hours' where workspace_id = $1",
        &[&stale],
    )
    .await
    .unwrap();
    assert_eq!(magpie::jobs::retention::pass(r.pool()).await.unwrap(), 2);
    let left: Vec<Uuid> = sql
        .query("select workspace_id from pending_workspaces", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(left, vec![pending]);
    let queued: Vec<serde_json::Value> = sql
        .query("select payload from outbox where kind = 'delete_workspace'", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(queued[0].to_string().contains(&stale.to_string()), "{queued:?}");
}
