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
        legacy_settle: Duration::from_secs(2),
        ..common::tuning()
    }
}

async fn replica(served: &aviary_fakes::Served, old: &Old) -> Harness {
    replica_at(served, &old.url, fast()).await
}

/// A replica with its own timings, its legacy upstream at `legacy`.
async fn replica_at(served: &aviary_fakes::Served, legacy: &str, tuning: Tuning) -> Harness {
    Harness::start_opts(Opts {
        tuning,
        magpie: Some(served.url()),
        legacy: Some(legacy.to_owned()),
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

/// A replica in a runtime of its own, so a test can kill it like a process: every task, socket and timer it has
/// stops at once (its TCP connections close), with nothing run on the way out.
struct Remote {
    h: Option<Harness>,
    rt: Option<tokio::runtime::Runtime>,
}

impl Remote {
    async fn start(served: &aviary_fakes::Served, old: &Old) -> Self {
        let (magpie, legacy) = (served.url(), old.url.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let h = rt.block_on(Harness::start_opts(Opts {
                tuning: fast(),
                magpie: Some(magpie),
                legacy: Some(legacy),
                ..Opts::default()
            }));
            let _ = tx.send((h, rt));
        });
        let (h, rt) = rx.await.unwrap();
        Self {
            h: Some(h),
            rt: Some(rt),
        }
    }

    fn h(&self) -> &Harness {
        self.h.as_ref().unwrap()
    }

    async fn ws(&self, email: &str) -> Ws {
        let h = self.h();
        Ws::hello(&h.url, &h.token(email), "").await.0.relayed()
    }

    /// The process dies.
    fn kill(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
        // its parts belonged to the runtime that is gone
        std::mem::forget(self.h.take());
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.kill();
    }
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

/// An ask of an old turn reaches the tab watching the conversation through LU, and so does its end: a question
/// made after the move is shown, and the one Stop ended leaves the tab (WP-54's ask persona, legacy-in-a-box).
#[tokio::test]
async fn an_old_turn_s_ask_and_its_resolution_reach_the_tab() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.old.st().agents.get_mut(&c).unwrap().running = true;
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    let _ = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail", "limit": 50}),
        )
        .await;
    until("the LU subscribed", || {
        e.old.st().seen.iter().any(|s| s.1.as_deref() == Some(LU_PAIR))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let request = json!({"id": "ask-1", "provider": "claude", "name": "AskUserQuestion", "kind": "question",
                         "input": {"questions": [{"question": "go on?", "options": [{"label": "Yes"}]}]}});
    e.old.push(
        c,
        "agent_permission_request",
        &json!({"agentId": c, "request": request}),
    );
    let v = ws
        .wait_for("the ask", |v| v["message"]["type"] == "agent_permission_request")
        .await;
    assert_eq!(v["message"]["payload"]["agentId"], c.to_string());
    assert_eq!(v["message"]["payload"]["request"]["id"], "ask-1");
    e.old.push(
        c,
        "agent_permission_resolved",
        &json!({"agentId": c, "requestId": "ask-1", "resolution": {"behavior": "deny", "message": "Stopped"}}),
    );
    let v = ws
        .wait_for("its resolution", |v| {
            v["message"]["type"] == "agent_permission_resolved"
        })
        .await;
    assert_eq!(v["message"]["payload"]["requestId"], "ask-1");
}

fn agent_update_of(v: &Value, c: Uuid) -> Option<&Value> {
    (v["message"]["type"] == "agent_update" && v["message"]["payload"]["agent"]["id"] == c.to_string())
        .then(|| &v["message"]["payload"])
}

/// On a moved tab a legacy conversation's agent state is the old stack's until its release, then magpie's: an ask
/// the old turn makes is listed with its turn (the question card and Stop), a native upsert of the conversation (the
/// running mirror, a title) keeps that state, Stop's answer and the old child's later resolution leave no question,
/// and after the release magpie's snapshot is shown as it is. The state pushes carry the tab's own list subscription
/// and no list position (WP-54's ask persona: no card, no Stop, and a card that stayed after Stop).
#[tokio::test]
async fn a_legacy_conversation_s_agent_state_is_the_old_stack_s_until_its_release() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    let (_, r) = ws
        .rpc(
            "fetch_agents_request",
            json!({"subscribe": {"subscriptionId": "agents"}}),
        )
        .await;
    let sub = r["subscriptionId"].as_str().unwrap().to_owned();
    let listed = r["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["agent"]["id"] == c.to_string())
        .unwrap()["agent"]
        .clone();
    assert_eq!(listed["activeTurn"], Value::Null);
    let updated = listed["updatedAt"].clone();
    watch(&mut ws, &[c]).await;
    let _ = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail", "limit": 50}),
        )
        .await;
    until("the LU", || {
        e.old.st().seen.iter().any(|s| s.1.as_deref() == Some(LU_PAIR))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // an old turn asks after the move
    let request = json!({"id": "ask-1", "provider": "claude", "name": "AskUserQuestion", "kind": "question",
                         "input": {"questions": [{"question": "go on?", "options": [{"label": "Yes"}]}]}});
    e.old.ask(c, &request);
    let v = ws
        .wait_for("the old turn's ask in the list", |v| {
            agent_update_of(v, c).is_some_and(|p| p["agent"]["pendingPermissions"][0]["id"] == "ask-1")
        })
        .await;
    let p = &v["message"]["payload"];
    assert_eq!(p["subscriptionId"], sub);
    assert_eq!(p["agent"]["status"], "running");
    assert_eq!(
        p["agent"]["activeTurn"]["turnId"], "t",
        "the old turn, as the LU's turn events name it"
    );
    assert_eq!(p["agent"]["requiresAttention"], true);
    assert_eq!(p["agent"]["attentionReason"], "permission");
    assert_eq!(
        p["agent"]["updatedAt"], updated,
        "magpie's updatedAt, not the old row's"
    );
    assert_eq!(p["agent"]["title"], "舊對話");
    assert!(
        p.get("seq").is_none() && p.get("generation").is_none(),
        "no list position: {p}"
    );
    // a native upsert of the conversation keeps the old stack's state
    e.m.edit(c, |x| x.title = Some("新的標題".into()));
    let v = ws
        .wait_for("magpie's upsert", |v| {
            agent_update_of(v, c).is_some_and(|p| p["agent"]["title"] == "新的標題")
        })
        .await;
    let p = &v["message"]["payload"];
    assert_eq!(p["agent"]["pendingPermissions"][0]["id"], "ask-1", "{p}");
    assert_eq!(p["agent"]["activeTurn"]["turnId"], "t");
    assert!(p["seq"].as_u64().is_some(), "magpie's own seq");
    // Stop from the moved tab: the answer and then the old child's resolution leave no question
    let (_, r) = ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    assert_eq!(r["error"], Value::Null, "{r}");
    let v = ws
        .wait_for("the stopped turn's state", |v| {
            agent_update_of(v, c).is_some_and(|p| p["agent"]["pendingPermissions"] == json!([]))
        })
        .await;
    let p = &v["message"]["payload"];
    assert_eq!(p["agent"]["activeTurn"], Value::Null);
    assert_eq!(p["agent"]["status"], "idle");
    assert_eq!(p["agent"]["requiresAttention"], false);
    // released: magpie's snapshot as it is, and the old child's frames of it no longer shown
    for _ in 0..200 {
        if e.m.try_release(c) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!e.m.st().convs[&c].conv.migrating);
    e.m.edit(c, |x| x.title = Some("已交接".into()));
    let v = ws
        .wait_for("an upsert after the release", |v| {
            agent_update_of(v, c).is_some_and(|p| p["agent"]["title"] == "已交接")
        })
        .await;
    assert_eq!(v["message"]["payload"]["agent"]["activeTurn"], Value::Null);
    e.old.ask(
        c,
        &json!({"id": "ask-2", "provider": "claude", "name": "AskUserQuestion", "kind": "question", "input": {}}),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    while let Some(v) = ws.next_raw(Duration::from_millis(50)).await {
        assert!(
            agent_update_of(&v, c).is_none_or(|p| !p["agent"]["pendingPermissions"].to_string().contains("ask-2")),
            "an old snapshot after the release: {v}"
        );
    }
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

/// The tab's send is in the old child and the replica that wrote it dies before its report; the tab resends the
/// same `messageId` through another replica within the reuse age (magpie hands back the open ticket): the old child
/// is read first and gets exactly one turn, and the tab hears `accepted: true`.
#[tokio::test]
async fn a_send_repeated_after_its_replica_died_reaches_the_old_child_once() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut a = Remote::start(&e.served, &e.old).await;
    let mut ws = a.ws(STAFF).await;
    list(&mut ws).await;
    e.old.st().hold_sends = true;
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-dup"}),
    )
    .await;
    until("the write is at the old child", || e.m.st().tickets.len() == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // the replica dies after the LU write and before its report; the old child then takes the send
    a.kill();
    drop(ws);
    until("the LU gone", || e.old.sockets() == 0).await;
    e.old.release_sends();
    until("the old child took it", || e.old.agent(c).sends.len() == 1).await;
    assert!(e.m.st().tickets[0].outcome.is_none(), "no report from a dead replica");
    // the tab resends through another replica, within the reuse age
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let r = send(&mut ws, c, "m-dup", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "exactly one old turn");
    let t = e.m.tickets_of(c);
    assert_eq!(t.len(), 1, "the open ticket was handed back: {t:?}");
    e.m.wait_tickets(|t| t[0].outcome == Some(TicketOutcome::Started)).await;
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A tab's socket drops while its send is being written; it resends the same `messageId` on a second replica within
/// 30 s (the open ticket handed back): the old child gets one send and the tab `accepted: true`.
#[tokio::test]
async fn a_resend_after_the_socket_dropped_mid_write_is_not_a_second_turn() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.old.st().hold_sends = true;
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-drop"}),
    )
    .await;
    until("the write is at the old child", || e.m.st().tickets.len() == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(ws);
    until("the LU closed with its tab", || e.h.app.legacy().lu_open() == 0).await;
    e.old.release_sends();
    until("the old child took it", || e.old.agent(c).sends.len() == 1).await;
    let other = replica(&e.served, &e.old).await;
    let mut ws = Ws::hello(&other.url, &other.token(STAFF), "").await.0.relayed();
    list(&mut ws).await;
    let r = send(&mut ws, c, "m-drop", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1);
    assert_eq!(e.m.tickets_of(c).len(), 1, "one ticket, reused");
}

/// A repeat is sent only when the old timeline lacks its user row; the busy state (per conversation, not per
/// message) never stands in for it. After an old child restart the row lost its id while the turn runs: the repeat
/// is sent and the old hub itself refuses it ("still running"), so there is no second turn and no message answered
/// as sent that was not. A send whose LU dropped with the write is settled from the old child and sent again; a
/// later resend of a send the old child accepted is answered without a send; a timeline that cannot be read sends
/// nothing.
#[tokio::test]
async fn a_repeat_is_sent_only_when_the_old_timeline_lacks_its_row() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut a = Remote::start(&e.served, &e.old).await;
    let mut ws = a.ws(STAFF).await;
    list(&mut ws).await;
    e.old.st().hold_sends = true;
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-old"}),
    )
    .await;
    until("the write is claimed", || {
        e.m.st().tickets.first().is_some_and(|t| t.claims == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.kill();
    drop(ws);
    e.old.release_sends();
    until("the old child took it", || e.old.agent(c).sends.len() == 1).await;
    // the old child restarted: the row is there without its id, and the turn still runs
    e.old.forget_row_ids(c);
    assert!(e.old.agent(c).running);
    // past the reuse age (and 85 s of the settle time gone, so the test waits only the rest)
    let first = e.m.st().tickets[0].id;
    e.m.age_ticket(first, Duration::from_secs(85));
    // a token that lives past the reopen rule's hour (the LU is reopened below after the old side drops it)
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    let r = send(&mut ws, c, "m-old", json!({})).await;
    assert_eq!(
        r["accepted"], false,
        "the old hub's own refusal, not a message answered as sent: {r}"
    );
    assert_eq!(r["error"], "對話仍在進行中，請稍候。");
    assert_eq!(e.old.agent(c).sends.len(), 1, "not a second turn");
    e.m.wait_tickets(|t| t.len() == 2 && t[1].outcome == Some(TicketOutcome::Refused))
        .await;
    // a send whose LU drops with it (the old side closes before its child sees it), the tab staying: settled from
    // the old child once the write can no longer land, found absent, and sent again under the same ticket
    e.old.end_turn(c);
    e.old.st().drop_on_send = true;
    let r = send(&mut ws, c, "m-lost", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 2);
    assert_eq!(e.old.agent(c).sends[1].0, "m-lost");
    let lost = e.m.tickets_of(c).last().unwrap().clone();
    assert_eq!(lost.claims, 2, "claimed again for the second write");
    e.m.wait_tickets(|t| t.last().unwrap().outcome == Some(TicketOutcome::Started))
        .await;
    // the tab never got that answer and resends: the old child accepted it, so it is answered with nothing sent
    let r = send(&mut ws, c, "m-lost", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 2, "not sent again");
    e.m.wait_tickets(|t| t.len() == 4 && t[3].outcome == Some(TicketOutcome::Refused))
        .await;
    // an open ticket whose write may still land, and a timeline that cannot be read: nothing is sent
    e.old.end_turn(c);
    e.old.st().hold_sends = true;
    let mut held = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut held).await;
    let rid = held.rid();
    held.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-x"}),
    )
    .await;
    until("its write is claimed", || {
        e.m.st()
            .tickets
            .last()
            .is_some_and(|t| t.cmid.as_deref() == Some("m-x") && t.claims == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(held);
    until("the dropped tab's LU closed", || e.h.app.legacy().lu_open() == 1).await;
    let x = e.m.tickets_of(c).last().unwrap().id;
    e.m.age_claim(x, Duration::from_secs(40));
    e.old.st().timeline_down = true;
    let r = send(&mut ws, c, "m-x", json!({})).await;
    assert_ne!(r["accepted"], true, "{r}");
    e.old.st().timeline_down = false;
    e.old.release_sends();
    until("the old child took the first write", || e.old.agent(c).sends.len() == 3).await;
    // the resend after it landed: found, not sent again
    let r = send(&mut ws, c, "m-x", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 3);
    assert_eq!(
        e.m.tickets_of(c)
            .iter()
            .filter(|t| t.cmid.as_deref() == Some("m-x"))
            .count(),
        1,
        "{:?}",
        e.m.tickets_of(c)
    );
}

/// A holder of a send's ticket stalls before its write (its claim held), the tab resends through another replica
/// (the open ticket handed back under a new take) and then closes: the first holder neither writes nor closes the
/// ticket; the resend writes once and its outcome closes the ticket. The conversation's background work (listed
/// `running` in the busy state) does not stop the send the old child never got.
#[tokio::test]
async fn a_holder_that_gives_up_does_not_close_the_ticket_its_resend_writes_under() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.old.st().background.insert(c);
    let claims = e.m.hold("legacy_ticket_claim");
    let mut first = e.ws(STAFF).await;
    list(&mut first).await;
    let rid = first.rid();
    first
        .session(
            json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                          "messageId": "m-stall"}),
        )
        .await;
    until("the first holder is at its claim", || {
        e.m.st().calls.iter().any(|x| x.0 == "legacy_ticket_claim")
    })
    .await;
    let other = replica(&e.served, &e.old).await;
    let mut second = Ws::hello(&other.url, &other.token(STAFF), "").await.0.relayed();
    list(&mut second).await;
    let rid2 = second.rid();
    second
        .session(
            json!({"type": "send_agent_message_request", "requestId": rid2, "agentId": c, "text": "繼續",
                          "messageId": "m-stall"}),
        )
        .await;
    until("the resend is at its claim", || {
        e.m.st().calls.iter().filter(|x| x.0 == "legacy_ticket_claim").count() == 2
    })
    .await;
    drop(first);
    until("the first tab is gone", || e.h.app.legacy().lu_open() == 0).await;
    claims.notify_waiters();
    let r = second.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "one write");
    let t = e.m.tickets_of(c);
    assert_eq!(t.len(), 1, "one ticket, handed back: {t:?}");
    assert_eq!(t[0].claims, 1, "only the latest take claimed");
    e.m.wait_tickets(|t| t[0].outcome == Some(TicketOutcome::Started)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        e.m.tickets_of(c)[0].outcome,
        Some(TicketOutcome::Started),
        "the resend's outcome stands"
    );
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// The old side drops its LU sockets while a send is in flight and the tab stays: pigeon reopens the LU, reads the
/// old child once the write has settled, and answers from what it shows (no LU error for the tab, one old turn).
#[tokio::test]
async fn an_lu_dropped_with_a_send_in_flight_settles_it_from_the_old_child() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    e.old.st().hold_sends = true;
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-flight"}),
    )
    .await;
    until("the write is claimed", || {
        e.m.st().tickets.first().is_some_and(|t| t.claims == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.old.drop_sockets();
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.old.release_sends();
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1);
    e.m.wait_tickets(|t| t[0].outcome.is_some()).await;
    assert_eq!(e.m.tickets_of(c).len(), 1);
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
}

/// A send whose file copies outlast its ticket's hold: the ticket closes `refused` with nothing written, the next one
/// carries the same copies (each file copied once), and the send goes once.
#[tokio::test]
async fn copies_that_outlast_the_hold_are_made_once() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = upload(&e, "slow.pdf", b"%PDF-1.4 slow").await;
    assert_eq!(r.status(), 201);
    let mpath = common::body_json(r).await["attachments"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(e.old.st().files.is_empty(), "no tee from the gate");
    e.m.st().hold_ms_next = Some(150);
    let read = e.m.hold("upload_raw");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        read.notify_waiters();
    });
    let att = json!({"attachments": [{"type": "uploaded_file", "path": mpath, "fileName": "slow.pdf",
                                      "mimeType": "application/pdf", "size": 13}]});
    let r = send(&mut ws, c, "m-slow", att).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.st().files.len(), 1, "one copy");
    assert_eq!(e.old.agent(c).sends.len(), 1);
    e.m.wait_tickets(|t| t.iter().all(|t| t.outcome.is_some())).await;
    let t = e.m.tickets_of(c);
    let sends: Vec<_> = t.iter().filter(|t| t.kind == TicketKind::Send).collect();
    assert_eq!(sends.len(), 2, "{t:?}");
    assert_eq!(sends[0].outcome, Some(TicketOutcome::Refused));
    assert_eq!(sends[1].outcome, Some(TicketOutcome::Started));
    assert_eq!(t.iter().filter(|t| t.kind == TicketKind::Upload).count(), 1);
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
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
    // an unmoved old-path tab sends the old path on its own socket to the old router: the old child has the file
    let mut direct = Ws::open(&e.old.url, &e.h.token(STAFF), "").await.unwrap().relayed();
    direct
        .raw(
            json!({"type": "hello", "clientId": "old-tab", "clientType": "browser", "protocolVersion": 1,
                    "capabilities": {}, "appVersion": "0.13.2"}),
        )
        .await;
    direct.next_session("status").await;
    let r = send(
        &mut direct,
        c,
        "old-path",
        json!({"attachments": [{"type": "uploaded_file", "path": path, "fileName": "q3.pdf",
                                "mimeType": "application/pdf", "size": 13}]}),
    )
    .await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends[0].2, vec![path.clone()]);
    assert!(e.old.st().old_path_requests > 0);
    drop(direct);
    e.old.end_turn(c);
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
    assert_eq!(e.old.agent(c).sends[1].2, vec![path.clone()]);
    // a pre-move old path sent to a native conversation goes to magpie as it is (magpie resolves it)
    let n = e.m.add(STAFF, None);
    list(&mut ws).await;
    let r = send(
        &mut ws,
        n,
        "native-old-path",
        json!({"attachments": [{"type": "uploaded_file", "path": path, "fileName": "q3.pdf",
                                "mimeType": "application/pdf", "size": 13}]}),
    )
    .await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.m.st().sent.last().unwrap().1.files[0].path, path);
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
        e.old.agent(c).sends[2].2,
        vec![files[1].0.clone()],
        "sent with the old path"
    );
    e.m.wait_tickets(|t| t.iter().all(|t| t.outcome.is_some())).await;
    let kinds: Vec<_> = e.m.tickets_of(c).iter().map(|t| (t.kind, t.outcome)).collect();
    assert_eq!(
        kinds[kinds.len() - 2..],
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

/// A send of magpie-only files to a conversation pigeon still holds as legacy, whose ticket call finds it released (the
/// fence won before the send's ticket, as magpie's fence needs no open ticket): it goes natively with the tab's own
/// message, every path magpie's `uploads/<id>`, and nothing is copied into the old child or ticketed for a copy. A
/// fence that would win while the send's ticket is open (at a copy's upload ticket) cannot: the copies and the send go
/// to the old child under their tickets.
#[tokio::test]
async fn a_copy_at_send_that_loses_to_the_fence_sends_natively() {
    let e = env().await;
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    let c = e.legacy(STAFF);
    let mut paths = Vec::new();
    for name in ["g1.pdf", "g2.pdf"] {
        let r = upload(&e, name, b"%PDF-1.4 g").await;
        let v = common::body_json(r).await;
        paths.push(v["attachments"][0]["path"].as_str().unwrap().to_owned());
    }
    assert!(paths.iter().all(|p| p.starts_with("uploads/")), "{paths:?}");
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let attachments: Vec<Value> = paths
        .iter()
        .map(|p| {
            json!({"type": "uploaded_file", "path": p, "fileName": "g.pdf", "mimeType": "application/pdf",
                         "size": 10})
        })
        .collect();
    // the fence wins at the send's own ticket call
    e.m.st().release_at_ticket.insert(c);
    let r = send(&mut ws, c, "fenced", json!({"attachments": attachments.clone()})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert!(e.old.agent(c).sends.is_empty());
    let sent = e.m.st().sent.clone();
    assert_eq!(sent.len(), 1);
    let native: Vec<String> = sent[0].1.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(native, paths, "the magpie paths, none of the old child's");
    assert!(e.old.st().files.is_empty(), "nothing copied");
    assert!(e.m.tickets_of(c).is_empty(), "no ticket: magpie owned it");
    assert_eq!(e.m.calls_to("upload_raw"), 0);
    // a fence trying to win while the send's ticket is open does not: copied and sent under the tickets
    let d = e.legacy(STAFF);
    list(&mut ws).await;
    e.m.st().release_on_kind = Some((d, TicketKind::Upload, 0));
    let r = send(&mut ws, d, "open", json!({"attachments": attachments})).await;
    assert_eq!(r["accepted"], true, "{r}");
    let old = e.old.agent(d);
    assert_eq!(old.sends.len(), 1);
    assert_eq!(e.old.st().files.len(), 2);
    let copied: Vec<String> = e.old.st().files.iter().map(|f| f.0.clone()).collect();
    assert_eq!(old.sends[0].2, copied, "sent with the copies' old paths");
    assert!(e.m.st().convs[&d].conv.migrating, "never released under an open ticket");
    e.m.st().release_on_kind = None;
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
    // over HTTP: 401 NOT_SIGNED_IN for the old child's files, shares and the upload tee
    let files =
        e.h.get_as(
            &format!("{}{c}/files/raw?path=a.txt", pigeon::paseo::compat::SESSIONS),
            STAFF,
        )
        .await;
    let shares =
        e.h.get_as(&format!("{}{c}/shares", pigeon::paseo::compat::SESSIONS), STAFF)
            .await;
    let tee = upload(&e, "a.pdf", b"%PDF").await;
    for (what, r) in [("files", files), ("shares", shares), ("tee", tee)] {
        let (status, code) = common::code(r).await;
        assert_eq!((status, code.as_str()), (401, "NOT_SIGNED_IN"), "{what}");
    }
}

/// A pigeon replica with LU sockets dies (its process killed): the old router sees its LU sockets close, its tabs
/// reconnect to another replica, which opens LU again, and the conversation keeps streaming; an LU ends with its tab.
#[tokio::test]
async fn a_replica_with_lu_sockets_killed_tabs_resume_and_lu_reopens() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open", || e.h.app.legacy().lu_open() == 1).await;
    drop(ws);
    until("LU closed with its tab", || e.h.app.legacy().lu_open() == 0).await;
    let mut a = Remote::start(&e.served, &e.old).await;
    let mut ws = a.ws(STAFF).await;
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open on the doomed replica", || e.old.sockets() == 1).await;
    a.kill();
    until("the old router saw it close", || e.old.sockets() == 0).await;
    assert!(
        ws.closed_code(Duration::from_secs(5)).await.is_some(),
        "the tab lost its socket"
    );
    let other = replica(&e.served, &e.old).await;
    let mut ws = Ws::hello(&other.url, &other.token(STAFF), "").await.0.relayed();
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU reopened", || other.app.legacy().lu_open() == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.old
        .stream_row(c, json!({"type": "assistant_message", "text": "接住"}));
    ws.wait_for("the row on the new replica", |v| is_stream(v, c)).await;
    assert_eq!(e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst), 3);
}

/// An LU that drops while its tab stays (an old ui pod restart, a bridge blip) is reopened at once: the tab gets one
/// replacement in the old child's epoch for each legacy conversation it watches and the running turn's later rows. An
/// LU that drops again before it has been up [`pigeon::legacy::REOPEN_MIN_UP`] is not reopened: the tab is closed
/// 1012 once request-free and resumes.
#[tokio::test]
async fn an_lu_dropped_under_a_live_tab_is_reopened_and_the_turn_keeps_streaming() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.old.st().agents.get_mut(&c).unwrap().running = true;
    // a token that lives past the reopen rule's hour
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open", || e.old.sockets() == 1).await;
    tokio::time::sleep(pigeon::legacy::REOPEN_MIN_UP + Duration::from_millis(200)).await;
    e.old.drop_sockets();
    let r = ws.next_session("agent.timeline.replacement").await;
    assert_eq!(r["agentId"], c.to_string());
    assert_eq!(r["epoch"], e.old.agent(c).epoch, "the old child's epoch");
    until("LU reopened and subscribed", || e.old.sockets() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seq = e
        .old
        .stream_row(c, json!({"type": "assistant_message", "text": "斷線之後"}));
    let v = ws.wait_for("the turn's later row", |v| is_stream(v, c)).await;
    assert_eq!(v["message"]["payload"]["seq"], seq);
    assert_eq!(e.metric("pigeon_lu_reopened_total"), 1.0);
    assert_eq!(e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst), 2);
    // dropped again at once: the tab resumes instead
    e.old.drop_sockets();
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(1012));
    assert_eq!(e.metric("pigeon_lu_reopened_total"), 1.0);
}

/// The drain closes request-free sockets first: a socket whose LU send is outstanding, longer than the drain's rpc
/// wait, is closed only after its answer.
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
    // held past the drain's rpc wait (3 s here): a ticketed LU write is waited for up to the transfer bound
    tokio::time::sleep(Duration::from_secs(4)).await;
    let early: Vec<Value> = busy.drain_for(Duration::from_millis(100)).await;
    assert!(busy.closed.is_none(), "closed while its LU write was open: {early:?}");
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
    // a replica up for less than the runner's 60 s look-back says its view is incomplete
    assert!(v["startedAt"].is_string(), "{v}");
    assert!(v["uptimeSeconds"].as_u64().unwrap() < 60);
    assert_eq!(v["complete"], false, "{v}");
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
    // a legacy conversation's files are the old child's
    let r =
        e.h.get_as(
            &format!("{}{c}/files/raw?path=a.txt", pigeon::paseo::compat::SESSIONS),
            STAFF,
        )
        .await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "old file bytes");
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

/// A small seeded generator (xorshift64*): the fuzz prints its seed, and `AVIARY_TEST_FUZZ_SEED` replays a run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// The fence-race fuzz through pigeon: two replicas, two tabs per conversation (one on each) sending at once at
/// seeded random instants, while each conversation is released, handed back (a rollback) and released again at
/// random instants (a release only with no open ticket, a reverse only with no native turn). Every send is accepted
/// exactly once on one stack, never refused, counted as lists (a duplicate is seen), each tab's sends in order on each
/// stack, with files copied for the old child (which takes a send only with paths it holds) and magpie's for a native
/// turn.
#[tokio::test]
async fn fence_race_fuzz_a_thousand_sends_each_accepted_once() {
    fuzz(20, 25, false, false).await;
}

/// The fuzz with the old hub's busy refusal: an old turn runs until a random later instant, a send meanwhile is
/// refused by the old hub (`accepted: false`, never by the switch), and the tab sends it again until it is taken.
/// Every send still lands exactly once, in order.
#[tokio::test]
async fn fence_race_fuzz_with_busy_old_turns_each_lands_once() {
    fuzz(8, 10, true, false).await;
}

/// The fuzz with LU drops (an old ui pod restart, a blip of bridge B1) at random instants while sends are in flight,
/// with tokens that let each LU be reopened: a send whose LU dropped is settled from the old timeline and written
/// again only when the old child lacks it, so every send still lands exactly once, in order, and none is refused.
#[tokio::test]
async fn fence_race_fuzz_with_lu_drops_each_lands_once() {
    fuzz(6, 24, false, true).await;
}

#[allow(clippy::print_stdout)] // the seed, for a rerun
#[allow(clippy::fn_params_excessive_bools)] // the fuzz's variants
async fn fuzz(conversations: usize, per_tab: usize, busy: bool, drops: bool) {
    let seed: u64 = aviary_core::env::test_only("AVIARY_TEST_FUZZ_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| (u64::from(std::process::id()) * 0x9e37_79b9) | 1);
    println!("AVIARY_TEST_FUZZ_SEED={seed}");
    let e = env().await;
    let other = replica(&e.served, &e.old).await;
    // with `busy` an old turn runs until the ender below ends it, and the old hub refuses a send meanwhile
    e.old.st().auto_end = !busy;
    e.m.st().auto_end = true;
    // after the gate: an upload is magpie's only, so a send of it to a legacy conversation copies it under tickets
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    let convs: Vec<Uuid> = (0..conversations).map(|_| e.legacy(STAFF)).collect();
    let r = upload(&e, "fuzz.pdf", b"%PDF-1.4 fuzz").await;
    let file = common::body_json(r).await["attachments"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(file.starts_with("uploads/"));
    let mut rng = Rng(seed);
    let mut tasks = tokio::task::JoinSet::new();
    let mut flips = tokio::task::JoinSet::new();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // with `drops`: how many times the LUs were dropped (a tab stays until the first, so a drop meets open tabs)
    let drop_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for (i, c) in convs.iter().copied().enumerate() {
        // with `drops` tokens that live past the reopen rule's hour, so a dropped LU is reopened under the tab
        let life = if drops { 7200 } else { 3600 };
        for (tab, url, token) in [
            (0, e.h.url.clone(), e.h.teams[0].mint(STAFF, life, None, None)),
            (1, other.url.clone(), other.teams[0].mint(STAFF, life, None, None)),
        ] {
            let mut ws = Ws::hello(&url, &token, "").await.0.relayed();
            let file = file.clone();
            let mut r = Rng(rng.next() | 1);
            let drop_count = drop_count.clone();
            tasks.spawn(async move {
                list(&mut ws).await;
                // the tab shows the conversation it sends to (with `drops`, the watch is what a dropped LU is reopened
                // for, lu_ended)
                watch(&mut ws, &[c]).await;
                let mut refused = Vec::new();
                for n in 0..per_tab {
                    tokio::time::sleep(Duration::from_millis(r.below(if drops { 500 } else { 15 }))).await;
                    let cmid = format!("{c}-{tab}-{n}");
                    let extra = if r.below(5) == 0 {
                        json!({"attachments": [{"type": "uploaded_file", "path": file, "fileName": "fuzz.pdf",
                                                "mimeType": "application/pdf", "size": 13}]})
                    } else {
                        json!({})
                    };
                    let mut tries = 0;
                    loop {
                        let a = send(&mut ws, c, &cmid, extra.clone()).await;
                        if a["accepted"] == true {
                            break;
                        }
                        // the old hub's own refusal while its turn runs: the person sends it again
                        if busy && a["error"] == common::old::BUSY && tries < 400 {
                            tries += 1;
                            tokio::time::sleep(Duration::from_millis(5 + r.below(10))).await;
                            continue;
                        }
                        refused.push(a);
                        break;
                    }
                }
                let until = tokio::time::Instant::now() + Duration::from_secs(20);
                while drops
                    && drop_count.load(std::sync::atomic::Ordering::SeqCst) == 0
                    && tokio::time::Instant::now() < until
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if drops {
                    // past the reopen that follows the drop (lu_ended waits 300 ms, then opens)
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                refused
            });
        }
        // release, hand back, release: each at a random instant, each only when the model's rule allows it. With
        // `drops` the first conversation is never flipped: it stays legacy, so every drop meets a watched legacy
        // conversation and the reopen rule has something to reopen for (all of them native by then reopens nothing)
        if drops && i == 0 {
            continue;
        }
        let m = e.m.clone();
        // with `drops` spread over the run, so drops meet legacy conversations
        let stretch = if drops { 40 } else { 1 };
        let at: Vec<u64> = (0..3).map(|_| 20 + rng.below(250) * stretch).collect();
        flips.spawn(async move {
            for (k, wait) in at.into_iter().enumerate() {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                if k % 2 == 0 {
                    while !(m.st().convs[&c].conv.migrating && m.try_release(c)) {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                } else {
                    while !m.try_reverse(c) {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                }
            }
            i
        });
    }
    // with `busy`: each running old turn ends a random while after it started
    let ender = busy.then(|| {
        let (old, done, convs) = (e.old.clone(), done.clone(), convs.clone());
        let mut r = Rng(rng.next() | 1);
        tokio::spawn(async move {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5 + r.below(30))).await;
                for c in &convs {
                    if old.agent(*c).running && r.below(2) == 0 {
                        old.end_turn(*c);
                    }
                }
            }
        })
    });
    // with `drops`: every LU dropped at random instants, each time after the reopen rule's minimum time up
    let dropper = drops.then(|| {
        let (old, done, drop_count) = (e.old.clone(), done.clone(), drop_count.clone());
        let mut r = Rng(rng.next() | 1);
        tokio::spawn(async move {
            let mut n = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let wait = pigeon::legacy::REOPEN_MIN_UP + Duration::from_millis(500 + r.below(1500));
                tokio::time::sleep(wait).await;
                old.drop_sockets();
                n += 1;
                drop_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            n
        })
    });
    while let Some(r) = tasks.join_next().await {
        let refused = r.unwrap();
        assert!(refused.is_empty(), "refused (seed {seed}): {refused:?}");
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(t) = ender {
        t.await.unwrap();
    }
    if let Some(t) = dropper {
        assert!(t.await.unwrap() >= 1, "no LU dropped during the run (seed {seed})");
        assert!(
            e.metric("pigeon_lu_reopened_total") >= 1.0,
            "no LU reopened under a tab (seed {seed})"
        );
    }
    while flips.join_next().await.is_some() {}
    assert!(
        e.old.st().broken_sends.is_empty(),
        "a send named a path the old child does not hold (seed {seed}): {:?}",
        e.old.st().broken_sends
    );
    for c in &convs {
        let old: Vec<String> = e.old.agent(*c).sends.iter().map(|s| s.0.clone()).collect();
        let native: Vec<String> = e.m.st().convs[c]
            .rows
            .iter()
            .filter_map(|r| match &r.item {
                magpie_api::rows::RowItem::User(u) => u.client_message_id.clone(),
                _ => None,
            })
            .collect();
        // each tab's sends arrive in the order it sent them, on each stack
        for stack in [&old, &native] {
            for tab in 0..2 {
                let ns: Vec<usize> = stack
                    .iter()
                    .filter_map(|m| m.strip_prefix(&format!("{c}-{tab}-")))
                    .map(|n| n.parse().unwrap())
                    .collect();
                assert!(ns.windows(2).all(|w| w[0] < w[1]), "out of order (seed {seed}): {ns:?}");
            }
        }
        let mut all: Vec<String> = old.iter().chain(native.iter()).cloned().collect();
        all.sort();
        let n = all.len();
        all.dedup();
        assert_eq!(all.len(), n, "a send went to both stacks or twice (seed {seed})");
        assert_eq!(n, 2 * per_tab, "every send accepted once (seed {seed})");
        // the attachment reached whichever stack ran the turn: an old path the old child holds, magpie's in magpie
        let held: Vec<String> = e.old.st().files.iter().map(|f| f.0.clone()).collect();
        for (cmid, _, paths) in &e.old.agent(*c).sends {
            for p in paths {
                assert!(p.contains(magpie_api::legacy::LEGACY_ASSET_DIR), "{cmid}: {paths:?}");
                assert!(held.contains(p), "{cmid}: {p} not in the old child");
            }
        }
        for (_, req) in e.m.st().sent.iter().filter(|(x, _)| x == c) {
            for f in &req.files {
                assert_eq!(f.path, file);
            }
        }
    }
    assert!(
        !e.old.st().files.is_empty(),
        "some sends copied their file into the old child"
    );
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
    assert_eq!(other.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A reverse hand-over (a rollback): every socket watching the conversation gets one replacement in the old child's
/// epoch, and the old child's frames from then.
#[tokio::test]
async fn a_reverse_hand_over_sends_one_replacement_in_the_old_epoch() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("新"));
    e.old.add(c, STAFF);
    let mut a = e.ws(STAFF).await;
    let mut idle = e.ws(STAFF).await;
    list(&mut a).await;
    list(&mut idle).await;
    watch(&mut a, &[c]).await;
    e.m.set_migrating(c, true);
    let r = a.next_session("agent.timeline.replacement").await;
    assert_eq!(r["epoch"], e.old.agent(c).epoch, "{r}");
    until("LU subscribed", || e.old.st().seen.iter().any(|s| s.1.is_some())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.old
        .stream_row(c, json!({"type": "assistant_message", "text": "回到舊的"}));
    a.wait_for("the old child's row", |v| is_stream(v, c)).await;
    let none: Vec<Value> = idle
        .drain_for(Duration::from_millis(300))
        .await
        .into_iter()
        .filter(|v| v["message"]["type"] == "agent.timeline.replacement")
        .collect();
    assert!(none.is_empty(), "{none:?}");
    assert_eq!(e.metric("pigeon_legacy_replacements_total"), 1.0);
}

/// Each replica keeps its own owner map and learns a release from its own change feed: a tab on each gets exactly
/// one replacement. A list that was asked before the release and answered after it, and a change that lags (an
/// upsert from before the release delivered late), say the conversation is legacy again: neither flips it back.
#[tokio::test]
async fn owner_changes_are_ordered_and_each_replica_sends_one_replacement() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let other = replica(&e.served, &e.old).await;
    let mut a = e.ws(STAFF).await;
    let mut b = Ws::hello(&other.url, &other.token(STAFF), "").await.0.relayed();
    for ws in [&mut a, &mut b] {
        list(ws).await;
        watch(ws, &[c]).await;
    }
    // past the list micro-cache, a list held in magpie, answering what it read before the release
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let lists = e.m.calls_to("list_conversations");
    let hold = e.m.hold("list_conversations");
    e.m.st().stale_list.insert(c, true);
    let rid = a.rid();
    a.session(json!({"type": "fetch_agents_request", "requestId": rid}))
        .await;
    until("the list is held", || e.m.calls_to("list_conversations") > lists).await;
    e.m.set_migrating(c, false);
    let epoch = e.m.epoch(c);
    for ws in [&mut a, &mut b] {
        let r = ws.next_session("agent.timeline.replacement").await;
        assert_eq!(r["epoch"], epoch, "{r}");
    }
    hold.notify_one();
    a.next_session("fetch_agents_response").await;
    e.m.st().stale_list.clear();
    // an upsert from before the release, delivered now
    let mut stale = e.m.st().convs[&c].conv.clone();
    stale.migrating = true;
    e.m.change(
        c,
        magpie_api::changes::ChangeBody::ConversationUpserted {
            conversation: Box::new(stale),
        },
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ws in [&mut a, &mut b] {
        let more: Vec<Value> = ws
            .drain_for(Duration::from_millis(200))
            .await
            .into_iter()
            .filter(|v| v["message"]["type"] == "agent.timeline.replacement")
            .collect();
        assert!(more.is_empty(), "one replacement per socket: {more:?}");
    }
    assert_eq!(e.h.app.legacy().is_legacy(STAFF, &c), Some(false));
    assert_eq!(other.app.legacy().is_legacy(STAFF, &c), Some(false));
    assert_eq!(e.metric("pigeon_legacy_replacements_total"), 1.0);
    assert_eq!(other.metric("pigeon_legacy_replacements_total"), 1.0);
    let r = send(&mut b, c, "native", json!({})).await;
    assert_eq!(r["accepted"], true);
    assert!(e.old.agent(c).sends.is_empty());
}

/// A share create whose owner pigeon read wrongly (owned; magpie answers 409 `route: "legacy"`) is re-routed to the
/// old child under a ticket; a revoke of that link on another replica, before the importer copied it into magpie
/// (magpie: 404), finds it among the old child's links and revokes it there.
#[tokio::test]
async fn share_writes_reach_the_old_child_whatever_replica_and_cache() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.m.st().stale_list.insert(c, false);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.m.st().stale_list.clear();
    assert_eq!(e.h.app.legacy().is_legacy(STAFF, &c), Some(false), "a stale owned read");
    let path = format!("{}{c}/shares", pigeon::paseo::compat::SESSIONS);
    let r =
        e.h.as_token(e.h.post(&path), &e.h.token(STAFF))
            .header("origin", &e.h.url)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 201);
    let share = common::body_json(r).await["id"].as_str().unwrap().to_owned();
    assert!(share.starts_with("sh"), "the old child's link: {share}");
    assert_eq!(e.old.st().shares.len(), 1);
    // a replica that has to look for the link's conversation and cannot read a list: a retryable 503, not a 404
    let other = replica(&e.served, &e.old).await;
    let revoke = || async {
        other
            .as_token(
                other
                    .http
                    .delete(format!("{}{}{share}", other.url, pigeon::paseo::compat::SHARES)),
                &other.token(STAFF),
            )
            .header("origin", &other.url)
            .send()
            .await
            .unwrap()
    };
    e.old.st().share_lists_down = true;
    let r = revoke().await;
    assert_eq!(r.status(), 503, "never a 404 for a link that may be live");
    assert!(e.old.st().shares[0].2, "still live");
    assert_eq!(
        e.metric("pigeon_switch_write_refused_total"),
        0.0,
        "another replica's count"
    );
    assert_eq!(other.metric("pigeon_switch_write_refused_total"), 1.0);
    e.old.st().share_lists_down = false;
    let r = other
        .as_token(
            other
                .http
                .delete(format!("{}{}{share}", other.url, pigeon::paseo::compat::SHARES)),
            &other.token(STAFF),
        )
        .header("origin", &other.url)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(!e.old.st().shares[0].2, "revoked in the old child");
    e.m.wait_tickets(|t| {
        t.iter()
            .any(|t| t.kind == TicketKind::ShareCreate && t.outcome == Some(TicketOutcome::Applied))
            && t.iter()
                .any(|t| t.kind == TicketKind::ShareRevoke && t.outcome == Some(TicketOutcome::Applied))
    })
    .await;
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A delete through LU then a restore: the restore goes under a Restore ticket (pigeon's view of Recently deleted
/// follows its own tickets, not magpie's lagging copy). A rename the old route answers 502 for leaves its ticket open
/// (it may have been applied) and counts as a write the switch failed.
#[tokio::test]
async fn restores_are_ticketed_and_a_5xx_is_not_a_refusal() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let (kind, _) = ws.rpc("archive_agent_request", json!({"agentId": c})).await;
    assert_eq!(kind, "agent_archived");
    assert!(e.old.agent(c).deleted);
    let _ = ws.rpc("refresh_agent_request", json!({"agentId": c})).await;
    assert!(!e.old.agent(c).deleted, "restored in the old child");
    e.m.wait_tickets(|t| {
        t.iter()
            .any(|t| t.kind == TicketKind::Restore && t.outcome == Some(TicketOutcome::Applied))
    })
    .await;
    // the shares route answers 502 (a gateway's error after the old router may have applied it)
    e.old.refuse.store(502, std::sync::atomic::Ordering::SeqCst);
    let path = format!("{}{c}/shares", pigeon::paseo::compat::SESSIONS);
    let r =
        e.h.as_token(e.h.post(&path), &e.h.token(STAFF))
            .header("origin", &e.h.url)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 502);
    e.old.refuse.store(0, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let create =
        e.m.tickets_of(c)
            .into_iter()
            .find(|t| t.kind == TicketKind::ShareCreate)
            .unwrap();
    assert_eq!(create.outcome, None, "left for the importer's expiry");
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 1.0);
}

/// While the person has legacy conversations, a read of a conversation whose owner pigeon cannot read answers a
/// retryable error (socket and HTTP), never magpie's copy; a release whose new epoch cannot be read closes the
/// watching tab 1012 (it resubscribes) rather than leaving it with no feed.
#[tokio::test]
async fn an_unreadable_owner_is_a_retryable_error_and_an_unreadable_release_reconnects() {
    let m = Model::default();
    let served = model::serve(&m).await;
    let old = Old::start(LU_PAIR).await;
    let h = Harness::start_opts(Opts {
        tuning: Tuning {
            legacy_release_wait: Duration::from_secs(1),
            ..fast()
        },
        magpie: Some(served.url()),
        legacy: Some(old.url.clone()),
        ..Opts::default()
    })
    .await;
    let e = Env { h, m, old, served };
    let c = e.legacy(STAFF);
    let unread = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    // pigeon has read neither; magpie's conversation read fails
    for _ in 0..4 {
        e.m.refuse_once(
            "get_conversation",
            magpie_api::errors::Refusal::new(magpie_api::errors::ErrorCode::Unavailable, "down"),
        );
    }
    let (kind, r) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": unread, "direction": "tail", "limit": 5}),
        )
        .await;
    assert_eq!(kind, "rpc_error", "{r}");
    let r =
        e.h.get_as(
            &format!("{}{unread}/files/raw?path=a.txt", pigeon::paseo::compat::SESSIONS),
            STAFF,
        )
        .await;
    assert_eq!(r.status(), 503);
    e.m.st().refuse.clear();
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    for _ in 0..50 {
        e.m.refuse_once(
            "timeline",
            magpie_api::errors::Refusal::new(magpie_api::errors::ErrorCode::Unavailable, "down"),
        );
    }
    e.m.set_migrating(c, false);
    assert_eq!(ws.closed_code(Duration::from_secs(8)).await, Some(1012));
    assert_eq!(e.metric("pigeon_legacy_release_unread_total"), 1.0);
    e.m.st().refuse.clear();
}

/// A send whose ticket's hold has run out when its write would start closes that ticket `refused` (before it asks
/// again, so the repeat cannot get the same ticket back) and writes under a new one: one old turn, two tickets.
#[tokio::test]
async fn a_write_past_its_tickets_hold_takes_another_ticket() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.m.st().hold_ms_next = Some(0);
    let r = send(&mut ws, c, "m-hold", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1);
    e.m.wait_tickets(|t| t.len() == 2 && t.iter().all(|t| t.outcome.is_some()))
        .await;
    let t = e.m.tickets_of(c);
    assert_eq!(t[0].outcome, Some(TicketOutcome::Refused), "nothing written under it");
    assert_eq!(t[1].outcome, Some(TicketOutcome::Started));
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A send's claim whose answer comes back later than [`pigeon::legacy::CLAIM_ANSWER_MAX`] (a slow magpie or network):
/// nothing is written under it, its hand-out closes `refused`, and the send goes once under a new ticket.
#[tokio::test]
async fn a_send_whose_claim_answers_late_is_written_under_a_new_ticket() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let claims = e.m.hold("legacy_ticket_claim");
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-late"}),
    )
    .await;
    until("at its claim", || e.m.calls_to("legacy_ticket_claim") == 1).await;
    tokio::time::sleep(pigeon::legacy::CLAIM_ANSWER_MAX + Duration::from_millis(500)).await;
    assert!(e.old.agent(c).sends.is_empty());
    claims.notify_one();
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "one write");
    e.m.wait_tickets(|t| t.len() == 2 && t.iter().all(|t| t.outcome.is_some()))
        .await;
    let t = e.m.tickets_of(c);
    assert_eq!(
        (t[0].claims, t[0].outcome),
        (1, Some(TicketOutcome::Refused)),
        "claimed late, nothing written under it"
    );
    assert_eq!(t[1].outcome, Some(TicketOutcome::Started));
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A repeat checked absent and written again, which the old hub refuses because the send's earlier write landed
/// late in between and is the turn that runs: the old child is read once more, and the send is answered accepted with
/// its ticket reported from what the old child shows (not `refused`, which would invite a duplicate resend).
#[tokio::test]
async fn a_repeat_refused_because_its_first_write_landed_late_is_answered_accepted() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    // an earlier request's ticket, open past the reuse age, nearly settled
    let first = e.m.open_send_ticket(c, "m-landed", Duration::from_secs(89));
    let claims = e.m.hold("legacy_ticket_claim");
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-landed"}),
    )
    .await;
    until("checked absent and at its claim", || {
        e.m.calls_to("legacy_ticket_claim") == 1
    })
    .await;
    // the earlier write lands now
    e.old.take_send(c, "m-landed", "繼續", Vec::new());
    claims.notify_one();
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "one turn");
    e.m.wait_tickets(|t| t.iter().any(|t| t.id != first && t.outcome.is_some()))
        .await;
    let t: Vec<_> = e.m.tickets_of(c).into_iter().filter(|t| t.id != first).collect();
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(
        t[0].outcome,
        Some(TicketOutcome::Started),
        "reported from the old child"
    );
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A repeat's check pages back through the whole old timeline: a user row far beyond the newest page is found (not
/// sent again), and a row that is nowhere in a long timeline is sent.
#[tokio::test]
async fn a_repeat_reads_the_whole_old_timeline_before_it_sends_again() {
    let e = env().await;
    let c = e.legacy(STAFF);
    {
        let mut st = e.old.st();
        let rows = &mut st.agents.get_mut(&c).unwrap().rows;
        for i in 0..1200 {
            rows.push(json!({"type": "tool_call", "text": format!("工具 {i}")}));
        }
        rows.insert(
            100,
            json!({"type": "user_message", "id": "paseo-user-m-deep", "clientMessageId": "m-deep", "text": "x"}),
        );
    }
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.m.open_send_ticket(c, "m-deep", Duration::from_secs(89));
    let r = send(&mut ws, c, "m-deep", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert!(e.old.agent(c).sends.is_empty(), "found 1100 rows back: not sent again");
    e.m.open_send_ticket(c, "m-nowhere", Duration::from_secs(89));
    let r = send(&mut ws, c, "m-nowhere", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "absent from every page: sent");
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A socket whose person has no unexpired token on this replica (its token ran out at the very moment of the request,
/// before the socket's own recheck closed it, or the book evicted it): its LU cannot open as the person, and the
/// write gets the signed-out answer (4401, counted with the LU 401s), never a write error.
#[tokio::test]
async fn a_socket_with_no_live_token_gets_the_signed_out_answer() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    e.h.app.legacy().tokens().forget(STAFF);
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "x",
                      "messageId": "m-exp"}),
    )
    .await;
    assert_eq!(ws.closed_code(Duration::from_secs(5)).await, Some(4401));
    assert!(e.metric("pigeon_lu_401_total") >= 1.0);
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
    assert_eq!(
        e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no upgrade without a token"
    );
    assert!(e.old.agent(c).sends.is_empty());
}

/// An LU that cannot open (the old ui down) does not block a write to a conversation magpie already owns while
/// pigeon still holds it as legacy: the ticket answers `native` and the send goes natively; under a `legacy` answer
/// the LU's failure is the answer, with nothing written under the ticket.
#[tokio::test]
async fn a_write_whose_lu_cannot_open_still_takes_its_ticket() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let d = e.legacy(STAFF);
    // a token that lives past the reopen rule's hour (the failed LU is tried again, not the tab closed)
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    e.old.refuse.store(503, std::sync::atomic::Ordering::SeqCst);
    e.m.st().release_at_ticket.insert(c);
    let r = send(&mut ws, c, "m-moved", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.m.st().sent.len(), 1, "sent natively");
    let (kind, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": d, "text": "x", "messageId": "m-down"}),
        )
        .await;
    assert_eq!(kind, "rpc_error", "{r}");
    e.m.wait_tickets(|t| t.iter().any(|t| t.conversation == d && t.outcome.is_some()))
        .await;
    assert_eq!(e.m.tickets_of(d)[0].outcome, Some(TicketOutcome::Refused));
    assert_eq!(e.m.tickets_of(d)[0].claims, 0);
    e.old.refuse.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// An LU that streams nothing and carries no call is closed while its tab stays (after every conversation it
/// watched was released): the old router's LU sockets reach zero, the tab keeps working, and its next legacy request
/// opens a new LU (no reconnect), which closes again once unused.
#[tokio::test]
async fn an_lu_that_streams_nothing_closes_while_its_tab_stays() {
    let m = Model::default();
    let served = model::serve(&m).await;
    let old = Old::start(LU_PAIR).await;
    let h = replica_at(
        &served,
        &old.url,
        Tuning {
            legacy_lu_idle: Duration::from_secs(1),
            ..fast()
        },
    )
    .await;
    let e = Env { h, m, old, served };
    let c = e.legacy(STAFF);
    let d = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open", || e.old.sockets() == 1).await;
    // watched: kept past the idle time
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(e.old.sockets(), 1);
    assert!(e.m.try_release(c));
    let r = ws.next_session("agent.timeline.replacement").await;
    assert_eq!(r["agentId"], c.to_string());
    until("the LU closed", || {
        e.old.sockets() == 0 && e.h.app.legacy().lu_open() == 0
    })
    .await;
    list(&mut ws).await;
    assert!(ws.closed.is_none(), "the tab stays");
    // the next legacy request opens a new one, with no reconnect
    let r = send(&mut ws, d, "m-again", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(d).sends.len(), 1);
    until("closed again once unused", || e.old.sockets() == 0).await;
    assert!(ws.closed.is_none());
    assert_eq!(e.old.upgrades.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
}

/// A send through a bridge whose far leg stalls (a partition behind the load balancer) until its write's bound passes:
/// pigeon resets its LU, but the bridge has acknowledged the frame and delivers it when it heals; the old side
/// refuses it there by its not-after instant, so the reopened LU's write, made once the write had settled and found it
/// absent, is the one turn.
#[tokio::test]
async fn a_write_through_a_stalled_bridge_never_lands_after_it_heals() {
    let m = Model::default();
    let served = model::serve(&m).await;
    let old = Old::start(LU_PAIR).await;
    let bridge = common::stall::Stall::start(&old.url).await;
    let h = replica_at(
        &served,
        &bridge.url,
        Tuning {
            legacy_write_timeout: Duration::from_millis(1500),
            legacy_settle: Duration::from_secs(2),
            ..fast()
        },
    )
    .await;
    let e = Env { h, m, old, served };
    let c = e.legacy(STAFF);
    // a token that lives past the reopen rule's hour
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    watch(&mut ws, &[c]).await;
    until("LU open", || e.old.sockets() == 1).await;
    tokio::time::sleep(pigeon::legacy::REOPEN_MIN_UP + Duration::from_millis(200)).await;
    bridge.stall();
    let r = send(&mut ws, c, "m-partition", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1);
    assert!(
        bridge.resets.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the stalled LU was reset"
    );
    // the turn of the second write ends before the bridge heals: the old hub's busy refusal is no guard then
    e.old.end_turn(c);
    bridge.heal();
    until("the held frame delivered", || {
        bridge.late.load(std::sync::atomic::Ordering::SeqCst) > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(e.old.agent(c).sends.len(), 1, "the late frame is not a second turn");
    assert!(e.old.st().late_refused >= 1, "refused as late");
    e.m.wait_tickets(|t| t.len() == 1 && t[0].outcome == Some(TicketOutcome::Started))
        .await;
    assert_eq!(e.m.tickets_of(c)[0].claims, 2, "written again under its ticket");
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A ticket's outcome is reported in the background after the tab's answer: until it lands it counts as a transfer of
/// the process, so a drain does not exit before magpie has it (the ticket would otherwise wait for the importer to
/// expire it, the conversation held legacy meanwhile).
#[tokio::test]
async fn a_ticket_outcome_still_being_reported_holds_the_drain() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    let done = e.m.hold("legacy_ticket_done");
    let r = send(&mut ws, c, "m-report", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    until("the outcome is being reported", || {
        e.m.calls_to("legacy_ticket_done") == 1
    })
    .await;
    assert_eq!(e.h.app.transfers().active(), 1, "a transfer while it is reported");
    done.notify_one();
    until("reported", || e.h.app.transfers().active() == 0).await;
    e.m.wait_tickets(|t| t[0].outcome == Some(TicketOutcome::Started)).await;
}

/// A resend meets the open ticket an earlier holder claimed and wrote under (its replica gone since), on a replica
/// whose LU cannot open: its `refused` closes nothing, so the ticket stays open, and the next resend reads the old
/// timeline, finds the send there and answers accepted with no second turn.
#[tokio::test]
async fn a_resend_whose_lu_cannot_open_leaves_an_earlier_claimed_ticket_open() {
    let e = env().await;
    let c = e.legacy(STAFF);
    // a token that lives past the reopen rule's hour (the failed LU is tried again, not the tab closed)
    let token = e.h.teams[0].mint(STAFF, 7200, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    let t = e.m.open_send_ticket(c, "m-earlier", Duration::from_secs(2));
    e.m.claim_by_holder(t);
    // the earlier holder's write landed: its turn runs
    e.old.take_send(c, "m-earlier", "繼續", Vec::new());
    e.old.refuse.store(503, std::sync::atomic::Ordering::SeqCst);
    let (kind, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "繼續", "messageId": "m-earlier"}),
        )
        .await;
    assert_eq!(kind, "rpc_error", "{r}");
    until("the resend reported", || e.m.calls_to("legacy_ticket_done") >= 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let tk = e.m.tickets_of(c);
    assert_eq!(tk.len(), 1, "{tk:?}");
    assert!(
        tk[0].open(),
        "an earlier take's claim stands: not closed refused ({tk:?})"
    );
    e.old.refuse.store(0, std::sync::atomic::Ordering::SeqCst);
    let r = send(&mut ws, c, "m-earlier", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "no second turn");
    e.m.wait_tickets(|t| t[0].outcome == Some(TicketOutcome::Started)).await;
}

/// An LU drops with a send in flight under a tab whose token has under an hour left, so it is not reopened (DESIGN
/// 9.5's E4): the write is left unanswered with its ticket open, the tab is closed 1012 and resends after it
/// reconnects, and the resend is settled from the old timeline and written once. No error reaches the tab and no
/// write counts as refused.
#[tokio::test]
async fn a_send_in_flight_under_a_tab_that_must_reconnect_is_answered_after_it() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let token = e.h.teams[0].mint(STAFF, 1800, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    tokio::time::sleep(pigeon::legacy::REOPEN_MIN_UP).await;
    // the old side's sockets drop as the send reaches it, before the child sees it
    e.old.st().drop_on_send = true;
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-reconnect"}),
    )
    .await;
    let frames = ws.drain_for(Duration::from_secs(5)).await;
    assert!(
        !frames
            .iter()
            .any(|f| f["message"]["payload"]["requestId"] == rid.as_str()),
        "the write was answered before the reconnect: {frames:?}"
    );
    assert_eq!(ws.closed.as_ref().map(|c| c.0), Some(1012), "{frames:?}");
    let t = e.m.tickets_of(c);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].open() && t[0].claims == 1, "left open for the resend: {t:?}");
    assert_eq!(e.old.agent(c).sends.len(), 0);
    // the claimed write has settled by the time the resend reads (a test does not wait the claim's 30 s)
    e.m.age_claim(t[0].id, magpie_api::legacy::TICKET_CLAIM_SETTLE);
    let token = e.h.teams[0].mint(STAFF, 1800, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    list(&mut ws).await;
    let r = send(&mut ws, c, "m-reconnect", json!({})).await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.old.agent(c).sends.len(), 1, "one turn");
    e.m.wait_tickets(|t| t[0].outcome == Some(TicketOutcome::Started)).await;
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
}

/// A holder whose claim answers that its resend holds the ticket answers its tab nothing, even while that tab is
/// still connected: the resend's answer is the send's, and an error would show a failed send that is delivered.
#[tokio::test]
async fn a_holder_whose_resend_holds_the_ticket_answers_nothing() {
    let e = env().await;
    let c = e.legacy(STAFF);
    let claims = e.m.hold("legacy_ticket_claim");
    let mut first = e.ws(STAFF).await;
    list(&mut first).await;
    let rid = first.rid();
    first
        .session(
            json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                          "messageId": "m-handed"}),
        )
        .await;
    until("the first holder is at its claim", || {
        e.m.st().calls.iter().any(|x| x.0 == "legacy_ticket_claim")
    })
    .await;
    let mut second = e.ws(STAFF).await;
    list(&mut second).await;
    let rid2 = second.rid();
    second
        .session(
            json!({"type": "send_agent_message_request", "requestId": rid2, "agentId": c, "text": "繼續",
                          "messageId": "m-handed"}),
        )
        .await;
    until("the resend is at its claim", || {
        e.m.st().calls.iter().filter(|x| x.0 == "legacy_ticket_claim").count() == 2
    })
    .await;
    claims.notify_waiters();
    let r = second.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    let frames = first.drain_for(Duration::from_millis(600)).await;
    assert!(
        !frames
            .iter()
            .any(|f| f["message"]["payload"]["requestId"] == rid.as_str()),
        "the first holder answered: {frames:?}"
    );
    assert_eq!(e.old.agent(c).sends.len(), 1, "one write");
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}

/// A send's ticket is unclaimed while its files are copied, so the importer may expire it (issued more than
/// `TICKET_SETTLE_MAX` before a read that showed nothing in flight) and release the conversation mid-copy: the send
/// then finds its conversation owned and goes natively once, with the tab's own message; the old child gets no turn.
#[tokio::test]
async fn a_send_released_during_its_copies_goes_natively_once() {
    let e = env().await;
    let c = e.legacy(STAFF);
    e.m.st().gate = Some(magpie_api::Timestamp::now());
    let mut ws = e.ws(STAFF).await;
    list(&mut ws).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = upload(&e, "late.pdf", b"%PDF-1.4 late").await;
    assert_eq!(r.status(), 201);
    let mpath = common::body_json(r).await["attachments"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let read = e.m.hold("upload_raw");
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "繼續",
                      "messageId": "m-mid-copy",
                      "attachments": [{"type": "uploaded_file", "path": mpath, "fileName": "late.pdf",
                                       "mimeType": "application/pdf", "size": 13}]}),
    )
    .await;
    until("the copy is reading the file", || e.m.calls_to("upload_raw") == 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let t = e.m.tickets_of(c);
    assert_eq!(t.len(), 1, "{t:?}");
    e.m.age_ticket(t[0].id, magpie_api::legacy::TICKET_SETTLE_MAX + Duration::from_secs(1));
    assert_eq!(
        e.m.expire_settled(c),
        vec![t[0].id],
        "the unclaimed send ticket expires"
    );
    assert!(e.m.try_release(c), "released mid-copy");
    read.notify_waiters();
    let r = ws.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true, "{r}");
    assert_eq!(e.m.st().sent.len(), 1, "sent natively");
    assert_eq!(e.old.agent(c).sends.len(), 0, "no old turn");
    assert_eq!(e.metric("pigeon_switch_write_refused_total"), 0.0);
}
