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

    fn drain(&self, jitter: Duration, rpc_wait: Duration) -> tokio::task::JoinHandle<()> {
        let socket = self.h.app.socket();
        tokio::spawn(async move { socket.drain(jitter, rpc_wait).await })
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
    // `gone` stops reading (a phone that left without a FIN): it never answers a ping. `alive` keeps reading, which
    // answers them.
    let started = Instant::now();
    let reader = tokio::spawn(async move {
        // reading answers the pings (tungstenite replies as it reads)
        let _ = tokio::time::timeout(Duration::from_millis(1500), alive.next_raw(Duration::from_secs(5))).await;
        alive
    });
    assert!(
        e.sockets_become(1, Duration::from_secs(2)).await,
        "{}",
        e.hub().sockets()
    );
    // within about two ping periods (the one it missed and the next)
    assert!(
        started.elapsed() < Duration::from_millis(1200),
        "{:?}",
        started.elapsed()
    );
    let mut alive = reader.await.unwrap();
    assert!(alive.closed.is_none(), "a socket that answers pings stays");
    assert_eq!(e.h.metric("pigeon_sockets{viewas=\"false\"}"), 1.0);
    assert_eq!(e.h.metric("pigeon_socket_closes_total{code=\"1006\"}"), 1.0);
    // the gone socket's subscription went with it: its timeline poller stops after the linger
    let started = Instant::now();
    while e.hub().pollers().1 > 0 && started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(e.hub().pollers().1, 0);
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
    e.h.hermit.fake.configure(|c| {
        c.users = vec![ADMIN.into(), common::ADMIN2.into()];
    });
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
    let started = Instant::now();
    for _ in 0..1_000 {
        ws.text(r#"{"type":"ping"}"#).await;
    }
    let sent_in = started.elapsed();
    let mut pongs = 0usize;
    while let Some(v) = ws.next_raw(Duration::from_millis(500)).await {
        if v["type"] == "pong" {
            pongs += 1;
        }
    }
    // the burst of 400 plus 200/s while they were sent
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let allowed = 400 + (sent_in.as_secs_f64() * 200.0).ceil() as usize + 1;
    assert!(pongs >= 400 && pongs <= allowed, "{pongs} pongs, {allowed} allowed");
    assert!(pongs < 1_000);
    #[allow(clippy::cast_precision_loss)]
    let dropped = (1_000 - pongs) as f64;
    assert_eq!(e.h.metric("pigeon_frames_dropped_total{reason=\"rate\"}"), dropped);
    // the socket stays open and is served again after a moment
    tokio::time::sleep(Duration::from_millis(100)).await;
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
    tokio::time::sleep(Duration::from_millis(300)).await;
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let drain = e.drain(Duration::from_millis(1200), Duration::from_secs(4));
    // well past the pollers' 1 s tick: the pollers still run
    tokio::time::sleep(Duration::from_millis(1100)).await;
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
    tokio::time::sleep(Duration::from_millis(200)).await;
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
    let started = Instant::now();
    while e.h.metric("pigeon_pollers{kind=\"timeline\"}") > 0.0 && started.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(e.h.metric("pigeon_pollers{kind=\"timeline\"}"), 0.0);
    assert_eq!(e.h.metric("pigeon_pollers{kind=\"change\"}"), 0.0);
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let drain = e.drain(Duration::ZERO, Duration::from_secs(4));
    tokio::time::sleep(Duration::from_millis(100)).await;
    held.notify_waiters();
    for (mut ws, rid) in socks {
        let r = ws.next_session("send_agent_message_response").await;
        assert_eq!(r["requestId"], rid.as_str());
        assert_eq!(r["accepted"], true);
        assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    }
    drain.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_send_arriving_across_the_drain_is_answered_before_its_1012() {
    // 18.3's 40 MiB image send, scaled: 12 MiB arriving over about 2 s as one WebSocket message
    let e = env_with(fast()).await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let mut idle = e.ws(STAFF).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rid = ws.rid();
    let frame = image_send(&rid, c, "m-big", 12 << 20);
    let mut bytes = Ws::text_header(frame.len() as u64);
    bytes.extend_from_slice(frame.as_bytes());
    let chunks: Vec<&[u8]> = bytes.chunks(256 << 10).collect();
    let mut drain = None;
    for (i, chunk) in chunks.iter().enumerate() {
        ws.raw_bytes(chunk).await;
        if i == 8 {
            // SIGTERM with 2 MiB of the message in
            drain = Some(e.drain(Duration::from_millis(1200), Duration::from_secs(4)));
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    // the request-free socket went in the first half; the arriving one was not cut
    assert_eq!(idle.closed_code(Duration::from_secs(3)).await, Some(1012));
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.unwrap().await.unwrap();
    let uploads = e.m.st().uploads.clone();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].1, (12 << 20) / 4 * 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_send_waiting_for_a_large_message_slot_is_not_cut() {
    let e = env_with(fast()).await;
    let held = e.m.hold("send_turn");
    // three large sends hold the replica's three large-message slots (their handlers wait at magpie)
    let mut holders = Vec::new();
    for i in 0..pigeon::feeds::LARGE_AT_ONCE {
        let c = e.m.add(STAFF, None);
        let mut ws = e.ws(STAFF).await;
        let rid = ws.rid();
        ws.text(&image_send(&rid, c, &format!("h{i}"), 9 << 20)).await;
        holders.push(ws);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    // the fourth waits for a slot, its message read but not handled
    let c = e.m.add(STAFF, None);
    let mut waiting = e.ws(STAFF).await;
    let rid = waiting.rid();
    waiting.text(&image_send(&rid, c, "w", 9 << 20)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let drain = e.drain(Duration::from_millis(1200), Duration::from_secs(4));
    // past the whole jitter: still open
    assert_eq!(
        waiting.drain_for(Duration::from_millis(1500)).await,
        Vec::<Value>::new()
    );
    assert!(waiting.closed.is_none());
    held.notify_waiters();
    let r = waiting.next_session("send_agent_message_response").await;
    assert_eq!(r["requestId"], rid.as_str());
    assert_eq!(r["accepted"], true);
    assert_eq!(waiting.closed_code(Duration::from_secs(5)).await, Some(1012));
    for mut h in holders {
        assert_eq!(h.next_session("send_agent_message_response").await["accepted"], true);
        assert_eq!(h.closed_code(Duration::from_secs(5)).await, Some(1012));
    }
    drain.await.unwrap();
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
    // a few poller ticks: the owner check runs outside the hub's lock
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(owners.consulted.load(Ordering::SeqCst));
    e.m.row(c, None, assistant("native-1"));
    ws.wait_for("native-1", |v| is_row(v, c, "native-1")).await;
    // the delegate takes the conversation (WP-50 hands it over); the native poller lingers but sends nothing, also
    // not for a history rewrite meanwhile
    *owners.delegated.lock().unwrap() = Some(c);
    e.m.row(c, None, assistant("delegated"));
    let epoch = e.m.rewrite(c);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let during = ws.drain_for(Duration::from_millis(300)).await;
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
    tokio::time::sleep(Duration::from_millis(500)).await;
    e.m.row(c, None, assistant("native-2"));
    let v = ws.wait_for("native-2", |v| is_row(v, c, "native-2")).await;
    assert_eq!(v["message"]["payload"]["epoch"], epoch.as_str());
    // nothing else: no old row sent again, no second replacement
    let rest = ws.drain_for(Duration::from_millis(500)).await;
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
    tokio::time::sleep(pigeon::feeds::LIST_CACHE).await;
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
    let (k, _) = ws.rpc("no_such_request", json!({})).await;
    assert_eq!(k, "rpc_error");
    e.m.refuse_once(
        "get_conversation",
        Refusal::new(ErrorCode::Unavailable, "down").with_status(503),
    );
    let (k, _) = ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "rpc_error");
    tokio::time::sleep(Duration::from_millis(300)).await;
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
