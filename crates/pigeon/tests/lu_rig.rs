//! The LU rig's cases (WP-54, DESIGN 9.9): the switch window's legacy upstream against the real old stack, not the
//! fake old router of `legacy.rs`: the ui image of the A1 commit (the router with WP-56's switch module and `CloudCLI`),
//! its web role and sandbox agent stand-in and a mock model, brought up by `testing/lu-rig/run.sh`; pigeon runs here in
//! front of the in-memory magpie model, its legacy upstream at the rig's ui through a relay the cases can cut. Every
//! old turn is a real Claude Code process in the stand-in's sandbox. Ignored unless `AVIARY_TEST_LU_RIG_UI` is set (`run.sh test`
//! sets it, with `AVIARY_TEST_LU_RIG_KEYS` so the harness signs with the key the ui trusts).
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::many_single_char_names
)] // tests

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::model::{self, Model};
use common::ws::Ws;
use common::{Harness, Opts, STAFF, STAFF2};
use magpie_api::legacy::{TicketKind, TicketOutcome};
use pigeon::Tuning;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uuid::Uuid;

/// The rig's addresses (`testing/lu-rig/run.sh up` prints them).
struct Rig {
    ui: String,
    old: String,
    model: String,
    switch: String,
    /// The old router's switch endpoints (`<ui>/<prefix>`: sockets, move, admit), from run.sh (the old stack's names
    /// stay out of this crate, platform 4.5).
    endpoints: String,
    http: reqwest::Client,
}

impl Rig {
    fn from_env() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let var = |k: &str| {
            aviary_core::env::test_only(k).unwrap_or_else(|| panic!("{k} is not set: run testing/lu-rig/run.sh"))
        };
        Self {
            ui: var("AVIARY_TEST_LU_RIG_UI"),
            old: var("AVIARY_TEST_LU_RIG_OLD"),
            model: var("AVIARY_TEST_LU_RIG_MODEL"),
            switch: var("AVIARY_TEST_LU_RIG_SWITCH_TOKEN"),
            endpoints: var("AVIARY_TEST_LU_RIG_SWITCH_URL"),
            http: reqwest::Client::new(),
        }
    }

    /// A conversation the old stack made for `person` (`CloudCLI`'s own create, through the router as the person).
    async fn session(&self, h: &Harness, person: &str) -> Uuid {
        let mut last = String::new();
        // the person's CloudCLI process may still be starting (503 with a retry-after): ask again
        for _ in 0..30 {
            let r = self
                .http
                .post(format!("{}/api/providers/sessions", self.ui))
                .header("cf-access-jwt-assertion", h.token(person))
                .json(&json!({"provider": "claude", "projectPath": "/workspace"}))
                .send()
                .await
                .unwrap();
            let status = r.status();
            last = r.text().await.unwrap_or_default();
            if let Some(id) = serde_json::from_str::<Value>(&last)
                .ok()
                .and_then(|v| v["data"]["sessionId"].as_str().and_then(|s| s.parse().ok()))
            {
                return id;
            }
            last = format!("{status} {last}");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        panic!("no session for {person}: {last}");
    }

    /// The old child's history of `id`: `(kind, role, text)` per message.
    async fn history(&self, h: &Harness, person: &str, id: Uuid) -> Vec<(String, String, String)> {
        let v: Value = self
            .http
            .get(format!("{}/api/providers/sessions/{id}/messages", self.ui))
            .header("cf-access-jwt-assertion", h.token(person))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["data"]["messages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| {
                        let s = |k: &str| m[k].as_str().unwrap_or_default().to_owned();
                        (s("kind"), s("role"), s("content"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The switch module's report (`GET <endpoints>/sockets`).
    async fn sockets(&self) -> Value {
        self.http
            .get(format!("{}/sockets", self.endpoints))
            .bearer_auth(&self.switch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn move_person(&self, person: &str) -> Value {
        self.http
            .post(format!("{}/move?person={person}", self.endpoints))
            .bearer_auth(&self.switch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// `POST <endpoints>/admit`: undoes the arming a move did (each case starts unarmed; the rig's own REST calls are
    /// not LU, so an armed router would pass them on to pigeon).
    async fn admit(&self) {
        let r = self
            .http
            .post(format!("{}/admit", self.endpoints))
            .bearer_auth(&self.switch)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
    }

    /// What the model was sent: the last user message's content of each request (JSON text).
    async fn model_prompts(&self) -> Vec<String> {
        let v: Vec<Value> = self
            .http
            .get(format!("{}/__log", self.model))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v.iter().filter_map(|x| x["user"].as_str().map(str::to_owned)).collect()
    }

    /// The stand-in's processes: `{conversation, args, exited}` each.
    async fn procs(&self) -> Vec<Value> {
        self.http
            .get(format!("{}/__rig/procs", self.old))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// The stand-in's log: a sandbox process whose /proc connection dropped while it ran (what kills an old turn).
    async fn cuts(&self) -> usize {
        let v: Vec<Value> = self
            .http
            .get(format!("{}/__rig/calls", self.old))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v.iter().filter(|c| c["cut"] == true).count()
    }
}

/// A TCP relay between pigeon and the ui (bridge B1's stand-in) whose connections a case can reset all at once.
#[derive(Clone)]
struct Cutter {
    url: String,
    cut: Arc<watch::Sender<u64>>,
    opened: Arc<Mutex<usize>>,
}

impl Cutter {
    async fn start(target: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (tx, _) = watch::channel(0u64);
        let me = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            cut: Arc::new(tx),
            opened: Arc::new(Mutex::new(0)),
        };
        let to = target.trim_start_matches("http://").to_owned();
        let r = me.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let Ok(mut up) = TcpStream::connect(&to).await else {
                    continue;
                };
                *r.opened.lock().unwrap() += 1;
                let mut rx = r.cut.subscribe();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut client, &mut up) => {}
                        // both legs end at once: what a restarted pod or a dropped bridge gives the LU
                        _ = rx.changed() => {}
                    }
                    let _ = client.shutdown().await;
                });
            }
        });
        me
    }

    fn cut_all(&self) {
        self.cut.send_modify(|n| *n += 1);
    }

    fn opened(&self) -> usize {
        *self.opened.lock().unwrap()
    }
}

struct Env {
    rig: Rig,
    h: Harness,
    m: Model,
    relay: Cutter,
    _served: aviary_fakes::Served,
}

fn tuning() -> Tuning {
    Tuning {
        poll_wait: Duration::from_secs(5),
        poll_backoff_min: Duration::from_millis(50),
        poll_backoff_max: Duration::from_millis(200),
        socket_quiet: Duration::from_millis(300),
        legacy_heartbeat: Duration::from_secs(2),
        ..common::tuning()
    }
}

async fn env() -> Env {
    let rig = Rig::from_env();
    rig.admit().await;
    let m = Model::default();
    let served = model::serve(&m).await;
    let relay = Cutter::start(&rig.ui).await;
    let h = Harness::start_opts(Opts {
        tuning: tuning(),
        magpie: Some(served.url()),
        legacy: Some(relay.url.clone()),
        ..Opts::default()
    })
    .await;
    Env {
        rig,
        h,
        m,
        relay,
        _served: served,
    }
}

impl Env {
    async fn ws(&self, email: &str) -> Ws {
        Ws::hello(&self.h.url, &self.h.token(email), "").await.0.relayed()
    }

    /// A conversation the old stack owns: made by `CloudCLI`, imported as legacy in magpie's model.
    async fn legacy(&self, person: &str) -> Uuid {
        let id = self.rig.session(&self.h, person).await;
        self.m.add_legacy_id(person, id, Some("舊對話"));
        id
    }

    fn metric(&self, name: &str) -> f64 {
        self.h.metric(name)
    }

    /// Every zero counter pigeon owns is still 0.
    fn zero_counters(&self) {
        self.zero_counters_but(&[]);
    }

    fn zero_counters_but(&self, but: &[&str]) {
        for c in [
            "pigeon_switch_write_refused_total",
            "pigeon_lu_errors_total",
            "pigeon_lu_503_total",
            "pigeon_lu_401_total",
            "pigeon_upload_tee_failed_total",
        ] {
            if but.contains(&c) {
                continue;
            }
            assert_eq!(self.metric(c), 0.0, "{c}");
        }
    }
}

async fn watch(ws: &mut Ws, ids: &[Uuid]) -> String {
    let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let (_, r) = ws
        .rpc("agent.timeline.set_subscription.request", json!({"agentIds": ids}))
        .await;
    r["subscriptionId"].as_str().unwrap_or_else(|| panic!("{r}")).to_owned()
}

fn is_stream(v: &Value, agent: Uuid) -> bool {
    v["message"]["type"] == "agent_stream" && v["message"]["payload"]["agentId"] == agent.to_string()
}

fn event_type(v: &Value) -> &str {
    v["message"]["payload"]["event"]["type"].as_str().unwrap_or_default()
}

async fn send(ws: &mut Ws, c: Uuid, cmid: &str, text: &str) -> Value {
    ws.rpc(
        "send_agent_message_request",
        json!({"agentId": c, "text": text, "messageId": cmid}),
    )
    .await
    .1
}

async fn eventually<F, Fut>(what: &str, secs: u64, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("never: {what}");
}

/// A send to a legacy conversation reaches the old child through LU once, as the person, under a ticket that closes
/// on the old stack's outcome; the real Claude Code turn streams back to the tab under its own subscription to its
/// end; the old router counts the socket as LU (never a non-LU socket of the person, never moved).
#[tokio::test]
#[ignore = "the LU rig: testing/lu-rig/run.sh test"]
async fn a_send_runs_once_on_the_old_child_and_streams_back_through_lu() {
    let e = env().await;
    let c = e.legacy(STAFF).await;
    let mut ws = e.ws(STAFF).await;
    let sub = watch(&mut ws, &[c]).await;
    let (_, page) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail", "limit": 50}),
        )
        .await;
    assert!(
        page["epoch"].as_str().is_some_and(|s| !s.is_empty()),
        "the old child's epoch: {page}"
    );
    let token = Uuid::new_v4();
    let r = send(&mut ws, c, "rig-1", &format!("[[slow 3]] hello {token}")).await;
    assert_eq!(r["accepted"], true, "{r}");
    let v = ws
        .wait_for("the turn's last chunk", |v| {
            is_stream(v, c) && v.to_string().contains("chunk 3")
        })
        .await;
    assert_eq!(v["message"]["payload"]["subscriptionId"], sub);
    ws.wait_for("the turn's end", |v| {
        is_stream(v, c) && event_type(v) == "turn_completed"
    })
    .await;
    // the old child's transcript has the message once
    eventually("the old history", 20, || async {
        let hist = e.rig.history(&e.h, STAFF, c).await;
        hist.iter().any(|m| m.1 == "assistant" && m.2.contains("done 3"))
    })
    .await;
    let hist = e.rig.history(&e.h, STAFF, c).await;
    assert_eq!(
        hist.iter()
            .filter(|m| m.1 == "user" && m.2.contains(&token.to_string()))
            .count(),
        1,
        "the message once in the old transcript: {hist:?}"
    );
    e.m.wait_tickets(|t| t.iter().any(|t| t.outcome.is_some())).await;
    let t = e.m.tickets_of(c);
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(
        (t[0].kind, t[0].outcome),
        (TicketKind::Send, Some(TicketOutcome::Started))
    );
    assert_eq!(e.m.calls_to("send_turn"), 0, "nothing went to magpie's write routes");
    // the old router: an LU socket of the person, no other; a move leaves it
    let rep = e.rig.sockets().await;
    let me = &rep["persons"][STAFF];
    assert!(me["lu"].as_u64().unwrap_or(0) >= 1, "{rep}");
    assert_eq!(me["sockets"].as_array().map_or(0, Vec::len), 0, "{rep}");
    let mv = e.rig.move_person(STAFF).await;
    assert_eq!(mv["closed"], 0, "{mv}");
    e.zero_counters();
    assert_eq!(e.rig.cuts().await, 0, "no old turn was cut");
}

/// A turn running on the old child keeps streaming while the person's sockets are moved and Stop from the tab ends
/// it under a Stop ticket; the old sandbox process ends by Stop, never by a dropped connection.
#[tokio::test]
#[ignore = "the LU rig: testing/lu-rig/run.sh test"]
async fn a_running_old_turn_streams_across_a_move_and_stop_ends_it() {
    let e = env().await;
    let c = e.legacy(STAFF2).await;
    let mut ws = e.ws(STAFF2).await;
    watch(&mut ws, &[c]).await;
    let r = send(&mut ws, c, "rig-long", "[[slow 60]] a long one").await;
    assert_eq!(r["accepted"], true, "{r}");
    ws.wait_for("chunk 2", |v| is_stream(v, c) && v.to_string().contains("chunk 2"))
        .await;
    let mv = e.rig.move_person(STAFF2).await;
    assert_eq!(mv["closed"], 0, "LU is never moved: {mv}");
    ws.wait_for("chunk 4 after the move", |v| {
        is_stream(v, c) && v.to_string().contains("chunk 4")
    })
    .await;
    let (kind, r) = ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    assert_eq!(kind, "cancel_agent_response");
    assert!(r["error"].is_null(), "{r}");
    ws.wait_for("the turn's end", |v| {
        is_stream(v, c) && matches!(event_type(v), "turn_canceled" | "turn_completed" | "turn_failed")
    })
    .await;
    eventually("the old Claude Code to end", 30, || async {
        e.rig
            .procs()
            .await
            .iter()
            .filter(|p| p["conversation"] == c.to_string())
            .all(|p| !p["exited"].is_null())
    })
    .await;
    e.m.wait_tickets(|t| t.iter().filter(|t| t.outcome.is_some()).count() >= 2)
        .await;
    let kinds: Vec<_> = e.m.tickets_of(c).iter().map(|t| (t.kind, t.outcome)).collect();
    assert!(
        kinds.contains(&(TicketKind::Stop, Some(TicketOutcome::Applied))),
        "{kinds:?}"
    );
    e.zero_counters();
    assert_eq!(e.rig.cuts().await, 0, "the turn ended by Stop, not by a dropped /proc");
}

/// An LU reset under a live tab (a ui pod restart or a bridge blip) is reopened by pigeon on its own (E4): the old
/// turn keeps running on the old child, the tab keeps its socket and gets the rest of the stream, nothing is cut.
#[tokio::test]
#[ignore = "the LU rig: testing/lu-rig/run.sh test"]
async fn an_lu_reset_under_a_live_tab_is_reopened_and_the_old_turn_is_not_cut() {
    let e = env().await;
    let c = e.legacy(STAFF).await;
    // a token with more than REOPEN_MIN_LIFE (1 h) left: the reopen rule's case (with less the tab is closed 1012 and
    // reconnects through the edge instead, legacy.rs)
    let token = e.h.teams[0].mint(STAFF, 3 * 3600, None, None);
    let mut ws = Ws::hello(&e.h.url, &token, "").await.0.relayed();
    watch(&mut ws, &[c]).await;
    let r = send(&mut ws, c, "rig-reset", "[[slow 12]] across a reset").await;
    assert_eq!(r["accepted"], true, "{r}");
    ws.wait_for("chunk 6", |v| is_stream(v, c) && v.to_string().contains("chunk 6"))
        .await;
    let before = e.relay.opened();
    e.relay.cut_all();
    ws.wait_for("the turn's end after the reset", |v| {
        is_stream(v, c) && event_type(v) == "turn_completed"
    })
    .await;
    eventually("the old history", 20, || async {
        e.rig
            .history(&e.h, STAFF, c)
            .await
            .iter()
            .any(|m| m.2.contains("done 12"))
    })
    .await;
    assert!(e.relay.opened() > before, "the LU was reopened");
    assert!(e.metric("pigeon_lu_reopened_total") >= 1.0);
    let hist = e.rig.history(&e.h, STAFF, c).await;
    assert!(
        hist.iter().any(|m| m.1 == "assistant" && m.2.contains("done 12")),
        "{hist:?}"
    );
    e.zero_counters();
    assert_eq!(e.rig.cuts().await, 0, "the old turn ran to its end");
}

/// An upload from T0 to the gate is teed: pigeon writes it into the old child through LU and answers the old asset
/// path; a send of that path to the legacy conversation reaches the agent, which reads the file.
#[tokio::test]
#[ignore = "the LU rig: testing/lu-rig/run.sh test"]
async fn a_teed_upload_reaches_the_old_agent() {
    let e = env().await;
    let c = e.legacy(STAFF).await;
    let b = "XbOuNdArY";
    let mut body = format!(
        "--{b}\r\ncontent-disposition: form-data; name=\"files\"; filename=\"note.txt\"\r\ncontent-type: text/plain\r\n\r\n"
    )
    .into_bytes();
    let marker = format!("teed-{}", Uuid::new_v4());
    body.extend_from_slice(marker.as_bytes());
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let r =
        e.h.as_token(e.h.post("/api/assets/files"), &e.h.token(STAFF))
            .header("content-type", format!("multipart/form-data; boundary={b}"))
            .header("origin", &e.h.url)
            .body(body)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 201);
    let v = common::body_json(r).await;
    let path = v["attachments"][0]["path"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .to_owned();
    assert!(
        path.contains(magpie_api::legacy::LEGACY_ASSET_DIR),
        "an old asset path: {path}"
    );
    assert_eq!(e.m.st().legacy_paths.len(), 1, "magpie has the bytes with the old path");
    let mut ws = e.ws(STAFF).await;
    watch(&mut ws, &[c]).await;
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "read the file", "messageId": "rig-file", "attachments": [
                {"type": "uploaded_file", "path": path, "fileName": "note.txt", "mimeType": "text/plain",
                 "size": marker.len()}]}),
        )
        .await;
    assert_eq!(r["accepted"], true, "{r}");
    ws.wait_for("the turn's end", |v| {
        is_stream(v, c) && event_type(v) == "turn_completed"
    })
    .await;
    // the agent got the file: the old child gave Claude Code the teed file's path, which holds the bytes
    let prompts = e.rig.model_prompts().await;
    let p = prompts
        .iter()
        .rev()
        .find(|p| p.contains("read the file"))
        .unwrap_or_else(|| panic!("{prompts:?}"));
    assert!(p.contains("note.txt"), "the attachment is in the agent's prompt: {p}");
    e.zero_counters();
}

/// The old router refuses the person's token with 401 (here: a token of the second Access team, which pigeon trusts
/// and the old ui does not): the tab gets pigeon's signed-out answer, never a write error, and nothing is ticketed.
#[tokio::test]
#[ignore = "the LU rig: testing/lu-rig/run.sh test"]
async fn an_old_router_401_is_the_signed_out_answer() {
    let e = env().await;
    let c = e.legacy(STAFF).await;
    let mut ws = Ws::hello(&e.h.url, &e.h.teams[1].token(STAFF), "").await.0.relayed();
    let rid = ws.rid();
    ws.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "x",
                      "messageId": "rig-401"}),
    )
    .await;
    assert_eq!(ws.closed_code(Duration::from_secs(10)).await, Some(4401));
    assert!(e.metric("pigeon_lu_401_total") >= 1.0);
    assert_eq!(e.metric("pigeon_lu_errors_total"), 0.0);
    assert!(e.m.tickets_of(c).is_empty(), "no ticket before the LU is open");
    let hist = e.rig.history(&e.h, STAFF, c).await;
    assert!(hist.iter().all(|m| !m.2.contains("rig-401")), "{hist:?}");
}
