//! Spec 7.1 / 13's transport limits and 11.2 / 18.3's drain cases on a real pigeon and the in-memory magpie model:
//! a socket that misses pings is dropped, the close codes of the limits (1009, 1008, 4403), the inbound rate limit,
//! rows streaming to open sockets while the drain runs, the 1012 after the reply it waited for, a large send arriving
//! across the drain, the WP-50 seams (an `Owners` that consults the hub, a conversation coming back from a delegate)
//! and the list micro-cache (per viewer, failures not kept), and spec 19's metric names.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use common::model::{self, Model};
use common::ws::Ws;
use common::{ADMIN, Harness, Opts, STAFF};
use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::rows::{AssistantItem, RowItem};
use magpie_api::turns::{TurnOrigin, TurnPhase};
use pigeon::Tuning;
use pigeon::feeds::{Delegate, Hub, Owners};
use pigeon::paseo::transport::Conn;
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    h: Harness,
    m: Model,
    _served: aviary_fakes::Served,
}

fn fast() -> Tuning {
    Tuning {
        poll_wait: Duration::from_secs(5),
        poll_backoff_min: Duration::from_millis(50),
        poll_backoff_max: Duration::from_millis(200),
        poller_linger: Duration::from_millis(300),
        drain_jitter: Duration::from_millis(1200),
        drain_rpc_wait: Duration::from_secs(3),
        socket_quiet: Duration::from_millis(300),
        ..common::tuning()
    }
}

async fn env_opts(tuning: Tuning, owners: Option<Arc<dyn Owners>>) -> Env {
    let m = Model::default();
    let served = model::serve(&m).await;
    let h = Harness::start_opts(Opts {
        tuning,
        magpie: Some(served.url()),
        owners,
        ..Opts::default()
    })
    .await;
    Env { h, m, _served: served }
}

async fn env_with(tuning: Tuning) -> Env {
    env_opts(tuning, None).await
}

impl Env {
    async fn ws(&self, email: &str) -> Ws {
        Ws::hello(&self.h.url, &self.h.token(email), "").await.0
    }

    fn hub(&self) -> Hub {
        self.h.app.hub().unwrap()
    }

    async fn sockets_become(&self, n: usize, within: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < within {
            if self.hub().sockets() == n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.hub().sockets() == n
    }

    async fn drain(&self, jitter: Duration, rpc_wait: Duration) -> tokio::task::JoinHandle<()> {
        self.drain_bound(jitter, rpc_wait, Duration::from_secs(30)).await
    }

    /// Starts the socket drain and returns once it has begun: its times count from then, on pigeon's clock.
    async fn drain_bound(&self, jitter: Duration, rpc_wait: Duration, bound: Duration) -> tokio::task::JoinHandle<()> {
        let socket = self.h.app.socket();
        let drain = tokio::spawn(async move { socket.drain(jitter, rpc_wait, bound).await });
        common::until("the drain began", || self.hub().draining()).await;
        drain
    }
}

fn assistant(text: &str) -> RowItem {
    RowItem::Assistant(AssistantItem {
        message_id: format!("msg_{text}"),
        block_index: 0,
        text: text.into(),
        error: None,
    })
}

fn is_stream(v: &Value, agent: Uuid) -> bool {
    v["message"]["type"] == "agent_stream" && v["message"]["payload"]["agentId"] == agent.to_string()
}

fn is_row(v: &Value, agent: Uuid, text: &str) -> bool {
    is_stream(v, agent)
        && v["message"]["payload"]["event"]["type"] == "timeline"
        && v["message"]["payload"]["event"]["item"]["text"] == text
}

async fn watch(ws: &mut Ws, ids: &[Uuid]) {
    let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    ws.rpc("agent.timeline.set_subscription.request", json!({"agentIds": ids}))
        .await;
}

/// A send request with one image whose base64 text is about `bytes` long.
fn image_send(rid: &str, c: Uuid, id: &str, bytes: usize) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(vec![7u8; bytes / 4 * 3]);
    json!({"type": "session", "message": {"type": "send_agent_message_request", "requestId": rid, "agentId": c,
        "text": "look", "messageId": id, "images": [{"data": data, "mimeType": "image/png"}]}})
    .to_string()
}

// ------------------------------------------------------------------------------------------------ transport limits

#[tokio::test]
async fn a_socket_that_misses_pings_is_dropped_and_unregistered() {
    let e = env_with(Tuning {
        socket_ping: Duration::from_millis(200),
        ..fast()
    })
    .await;
    let c = e.m.add(STAFF, None);
    let mut alive = e.ws(STAFF).await;
    let mut gone = e.ws(STAFF).await;
    watch(&mut gone, &[c]).await;
    assert_eq!(e.hub().sockets(), 2);
    assert_eq!(e.h.metric("pigeon_sockets{viewas=\"false\"}"), 2.0);
    // `gone` stops reading (a phone that left without a FIN): it never answers a ping. Each ping period of the clock,
    // `alive` reads the ping and answers it (and makes a request whose reply shows pigeon read the pong first).
    let ping = e.h.tuning.socket_ping;
    let mut periods = 0;
    while e.hub().sockets() > 1 {
        assert!(
            periods < 3,
            "the silent socket is still there after {periods} ping periods"
        );
        e.h.advance(ping);
        periods += 1;
        alive.answer_ping().await;
    }
    assert!(alive.closed.is_none(), "a socket that answers pings stays");
    // the socket's metrics are counted just after the hub let it go
    common::until("the close counted", || {
        e.h.metric("pigeon_socket_closes_total{code=\"1006\"}") >= 1.0
    })
    .await;
    assert_eq!(e.h.metric("pigeon_socket_closes_total{code=\"1006\"}"), 1.0);
    assert_eq!(e.h.metric("pigeon_sockets{viewas=\"false\"}"), 1.0);
    // the gone socket's subscription went with it: its timeline poller stops after the linger (ping periods go by,
    // which `alive` answers)
    while e.hub().pollers().1 > 0 {
        assert!(periods < 20, "the poller outlived its linger");
        e.h.advance(ping);
        periods += 1;
        alive.answer_ping().await;
    }
    let (_, r) = alive.rpc("fetch_agents_request", json!({})).await;
    assert!(r["entries"].is_array());
    drop(gone);
}

#[tokio::test]
async fn a_message_over_100_mib_is_closed_1009() {
    let e = env_with(fast()).await;
    let mut ws = e.ws(STAFF).await;
    // the header of a 100 MiB + 1 text frame is enough: the length is refused before the payload
    ws.raw_bytes(&Ws::text_header((100 << 20) + 1)).await;
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1009));
    assert!(e.sockets_become(0, Duration::from_secs(5)).await);
    // just under the limit is read (and, not being JSON, ignored)
    let mut ws = e.ws(STAFF).await;
    let len = 100u64 << 20;
    ws.raw_bytes(&Ws::text_header(len)).await;
    let chunk = vec![b'x'; 1 << 20];
    for _ in 0..100 {
        ws.raw_bytes(&chunk).await;
    }
    ws.raw(json!({"type": "ping"})).await;
    let pong = ws.wait_for("pong", |v| v["type"] == "pong").await;
    assert_eq!(pong, json!({"type": "pong"}));
}

#[tokio::test]
async fn a_persons_65th_socket_closes_the_oldest_1008() {
    let e = env_with(fast()).await;
    let mut first = e.ws(STAFF).await;
    let mut rest = Vec::new();
    for _ in 0..pigeon::feeds::MAX_PERSON_SOCKETS {
        rest.push(Ws::open(&e.h.url, &e.h.token(STAFF), "").await.unwrap());
    }
    assert_eq!(first.closed_code(Duration::from_secs(5)).await, Some(1008));
    assert!(
        e.sockets_become(pigeon::feeds::MAX_PERSON_SOCKETS, Duration::from_secs(5))
            .await
    );
    // another person is not affected
    let mut other = e.ws(common::STAFF2).await;
    other.raw(json!({"type": "ping"})).await;
    other.wait_for("pong", |v| v["type"] == "pong").await;
}

#[tokio::test]
async fn an_open_socket_closes_4403_when_no_longer_allowed() {
    let e = env_with(Tuning {
        socket_recheck: Duration::from_millis(300),
        ..fast()
    })
    .await;
    let mut ws = e.ws(STAFF).await;
    let mut admin = e.ws(ADMIN).await;
    let staff = e.hub().conns_of(STAFF);
    e.h.hermit.fake.configure(|c| {
        c.users = vec![ADMIN.into(), common::ADMIN2.into()];
    });
    // a recheck each period of the clock; the person's answer is cached a while, so it takes a few
    e.h.advance_until(Duration::from_millis(300), "the 4403", || {
        staff.iter().all(|c| c.is_closing())
    })
    .await;
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(4403));
    // still allowed: still open
    admin.raw(json!({"type": "ping"})).await;
    admin.wait_for("pong", |v| v["type"] == "pong").await;
    assert!(admin.closed.is_none());
}

#[tokio::test]
async fn inbound_frames_over_the_rate_are_dropped() {
    let e = env_with(fast()).await;
    let mut ws = e.ws(STAFF).await;
    // pigeon's clock does not move while they arrive: the burst of 400 frames is answered, the rest dropped
    for _ in 0..1_000 {
        ws.text(r#"{"type":"ping"}"#).await;
    }
    // all read (the last ones are the drops), then 10 ms of the clock refill two frames (200/s): a request whose reply
    // comes after every answer to the burst
    common::until("the burst read", || {
        e.h.metric("pigeon_frames_dropped_total{reason=\"rate\"}") >= 600.0
    })
    .await;
    e.h.advance(Duration::from_millis(10));
    ws.rpc("fetch_agents_request", json!({})).await;
    let pongs = ws
        .drain_for(Duration::ZERO)
        .await
        .iter()
        .filter(|v| v["type"] == "pong")
        .count();
    // the burst of 400 frames, the socket's hello the first of them
    assert_eq!(pongs, 399);
    assert_eq!(e.h.metric("pigeon_frames_dropped_total{reason=\"rate\"}"), 601.0);
    // the socket stays open and is served again once the clock refills it
    e.h.advance(Duration::from_millis(100));
    ws.raw(json!({"type": "ping"})).await;
    ws.wait_for("pong", |v| v["type"] == "pong").await;
}

// ------------------------------------------------------------------------------------------------ the drain

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rows_keep_streaming_to_open_sockets_until_their_1012() {
    let e = env_with(fast()).await;
    let c1 = e.m.add(STAFF, None);
    let c2 = e.m.add(STAFF, None);
    let c3 = e.m.add(STAFF, None);
    let mut a = e.ws(STAFF).await;
    let mut b = e.ws(STAFF).await;
    watch(&mut a, &[c1]).await;
    e.m.polling(c1).await;
    // both are busy with a send magpie holds: they close in the drain's second half, after their answers
    let held = e.m.hold("send_turn");
    for (ws, c, id) in [(&mut a, c2, "m-a"), (&mut b, c3, "m-b")] {
        let rid = ws.rid();
        ws.session(
            json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "hi",
                          "messageId": id}),
        )
        .await;
    }
    e.m.waiting_n("hold send_turn", 2).await;
    let drain = e.drain(Duration::from_millis(1200), Duration::from_secs(4)).await;
    // the drain's jitter passes on the clock (not its 4 s rpc wait): both sockets wait for their answers, the pollers
    // still run
    e.h.advance(Duration::from_millis(1300));
    let turn = e.m.start_turn(c1, TurnOrigin::Person);
    e.m.row(c1, Some(turn), assistant("during-1"));
    e.m.row(c1, Some(turn), assistant("during-2"));
    e.m.end_turn(c1, TurnPhase::Completed, None);
    a.wait_for("row 1", |v| is_row(v, c1, "during-1")).await;
    a.wait_for("row 2", |v| is_row(v, c1, "during-2")).await;
    a.wait_for("turn_completed", |v| {
        is_stream(v, c1) && v["message"]["payload"]["event"]["type"] == "turn_completed"
    })
    .await;
    // a subscription made during the drain gets live rows too
    watch(&mut b, &[c1]).await;
    e.m.polling(c1).await;
    e.m.row(c1, None, assistant("during-3"));
    b.wait_for("row 3", |v| is_row(v, c1, "during-3")).await;
    a.wait_for("row 3", |v| is_row(v, c1, "during-3")).await;
    assert!(a.closed.is_none() && b.closed.is_none());
    // the answers come, each followed by its 1012
    held.notify_waiters();
    let ra = a.next_session("send_agent_message_response").await;
    let rb = b.next_session("send_agent_message_response").await;
    assert_eq!(ra["accepted"], true);
    assert_eq!(rb["accepted"], true);
    assert_eq!(a.closed_code(Duration::from_secs(5)).await, Some(1012));
    assert_eq!(b.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
    // then the pollers stop
    assert_eq!(e.hub().pollers(), (0, 0));
    // each gauge goes down as its poller's task ends, after the hub let it go
    common::until("the pollers ended", || {
        e.h.metric("pigeon_pollers{kind=\"timeline\"}") == 0.0 && e.h.metric("pigeon_pollers{kind=\"change\"}") == 0.0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_1012_follows_the_reply_the_drain_waited_for() {
    // eight busy sockets on a multi-thread runtime, closed the moment their sends answer: each reply is written
    // before its close, every time
    let e = env_with(fast()).await;
    let held = e.m.hold("send_turn");
    let mut socks = Vec::new();
    for i in 0..8 {
        let c = e.m.add(STAFF, None);
        let mut ws = e.ws(STAFF).await;
        let rid = ws.rid();
        ws.session(
            json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "hi",
                          "messageId": format!("m{i}")}),
        )
        .await;
        socks.push((ws, rid));
    }
    e.m.waiting_n("hold send_turn", 8).await;
    let drain = e.drain(Duration::ZERO, Duration::from_secs(4)).await;
    held.notify_waiters();
    for (mut ws, rid) in socks {
        let r = ws.next_session("send_agent_message_response").await;
        assert_eq!(r["requestId"], rid.as_str());
        assert_eq!(r["accepted"], true);
        assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    }
    drain.await.unwrap();
}

/// One ping period of pigeon's clock: the clock moves `ping`, and each of `socks` reads its ping, answers it and makes
/// a request whose reply shows pigeon read the pong (a socket that answers is never dropped for a missed pong).
async fn ping_round(e: &Env, socks: &mut [Ws], ping: Duration) {
    e.h.advance(ping);
    for ws in socks.iter_mut() {
        ws.answer_ping().await;
    }
}

/// Sends `frame` as one WebSocket message in `chunk`-byte pieces, pigeon's clock moving `gap` after each, starting the
/// drain (`jitter`, `rpc_wait`, transfer bound 30 s) once `drain_after` bytes are in; returns the drain.
async fn send_slowly(
    e: &Env,
    ws: &mut Ws,
    frame: &str,
    chunk: usize,
    gap: Duration,
    drain_after: usize,
    (jitter, rpc_wait): (Duration, Duration),
) -> tokio::task::JoinHandle<()> {
    let mut bytes = Ws::text_header(frame.len() as u64);
    bytes.extend_from_slice(frame.as_bytes());
    let mut drain = None;
    let mut sent = 0;
    for piece in bytes.chunks(chunk) {
        ws.raw_bytes(piece).await;
        sent += piece.len();
        if drain.is_none() && sent >= drain_after {
            // pigeon has read the message's first frame header: the drain sees a message arriving
            common::until("the message arriving", || {
                e.hub().conns_of(STAFF).iter().any(|c| c.arriving())
            })
            .await;
            drain = Some(e.drain(jitter, rpc_wait).await);
        }
        e.h.advance(gap);
    }
    drain.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_send_arriving_across_the_drain_is_answered_before_its_1012() {
    // 18.3's 40 MiB image send, scaled: 12 MiB arriving over 4.8 s of the clock as one WebSocket message, with the
    // in-flight request wait at 1 s: the send outlasts jitter + rpc_wait (2.2 s) by far, and has the transfer bound
    let e = env_with(fast()).await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let mut idle = e.ws(STAFF).await;
    // past the quiet time: `idle` is request-free
    e.h.advance(Duration::from_millis(400));
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-big", 12 << 20);
    let drain = send_slowly(
        &e,
        &mut ws,
        &frame,
        256 << 10,
        Duration::from_millis(100),
        2 << 20,
        (Duration::from_millis(1200), Duration::from_secs(1)),
    )
    .await;
    // the socket drain is still waiting (so the process does not exit under the send)
    assert!(!drain.is_finished());
    // the request-free socket went in the first half; the arriving one was not cut
    assert_eq!(idle.closed_code(Duration::from_secs(3)).await, Some(1012));
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
    let uploads = e.m.st().uploads.clone();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].1, (12 << 20) / 4 * 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spec scale: about 170 s"]
async fn a_40_mib_image_send_at_2_mbit_s_across_the_drain_gets_its_reply_before_its_1012() {
    // spec 18.3 at its own scale and pigeon's own drain times (jitter 15 s, in-flight requests 10 s, transfers 300 s,
    // pings every 30 s): 40 MiB at 2 Mbit/s, SIGTERM 10 s in
    let e = env_with(Tuning {
        drain_jitter: Duration::from_secs(15),
        drain_rpc_wait: Duration::from_secs(10),
        socket_quiet: Duration::from_secs(5),
        socket_ping: Duration::from_secs(30),
        ..fast()
    })
    .await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-40", 40 << 20);
    let started = Instant::now();
    // 2 Mbit/s = 250 000 bytes/s: 25 000 bytes every 100 ms; SIGTERM after 2.5 MB (10 s)
    let drain = {
        let mut bytes = Ws::text_header(frame.len() as u64);
        bytes.extend_from_slice(frame.as_bytes());
        let mut drain = None;
        let mut sent = 0;
        let mut next = tokio::time::Instant::now();
        for piece in bytes.chunks(25_000) {
            ws.raw_bytes(piece).await;
            sent += piece.len();
            if drain.is_none() && sent >= 2_500_000 {
                let socket = e.h.app.socket();
                drain = Some(tokio::spawn(async move {
                    socket
                        .drain(
                            Duration::from_secs(15),
                            Duration::from_secs(10),
                            Duration::from_secs(300),
                        )
                        .await;
                }));
            }
            next += Duration::from_millis(100);
            tokio::time::sleep_until(next).await;
        }
        drain.unwrap()
    };
    let sending = started.elapsed();
    assert!(sending > Duration::from_secs(150), "{sending:?}");
    assert!(!drain.is_finished());
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
    println!(
        "sent in {sending:?}; drain ended {:?} after the start",
        started.elapsed()
    );
    assert_eq!(e.m.st().uploads[0].1, (40 << 20) / 4 * 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_whose_first_bytes_arrived_just_before_its_1012_is_not_cut() {
    // the drain sees a message arriving from its first frame header, not after some threshold of bytes
    let e = env_with(fast()).await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let mut idle = e.ws(STAFF).await;
    // past the quiet time: `idle` is request-free
    e.h.advance(Duration::from_millis(400));
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-start", 1 << 20);
    // 4 KiB in, then the drain with no jitter: a request-free socket would be closed at once
    let drain = send_slowly(
        &e,
        &mut ws,
        &frame,
        4 << 10,
        Duration::from_millis(5),
        4 << 10,
        (Duration::ZERO, Duration::from_secs(1)),
    )
    .await;
    assert_eq!(idle.closed_code(Duration::from_secs(2)).await, Some(1012));
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
    assert_eq!(e.m.st().uploads.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_that_begins_while_the_queue_ahead_of_its_1012_is_written_is_not_cut() {
    // the drain decides to close a request-free socket whose client is not reading: its 1012 is queued behind
    // several MiB of rows. The client then begins a send and only then reads the rows. The 1012 waits for that
    // send's reply instead of cutting the message half-read
    let e = env_with(Tuning {
        // no pings: the client's own pongs would land inside its hand-written frame
        socket_ping: Duration::from_mins(10),
        ..fast()
    })
    .await;
    let watched = e.m.add(STAFF, None);
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    watch(&mut ws, &[watched]).await;
    // the poller has read its starting point: every row from here on is live
    e.m.polling(watched).await;
    let conn = e.hub().conns_of(STAFF).pop().unwrap();
    // the client reads nothing: rows pile up in the socket's queue once the TCP buffers are full
    let big = "x".repeat(60_000);
    let mut n = 0;
    // ten rows at a time, each batch pushed by the poller before the queue is looked at (no more rows than it takes)
    while conn.queued_bytes() < 2 << 20 {
        assert!(n < 1_000, "the rows never queued up");
        let polls = e.m.polls_of(watched);
        for _ in 0..10 {
            n += 1;
            e.m.row(watched, None, assistant(&format!("{n}-{big}")));
        }
        e.m.polled_after(watched, polls).await;
    }
    // every row is in the queue (or written) once the poller holds its poll again: it has pushed all of them
    e.m.polling(watched).await;
    assert!(conn.queued_bytes() > 0);
    // request-free (nothing but rows for the quiet time of the clock), no jitter: the 1012 is queued behind the rows
    e.h.advance(Duration::from_millis(400));
    assert!(conn.request_free());
    let drain = e.drain(Duration::ZERO, Duration::from_secs(1)).await;
    assert!(!conn.busy());
    // now the send begins; the client reads the rows between its chunks
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-late", 1 << 20);
    let mut bytes = Ws::text_header(frame.len() as u64);
    bytes.extend_from_slice(frame.as_bytes());
    let mut got = Vec::new();
    for piece in bytes.chunks(16 << 10) {
        ws.raw_bytes(piece).await;
        while let Some(v) = ws.next_raw(Duration::from_millis(5)).await {
            got.push(v);
        }
    }
    let reply = loop {
        if let Some(v) = got
            .iter()
            .find(|v| v["type"] == "session" && v["message"]["type"] == "send_agent_message_response")
        {
            break v["message"]["payload"].clone();
        }
        let Some(v) = ws.next_raw(Duration::from_secs(10)).await else {
            panic!("no reply: closed {:?} after {} frames", ws.closed, got.len());
        };
        got.push(v);
    };
    assert_eq!(reply["requestId"], rid.as_str());
    assert_eq!(reply["accepted"], true);
    // every row is written, and the 1012 comes after them and the reply
    while let Some(v) = ws.next_raw(Duration::from_secs(5)).await {
        got.push(v);
    }
    assert_eq!(ws.closed.as_ref().map(|c| c.0), Some(1012));
    let rows = got
        .iter()
        .filter(|v| is_stream(v, watched) && v["message"]["payload"]["event"]["type"] == "timeline")
        .count();
    let before = got
        .iter()
        .take_while(|v| v["message"]["type"] != "send_agent_message_response")
        .filter(|v| is_stream(v, watched))
        .count();
    eprintln!("rows {rows} of {n}, {before} before the reply");
    assert_eq!(rows, n);
    drain.await.unwrap();
    assert_eq!(e.m.st().uploads.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_sending_one_long_frame_is_not_dropped_for_the_pongs_it_cannot_send() {
    // a client part-way through one frame cannot send a pong until the frame ends
    let e = env_with(Tuning {
        socket_ping: Duration::from_millis(200),
        ..fast()
    })
    .await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-long", 2 << 20);
    let mut bytes = Ws::text_header(frame.len() as u64);
    bytes.extend_from_slice(frame.as_bytes());
    // 1.6 s of the clock: eight ping periods. Each piece is read by pigeon before the clock moves on, so at every ping
    // bytes have arrived since the last one (alive), though no pong can
    for piece in bytes.chunks(bytes.len() / 32 + 1) {
        let before = e.hub().inbound_bytes();
        ws.raw_bytes(piece).await;
        common::until("pigeon read the piece", || {
            e.hub().inbound_bytes() >= before + piece.len() as u64
        })
        .await;
        e.h.advance(Duration::from_millis(50));
    }
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true);
    assert!(ws.closed.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_send_waiting_for_a_large_message_slot_is_not_cut() {
    // pings every 400 ms of the clock: the parked socket reads no pong for 3.6 s and must not be dropped for it; the
    // in-flight request wait (1 s) is shorter than the wait for a slot: a large send has the transfer bound
    let ping = Duration::from_millis(400);
    let e = env_with(Tuning {
        socket_ping: ping,
        ..fast()
    })
    .await;
    let held = e.m.hold("send_turn");
    let convs: Vec<Uuid> = (0..=pigeon::feeds::LARGE_AT_ONCE)
        .map(|_| e.m.add(STAFF, None))
        .collect();
    let frames: Vec<String> = convs
        .iter()
        .enumerate()
        .map(|(i, c)| image_send(&format!("req-{}", 1), *c, &format!("h{i}"), 9 << 20))
        .collect();
    // three large sends hold the replica's three large-message slots (their handlers wait at magpie)
    let mut socks = Vec::new();
    for frame in &frames[..pigeon::feeds::LARGE_AT_ONCE] {
        let mut ws = e.ws(STAFF).await;
        let _ = ws.rid();
        ws.text(frame).await;
        socks.push(ws);
    }
    e.m.waiting_n("hold send_turn", pigeon::feeds::LARGE_AT_ONCE).await;
    // the fourth waits for a slot, its message begun and not read (the listener reads it once a slot is free, so
    // its client's send ends only then)
    let mut waiting = e.ws(STAFF).await;
    let rid = waiting.rid();
    let last = frames[pigeon::feeds::LARGE_AT_ONCE].clone();
    let before = e.hub().inbound_bytes();
    let sending = tokio::spawn(async move {
        waiting.text(&last).await;
        waiting
    });
    common::until("its message begun", || e.hub().inbound_bytes() > before).await;
    let drain = e.drain(Duration::from_millis(1200), Duration::from_secs(1)).await;
    // past the whole jitter and the in-flight wait, nine ping periods: all still open
    for _ in 0..9 {
        ping_round(&e, &mut socks, ping).await;
    }
    for ws in &socks {
        assert!(ws.closed.is_none());
    }
    assert!(
        !sending.is_finished(),
        "the fourth large message was read before a slot was free"
    );
    held.notify_waiters();
    let mut waiting = sending.await.unwrap();
    assert!(waiting.closed.is_none(), "the socket waiting for a slot was cut");
    let r = waiting.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(waiting.closed_code(Duration::from_secs(5)).await, Some(1012));
    for mut h in socks {
        assert_eq!(h.next_session("send_agent_message_response").await["accepted"], true);
        assert_eq!(h.closed_code(Duration::from_secs(5)).await, Some(1012));
    }
    drain.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_message_waiting_for_a_slot_is_not_read_and_its_socket_ends_at_the_transfer_bound() {
    // pings every 200 ms of the clock, the transfer bound 1.5 s. While the three slots are held, a fourth large message
    // is not read at all (pigeon 7.5.4: the slot bounds the large messages in memory, so it is had before the bytes are
    // read; before, every socket of a person could hold a 100 MiB message waiting). Its socket reads nothing
    // meanwhile, pongs included, so it counts as alive up to the transfer bound and is then dropped.
    let ping = Duration::from_millis(200);
    let e = env_with(Tuning {
        socket_ping: ping,
        transfer_bound: Duration::from_millis(1500),
        ..fast()
    })
    .await;
    let held = e.m.hold("send_turn");
    let convs: Vec<Uuid> = (0..=pigeon::feeds::LARGE_AT_ONCE)
        .map(|_| e.m.add(STAFF, None))
        .collect();
    let frames: Vec<String> = convs
        .iter()
        .enumerate()
        .map(|(i, c)| image_send(&format!("req-{}", 1), *c, &format!("d{i}"), 9 << 20))
        .collect();
    // three large sends hold the slots; their clients answer every ping until their replies
    let mut holders = Vec::new();
    for frame in &frames[..pigeon::feeds::LARGE_AT_ONCE] {
        let mut ws = e.ws(STAFF).await;
        let _ = ws.rid();
        ws.text(frame).await;
        holders.push(ws);
    }
    e.m.waiting_n("hold send_turn", pigeon::feeds::LARGE_AT_ONCE).await;
    let open = e.hub().sockets();
    let mut waiting = e.ws(STAFF).await;
    assert!(e.sockets_become(open + 1, Duration::from_secs(5)).await);
    let before = e.hub().inbound_bytes();
    let last = frames[pigeon::feeds::LARGE_AT_ONCE].clone();
    // its client's write fails once the socket is dropped (the helper panics on it, ending only this task)
    let sending = tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(30), waiting.text(&last)).await;
    });
    common::until("its message begun", || e.hub().inbound_bytes() > before).await;
    // 1 s of the clock (five ping periods): one read's bytes at most, the socket still there
    for _ in 0..5 {
        ping_round(&e, &mut holders, ping).await;
    }
    let read = e.hub().inbound_bytes().saturating_sub(before);
    assert!(read < 1 << 20, "{read} bytes of a message waiting for a slot were read");
    assert_eq!(e.hub().sockets(), open + 1, "dropped before the transfer bound");
    // past the 1.5 s bound (and the ping that notices it): dropped
    let mut periods = 5;
    while e.hub().sockets() > open {
        assert!(
            periods < 10,
            "a socket waiting for a slot stays past the transfer bound"
        );
        ping_round(&e, &mut holders, ping).await;
        periods += 1;
    }
    held.notify_waiters();
    for mut h in holders {
        assert_eq!(
            h.next_session("send_agent_message_response").await["accepted"],
            true,
            "a slot holder's send"
        );
    }
    let _ = sending.await;
}

// ------------------------------------------------------------------------------------------------ WP-50's seams

/// An `Owners` that consults the hub (as WP-50's may) and delegates the conversations in `delegated`.
#[derive(Default)]
struct Consulting {
    hub: OnceLock<Hub>,
    delegated: std::sync::Mutex<Option<Uuid>>,
    consulted: AtomicBool,
    delegate: Arc<Recording>,
}

#[derive(Default)]
struct Recording {
    subscribed: std::sync::Mutex<Vec<Uuid>>,
}

impl Delegate for Recording {
    fn request(&self, _socket: Arc<Conn>, _kind: String, _message: Value) -> pigeon::feeds::BoxFuture {
        Box::pin(async {})
    }
    fn subscribe(&self, _socket: Arc<Conn>, conversation: Uuid) {
        self.subscribed.lock().unwrap().push(conversation);
    }
    fn unsubscribe(&self, _socket: &Arc<Conn>, _conversation: Uuid) {}
}

impl Owners for Consulting {
    fn delegate(&self, person: &str, conversation: &Uuid) -> Option<Arc<dyn Delegate>> {
        if let Some(hub) = self.hub.get() {
            // would deadlock if called under the hub's lock
            let _ = hub.timeline_subscribers(person, conversation);
            let _ = hub.conns_of(person);
            self.consulted.store(true, Ordering::SeqCst);
        }
        let d = *self.delegated.lock().unwrap();
        (d == Some(*conversation)).then(|| self.delegate.clone() as Arc<dyn Delegate>)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owners_that_consults_the_hub_and_a_conversation_back_from_a_delegate() {
    let owners = Arc::new(Consulting::default());
    let e = env_opts(
        Tuning {
            poller_linger: Duration::from_secs(10),
            ..fast()
        },
        Some(owners.clone()),
    )
    .await;
    owners.hub.set(e.hub()).ok().unwrap();
    let c = e.m.add(STAFF, None);
    e.m.row(c, None, assistant("old"));
    let mut ws = e.ws(STAFF).await;
    watch(&mut ws, &[c]).await;
    e.m.polling(c).await;
    // the poller's tick (each second of real time, only a look at the state) asks the owner outside the hub's lock
    common::until("the owner consulted", || owners.consulted.load(Ordering::SeqCst)).await;
    e.m.row(c, None, assistant("native-1"));
    ws.wait_for("native-1", |v| is_row(v, c, "native-1")).await;
    // the delegate takes the conversation (WP-50 hands it over); the native poller lingers but sends nothing, also
    // not for a history rewrite meanwhile
    *owners.delegated.lock().unwrap() = Some(c);
    let polls = e.m.polls_of(c);
    e.m.row(c, None, assistant("delegated"));
    let epoch = e.m.rewrite(c);
    // the native poller has read both and holds again: whatever it would send is queued by now
    e.m.polled_after(c, polls).await;
    let during = ws.drain_for(Duration::ZERO).await;
    assert!(
        !during
            .iter()
            .any(|v| is_stream(v, c) || v["message"]["type"] == "agent.timeline.replacement"),
        "the native poller sent beside the delegate: {during:?}"
    );
    assert_eq!(e.hub().pollers().1, 1, "the native poller lingers");
    // back to native: one replacement, then only new rows, in the new epoch
    *owners.delegated.lock().unwrap() = None;
    e.hub().owner_changed(STAFF, c, &epoch);
    let rep = ws
        .wait_for("replacement", |v| v["message"]["type"] == "agent.timeline.replacement")
        .await;
    assert_eq!(rep["message"]["payload"]["epoch"], epoch.as_str());
    // the poller restarted from the new tail and holds its poll
    let polls = e.m.polls_of(c);
    e.m.polled_after(c, polls).await;
    e.m.row(c, None, assistant("native-2"));
    let v = ws.wait_for("native-2", |v| is_row(v, c, "native-2")).await;
    assert_eq!(v["message"]["payload"]["epoch"], epoch.as_str());
    // nothing else up to a last row: no old row sent again, no second replacement
    e.m.row(c, None, assistant("native-3"));
    let mut rest = Vec::new();
    loop {
        let v = ws
            .next_raw(Duration::from_secs(10))
            .await
            .unwrap_or_else(|| panic!("native-3"));
        if is_row(&v, c, "native-3") {
            break;
        }
        rest.push(v);
    }
    assert!(
        !rest
            .iter()
            .any(|v| v["message"]["type"] == "agent.timeline.replacement" || is_stream(v, c)),
        "{rest:?}"
    );
}

// ------------------------------------------------------------------------------------------------ the list cache

#[tokio::test]
async fn list_calls_carry_the_viewer_and_failures_are_not_kept() {
    let e = env_with(fast()).await;
    e.m.add(STAFF, Some("theirs"));
    let vt = e.h.view_token(ADMIN, STAFF, common::now() + 600);
    let (mut va, _) = Ws::hello(
        &e.h.url,
        &e.h.token(ADMIN),
        &format!("?{}={vt}", pigeon::paseo::compat::VIEW_AS_QUERY),
    )
    .await;
    let mut own = e.ws(STAFF).await;
    let (_, r) = own.rpc("fetch_agents_request", json!({})).await;
    assert_eq!(r["entries"].as_array().unwrap().len(), 1);
    let (_, r) = va.rpc("fetch_agents_request", json!({})).await;
    assert_eq!(r["entries"].as_array().unwrap().len(), 1);
    // two calls (the view-as socket does not share the person's cached answer), the second with the viewer
    let calls: Vec<_> =
        e.m.st()
            .calls
            .iter()
            .filter(|c| c.0 == "list_conversations")
            .cloned()
            .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].2, None);
    assert_eq!(calls[1].1.as_deref(), Some(STAFF));
    assert_eq!(calls[1].2.as_deref(), Some(ADMIN));
    // a failed answer is not kept for the cache's 2 s: the next request asks again
    e.h.advance(pigeon::feeds::LIST_CACHE);
    e.m.refuse_once(
        "list_conversations",
        Refusal::new(ErrorCode::Unavailable, "down").with_status(503),
    );
    let (k, _) = own.rpc("fetch_agents_request", json!({})).await;
    assert_eq!(k, "rpc_error");
    let (k, r) = own.rpc("fetch_agents_request", json!({})).await;
    assert_eq!(k, "fetch_agents_response", "{r}");
    assert_eq!(e.m.calls_to("list_conversations"), 4);
}

// ------------------------------------------------------------------------------------------------ metrics

#[tokio::test]
async fn the_socket_metrics_carry_spec_19s_names_and_labels() {
    let e = env_with(fast()).await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    watch(&mut ws, &[c]).await;
    ws.rpc("fetch_agents_request", json!({})).await;
    let (k, _) = ws.rpc_invalid("no_such_request", json!({})).await;
    assert_eq!(k, "rpc_error");
    e.m.refuse_once(
        "get_conversation",
        Refusal::new(ErrorCode::Unavailable, "down").with_status(503),
    );
    let (k, _) = ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "rpc_error");
    // both pollers have read once and hold their polls
    e.m.polling(c).await;
    e.m.polling_changes(STAFF).await;
    let text = e.h.metrics();
    for want in [
        "pigeon_hello_total{protocol=\"1\",clientType=\"browser\"} 1",
        "pigeon_pollers{kind=\"change\"} 1",
        "pigeon_pollers{kind=\"timeline\"} 1",
        "pigeon_rpc_seconds_count{type=\"fetch_agents_request\"} 1",
        "pigeon_frames_out_total{type=\"status\"}",
        "pigeon_frames_out_total{type=\"fetch_agents_response\"} 1",
        "pigeon_rpc_errors_total{type=\"other\",code=\"unsupported\"} 1",
        "pigeon_rpc_errors_total{type=\"fetch_agent_request\",code=",
        "pigeon_poll_seconds_count{kind=\"change\"}",
        "pigeon_poll_seconds_count{kind=\"timeline\"}",
    ] {
        assert!(text.contains(want), "no {want} in\n{text}");
    }
}

#[tokio::test]
async fn an_expired_change_cursor_whose_head_read_fails_backs_off_before_the_resync() {
    // 410 on the cursor, then the head read fails four times: the poller retries the head with its backoff (50,
    // 100, 200, 200 ms of the clock), never the expired cursor in a hot loop, and only then makes the sockets
    // re-snapshot
    let e = env_with(fast()).await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    e.m.polling_changes(STAFF).await;
    e.m.refuse_once("changes", Refusal::new(ErrorCode::CursorExpired, "expired"));
    for _ in 0..4 {
        e.m.refuse_once("changes", Refusal::new(ErrorCode::Unavailable, "down"));
    }
    let before = e.m.calls_to("changes");
    let clock = e.h.app.clock();
    let started = clock.now();
    // a change ends the long-poll in flight; the next poll meets the 410; the clock moves 10 ms at a time
    e.m.edit(c, |conv| conv.title = Some("x".into()));
    e.h.advance_until(Duration::from_millis(10), "the re-snapshot's 1012", || {
        e.h.metric("pigeon_socket_closes_total{code=\"1012\"}") >= 1.0
    })
    .await;
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    let took = clock.elapsed(started);
    assert!(took >= Duration::from_millis(550), "the backoff: {took:?}");
    // the poll, the 410, four failed head reads and the one that worked
    let calls = e.m.calls_to("changes") - before;
    assert!(calls <= 8, "{calls} calls");
}
