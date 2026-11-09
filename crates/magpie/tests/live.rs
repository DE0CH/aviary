//! Live frames (magpie 7.5): the receiving side (lease, `turn_not_active`, idempotent batches and deltas), the
//! `LiveState` in M9 and `GET …/live` (`liveAfter`, `reset`, `afterRowSeq`), supersede by the durable row, a held
//! poll woken by a frame, and the cleanup 60 s after the turn's end.

#![allow(
    clippy::assert_is_empty,
    clippy::items_after_statements,
    clippy::many_single_char_names,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines
)]

mod common;

use std::time::{Duration, Instant};

use common::ev::{self, ALICE};
use magpie_api::live::{BlockType, FrameOp, LiveFrame, LiveFramesRequest};
use magpie_api::rows::TimelineQuery;
use serde_json::json;
use uuid::Uuid;

fn frame(n: u64, op: FrameOp) -> LiveFrame {
    LiveFrame {
        n,
        message_id: Some("msg_1".into()),
        index: Some(0),
        op,
        block: None,
        name: None,
        offset: None,
        text: None,
        parent_tool_use_id: None,
        status: None,
        attempt: None,
        retry_in_ms: None,
        at: None,
    }
}

fn start(n: u64, index: u32) -> LiveFrame {
    LiveFrame {
        index: Some(index),
        block: Some(BlockType::Text),
        ..frame(n, FrameOp::Start)
    }
}

fn delta(n: u64, index: u32, offset: u64, text: &str) -> LiveFrame {
    LiveFrame {
        index: Some(index),
        offset: Some(offset),
        text: Some(text.into()),
        ..frame(n, FrameOp::Delta)
    }
}

struct World {
    _db: common::TestDb,
    a: common::Replica,
    sql: aviary_pg::Pool,
    id: Uuid,
    turn: Uuid,
    lease: Uuid,
    sandbox: Uuid,
}

/// A conversation with a running turn and a sandbox holding the lease.
async fn world(name: &str) -> World {
    let db = common::fresh_db(name).await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let id = ev::conversation(&a.pigeon(), ALICE).await;
    let turn = Uuid::new_v4();
    let lease = Uuid::new_v4();
    let sandbox = Uuid::new_v4();
    let c = sql.get().await.unwrap();
    c.execute(
        "insert into turns (id, conversation_id, origin, state) values ($1, $2, 'person', 'running')",
        &[&turn, &id],
    )
    .await
    .unwrap();
    c.execute(
        "update conversations set active_turn = $2, run_state = 'running', lease_id = $3, lease_sandbox = $4
         where id = $1",
        &[&id, &turn, &lease, &sandbox],
    )
    .await
    .unwrap();
    drop(c);
    World {
        _db: db,
        a,
        sql,
        id,
        turn,
        lease,
        sandbox,
    }
}

impl World {
    /// The supervisor's call as the gateway forwards it (the sandbox identity and the lease as headers); the error
    /// code on a refusal.
    async fn send_as(&self, turn: Uuid, lease: Uuid, frames: Vec<LiveFrame>) -> Result<(), (u16, String)> {
        let r = common::http()
            .post(format!("{}/v1/sandbox/turns/{turn}/live", self.a.url))
            .bearer_auth(&self.a.tokens.gateway)
            .header("aviary-sandbox-id", self.sandbox.to_string())
            .header("aviary-sandbox-conversation", self.id.to_string())
            .header("aviary-sandbox-person", ALICE)
            .header("aviary-lease", lease.to_string())
            .json(&LiveFramesRequest { frames })
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        if status == 204 {
            return Ok(());
        }
        let body: serde_json::Value = r.json().await.unwrap();
        Err((status, body["code"].as_str().unwrap_or_default().to_owned()))
    }

    async fn send(&self, turn: Uuid, frames: Vec<LiveFrame>) -> Result<(), (u16, String)> {
        self.send_as(turn, self.lease, frames).await
    }
}

#[tokio::test]
async fn frames_build_the_live_state_and_the_durable_row_supersedes_them() {
    let w = world("live").await;
    let alice_client = w.a.pigeon();
    let alice = alice_client.person(ALICE);
    assert!(alice.live(w.id).await.unwrap().is_none(), "no frames: 204");
    ev::append1(w.a.pool(), ALICE, w.id, ev::user_sent("c1", "hi")).await;
    w.send(
        w.turn,
        vec![start(1, 0), delta(2, 0, 0, "今日"), delta(3, 0, 6, "港珠澳")],
    )
    .await
    .unwrap();
    // a re-sent batch and a repeated delta change nothing
    w.send(w.turn, vec![delta(2, 0, 0, "今日"), delta(9, 0, 6, "港珠澳")])
        .await
        .unwrap();
    let l = alice.live(w.id).await.unwrap().unwrap();
    assert!(l.reset);
    assert_eq!(l.n, 3);
    assert_eq!(l.blocks.len(), 1);
    let b = &l.blocks[0];
    assert_eq!(
        (b.offset, b.text.as_str(), b.done, b.after_row_seq),
        (0, "今日港珠澳", false, 1)
    );
    // in M9: a tail page carries the state; an after poll with liveAfter gets only what is new
    let p = alice.timeline(w.id, &ev::tail(10)).await.unwrap();
    assert_eq!(p.live.as_ref().unwrap().blocks[0].text, "今日港珠澳");
    w.send(w.turn, vec![delta(4, 0, 15, "大橋"), frame(5, FrameOp::Stop)])
        .await
        .unwrap();
    let p = alice
        .timeline(
            w.id,
            &TimelineQuery {
                after: Some(1),
                live_after: Some(3),
                live_turn: Some(w.turn),
                ..TimelineQuery::default()
            },
        )
        .await
        .unwrap();
    let l = p.live.unwrap();
    assert!(!l.reset);
    assert_eq!(
        (l.n, l.blocks[0].offset, l.blocks[0].text.as_str(), l.blocks[0].done),
        (5, 15, "大橋", true)
    );
    // a liveAfter of another turn, even one lower than this turn's last frame: reset, whole blocks
    let p = alice
        .timeline(
            w.id,
            &TimelineQuery {
                after: Some(1),
                live_after: Some(3),
                live_turn: Some(Uuid::new_v4()),
                ..TimelineQuery::default()
            },
        )
        .await
        .unwrap();
    let l = p.live.unwrap();
    assert!(l.reset);
    assert_eq!((l.blocks[0].offset, l.blocks[0].text.as_str()), (0, "今日港珠澳大橋"));
    // a liveAfter beyond the turn's frames: reset, whole blocks
    let p = alice
        .timeline(
            w.id,
            &TimelineQuery {
                after: Some(1),
                live_after: Some(99),
                live_turn: Some(w.turn),
                ..TimelineQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(p.live.as_ref().unwrap().reset);
    assert_eq!(p.live.unwrap().blocks[0].text, "今日港珠澳大橋");
    // a held poll wakes on a frame
    let poll = {
        let pigeon = w.a.pigeon();
        let id = w.id;
        let turn = w.turn;
        tokio::spawn(async move {
            pigeon
                .person(ALICE)
                .timeline(
                    id,
                    &TimelineQuery {
                        after: Some(1),
                        live_after: Some(5),
                        live_turn: Some(turn),
                        wait_ms: Some(20_000),
                        ..TimelineQuery::default()
                    },
                )
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = Instant::now();
    w.send(w.turn, vec![start(6, 1), delta(7, 1, 0, "next")]).await.unwrap();
    let p = poll.await.unwrap();
    assert!(sent.elapsed() < Duration::from_secs(1));
    let l = p.live.unwrap();
    assert_eq!(l.blocks.len(), 1);
    assert_eq!((l.blocks[0].block_index, l.blocks[0].text.as_str()), (1, "next"));
    // the durable row of block 0 deletes its frames in its own transaction; a late frame of it is ignored
    ev::append1(
        w.a.pool(),
        ALICE,
        w.id,
        ev::in_turn(ev::assistant("msg_1", json!([ev::text("今日港珠澳大橋")])), w.turn),
    )
    .await;
    w.send(w.turn, vec![delta(8, 0, 21, "!")]).await.unwrap();
    let p = alice.timeline(w.id, &ev::tail(10)).await.unwrap();
    assert_eq!(p.rows.len(), 2);
    let l = p.live.unwrap();
    assert_eq!(l.blocks.iter().map(|b| b.block_index).collect::<Vec<_>>(), vec![1]);
    // live frames never take a row seq
    assert_eq!(p.max_seq, 2);
}

#[tokio::test]
async fn a_frame_for_a_turn_that_is_not_active_answers_409() {
    let w = world("live409").await;
    let e = w.send(Uuid::new_v4(), vec![start(1, 0)]).await.unwrap_err();
    assert_eq!(e, (409, "turn_not_active".to_owned()));
    // the turn ends: its frames are refused from then on
    w.send(w.turn, vec![start(1, 0)]).await.unwrap();
    w.sql
        .get()
        .await
        .unwrap()
        .execute("update conversations set active_turn = null where id = $1", &[&w.id])
        .await
        .unwrap();
    let e = w.send(w.turn, vec![delta(2, 0, 0, "x")]).await.unwrap_err();
    assert_eq!(e, (409, "turn_not_active".to_owned()));
    // another lease: lease_lost
    let e = w.send_as(w.turn, Uuid::new_v4(), vec![]).await.unwrap_err();
    assert_eq!(e, (409, "lease_lost".to_owned()));
}

#[tokio::test]
async fn lost_frames_and_gaps_reset_and_the_cleanup_sweeps_ended_turns() {
    let w = world("livegap").await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    w.send(w.turn, vec![start(1, 0), delta(2, 0, 0, "abc")]).await.unwrap();
    // the table lost the frames (a crash truncates an unlogged table); text resumes from the next frame
    w.sql
        .get()
        .await
        .unwrap()
        .execute("delete from live_frames", &[])
        .await
        .unwrap();
    w.send(w.turn, vec![delta(3, 0, 3, "def")]).await.unwrap();
    let q = TimelineQuery {
        after: Some(0),
        live_after: Some(2),
        live_turn: Some(w.turn),
        ..TimelineQuery::default()
    };
    let l = alice.timeline(w.id, &q).await.unwrap().live.unwrap();
    assert!(l.reset);
    assert_eq!((l.blocks[0].offset, l.blocks[0].text.as_str()), (3, "def"));
    // a gap frame past liveAfter resets
    w.send(w.turn, vec![frame(4, FrameOp::Gap)]).await.unwrap();
    let q = TimelineQuery {
        after: Some(0),
        live_after: Some(3),
        live_turn: Some(w.turn),
        ..TimelineQuery::default()
    };
    assert!(alice.timeline(w.id, &q).await.unwrap().live.unwrap().reset);
    // a status frame
    w.send(
        w.turn,
        vec![LiveFrame {
            message_id: None,
            index: None,
            status: Some("api_retry".into()),
            attempt: Some(2),
            retry_in_ms: Some(4000),
            ..frame(5, FrameOp::Status)
        }],
    )
    .await
    .unwrap();
    let s = alice.live(w.id).await.unwrap().unwrap().status.unwrap();
    assert_eq!(
        (s.status.as_str(), s.attempt, s.retry_in_ms),
        ("api_retry", Some(2), Some(4000))
    );
    // the turn ended 30 s ago: kept; 61 s ago: swept
    let c = w.sql.get().await.unwrap();
    c.execute(
        "update turns set ended_at = now() - interval '30 seconds' where id = $1",
        &[&w.turn],
    )
    .await
    .unwrap();
    assert_eq!(magpie::live::cleanup(w.a.pool()).await.unwrap(), 0);
    c.execute(
        "update turns set ended_at = now() - interval '61 seconds' where id = $1",
        &[&w.turn],
    )
    .await
    .unwrap();
    // another replica's pass holds one frame: this pass skips it and does not wait (skip locked)
    let turn = w.turn;
    let pool = w.a.pool().clone();
    let pool = &pool; // borrowed: the transaction may run more than once
    let swept = aviary_pg::tx!(&w.sql, |t| -> magpie::error::Result<(u64, Duration)> {
        t.execute(
            "select 1 from live_frames where turn_id = $1 and n = 4 for update",
            &[&turn],
        )
        .await?;
        let started = Instant::now();
        let n = magpie::live::cleanup(pool).await?;
        Ok((n, started.elapsed()))
    })
    .await
    .unwrap();
    assert_eq!(swept.0, 2);
    assert!(swept.1 < Duration::from_secs(2), "the pass waited on a locked frame");
    assert_eq!(magpie::live::cleanup(w.a.pool()).await.unwrap(), 1);
    assert!(alice.live(w.id).await.unwrap().is_none());
}
