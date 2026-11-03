//! The switch window (WP-50; DESIGN 9.2-9.5) against the LU contract rig: a real pigeon in front of the in-memory
//! magpie model (`common/model.rs`, with the window's tickets) and a fake old router (`common/old.rs`). A legacy
//! conversation's timeline, live stream and writes go through the legacy upstream under tickets that close only on
//! the old stack's outcome; a release sends one replacement per subscribed socket; uploads are teed from T0 to the
//! gate and copied at send after it; the signed-out answer for an LU 401; the old view-as key; the move's activity.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::assert_is_empty
)] // tests

mod common;

use std::time::Duration;

use common::model::{self, Model};
use common::old::Old;
use common::ws::Ws;
use common::{ADMIN, Harness, LU_PAIR, OLD_VIEW_KEY, Opts, STAFF, STAFF2};
use magpie_api::legacy::{TicketKind, TicketOutcome};
use pigeon::{KeyList, Tuning};
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    h: Harness,
    m: Model,
    old: Old,
    served: aviary_fakes::Served,
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
        legacy_window: Duration::from_millis(50),
        legacy_heartbeat: Duration::from_millis(300),
        ..common::tuning()
    }
}

async fn replica(served: &aviary_fakes::Served, old: &Old) -> Harness {
    Harness::start_opts(Opts {
        tuning: fast(),
        magpie: Some(served.url()),
        legacy: Some(old.url.clone()),
        ..Opts::default()
    })
    .await
}

async fn env() -> Env {
    let m = Model::default();
    let served = model::serve(&m).await;
    let old = Old::start(LU_PAIR).await;
    let h = replica(&served, &old).await;
    Env { h, m, old, served }
}

impl Env {
    async fn ws(&self, email: &str) -> Ws {
        Ws::hello(&self.h.url, &self.h.token(email), "").await.0.relayed()
    }

    /// A legacy conversation of `person` in magpie and in the person's old child.
    fn legacy(&self, person: &str) -> Uuid {
        let c = self.m.add_legacy(person, Some("舊對話"));
        self.old.add(c, person);
        c
    }

    fn metric(&self, name: &str) -> f64 {
        self.h.metric(name)
    }
}

async fn list(ws: &mut Ws) {
    let (_, r) = ws
        .rpc(
            "fetch_agents_request",
            json!({"subscribe": {"subscriptionId": "agents"}}),
        )
        .await;
    assert!(r["entries"].is_array(), "{r}");
}

async fn watch(ws: &mut Ws, ids: &[Uuid]) -> String {
    let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let (_, r) = ws
        .rpc("agent.timeline.set_subscription.request", json!({"agentIds": ids}))
        .await;
    r["subscriptionId"].as_str().unwrap().to_owned()
}

fn is_stream(v: &Value, agent: Uuid) -> bool {
    v["message"]["type"] == "agent_stream" && v["message"]["payload"]["agentId"] == agent.to_string()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

async fn send(ws: &mut Ws, c: Uuid, cmid: &str, extra: Value) -> Value {
    let mut m = json!({"agentId": c, "text": "繼續", "messageId": cmid});
    if let (Some(o), Some(x)) = (m.as_object_mut(), extra.as_object()) {
        for (k, v) in x {
            o.insert(k.clone(), v.clone());
        }
    }
    ws.rpc("send_agent_message_request", m).await.1
}

/// A running old turn streams to its end through LU and Stop ends it; every write is accepted once under a ticket
/// that closes on the old stack's outcome; the LU carries pigeon's pair token and the person's newest token.
#[tokio::test]
async fn a_legacy_conversation_is_served_through_lu_and_every_write_goes_once() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.old.st().agents.get_mut(&c).unwrap().running = true;
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let sub = watch(&mut ws, &[c]).await;
    let (_, page) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail", "limit": 50}),
        )
        .await;
    assert_eq!(
        page["epoch"],
        e.old.agent(c).epoch,
        "the old child's epoch: the tab's cursors stay valid"
    );
    // the running old turn streams through LU, under the tab's own subscription
    until("the LU subscribed", || {
        e.old.st().seen.iter().any(|s| s.1.as_deref() == Some(LU_PAIR))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let seq = e
        .old
        .stream_row(c, json!({"type": "assistant_message", "text": "還在做"}));
    let v = ws.wait_for("the old row", |v| is_stream(v, c)).await;
    assert_eq!(v["message"]["payload"]["subscriptionId"], sub);
    assert_eq!(v["message"]["payload"]["seq"], seq);
    // Stop ends it, under a ticket closed on the old child's answer
    let (kind, r) = ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    assert_eq!(kind, "cancel_agent_response");
    assert!(r["error"].is_null(), "{r}");
    assert_eq!(e.old.agent(c).cancels, 1);
    ws.wait_for("turn_canceled", |v| {
        is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_canceled"
    })
    .await;
    // a send, a rename: accepted once each
    let r = send(&mut ws, c, "m-1", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    let (kind, r) = ws
        .rpc("update_agent_request", json!({"agentId": c, "title": "新名"}))
        .await;
    assert_eq!(kind, "update_agent_response");
    assert_eq!(r["accepted"], true);
    let a = e.old.agent(c);
    assert_eq!(a.sends.len(), 1);
    assert_eq!(a.sends[0].0, "m-1");
    assert_eq!(a.title.as_deref(), Some("新名"));
    e.m.wait_tickets(|t| t.len() == 3 && t.iter().all(|t| t.outcome.is_some()))
        .await;
    let t = e.m.tickets_of(c);
    assert_eq!(
        t.iter().map(|t| (t.kind, t.outcome)).collect::<Vec<_>>(),
        vec![
            (TicketKind::Stop, Some(TicketOutcome::Applied)),
            (TicketKind::Send, Some(TicketOutcome::Started)),
            (TicketKind::Rename, Some(TicketOutcome::Applied)),
        ]
    );
    assert_eq!(t[1].cmid.as_deref(), Some("m-1"));
    // nothing went to magpie's write routes; nothing refused or failed
    assert_eq!(e.m.calls_to("send_turn"), 0);
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
    // as the person, with pigeon's pair token
    let seen = e.old.st().seen.clone();
    assert!(seen.iter().all(|s| s.1.as_deref() == Some(LU_PAIR) && !s.0.is_empty()));
    assert!(seen.iter().all(|s| common::old::email_of(&s.0) == STAFF));
    // heartbeats keep the old child warm
    until("a heartbeat", || e.old.st().heartbeats > 0).await;
}

/// At the release every socket subscribed to the conversation gets exactly one replacement in magpie's epoch and
/// the native feed; none before, none on a socket that does not watch it, and the old child's frames stop.
#[tokio::test]
async fn one_replacement_per_subscribed_socket_at_release_and_none_before() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut a = e.ws(STAFF).await;
    let mut b = e.ws(STAFF).await;
    let mut other = e.ws(STAFF).await;
    for ws in [&mut a, &mut b, &mut other] {
        list(ws).await;
    }
    watch(&mut a, &[c]).await;
    watch(&mut b, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for ws in [&mut a, &mut b, &mut other] {
        let none: Vec<Value> = ws
            .drain_for(Duration::from_millis(200))
            .await
            .into_iter()
            .filter(|v| v["message"]["type"] == "agent.timeline.replacement")
            .collect();
        assert!(none.is_empty(), "no replacement before the release: {none:?}");
    }
    e.m.set_migrating(c, false);
    let epoch = e.m.epoch(c);
    for ws in [&mut a, &mut b] {
        let r = ws.next_session("agent.timeline.replacement").await;
        assert_eq!(r["agentId"], c.to_string());
        assert_eq!(r["epoch"], epoch, "{r}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for ws in [&mut a, &mut b, &mut other] {
        let more: Vec<Value> = ws
            .drain_for(Duration::from_millis(200))
            .await
            .into_iter()
            .filter(|v| v["message"]["type"] == "agent.timeline.replacement")
            .collect();
        assert!(more.is_empty(), "one replacement per socket: {more:?}");
    }
    assert_eq!(e.metric("pigeon_legacy_replacements_total"), 2.0);
    // the old child's frames no longer reach the tab; magpie's do
    e.old.stream_row(c, json!({"type": "assistant_message", "text": "舊"}));
    let quiet: Vec<Value> = a
        .drain_for(Duration::from_millis(300))
        .await
        .into_iter()
        .filter(|v| is_stream(v, c))
        .collect();
    assert!(quiet.is_empty(), "{quiet:?}");
    let r = send(&mut a, c, "after", json!({})).await;
    assert_eq!(r["accepted"], true);
    assert_eq!(e.m.st().sent.len(), 1, "native after the release");
    assert!(e.old.agent(c).sends.is_empty());
}

/// A send whose ticket finds the conversation released (the fence won) is a native send; one written to a
/// conversation pigeon read as owned that magpie answers 409 `route: "legacy"` goes through LU (the re-route).
#[tokio::test]
async fn the_fence_winning_sends_natively_and_a_409_reroutes_to_lu() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.m.st().release_at_ticket.insert(c);
    let r = send(&mut ws, c, "raced", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.m.st().sent.len(), 1);
    assert!(e.old.agent(c).sends.is_empty());
    // owned in pigeon's eyes, legacy in magpie's (a reverse hand-over whose change has not arrived)
    let d = e.m.add(STAFF, None);
    e.old.add(d, STAFF);
    list(&mut ws).await;
    e.m.st().convs.get_mut(&d).unwrap().conv.migrating = true;
    let r = send(&mut ws, d, "rerouted", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(d).sends.len(), 1);
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// The old stack does not de-duplicate a send: a repeat whose ticket supersedes an open one (pigeon died before
/// its report) reads the old child first, and the old child gets exactly one turn; when the first send never
/// arrived, the repeat sends.
#[tokio::test]
async fn a_repeated_send_past_the_reuse_age_reaches_the_old_child_once() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    // the first send reached the old child and its ticket stayed open (the reporting replica was killed)
    e.old.st().hold_sends = false;
    {
        let mut st = e.old.st();
        let a = st.agents.get_mut(&c).unwrap();
        a.sends.push(("m-dup".into(), "繼續".into(), vec![]));
        a.rows
            .push(json!({"type": "user_message", "id": "paseo-user-m-dup", "clientMessageId": "m-dup"}));
    }
    e.m.st().tickets.push(common::model::Ticket {
        id: Uuid::new_v4(),
        conversation: c,
        kind: TicketKind::Send,
        cmid: Some("m-dup".into()),
        outcome: None,
    });
    e.m.st().supersede_next = Some(150);
    let r = send(&mut ws, c, "m-dup", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "exactly one old turn");
    e.m.wait_tickets(|t| t.last().is_some_and(|t| t.outcome == Some(TicketOutcome::Completed)))
        .await;
    // a repeat whose first send never arrived is sent
    e.m.st().tickets.push(common::model::Ticket {
        id: Uuid::new_v4(),
        conversation: c,
        kind: TicketKind::Send,
        cmid: Some("m-lost".into()),
        outcome: None,
    });
    e.m.st().supersede_next = Some(100);
    let r = send(&mut ws, c, "m-lost", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 2);
    assert_eq!(e.old.agent(c).sends[1].0, "m-lost");
}

fn multipart(name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let b = "XbOuNdArY";
    let mut body = format!(
        "--{b}\r\ncontent-disposition: form-data; name=\"files\"; filename=\"{name}\"\r\ncontent-type: application/pdf\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={b}"), body)
}

async fn upload(e: &Env, name: &str, bytes: &[u8]) -> reqwest::Response {
    let (ct, body) = multipart(name, bytes);
    e.h.as_token(e.h.post("/api/assets/files"), &e.h.token(STAFF))
        .header("content-type", ct)
        .header("origin", &e.h.url)
        .body(body)
        .send()
        .await
        .unwrap()
}

/// From T0 to the gate an upload is teed: the answer is the old path, the old child has the file, magpie has the
/// bytes with the old path recorded, and a send on the old path (as an unmoved socket's would be) reaches the old
/// child. From the gate an upload is magpie's only and a send of it to a legacy conversation copies it into the old
/// child under its own upload ticket. A tee the old child refuses answers with the old route's error.
#[tokio::test]
async fn uploads_are_teed_until_the_gate_and_copied_at_send_after_it() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let r = upload(&e, "q3.pdf", b"%PDF-1.4 teed").await;
    assert_eq!(r.status(), 201);
    let v = common::body_json(r).await;
    let path = v["attachments"][0]["path"].as_str().unwrap().to_owned();
    assert_eq!(path, common::old::old_path(STAFF, 1, "q3.pdf"));
    assert_eq!(e.old.st().files.len(), 1);
    assert_eq!(e.old.st().files[0].1.as_ref(), b"%PDF-1.4 teed");
    assert_eq!(e.m.st().uploads.len(), 1);
    assert_eq!(e.m.st().legacy_paths.len(), 1);
    assert_eq!(e.m.st().legacy_paths[0].1, path);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let r = send(
        &mut ws,
        c,
        "with-file",
        json!({"attachments": [{"type": "uploaded_file", "path": path, "fileName": "q3.pdf",
                                "mimeType": "application/pdf", "size": 13}]}),
    )
    .await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends[0].2, vec![path.clone()]);
    // the gate: magpie only, then copied at send
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.old.end_turn(c);
    let r = upload(&e, "gate.pdf", b"%PDF-1.4 gate").await;
    assert_eq!(r.status(), 201);
    let v = common::body_json(r).await;
    let mpath = v["attachments"][0]["path"].as_str().unwrap().to_owned();
    assert!(mpath.starts_with("uploads/"), "{mpath}");
    assert_eq!(e.old.st().files.len(), 1, "no tee from the gate");
    let r = send(
        &mut ws,
        c,
        "copied",
        json!({"attachments": [{"type": "uploaded_file", "path": mpath, "fileName": "gate.pdf",
                                "mimeType": "application/pdf", "size": 13}]}),
    )
    .await;
    assert_eq!(r["accepted"], true, "{r}");
    let files = e.old.st().files.clone();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].1.as_ref(), b"%PDF-1.4 gate");
    assert_eq!(
        e.old.agent(c).sends[1].2,
        vec![files[1].0.clone()],
        "sent with the old path"
    );
    e.m.wait_tickets(|t| t.iter().all(|t| t.outcome.is_some())).await;
    let kinds: Vec<_> = e.m.tickets_of(c).iter().map(|t| (t.kind, t.outcome)).collect();
    assert_eq!(
        kinds[1..],
        [
            (TicketKind::Send, Some(TicketOutcome::Started)),
            (TicketKind::Upload, Some(TicketOutcome::Applied)),
        ],
        "the send's ticket first, then the copy's"
    );
    // a refused tee answers with the old route's error, never a magpie-only path
    e.m.st().gate = None;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let uploads = e.m.st().uploads.len();
    e.old.refuse.store(500, std::sync::atomic::Ordering::SeqCst);
    let r = upload(&e, "bad.pdf", b"x").await;
    assert_eq!(r.status(), 500);
    assert_eq!(e.m.st().uploads.len(), uploads);
    assert_eq!(e.metric("pigeon_upload_tee_failed_total"), 1.0);
    e.old.refuse.store(0, std::sync::atomic::Ordering::SeqCst);
    // the old route's cap while any conversation is legacy
    let big = vec![b'a'; 31 << 20];
    let r = upload(&e, "big.bin", &big).await;
    assert_eq!(r.status(), 413);
}

/// A copy-at-send whose upload ticket finds the conversation released sends natively with the magpie ids.
#[tokio::test]
async fn a_copy_at_send_that_loses_to_the_fence_sends_natively() {
    let e = env().await;
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    let c = e.legacy(STAFF);
    let r = upload(&e, "g.pdf", b"%PDF-1.4 g").await;
    let v = common::body_json(r).await;
    let mpath = v["attachments"][0]["path"].as_str().unwrap().to_owned();
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.m.st().release_on_kind = Some((c, TicketKind::Upload));
    let r = send(
        &mut ws,
        c,
        "fenced",
        json!({"attachments": [{"type": "uploaded_file", "path": mpath, "fileName": "g.pdf",
                                "mimeType": "application/pdf", "size": 10}]}),
    )
    .await;
    assert_eq!(r["accepted"], true, "{r}");
    assert!(e.old.agent(c).sends.is_empty());
    let sent = e.m.st().sent.clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.files[0].path, mpath, "the magpie path");
    assert!(e.old.st().files.is_empty());
}

/// An LU 401 gives the tab pigeon's signed-out answer (4401), not a write error.
#[tokio::test]
async fn an_lu_401_is_the_signed_out_answer() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.old.refuse.store(401, std::sync::atomic::Ordering::SeqCst);
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "x",
                      "messageId": "m401"}),
    )
    .await;
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(4401));
    assert!(e.metric("pigeon_lu_401_total") >= 1.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
    assert!(e.m.tickets_of(c).is_empty(), "no ticket before the LU is open");
}

/// A pigeon replica with LU sockets goes away: its tabs reconnect to another replica, which opens LU again, and the
/// conversation keeps streaming; an LU ends with its tab.
#[tokio::test]
async fn a_replica_with_lu_sockets_gone_tabs_resume_and_lu_reopens() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open", || {
        e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(e.h.app.legacy().lu_open(), 1);
    drop(ws);
    until("LU closed with its tab", || e.h.app.legacy().lu_open() == 0).await;
    let other = replica(&e.served, &e.old).await;
    let mut ws = Ws::hello(&other.url, &other.token(STAFF), "").await.0.relayed();
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU reopened", || other.app.legacy().lu_open() == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.old
        .stream_row(c, json!({"type": "assistant_message", "text": "接住"}));
    ws.wait_for("the row on the new replica", |v| is_stream(v, c)).await;
    assert_eq!(e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// The drain closes request-free sockets first: a socket whose LU send is outstanding is closed only after its
/// answer.
#[tokio::test]
async fn the_drain_waits_for_an_lu_write_while_closing_request_free_sockets_first() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut idle = e.ws(STAFF).await;
    let mut busy = e.ws(STAFF).await;
    list(&mut busy).await;
    e.old.st().hold_sends = true;
    let rid = busy.rid();
    busy.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "hi",
                        "messageId": "m-drain"}),
    )
    .await;
    until("the send reached the old child", || {
        e.old.st().hold_sends && e.m.st().tickets.len() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let socket = e.h.app.socket();
    let drain = tokio::spawn(async move {
        socket
            .drain(
                Duration::from_millis(1200),
                Duration::from_secs(3),
                Duration::from_secs(30),
            )
            .await;
    });
    assert_eq!(idle.closed_code(Duration::from_secs(3)).await, Some(1012));
    assert!(busy.closed.is_none());
    e.old.release_sends();
    let r = busy.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true);
    assert_eq!(busy.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
}

/// An admin's view-as tab opened on the old router keeps working on pigeon while any conversation is legacy: its
/// token is checked with the old key, and LU calls carry the same claims signed with it.
#[tokio::test]
async fn an_old_view_as_token_is_honoured_while_anything_is_legacy() {
    let e = env().await;
    let claims = pigeon::viewas::Claims {
        a: ADMIN.into(),
        u: STAFF.into(),
        s: Uuid::new_v4().to_string(),
        e: common::now() + 3600,
    };
    let old_token = pigeon::viewas::sign(&KeyList(vec![OLD_VIEW_KEY]), &claims);
    let q = format!("?{}={old_token}", pigeon::paseo::compat::VIEW_AS_QUERY);
    // nothing legacy yet: refused
    assert_eq!(
        Ws::open(&e.h.url, &e.h.token(ADMIN), &q).await.err(),
        Some(403),
        "an old token is not honoured outside the window"
    );
    let c = e.legacy(STAFF);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut ws = Ws::hello(&e.h.url, &e.h.token(ADMIN), &q).await.0.relayed();
    list(&mut ws).await;
    let (_, page) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail"}),
        )
        .await;
    assert_eq!(page["epoch"], e.old.agent(c).epoch);
    let seen = e.old.st().seen.clone();
    let view = seen.iter().find_map(|s| s.2.clone()).expect("a view-as token on LU");
    let got = pigeon::viewas::verify(&KeyList(vec![OLD_VIEW_KEY]), &view, common::now()).unwrap();
    assert_eq!(got, claims, "the same claims, signed with the old key");
    assert!(
        seen.iter()
            .any(|s| common::old::email_of(&s.0) == ADMIN && s.2.is_some()),
        "as the admin"
    );
    // a change is refused before any call, as pigeon refuses it for every view
    let (kind, r) = ws
        .rpc("update_agent_request", json!({"agentId": c, "title": "x"}))
        .await;
    assert_eq!(kind, "rpc_error");
    assert_eq!(r["code"], "view_as_read_only");
}

/// The move's input: the last warm call and upload per person on the ops port; a legacy conversation's warm before
/// the gate is the old stack's (ticketed), from the gate a hermit hint only.
#[tokio::test]
async fn activity_and_warm_calls() {
    use tower::ServiceExt as _;
    let e = env().await;
    let c = e.legacy(STAFF);
    let warm = |e: &Env| {
        e.h.as_token(e.h.post(pigeon::paseo::compat::WARM), &e.h.token(STAFF))
            .header("origin", &e.h.url)
            .header("content-type", "application/json")
            .body(json!({"sessionId": c}).to_string())
    };
    assert_eq!(warm(&e).send().await.unwrap().status(), 202);
    until("the old warm", || e.old.st().warms.len() == 1).await;
    e.m.wait_tickets(|t| {
        t.iter()
            .any(|t| t.kind == TicketKind::Warm && t.outcome == Some(TicketOutcome::Applied))
    })
    .await;
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(warm(&e).send().await.unwrap().status(), 202);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.old.st().warms.len(), 1, "from the gate: a hermit hint only");
    let r =
        e.h.app
            .internal_router()
            .oneshot(
                axum::http::Request::get(pigeon::legacy::activity::PATH)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert!(v["persons"][STAFF]["lastWarm"].is_string(), "{v}");
    assert!(v["persons"][STAFF]["lastUpload"].is_null());
    assert!(v["persons"][STAFF2].is_null());
}

/// While a rollback is under way a create goes through the old child (born legacy); a legacy conversation's share
/// links are the old child's, created and revoked under tickets.
#[tokio::test]
async fn rollback_creates_and_shares_go_to_the_old_child() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.m.st().rollback = Some(magpie_api::Timestamp::now());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut ws = e.ws(STAFF).await;
    let (kind, r) = ws
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": "k1", "agent": {"initialPrompt": "新", "clientMessageId": "k1:initial-message"}}),
        )
        .await;
    assert_eq!(kind, "workspace.create.response");
    assert!(r["error"].is_null(), "{r}");
    let id = Uuid::parse_str(r["agent"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(e.old.agent(id).sends.len(), 1);
    assert_eq!(e.m.calls_to("create_conversation"), 0);
    assert_eq!(e.h.app.legacy().is_legacy(STAFF, &id), Some(true));
    // shares of a legacy conversation
    let path = format!("{}{c}/shares", pigeon::paseo::compat::SESSIONS);
    let r =
        e.h.as_token(e.h.post(&path), &e.h.token(STAFF))
            .header("origin", &e.h.url)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 201);
    let share = common::body_json(r).await["id"].as_str().unwrap().to_owned();
    let r = e.h.get_as(&path, STAFF).await;
    assert_eq!(common::body_json(r).await["shares"][0]["id"], share);
    let r =
        e.h.as_token(
            e.h.http
                .delete(format!("{}{}{share}", e.h.url, pigeon::paseo::compat::SHARES)),
            &e.h.token(STAFF),
        )
        .header("origin", &e.h.url)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(!e.old.st().shares[0].2, "revoked in the old child");
    e.m.wait_tickets(|t| {
        t.iter()
            .filter(|t| t.kind == TicketKind::ShareCreate || t.kind == TicketKind::ShareRevoke)
            .all(|t| t.outcome == Some(TicketOutcome::Applied))
            && t.iter().any(|t| t.kind == TicketKind::ShareRevoke)
    })
    .await;
}

/// The fence-race fuzz through pigeon: sends at random instants around each conversation's release, every one
/// accepted exactly once (on the old child or in magpie), never refused.
#[tokio::test]
async fn fence_race_fuzz_a_thousand_sends_each_accepted_once() {
    let e = env().await;
    e.old.st().auto_end = true;
    let convs: Vec<Uuid> = (0..20).map(|_| e.legacy(STAFF)).collect();
    let mut tasks = tokio::task::JoinSet::new();
    for (i, c) in convs.iter().copied().enumerate() {
        let mut ws = e.ws(STAFF).await;
        let m = e.m.clone();
        let release_at = (i * 7) % 50;
        tasks.spawn(async move {
            list(&mut ws).await;
            let mut refused = Vec::new();
            for n in 0..50 {
                if n == release_at {
                    let m2 = m.clone();
                    // the importer's release at a random instant, only with no open ticket (the fence's rule)
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis((n as u64 * 13) % 20)).await;
                        while !m2.try_release(c) {
                            tokio::time::sleep(Duration::from_millis(2)).await;
                        }
                    });
                }
                let cmid = format!("{c}-{n}");
                let r = send(&mut ws, c, &cmid, json!({})).await;
                if r["accepted"] != true {
                    refused.push(r);
                }
                // the native turn ends before the next send (the old child's ends on its own)
                if m.st().convs[&c].conv.turn.is_some() {
                    m.end_turn(c, magpie_api::turns::TurnPhase::Completed, None);
                }
            }
            refused
        });
    }
    while let Some(r) = tasks.join_next().await {
        let refused = r.unwrap();
        assert!(refused.is_empty(), "refused: {refused:?}");
    }
    for c in &convs {
        let old: Vec<String> = e.old.agent(*c).sends.iter().map(|s| s.0.clone()).collect();
        let native: Vec<String> = e.m.st().convs[c].sends.keys().cloned().collect();
        let mut all: Vec<String> = old.iter().chain(native.iter()).cloned().collect();
        all.sort();
        let n = all.len();
        all.dedup();
        assert_eq!(all.len(), n, "a send went to both stacks or twice");
        assert_eq!(n, 50, "every send accepted once");
    }
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}
