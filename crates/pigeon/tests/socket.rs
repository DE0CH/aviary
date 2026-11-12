//! Spec 18.1's socket cases against a real pigeon and the in-memory magpie model (`common/model.rs`): handshake,
//! dispatch rules, lists and paging, the timeline, live rows and turn ordering, joins, replacements, backpressure,
//! the drain's request-free-first rule, sends, creates, cancels, renames, deletes, asks and view-as.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use std::time::Duration;

use common::model::{self, Model};
use common::ws::Ws;
use common::{ADMIN, Harness, Opts, STAFF, STAFF2};
use magpie_api::changes::ChangeBody;
use magpie_api::conversations::{Ask, SandboxPhase};
use magpie_api::errors::{DataCode, ErrorCode, ErrorInfo, Refusal};
use magpie_api::rows::{AssistantItem, ErrorItem, RowItem, ToolItem, ToolResult, ToolStatus};
use magpie_api::turns::{TurnOrigin, TurnPhase};
use pigeon::Tuning;
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

async fn env_with(tuning: Tuning) -> Env {
    let m = Model::default();
    let served = model::serve(&m).await;
    let h = Harness::start_opts(Opts {
        tuning,
        magpie: Some(served.url()),
        ..Opts::default()
    })
    .await;
    Env { h, m, _served: served }
}

async fn env() -> Env {
    env_with(fast()).await
}

impl Env {
    async fn ws(&self, email: &str) -> Ws {
        Ws::hello(&self.h.url, &self.h.token(email), "").await.0
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

fn tool(call: &str, name: &str, input: Value) -> RowItem {
    RowItem::Tool(ToolItem {
        call_id: call.into(),
        message_id: "msg_t".into(),
        block_index: 1,
        name: name.into(),
        input,
        status: ToolStatus::Running,
        result: None,
        sent_files: None,
        task: None,
        error: None,
        answered_at: None,
    })
}

fn complete(item: &mut RowItem, text: &str) {
    if let RowItem::Tool(t) = item {
        t.status = ToolStatus::Completed;
        t.result = Some(ToolResult {
            text: text.into(),
            truncated: false,
            bytes: text.len() as u64,
            blob_id: None,
            is_error: false,
            images: None,
        });
    }
}

fn is_stream(v: &Value, agent: Uuid) -> bool {
    v["message"]["type"] == "agent_stream" && v["message"]["payload"]["agentId"] == agent.to_string()
}

async fn watch(ws: &mut Ws, ids: &[Uuid]) -> String {
    let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let (_, r) = ws
        .rpc("agent.timeline.set_subscription.request", json!({"agentIds": ids}))
        .await;
    r["subscriptionId"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn handshake_ping_and_protocols() {
    let e = env().await;
    let token = e.h.token(STAFF);
    let mut ws = Ws::open(&e.h.url, &token, "").await.unwrap();
    // ping before hello: a raw pong; session frames before hello are ignored
    ws.raw(json!({"type": "ping"})).await;
    assert_eq!(
        ws.next_raw(Duration::from_secs(5)).await.unwrap(),
        json!({"type": "pong"})
    );
    ws.session(json!({"type": "fetch_agents_request", "requestId": "early"}))
        .await;
    ws.raw(
        json!({"type": "hello", "clientId": "c", "clientType": "browser", "protocolVersion": 1,
                  "capabilities": {}, "appVersion": "0.14.1"}),
    )
    .await;
    let info = ws.next_session("status").await;
    assert_eq!(info["status"], "server_info");
    assert_eq!(info["serverId"], "srv_conversations");
    assert_eq!(info["hostname"], common::PRODUCT);
    assert_eq!(info["version"], "0.14.1");
    assert_eq!(info["features"]["creationLifecycle"], true);
    assert!(info["features"].get("forgeSearch").is_none());
    let early = ws.drain_for(Duration::from_millis(300)).await;
    assert!(
        early.iter().all(|v| v["message"]["payload"]["requestId"] != "early"),
        "{early:?}"
    );
    ws.raw(json!({"type": "ping"})).await;
    assert_eq!(
        ws.next_raw(Duration::from_secs(5)).await.unwrap(),
        json!({"type": "pong"})
    );

    // protocol 2 is not spoken yet: hello.rejected, close 1002
    let mut ws2 = Ws::open(&e.h.url, &token, "").await.unwrap();
    ws2.raw(
        json!({"type": "hello", "clientId": "c", "clientType": "browser", "protocolVersion": 2,
                   "capabilities": {}}),
    )
    .await;
    let r = ws2.next_raw(Duration::from_secs(5)).await.unwrap();
    assert_eq!(
        r,
        json!({"type": "hello.rejected", "reason": "incompatible_protocol", "accepts": []})
    );
    assert_eq!(ws2.closed_code(Duration::from_secs(5)).await, Some(1002));

    // the upgrade needs a person
    assert_eq!(Ws::open(&e.h.url, "nonsense", "").await.err(), Some(401));
    assert_eq!(
        Ws::open(&e.h.url, &e.h.token(common::OUTSIDER), "").await.err(),
        Some(403)
    );
}

#[tokio::test]
async fn static_stubs_unknown_requests_and_malformed_frames() {
    let e = env().await;
    let mut ws = e.ws(STAFF).await;
    let (k, p) = ws.rpc("get_daemon_config_request", json!({})).await;
    assert_eq!(k, "get_daemon_config_response");
    assert_eq!(p["config"]["trustedProxies"], json!(["loopback"]));
    let (k, p) = ws.rpc("checkout_status_request", json!({"cwd": "/workspace"})).await;
    assert_eq!(k, "checkout_status_response");
    assert_eq!(p["isGit"], false);
    let (k, p) = ws.rpc("hosts.list.request", json!({})).await;
    assert_eq!(k, "hosts.list.response");
    assert_eq!(p["directory"]["self"]["serverId"], "srv_conversations");
    let (k, _) = ws
        .rpc(
            "subscribe_terminals_request",
            json!({"cwd": "/workspace", "workspaceId": "w"}),
        )
        .await;
    assert_eq!(k, "terminals_changed");
    let (k, p) = ws
        .rpc("get_providers_snapshot_request", json!({"cwd": "/workspace"}))
        .await;
    assert_eq!(k, "get_providers_snapshot_response");
    assert_eq!(p["compactSnapshot"]["entries"][0]["label"], "Model A");
    // every model, the default marked; a model's thinking levels are a set of options, its own level the default
    let level = |id: &str, own: bool| json!({"id": id, "label": id, "isDefault": own});
    assert_eq!(
        p["compactSnapshot"]["thinkingSets"],
        json!([
            {"options": [level("none", false), level("low", false), level("high", true), level("max", false)], "defaultOptionId": "high"},
            {"options": [level("low", false), level("high", true)], "defaultOptionId": "high"}])
    );
    assert_eq!(
        p["compactSnapshot"]["entries"][0]["models"],
        json!([
            {"id": "vendor/model-a", "label": "Model A", "description": "", "isDefault": true, "thinkingSet": 0},
            {"id": "vendor/model-b", "label": "vendor/model-b", "description": "", "isDefault": false},
            {"id": "vendor/model-c", "label": "Model C", "description": "", "isDefault": false, "thinkingSet": 1}])
    );
    assert!(p["snapshotHash"].as_str().unwrap().starts_with("models:"));
    let (k, p) = ws.rpc("project.list.request", json!({"subscribe": true})).await;
    assert_eq!(k, "project.list.response");
    assert_eq!(p["projects"][0]["projectId"], "prj_conversations");
    assert_eq!(p["sync"]["generation"], model::GENERATION);
    // unknown with a requestId: unsupported; without: nothing; malformed JSON: nothing
    let (k, p) = ws.rpc_invalid("agent.rewind", json!({})).await;
    assert_eq!(k, "rpc_error");
    assert_eq!(p["code"], "unsupported");
    assert_eq!(p["error"], "Not available here");
    assert_eq!(p["requestType"], "agent.rewind");
    ws.session_invalid(json!({"type": "agent.rewind"})).await;
    ws.text("{not json").await;
    ws.session(
        json!({"type": "client_heartbeat", "deviceType": "web", "focusedAgentId": null,
                      "lastActivityAt": "2026-11-02T00:00:00.000Z", "appVisible": true}),
    )
    .await;
    assert_eq!(ws.drain_for(Duration::from_millis(300)).await, Vec::<Value>::new());
    // a release of the timeline subscription empties it
    let sub = watch(&mut ws, &[Uuid::new_v4()]).await;
    let (k, p) = ws
        .rpc("subscription.release.request", json!({"subscriptionId": sub}))
        .await;
    assert_eq!(k, "subscription.release.response");
    assert_eq!(p["subscriptionId"], sub);
}

#[tokio::test]
async fn lists_page_and_empty_list_shows_the_project() {
    let e = env().await;
    let mut ws = e.ws(STAFF).await;
    // the apps' subscribe is an object (`{}`, or with the id the client chose); `true` is the create requests'
    let (_, p) = ws.rpc("fetch_workspaces_request", json!({"subscribe": {}})).await;
    assert!(p["subscriptionId"].is_string(), "{p}");
    let (_, chosen) = ws
        .rpc(
            "fetch_agents_request",
            json!({"subscribe": {"subscriptionId": "sub-mine"}}),
        )
        .await;
    assert_eq!(chosen["subscriptionId"], "sub-mine");
    let (_, none) = ws.rpc("fetch_agents_request", json!({})).await;
    assert!(none.get("subscriptionId").is_none_or(Value::is_null), "{none}");
    assert_eq!(p["entries"], json!([]));
    assert_eq!(p["emptyProjects"][0]["projectId"], "prj_conversations");
    assert_eq!(p["pageInfo"]["hasMore"], false);
    for i in 0..230 {
        e.m.add(STAFF, Some(&format!("c{i}")));
    }
    e.m.add(STAFF2, Some("someone else's"));
    tokio::time::sleep(model_cache()).await;
    let (_, p) = ws.rpc("fetch_agents_request", json!({"page": {"limit": 200}})).await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 200);
    assert_eq!(p["pageInfo"]["hasMore"], true);
    let cursor = p["pageInfo"]["nextCursor"].as_str().unwrap().to_owned();
    let (_, p2) = ws
        .rpc(
            "fetch_agents_request",
            json!({"page": {"limit": 200, "cursor": cursor}}),
        )
        .await;
    assert_eq!(p2["entries"].as_array().unwrap().len(), 30);
    assert_eq!(p2["pageInfo"]["hasMore"], false);
    let a = &p["entries"][0]["agent"];
    assert_eq!(a["provider"], "claude");
    assert_eq!(a["status"], "idle");
    assert_eq!(a["model"], "vendor/model-a");
    assert_eq!(p["sync"]["mode"], "snapshot");
    // the two list requests of a connect share one magpie call
    tokio::time::sleep(model_cache()).await;
    let before = e.m.calls_to("list_conversations");
    let mut fresh = e.ws(STAFF).await;
    let (r1, r2) = (fresh.rid(), fresh.rid());
    fresh
        .session(json!({"type": "fetch_workspaces_request", "requestId": r1, "subscribe": {}}))
        .await;
    fresh
        .session(json!({"type": "fetch_agents_request", "requestId": r2, "subscribe": {}}))
        .await;
    fresh.next_session("fetch_workspaces_response").await;
    fresh.next_session("fetch_agents_response").await;
    assert_eq!(e.m.calls_to("list_conversations") - before, 1);
}

fn model_cache() -> Duration {
    pigeon::feeds::LIST_CACHE + Duration::from_millis(100)
}

#[tokio::test]
async fn timeline_pages_stale_epochs_prompts_and_not_found() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("t"));
    for i in 0..10 {
        e.m.row(c, None, assistant(&format!("a{i}")));
    }
    let epoch = e.m.epoch(c);
    let mut ws = e.ws(STAFF).await;
    let (_, p) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "tail", "limit": 4}),
        )
        .await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 4);
    assert_eq!(p["epoch"], epoch);
    assert_eq!(p["startCursor"]["seq"], 7);
    assert_eq!(p["endCursor"]["seq"], 10);
    assert_eq!(p["window"], json!({"minSeq": 1, "maxSeq": 10, "nextSeq": 11}));
    assert_eq!(p["hasOlder"], true);
    assert_eq!(p["agent"]["id"], c.to_string());
    assert_eq!(
        p["entries"][0]["item"],
        json!({"type": "assistant_message", "text": "a6", "messageId": "msg_a6"})
    );
    let (_, p) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "before", "cursor": {"epoch": epoch, "seq": 7}, "limit": 3}),
        )
        .await;
    let seqs: Vec<u64> = p["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["seqStart"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![4, 5, 6]);
    let (_, p) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "after", "cursor": {"epoch": epoch, "seq": 8}, "mergeWindow": true}),
        )
        .await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 2);
    assert_eq!(p["mergeWindow"], true);
    assert_eq!(p["reset"], false);
    // a cursor of another epoch: the tail, reset
    let (_, p) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "after", "cursor": {"epoch": "old", "seq": 3}, "limit": 2}),
        )
        .await;
    assert_eq!(p["reset"], true);
    assert_eq!(p["staleCursor"], true);
    assert_eq!(p["direction"], "after");
    assert_eq!(p["endCursor"]["seq"], 10);
    // not found, another person's conversation included
    let other = e.m.add(STAFF2, None);
    for id in [Value::from(other.to_string()), Value::from("not-a-uuid")] {
        let (_, p) = ws.rpc("fetch_agent_timeline_request", json!({"agentId": id})).await;
        assert_eq!(p["error"], "Conversation not found");
        assert_eq!(p["agent"], Value::Null);
        assert_eq!(p["epoch"], "");
        let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
        assert_eq!(p["error"], "Conversation not found");
        let (_, p) = ws
            .rpc("agent.timeline.list_prompts.request", json!({"agentId": id}))
            .await;
        assert_eq!(p["error"], "Conversation not found");
    }
    let (_, p) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "hello there", "messageId": "m1"}),
        )
        .await;
    assert_eq!(p["accepted"], true);
    let (_, p) = ws
        .rpc("agent.timeline.list_prompts.request", json!({"agentId": c}))
        .await;
    assert_eq!(p["prompts"][0]["preview"], "hello there");
    assert_eq!(p["prompts"][0]["seq"], 11);
}

#[tokio::test]
async fn live_rows_are_dense_and_turn_completed_follows_the_last_row() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("live"));
    e.m.row(c, None, assistant("history"));
    let mut ws = e.ws(STAFF).await;
    let sub = watch(&mut ws, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let turn = e.m.start_turn(c, TurnOrigin::Person);
    let started = ws.wait_for("turn_started", |v| is_stream(v, c)).await;
    assert_eq!(started["message"]["payload"]["event"]["type"], "turn_started");
    assert_eq!(started["message"]["payload"]["subscriptionId"], sub);
    let t =
        e.m.row(c, Some(turn), tool("toolu_1", "Bash", json!({"command": "ls"})));
    e.m.row(c, Some(turn), assistant("one"));
    e.m.update(c, t, |i| complete(i, "a.txt\n"));
    e.m.row(c, Some(turn), assistant("two"));
    e.m.end_turn(c, TurnPhase::Completed, None);
    let mut seqs = Vec::new();
    let mut kinds = Vec::new();
    loop {
        let v = ws.wait_for("stream", |v| is_stream(v, c)).await;
        let p = &v["message"]["payload"];
        assert_eq!(p["subscriptionId"], sub);
        let kind = p["event"]["type"].as_str().unwrap().to_owned();
        if kind == "timeline" {
            seqs.push(p["seq"].as_u64().unwrap());
            assert_eq!(p["epoch"], e.m.epoch(c));
            assert_eq!(p["event"]["turnId"], turn.to_string());
        }
        kinds.push(kind.clone());
        if kind == "turn_completed" {
            break;
        }
    }
    // rows come in seqEnd order; a tool row updated before the poll read it comes once, with its new seq (the
    // app fetches the gap)
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    assert_eq!(seqs.last(), Some(&5));
    assert!(seqs.contains(&4) && seqs.contains(&3), "{seqs:?}");
    assert_eq!(kinds.last().unwrap(), "turn_completed");
    // a failed turn: the localized sentence and the code
    e.m.start_turn(c, TurnOrigin::Agent);
    e.m.row(
        c,
        None,
        RowItem::Error(ErrorItem {
            code: DataCode::SandboxLost,
            message: "lost".into(),
            detail: None,
        }),
    );
    e.m.end_turn(
        c,
        TurnPhase::Failed,
        Some(ErrorInfo::new(DataCode::SandboxLost, "lost")),
    );
    let row = ws
        .wait_for("error row", |v| {
            is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "timeline"
        })
        .await;
    assert!(
        row["message"]["payload"]["event"]["item"]["message"]
            .as_str()
            .unwrap()
            .starts_with("這個對話的電腦")
    );
    let failed = ws
        .wait_for("turn_failed", |v| {
            is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_failed"
        })
        .await;
    assert_eq!(failed["message"]["payload"]["event"]["code"], "sandbox.lost");
}

#[tokio::test]
async fn a_socket_joining_mid_turn_sees_the_next_rows_and_catches_up_by_its_own_fetch() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let mut a = e.ws(STAFF).await;
    watch(&mut a, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.m.row(c, None, assistant("r1"));
    a.wait_for("r1", |v| is_stream(v, c)).await;
    // b joins the running poller: its first live row (seq 2) is ahead of nothing it has; the app fetches after
    let mut b = e.ws(STAFF).await;
    watch(&mut b, &[c]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.m.row(c, None, assistant("r2"));
    let v = b.wait_for("r2", |v| is_stream(v, c)).await;
    assert_eq!(v["message"]["payload"]["seq"], 2);
    let (_, p) = b
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": c, "direction": "after", "cursor": {"epoch": e.m.epoch(c), "seq": 0}}),
        )
        .await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 2);
}

/// The answer as it comes (magpie's live state): the text added to an open block goes out as chunks without a `seq`,
/// under the message's id, so the app appends them; the block's row then carries only what a socket has not had, with
/// its `seq`. A socket that joins while the block is open gets it from its start.
#[tokio::test]
async fn the_answer_is_sent_as_it_is_written() {
    use magpie_api::live::{BlockType, LiveBlock, LiveState};
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let turn = Uuid::now_v7();
    let live = |n: u64, text: &str| LiveState {
        turn_id: turn,
        n,
        reset: true,
        blocks: vec![LiveBlock {
            message_id: "msg_1".into(),
            block_index: 0,
            block: BlockType::Text,
            name: None,
            offset: 0,
            text: text.into(),
            done: false,
            after_row_seq: 0,
        }],
        status: None,
    };
    let chunk = |v: &Value| {
        v["message"]["payload"]["event"]["item"]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mut a = e.ws(STAFF).await;
    watch(&mut a, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.m.with_conv(c, |conv| conv.live = Some(live(1, "你好")));
    let v = a.wait_for("the first chunk", |v| is_stream(v, c)).await;
    let p = &v["message"]["payload"];
    assert_eq!(chunk(&v), "你好");
    assert_eq!(p["event"]["item"]["type"], "assistant_message");
    assert_eq!(p["event"]["item"]["messageId"], "msg_1");
    assert_eq!(p["event"]["turnId"], turn.to_string());
    assert!(
        p.get("seq").is_none() && p.get("epoch").is_none(),
        "a chunk is no row: {p}"
    );
    // more text: only what was added
    e.m.with_conv(c, |conv| conv.live = Some(live(2, "你好, wor")));
    let v = a.wait_for("the second chunk", |v| is_stream(v, c)).await;
    assert_eq!(chunk(&v), ", wor");
    // a socket that joins now gets the open block from its start
    let mut b = e.ws(STAFF).await;
    watch(&mut b, &[c]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.m.with_conv(c, |conv| conv.live = Some(live(3, "你好, world")));
    let v = b.wait_for("the late socket's first chunk", |v| is_stream(v, c)).await;
    assert_eq!(chunk(&v), "你好, world");
    let v = a.wait_for("the third chunk", |v| is_stream(v, c)).await;
    assert_eq!(chunk(&v), "ld");
    // the block is complete: its row, with what each socket has not had (nothing: the text is whole already)
    e.m.with_conv(c, |conv| conv.live = None);
    e.m.row(
        c,
        Some(turn),
        RowItem::Assistant(AssistantItem {
            message_id: "msg_1".into(),
            block_index: 0,
            text: "你好, world!".into(),
            error: None,
        }),
    );
    for ws in [&mut a, &mut b] {
        let v = ws.wait_for("the row", |v| is_stream(v, c)).await;
        assert_eq!(chunk(&v), "!", "the rest of the text");
        assert_eq!(v["message"]["payload"]["seq"], 1, "the row moves the cursor");
        assert_eq!(v["message"]["payload"]["event"]["item"]["messageId"], "msg_1");
    }
    // a row whose block never went out live is sent whole, as before
    e.m.row(c, Some(turn), assistant("whole"));
    let v = a.wait_for("a whole row", |v| is_stream(v, c)).await;
    assert_eq!(chunk(&v), "whole");
}

/// The app's order when a page that watches one conversation opens another (a new chat): the new set first, then
/// a release of the subscription it held before. That release ends the earlier subscription only: the socket goes
/// on watching the new set, so the new conversation's first reply arrives live.
#[tokio::test]
async fn releasing_the_previous_timeline_subscription_keeps_the_new_one() {
    let e = env().await;
    let open = e.m.add(STAFF, Some("open already"));
    let mut ws = e.ws(STAFF).await;
    let first = watch(&mut ws, &[open]).await;
    let new = e.m.add(STAFF, Some("new chat"));
    let second = watch(&mut ws, &[open, new]).await;
    assert_ne!(first, second, "each set_subscription is a subscription of its own");
    let (k, p) = ws
        .rpc("subscription.release.request", json!({"subscriptionId": first}))
        .await;
    assert_eq!(k, "subscription.release.response");
    assert_eq!(p["subscriptionId"], first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.m.row(new, None, assistant("the first reply"));
    let v = ws.wait_for("the new conversation's row", |v| is_stream(v, new)).await;
    assert_eq!(v["message"]["payload"]["subscriptionId"], second);
    e.m.row(open, None, assistant("and the one open before"));
    ws.wait_for("the earlier conversation's row", |v| is_stream(v, open))
        .await;
    // a release of the subscription the socket holds now empties it
    ws.rpc("subscription.release.request", json!({"subscriptionId": second}))
        .await;
    e.m.row(new, None, assistant("unwatched"));
    let after = ws.drain_for(Duration::from_millis(400)).await;
    assert!(!after.iter().any(|v| is_stream(v, new)), "{after:?}");
}

#[tokio::test]
async fn list_changes_join_without_loss_or_duplicate() {
    let e = env().await;
    let mut a = e.ws(STAFF).await;
    let (_, snap) = a.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    let sub = snap["subscriptionId"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // three changes go by the poller (P > the cached snapshot's head)
    let c1 = e.m.add(STAFF, Some("one"));
    let c2 = e.m.add(STAFF, Some("two"));
    let c3 = e.m.add(STAFF, Some("three"));
    for c in [c1, c2, c3] {
        let v = a
            .wait_for("upsert", |v| {
                v["message"]["type"] == "agent_update" && v["message"]["payload"]["agent"]["id"] == c.to_string()
            })
            .await;
        assert_eq!(v["message"]["payload"]["subscriptionId"], sub);
    }
    // b gets the 2 s cached snapshot (head before the three): the poller fetches the three for it once
    let mut b = e.ws(STAFF).await;
    let (_, bsnap) = b.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    assert_eq!(bsnap["sync"]["headSeq"], snap["sync"]["headSeq"], "the cached snapshot");
    let c4 = e.m.add(STAFF, Some("four"));
    let mut got = Vec::new();
    loop {
        let v = b.wait_for("b upsert", |v| v["message"]["type"] == "agent_update").await;
        let id = v["message"]["payload"]["agent"]["id"].as_str().unwrap().to_owned();
        got.push((id.clone(), v["message"]["payload"]["seq"].as_u64().unwrap()));
        if id == c4.to_string() {
            break;
        }
    }
    let ids: Vec<String> = got.iter().map(|g| g.0.clone()).collect();
    assert_eq!(
        ids,
        vec![c1.to_string(), c2.to_string(), c3.to_string(), c4.to_string()],
        "{got:?}"
    );
    let seqs: Vec<u64> = got.iter().map(|g| g.1).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    assert!(
        b.drain_for(Duration::from_millis(300))
            .await
            .iter()
            .all(|v| v["message"]["type"] != "agent_update")
    );
    // a soft delete is a remove on both lists; a restore an upsert; a purge a remove
    let (_, _) = a.rpc("fetch_workspaces_request", json!({"subscribe": {}})).await;
    let (_, r) = a.rpc("archive_agent_request", json!({"agentId": c1})).await;
    assert_eq!(r["agentId"], c1.to_string());
    a.wait_for("agent remove", |v| {
        v["message"]["type"] == "agent_update" && v["message"]["payload"]["kind"] == "remove"
    })
    .await;
    let w = a
        .wait_for("workspace remove", |v| {
            v["message"]["type"] == "workspace_update" && v["message"]["payload"]["kind"] == "remove"
        })
        .await;
    assert_eq!(w["message"]["payload"]["id"], format!("wks_{c1}"));
}

/// 8.3 on a subscription made again: changes the poller has not yet delivered when the new snapshot is taken are in
/// that snapshot, so none of them follows it (an older title never lands over the newer snapshot).
#[tokio::test]
async fn a_list_subscribed_again_gets_no_change_older_than_its_new_snapshot() {
    let e = env().await;
    let mut a = e.ws(STAFF).await;
    let (_, _) = a.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // the poller's next call after c's upsert waits: the two renames below are not delivered yet
    let feed = e.m.hold("changes");
    let c = e.m.add(STAFF, Some("one"));
    a.wait_for("upsert", |v| {
        v["message"]["type"] == "agent_update" && v["message"]["payload"]["agent"]["id"] == c.to_string()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.m.edit(c, |conv| conv.title = Some("two".into()));
    e.m.edit(c, |conv| conv.title = Some("three".into()));
    // subscribed again once the list cache has expired: from the request on, until the new snapshot's join, the
    // socket has no floor, so no change goes out (with the old floor kept, a poll answered between the snapshot and
    // the join would send "two" after a snapshot that has "three")
    tokio::time::sleep(pigeon::feeds::LIST_CACHE).await;
    let list = e.m.hold("list_conversations");
    let rid = a.rid();
    a.session(json!({"type": "fetch_agents_request", "requestId": rid, "subscribe": {}}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hub = e.h.app.socket().hub().unwrap();
    let conns = hub.conns_of(STAFF);
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].floor(pigeon::paseo::transport::ListKind::Agents), None);
    list.notify_one();
    let snap = a.next_session("fetch_agents_response").await;
    let head = snap["sync"]["headSeq"].as_u64().unwrap();
    assert_eq!(snap["entries"][0]["agent"]["title"], "three", "{snap}");
    feed.notify_one();
    let late = a.drain_for(Duration::from_millis(600)).await;
    let updates: Vec<&Value> = late.iter().filter(|v| v["message"]["type"] == "agent_update").collect();
    assert!(
        updates.is_empty(),
        "changes up to the snapshot's head {head} followed it: {updates:?}"
    );
    // the feed goes on past the snapshot
    e.m.edit(c, |conv| conv.title = Some("four".into()));
    let v = a
        .wait_for("the next upsert", |v| v["message"]["type"] == "agent_update")
        .await;
    assert_eq!(v["message"]["payload"]["agent"]["title"], "four");
    assert!(v["message"]["payload"]["seq"].as_u64().unwrap() > head);
}

#[tokio::test]
async fn a_history_rewrite_sends_one_replacement() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    e.m.row(c, None, assistant("a"));
    e.m.row(c, None, assistant("b"));
    let mut ws = e.ws(STAFF).await;
    watch(&mut ws, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let epoch = e.m.rewrite(c);
    // a row in the new epoch makes the poll see the new epoch too
    e.m.row(c, None, assistant("c"));
    let v = ws
        .wait_for("replacement", |v| v["message"]["type"] == "agent.timeline.replacement")
        .await;
    assert_eq!(v["message"]["payload"]["epoch"], epoch);
    let rest = ws.drain_for(Duration::from_millis(800)).await;
    assert!(
        rest.iter()
            .all(|v| v["message"]["type"] != "agent.timeline.replacement"),
        "exactly one: {rest:?}"
    );
    assert!(
        rest.iter().all(|v| !is_stream(v, c)),
        "no new-epoch rows under the old one: {rest:?}"
    );
    // the next row comes in the new epoch
    e.m.row(c, None, assistant("d"));
    let v = ws.wait_for("row", |v| is_stream(v, c)).await;
    assert_eq!(v["message"]["payload"]["epoch"], epoch);
    assert_eq!(v["message"]["payload"]["seq"], 4);
}

#[tokio::test]
async fn sandbox_status_goes_to_every_socket_and_is_replayed_on_connect() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let mut a = e.ws(STAFF).await;
    a.rpc("fetch_agents_request", json!({})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.m.sandbox(c, SandboxPhase::Preparing);
    let v = a.next_session(pigeon::paseo::compat::SANDBOX_STATUS).await;
    assert_eq!(v, json!({"agentId": c, "state": "preparing"}));
    // a tab that connects now gets the chip after its list
    tokio::time::sleep(model_cache()).await;
    let mut b = e.ws(STAFF).await;
    let (_, _) = b.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    let v = b.next_session(pigeon::paseo::compat::SANDBOX_STATUS).await;
    assert_eq!(v["state"], "preparing");
    e.m.sandbox(c, SandboxPhase::Ready);
    assert_eq!(
        a.next_session(pigeon::paseo::compat::SANDBOX_STATUS).await["state"],
        "ready"
    );
}

#[tokio::test]
async fn a_slow_socket_is_closed_1013_without_slowing_the_others() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let mut slow = e.ws(STAFF).await;
    watch(&mut slow, &[c]).await;
    let mut fast = e.ws(STAFF).await;
    watch(&mut fast, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 48 MiB in bursts of 2 MiB: the reading socket never holds more than a burst; the other fills its queue
    let big = "x".repeat(1 << 20);
    let started = std::time::Instant::now();
    for burst in 0..24u64 {
        e.m.row(c, None, assistant(&big));
        e.m.row(c, None, assistant(&big));
        for n in 1..=2u64 {
            let v = fast.wait_for("row", |v| is_stream(v, c)).await;
            assert_eq!(v["message"]["payload"]["seq"], burst * 2 + n);
        }
    }
    // well inside the slow socket's 30 s write bound: the fast one never waited for it
    assert!(started.elapsed() < Duration::from_secs(25), "{:?}", started.elapsed());
    // the slow one (never read) was cut: reading now finds the close after what was buffered
    let code = slow.closed_code(Duration::from_secs(40)).await;
    assert_eq!(code, Some(1013));
}

#[tokio::test]
async fn the_drain_closes_request_free_sockets_first() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let mut idle = e.ws(STAFF).await;
    let mut busy = e.ws(STAFF).await;
    let held = e.m.hold("send_turn");
    let rid = busy.rid();
    busy.session(
        json!({"type": "send_agent_message_request", "requestId": rid, "agentId": c, "text": "hi",
                        "messageId": "m-drain"}),
    )
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
    // the idle socket closes 1012 in the first half; the busy one waits for its send
    assert_eq!(idle.closed_code(Duration::from_secs(3)).await, Some(1012));
    assert_eq!(busy.drain_for(Duration::from_millis(500)).await, Vec::<Value>::new());
    assert!(busy.closed.is_none());
    held.notify_one();
    let r = busy.next_session("send_agent_message_response").await;
    assert_eq!(r["accepted"], true);
    assert_eq!(busy.closed_code(Duration::from_secs(5)).await, Some(1012));
    drain.await.unwrap();
    // nothing new after the drain
    assert_eq!(Ws::open(&e.h.url, &e.h.token(STAFF), "").await.err(), Some(503));
}

#[tokio::test]
async fn sends_creates_and_cancels() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    let mut ws = e.ws(STAFF).await;
    let send = |text: &str, id: &str| {
        json!({"agentId": c, "text": text, "messageId": id,
        "images": [{"data": "aGVsbG8=", "mimeType": "image/png"}],
        "attachments": [{"type": "uploaded_file", "id": "uploads/u1/a.pdf", "fileName": "a.pdf",
                         "mimeType": "application/pdf", "size": 3, "path": "uploads/u1/a.pdf"}]})
    };
    let (_, r) = ws.rpc("send_agent_message_request", send("one", "m1")).await;
    assert_eq!(
        r,
        json!({"requestId": r["requestId"], "agentId": c, "accepted": true, "error": null})
    );
    assert_eq!(
        e.m.st().uploads,
        vec![("image/png".to_owned(), 5)],
        "the image went up first, decoded"
    );
    {
        let st = e.m.st();
        let (_, req) = st.sent.last().unwrap();
        assert_eq!(req.client_message_id, "m1");
        assert_eq!(req.images.len(), 1);
        assert_eq!(req.files[0].path, "uploads/u1/a.pdf");
    }
    // a repeat of the same message id is accepted (magpie's idempotency); a new one while running is busy
    let (_, r) = ws.rpc("send_agent_message_request", send("one", "m1")).await;
    assert_eq!(r["accepted"], true);
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "two", "messageId": "m2"}),
        )
        .await;
    assert_eq!(r["accepted"], false);
    assert_eq!(r["error"], "對話仍在進行中，請稍候。");
    // cancel: the turn is stopped; the snapshot comes back; the end arrives as turn_canceled
    watch(&mut ws, &[c]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, r) = ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["agent"]["status"], "idle");
    let v = ws
        .wait_for("canceled", |v| {
            is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_canceled"
        })
        .await;
    assert_eq!(v["message"]["payload"]["event"]["reason"], "Stopped");
    // cancel with nothing running: no error, never an error row
    let (_, r) = ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    assert_eq!(r["error"], Value::Null);
    // deleted conversation, unreachable magpie
    e.m.refuse_once("send_turn", Refusal::new(ErrorCode::ConversationDeleted, "deleted"));
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "x", "messageId": "m3"}),
        )
        .await;
    assert_eq!(r["error"], "這個對話已刪除。");
    e.m.refuse_once(
        "send_turn",
        Refusal::new(ErrorCode::Unavailable, "down").with_status(503),
    );
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "x", "messageId": "m4"}),
        )
        .await;
    assert_eq!(r["error"], "暫時連接不到這個對話，請再試一次。");
    e.m.refuse_once("upload", Refusal::new(ErrorCode::UnsupportedType, "no"));
    let (_, r) = ws.rpc("send_agent_message_request", send("x", "m5")).await;
    assert_eq!(r["accepted"], false);
    assert_eq!(
        e.m.st().sent.iter().filter(|s| s.1.client_message_id == "m5").count(),
        0,
        "nothing sent"
    );

    // create: five updates then the response; the same key on another socket gives the same conversation
    let (k, r) = ws
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": "key-1", "subscribe": true, "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"},
                   "agent": {"initialPrompt": "start", "clientMessageId": "cm-1", "config": {"provider": "claude", "cwd": "/workspace"}}}),
        )
        .await;
    assert_eq!(k, "workspace.create.response");
    let id = r["agent"]["id"].as_str().unwrap().to_owned();
    assert_eq!(r["workspace"]["id"], format!("wks_{id}"));
    assert_eq!(r["agent"]["status"], "running");
    let mut other = e.ws(STAFF).await;
    let (_, r2) = other
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": "key-1", "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"}, "agent": {"initialPrompt": "start", "config": {"provider": "claude", "cwd": "/workspace"}}}),
        )
        .await;
    assert_eq!(r2["agent"]["id"], id.as_str());
    let (_, s) = other
        .rpc(
            "creation.subscribe.request",
            json!({"kind": "workspace", "idempotencyKey": "key-1"}),
        )
        .await;
    assert_eq!(s["snapshot"]["phase"], "completed");
    assert_eq!(s["snapshot"]["agentId"], id.as_str());
    let (_, s) = other
        .rpc(
            "creation.subscribe.request",
            json!({"kind": "workspace", "idempotencyKey": "never"}),
        )
        .await;
    assert_eq!(s["snapshot"], Value::Null);
    e.m.refuse_once("create_conversation", Refusal::new(ErrorCode::BadRequest, "no"));
    let (_, r) = other
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": "key-2", "subscribe": true, "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"}, "agent": {"config": {"provider": "claude", "cwd": "/workspace"}}}),
        )
        .await;
    assert_eq!(r["workspace"], Value::Null);
    assert_eq!(r["error"], "未能開始對話，請再試一次。");
}

/// The app's model picker: where the deployment lets people pick, a model of the list becomes the conversation's
/// (`pickedModel`, which the agent then shows); elsewhere, and for a model that is not listed, nothing is picked.
#[tokio::test]
async fn a_person_picks_a_listed_model_where_the_deployment_lets_them() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("chat"));
    let mut ws = e.ws(STAFF).await;
    // the model list is read (the app's first request for it)
    ws.rpc("get_providers_snapshot_request", json!({"cwd": "/workspace"}))
        .await;
    let (_, a) = ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    assert_eq!(a["agent"]["model"], "vendor/model-a", "the deployment's default");
    // people do not pick here (the default deployment): refused, magpie never asked
    let (k, r) = ws
        .rpc(
            "set_agent_model_request",
            json!({"agentId": c, "modelId": "vendor/model-b"}),
        )
        .await;
    assert_eq!(k, "set_agent_model_response");
    assert_eq!((&r["accepted"], &r["agentId"]), (&json!(false), &json!(c.to_string())));
    assert_eq!(e.m.calls_to("rename_conversation"), 0);

    e.h.set_facts(|d| d.conversations.people_pick_model = true).await;
    let (_, r) = ws
        .rpc(
            "set_agent_model_request",
            json!({"agentId": c, "modelId": "vendor/not-listed"}),
        )
        .await;
    assert_eq!(r["accepted"], false);
    assert_eq!(e.m.calls_to("rename_conversation"), 0);
    let (_, r) = ws
        .rpc(
            "set_agent_model_request",
            json!({"agentId": c, "modelId": "vendor/model-b"}),
        )
        .await;
    assert_eq!((&r["accepted"], &r["error"]), (&json!(true), &Value::Null));
    assert_eq!(e.m.st().convs[&c].conv.picked_model.as_deref(), Some("vendor/model-b"));
    assert_eq!(
        e.m.st().convs[&c].conv.title.as_deref(),
        Some("chat"),
        "the title is not touched"
    );
    let (_, a) = ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    assert_eq!(a["agent"]["model"], "vendor/model-b");
    assert_eq!(a["agent"]["runtimeInfo"]["model"], "vendor/model-b");
    // null: back to the list's default
    let (_, r) = ws
        .rpc("set_agent_model_request", json!({"agentId": c, "modelId": null}))
        .await;
    assert_eq!(r["accepted"], true);
    assert_eq!(e.m.st().convs[&c].conv.picked_model.as_deref(), Some("vendor/model-a"));
    // someone else's conversation
    let other = e.m.add("other@example.org", None);
    let (_, r) = ws
        .rpc(
            "set_agent_model_request",
            json!({"agentId": other, "modelId": "vendor/model-b"}),
        )
        .await;
    assert_eq!(r["accepted"], false);
}

#[tokio::test]
async fn a_deployment_that_starts_conversations_refuses_a_persons_create() {
    let e = env().await;
    e.h.set_facts(|d| d.conversations.people_create = false).await;
    let mut ws = e.ws(STAFF).await;
    let (k, r) = ws
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": "key-c", "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"},
                   "agent": {"initialPrompt": "start", "config": {"provider": "claude", "cwd": "/workspace"}}}),
        )
        .await;
    assert_eq!(k, "workspace.create.response");
    assert_eq!(r["workspace"], Value::Null);
    assert_eq!(r["error"], "這裡不能開始新對話。");
    assert_eq!(e.m.calls_to("create_conversation"), 0, "magpie is never asked");
}

#[tokio::test]
async fn rename_delete_restore_and_asks() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("old"));
    let mut ws = e.ws(STAFF).await;
    let (_, r) = ws
        .rpc("update_agent_request", json!({"agentId": c, "title": "  "}))
        .await;
    assert_eq!(r["accepted"], true);
    assert_eq!(e.m.calls_to("rename_conversation"), 0);
    let (_, r) = ws
        .rpc("update_agent_request", json!({"agentId": c, "title": "new"}))
        .await;
    assert_eq!(r["accepted"], true);
    let (_, r) = ws
        .rpc(
            "workspace.title.set.request",
            json!({"workspaceId": format!("wks_{c}"), "title": ""}),
        )
        .await;
    assert_eq!(r["error"], "名稱不能留空。");
    let (_, r) = ws
        .rpc(
            "workspace.title.set.request",
            json!({"workspaceId": format!("wks_{c}"), "title": "x".repeat(250)}),
        )
        .await;
    assert_eq!(r["title"].as_str().unwrap().chars().count(), 200);
    assert_eq!(e.m.st().convs[&c].conv.title.as_deref().unwrap().chars().count(), 200);
    let (k, r) = ws
        .rpc("archive_workspace_request", json!({"workspaceId": format!("wks_{c}")}))
        .await;
    assert_eq!(k, "archive_workspace_response");
    assert!(r["archivedAt"].is_string());
    let (_, h) = ws.rpc("fetch_agent_history_request", json!({})).await;
    assert_eq!(h["entries"][0]["agent"]["id"], c.to_string());
    assert!(h["entries"][0]["agent"]["archivedAt"].is_string());
    let (k, r) = ws.rpc("refresh_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "status");
    assert_eq!(r["status"], "agent_refreshed");
    assert!(e.m.st().convs[&c].conv.deleted_at.is_none());
    // delete: soft first, then for good
    let (k, _) = ws.rpc("delete_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "agent_deleted");
    assert!(e.m.st().convs[&c].conv.deleted_at.is_some());
    let (k, _) = ws.rpc("delete_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "agent_deleted");
    assert!(!e.m.st().convs.contains_key(&c));

    // asks: relayed to watchers and answered as is
    let d = e.m.add(STAFF, None);
    watch(&mut ws, &[d]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ask = Ask {
        id: "ask-1".into(),
        tool_name: "AskUserQuestion".into(),
        tool_use_id: Some("toolu_q".into()),
        input: serde_json::from_value(json!({"questions": [{"question": "Which?"}]})).unwrap(),
        title: None,
        description: None,
        created_at: magpie_api::Timestamp::now(),
    };
    e.m.open_ask(d, ask);
    let v = ws.next_session("agent_permission_request").await;
    assert_eq!(v["request"]["kind"], "question");
    assert_eq!(v["agentId"], d.to_string());
    let (_, a) = ws.rpc("fetch_agent_request", json!({"agentId": d})).await;
    assert_eq!(a["agent"]["requiresAttention"], true);
    assert_eq!(a["agent"]["pendingPermissions"][0]["id"], "ask-1");
    ws.session(
        json!({"type": "agent_permission_response", "agentId": d, "requestId": "ask-1",
                      "response": {"behavior": "allow", "updatedInput": {"answers": {"Which?": "A"}}}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.m.st().answers[0].1, "ask-1");
    assert_eq!(
        e.m.st().answers[0].2.updated_input.as_ref().unwrap()["answers"],
        json!({"Which?": "A"})
    );
    // the app's card keys an answer by the question's header: magpie gets it by the question's text, as Claude Code
    // and the supervisor's answer message read it (rehearsal R1, 2026-11-06)
    let asked = json!({"questions": [{"question": "Do you prefer Red or Blue?", "header": "Color choice",
        "multiSelect": false, "options": [{"label": "Red", "description": "r"}, {"label": "Blue", "description": "b"}]}]});
    let mut given = asked.clone();
    given["answers"] = json!({"Color choice": "Blue"});
    ws.session(
        json!({"type": "agent_permission_response", "agentId": d, "requestId": "ask-2",
                      "response": {"behavior": "allow", "updatedInput": given}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let answers = e.m.st().answers.clone();
    let (_, _, r) = answers.iter().find(|a| a.1 == "ask-2").unwrap();
    let input = r.updated_input.as_ref().unwrap();
    assert_eq!(input["answers"], json!({"Do you prefer Red or Blue?": "Blue"}));
    assert_eq!(input["questions"], asked["questions"]);
    e.m.change(
        d,
        ChangeBody::AskResolved(magpie_api::changes::AskResolvedChange {
            ask_id: "ask-1".into(),
            resolution: magpie_api::turns::AgentPermissionResponse::canceled(),
        }),
    );
    let v = ws.next_session("agent_permission_resolved").await;
    assert_eq!(v["resolution"]["behavior"], "deny");
}

#[tokio::test]
async fn view_as_refuses_changes_without_a_call_and_sends_with_the_viewer() {
    let e = env().await;
    let c = e.m.add(STAFF, Some("theirs"));
    let token = e.h.view_token(ADMIN, STAFF, common::now() + 600);
    let (mut ws, _) = Ws::hello(
        &e.h.url,
        &e.h.token(ADMIN),
        &format!("?{}={token}", pigeon::paseo::compat::VIEW_AS_QUERY),
    )
    .await;
    for (kind, msg) in [
        ("update_agent_request", json!({"agentId": c, "title": "x"})),
        (
            "set_agent_model_request",
            json!({"agentId": c, "modelId": "vendor/model-a"}),
        ),
        (
            "workspace.title.set.request",
            json!({"workspaceId": format!("wks_{c}"), "title": "x"}),
        ),
        ("archive_agent_request", json!({"agentId": c})),
        ("archive_workspace_request", json!({"workspaceId": format!("wks_{c}")})),
        ("delete_agent_request", json!({"agentId": c})),
    ] {
        let (k, r) = ws.rpc(kind, msg).await;
        assert_eq!(k, "rpc_error", "{kind}");
        assert_eq!(r["code"], "view_as_read_only");
        assert_eq!(r["error"], "Read-only while viewing as someone else.");
    }
    assert_eq!(
        e.m.calls_to("rename_conversation") + e.m.calls_to("delete_conversation"),
        0
    );
    // the list is the person's
    let (_, l) = ws.rpc("fetch_agents_request", json!({})).await;
    assert_eq!(l["entries"][0]["agent"]["id"], c.to_string());
    // sends carry the viewer; magpie's refusal to record surfaces
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "hi", "messageId": "v1"}),
        )
        .await;
    assert_eq!(r["accepted"], true);
    let viewer =
        e.m.st()
            .calls
            .iter()
            .rev()
            .find(|c| c.0 == "send_turn")
            .unwrap()
            .clone();
    assert_eq!(viewer.1.as_deref(), Some(STAFF));
    assert_eq!(viewer.2.as_deref(), Some(ADMIN));
    e.m.end_turn(c, TurnPhase::Completed, None);
    e.m.refuse_once("send_turn", Refusal::new(ErrorCode::ViewAsNotRecorded, "no"));
    let (_, r) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": c, "text": "hi", "messageId": "v2"}),
        )
        .await;
    assert_eq!(r["error"], "未能傳送：未能寫入代看記錄，請再試一次。");
    // restoring a deleted conversation is refused; refreshing a live one is not
    let (k, _) = ws.rpc("refresh_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "status");
    e.m.edit(c, |c| c.deleted_at = Some(magpie_api::Timestamp::now()));
    let (k, r) = ws.rpc("refresh_agent_request", json!({"agentId": c})).await;
    assert_eq!(k, "rpc_error");
    assert_eq!(r["code"], "view_as_read_only");
}

#[tokio::test]
async fn a_socket_closes_4401_when_its_token_expires() {
    let e = env().await;
    let token = e.h.teams[0].mint(STAFF, 2, None, None);
    let (mut ws, _) = Ws::hello(&e.h.url, &token, "").await;
    assert_eq!(ws.closed_code(Duration::from_secs(8)).await, Some(4401));
}

#[tokio::test]
async fn an_unknown_row_kind_and_an_oversized_tool_input_are_handled() {
    let e = env().await;
    let c = e.m.add(STAFF, None);
    e.m.row(c, None, RowItem::Unknown);
    e.m.row(
        c,
        None,
        tool(
            "toolu_big",
            "Write",
            json!({"file_path": "/workspace/x", "content": "y".repeat(20 << 20)}),
        ),
    );
    let mut ws = e.ws(STAFF).await;
    let (_, p) = ws.rpc("fetch_agent_timeline_request", json!({"agentId": c})).await;
    let entries = p["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "the unknown kind shows nothing");
    let content = entries[0]["item"]["detail"]["content"].as_str().unwrap();
    assert!(content.len() < (2 << 20));
    assert!(content.ends_with("…（輸出過長，只顯示開頭部分）"));
}

// ------------------------------------------------------------------------------------------------ the frame corpus

async fn tour(e: &Env, log: &std::sync::Arc<std::sync::Mutex<Vec<common::ws::Record>>>) {
    let token = e.h.token(STAFF);
    let mut raw = Ws::open(&e.h.url, &token, "").await.unwrap().record(log, "handshake");
    raw.raw(json!({"type": "ping"})).await;
    raw.next_raw(Duration::from_secs(5)).await.unwrap();
    raw.raw(
        json!({"type": "hello", "clientId": "c", "clientType": "browser", "protocolVersion": 1,
                   "capabilities": {}, "appVersion": "0.13.2"}),
    )
    .await;
    raw.next_session("status").await;
    let mut rej = Ws::open(&e.h.url, &token, "").await.unwrap().record(log, "protocol 2");
    rej.raw(
        json!({"type": "hello", "clientId": "c", "clientType": "browser", "protocolVersion": 2,
                   "capabilities": {}}),
    )
    .await;
    rej.next_raw(Duration::from_secs(5)).await.unwrap();

    let mut ws = raw;
    ws.set_case("static and developer-machine requests");
    for (kind, msg) in [
        (
            "session.events.set_subscription.request",
            json!({"events": ["agent_permission_request"]}),
        ),
        ("workspace.label.list.request", json!({"subscribe": {}})),
        (
            "workspace.label.list.request",
            json!({"sync": {"generation": "g", "afterSeq": 0}}),
        ),
        ("hosts.list.request", json!({})),
        ("usage.list_reports.request", json!({})),
        ("plugin.catalog.get.request", json!({})),
        ("project.icon.get.request", json!({"projectId": "prj_conversations"})),
        ("project_icon_request", json!({"cwd": "/workspace"})),
        (
            "list_provider_features_request",
            json!({"draftConfig": {"provider": "claude", "cwd": "/workspace"}}),
        ),
        ("get_daemon_config_request", json!({})),
        ("checkout_status_request", json!({"cwd": "/workspace"})),
        ("checkout_pr_status_request", json!({"cwd": "/workspace"})),
        (
            "subscribe_terminals_request",
            json!({"cwd": "/workspace", "workspaceId": "wks_x"}),
        ),
        ("list_terminals_request", json!({"cwd": "/workspace"})),
        ("workspace.script.list.request", json!({"workspaceId": "wks_x"})),
        ("agent.provider_subagents.list.request", json!({"parentAgentId": "a"})),
        ("get_providers_snapshot_request", json!({"cwd": "/workspace"})),
        ("project.list.request", json!({"subscribe": true})),
        (
            "project.list.request",
            json!({"sync": {"generation": "g", "headSeq": 1}}),
        ),
        ("subscription.release.request", json!({"subscriptionId": "s"})),
    ] {
        ws.rpc(kind, msg).await;
    }
    // a request no client build knows (a newer client's): unsupported
    ws.rpc_invalid("agent.rewind", json!({})).await;

    ws.set_case("lists");
    ws.rpc("fetch_workspaces_request", json!({"subscribe": {}})).await;
    ws.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    let c = e.m.add(STAFF, Some("A conversation"));
    let untitled = e.m.add(STAFF, None);
    ws.wait_for("upsert", |v| v["message"]["type"] == "workspace_update")
        .await;
    ws.wait_for("upsert", |v| v["message"]["type"] == "agent_update").await;
    e.m.sandbox(untitled, SandboxPhase::Preparing);
    ws.next_session(pigeon::paseo::compat::SANDBOX_STATUS).await;
    tokio::time::sleep(model_cache()).await;
    ws.rpc(
        "fetch_workspaces_request",
        json!({"subscribe": {}, "page": {"limit": 1}}),
    )
    .await;
    ws.rpc("fetch_agents_request", json!({"subscribe": {}})).await;
    ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    ws.rpc("fetch_agent_request", json!({"agentId": Uuid::new_v4()})).await;

    ws.set_case("timeline");
    let sub = watch(&mut ws, &[c]).await;
    let _ = sub;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let turn = e.m.start_turn(c, TurnOrigin::Person);
    let blob: magpie_api::uploads::BlobId = "b-1".to_owned();
    e.m.row(
        c,
        None,
        RowItem::User(magpie_api::rows::UserItem {
            message_id: "cm-1".into(),
            client_message_id: Some("cm-1".into()),
            text: "Please read the sheet".into(),
            images: vec![magpie_api::rows::RowImage {
                blob_id: blob,
                mime_type: "image/png".into(),
                width: None,
                height: None,
            }],
            files: vec![
                magpie_api::rows::RowFile {
                    path: "uploads/u/q3.xlsx".into(),
                    name: "q3.xlsx".into(),
                    mime_type: String::new(),
                    size: 1200,
                    missing: None,
                },
                magpie_api::rows::RowFile {
                    path: "../escape".into(),
                    name: "x".into(),
                    mime_type: String::new(),
                    size: 1,
                    missing: None,
                },
            ],
            viewer: None,
        }),
    );
    e.m.row(
        c,
        Some(turn),
        RowItem::Reasoning(magpie_api::rows::ReasoningItem {
            message_id: "m".into(),
            block_index: 0,
            text: "thinking".into(),
        }),
    );
    let tools = [
        ("toolu_b", "Bash", json!({"command": "ls uploads"}), Some("q3.xlsx\n")),
        (
            "toolu_r",
            "Read",
            json!({"file_path": "/workspace/a.py"}),
            Some("     1\timport os"),
        ),
        (
            "toolu_w",
            "Write",
            json!({"file_path": "/workspace/s.md", "content": "# S"}),
            Some("ok"),
        ),
        (
            "toolu_e",
            "Edit",
            json!({"file_path": "/workspace/n.md", "old_string": "a", "new_string": "b"}),
            None,
        ),
        ("toolu_g", "Glob", json!({"pattern": "**/*.xlsx"}), None),
        (
            "toolu_f",
            "WebFetch",
            json!({"url": "https://example.com", "prompt": "p"}),
            None,
        ),
        ("toolu_k", "Skill", json!({"skill": "xlsx"}), Some("loaded")),
        ("toolu_p", "ExitPlanMode", json!({"plan": "1. read"}), None),
        ("toolu_m", "mcp__old__search", json!({"query": "hsi"}), Some("results")),
        (
            "toolu_n",
            "mcp__tools__fetch_url",
            json!({"url": "https://example.com"}),
            Some("page"),
        ),
        ("toolu_t", "TodoWrite", json!({"todos": []}), Some("ok")),
    ];
    for (call, name, input, result) in tools {
        let s = e.m.row(c, Some(turn), tool(call, name, input));
        if let Some(r) = result {
            e.m.update(c, s, |i| complete(i, r));
        }
    }
    let failed = e.m.row(
        c,
        Some(turn),
        tool("toolu_x", "Read", json!({"file_path": "/workspace/missing"})),
    );
    e.m.update(c, failed, |i| {
        if let RowItem::Tool(t) = i {
            t.status = ToolStatus::Failed;
            t.result = Some(ToolResult {
                text: "File does not exist.".into(),
                truncated: true,
                bytes: 20,
                blob_id: None,
                is_error: true,
                images: None,
            });
        }
    });
    let sent = e.m.row(
        c,
        Some(turn),
        tool("toolu_s", "SendUserFile", json!({"path": "/workspace/out.csv"})),
    );
    e.m.update(c, sent, |i| {
        complete(i, "sent");
        if let RowItem::Tool(t) = i {
            t.sent_files = Some(vec![magpie_api::rows::RowSentFile {
                path: "out.csv".into(),
                name: "out.csv".into(),
                size: 10,
                mime_type: "text/csv".into(),
                is_image: false,
            }]);
        }
    });
    e.m.row(c, Some(turn), assistant("The total is 42."));
    for code in [
        DataCode::ModelBudget,
        DataCode::ModelRateLimited,
        DataCode::ModelUnavailable,
        DataCode::ModelAccess,
        DataCode::ModelRefused,
        DataCode::ModelConnection,
    ] {
        e.m.row(
            c,
            Some(turn),
            RowItem::Assistant(AssistantItem {
                message_id: format!("msg_{code}"),
                block_index: 0,
                text: "API Error: …".into(),
                error: Some(ErrorInfo::new(code, "provider words")),
            }),
        );
    }
    for code in [
        DataCode::SandboxUnreachable,
        DataCode::SandboxBusy,
        DataCode::SandboxNotAllowed,
        DataCode::SandboxFailed,
        DataCode::SandboxTimeout,
        DataCode::SandboxLost,
        DataCode::RunExited,
        DataCode::RunError,
        DataCode::RunOom,
        DataCode::StopForced,
        DataCode::Internal,
        DataCode::from_wire("sandbox.budget"),
        DataCode::from_wire("cc.write_stuck"),
    ] {
        e.m.row(
            c,
            Some(turn),
            RowItem::Error(ErrorItem {
                code,
                message: "failed".into(),
                detail: None,
            }),
        );
    }
    e.m.end_turn(
        c,
        TurnPhase::Failed,
        Some(ErrorInfo::new(DataCode::SandboxLost, "lost")),
    );
    ws.wait_for("turn_failed", |v| {
        is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_failed"
    })
    .await;
    e.m.start_turn(c, TurnOrigin::Agent);
    e.m.end_turn(c, TurnPhase::Completed, None);
    ws.wait_for("turn_completed", |v| {
        is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_completed"
    })
    .await;
    let epoch = e.m.epoch(c);
    ws.rpc(
        "fetch_agent_timeline_request",
        json!({"agentId": c, "direction": "tail", "limit": 0}),
    )
    .await;
    ws.rpc(
        "fetch_agent_timeline_request",
        json!({"agentId": c, "direction": "before", "cursor": {"epoch": epoch, "seq": 5}, "limit": 2}),
    )
    .await;
    ws.rpc(
        "fetch_agent_timeline_request",
        json!({"agentId": c, "direction": "after", "cursor": {"epoch": "stale", "seq": 1}, "mergeWindow": true}),
    )
    .await;
    ws.rpc("fetch_agent_timeline_request", json!({"agentId": Uuid::new_v4()}))
        .await;
    ws.rpc("agent.timeline.list_prompts.request", json!({"agentId": c}))
        .await;
    ws.rpc(
        "agent.timeline.list_prompts.request",
        json!({"agentId": Uuid::new_v4()}),
    )
    .await;
    e.m.rewrite(c);
    ws.wait_for("replacement", |v| v["message"]["type"] == "agent.timeline.replacement")
        .await;

    ws.set_case("send, stop, asks");
    ws.rpc(
        "send_agent_message_request",
        json!({"agentId": c, "text": "go", "messageId": "s1"}),
    )
    .await;
    ws.rpc(
        "send_agent_message_request",
        json!({"agentId": c, "text": "again", "messageId": "s2"}),
    )
    .await;
    ws.rpc("cancel_agent_request", json!({"agentId": c})).await;
    ws.wait_for("canceled", |v| {
        is_stream(v, c) && v["message"]["payload"]["event"]["type"] == "turn_canceled"
    })
    .await;
    e.m.open_ask(
        c,
        Ask {
            id: "ask-1".into(),
            tool_name: "AskUserQuestion".into(),
            tool_use_id: Some("toolu_q".into()),
            input: serde_json::from_value(json!({"questions": [{"question": "Which?"}]})).unwrap(),
            title: Some("A question".into()),
            description: None,
            created_at: magpie_api::Timestamp::now(),
        },
    );
    ws.next_session("agent_permission_request").await;
    ws.rpc("fetch_agent_request", json!({"agentId": c})).await;
    e.m.change(
        c,
        ChangeBody::AskResolved(magpie_api::changes::AskResolvedChange {
            ask_id: "ask-1".into(),
            resolution: magpie_api::turns::AgentPermissionResponse::canceled(),
        }),
    );
    ws.next_session("agent_permission_resolved").await;
    e.m.sandbox(untitled, SandboxPhase::Ready);
    ws.next_session(pigeon::paseo::compat::SANDBOX_STATUS).await;

    ws.set_case("new conversation");
    ws.rpc(
        "workspace.create.request",
        json!({"idempotencyKey": "k1", "subscribe": true, "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"}, "agent": {"initialPrompt": "hi", "config": {"provider": "claude", "cwd": "/workspace"}}}),
    )
    .await;
    ws.rpc(
        "creation.subscribe.request",
        json!({"kind": "workspace", "idempotencyKey": "k1", "subscribe": true}),
    )
    .await;
    ws.rpc(
        "creation.subscribe.request",
        json!({"kind": "workspace", "idempotencyKey": "none"}),
    )
    .await;
    e.m.refuse_once("create_conversation", Refusal::new(ErrorCode::BadRequest, "no"));
    ws.rpc(
        "workspace.create.request",
        json!({"idempotencyKey": "k2", "subscribe": true, "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"}, "agent": {"config": {"provider": "claude", "cwd": "/workspace"}}}),
    )
    .await;

    ws.set_case("rename, delete, restore");
    ws.rpc("update_agent_request", json!({"agentId": c, "title": "Renamed"}))
        .await;
    ws.rpc(
        "workspace.title.set.request",
        json!({"workspaceId": format!("wks_{c}"), "title": ""}),
    )
    .await;
    ws.rpc(
        "workspace.title.set.request",
        json!({"workspaceId": format!("wks_{c}"), "title": "Again"}),
    )
    .await;
    ws.rpc(
        "archive_workspace_request",
        json!({"workspaceId": format!("wks_{untitled}")}),
    )
    .await;
    ws.rpc("fetch_agent_history_request", json!({"search": ""})).await;
    ws.rpc("refresh_agent_request", json!({"agentId": untitled})).await;
    ws.rpc("archive_agent_request", json!({"agentId": untitled})).await;
    ws.rpc("archive_agent_request", json!({"agentId": Uuid::new_v4()}))
        .await;
    ws.rpc("delete_agent_request", json!({"agentId": untitled})).await;
    ws.rpc("delete_agent_request", json!({"agentId": untitled})).await;
    let _ = ws.drain_for(Duration::from_millis(500)).await;

    // view-as refusals
    let vt = e.h.view_token(ADMIN, STAFF, common::now() + 600);
    let (va, _) = Ws::hello(
        &e.h.url,
        &e.h.token(ADMIN),
        &format!("?{}={vt}", pigeon::paseo::compat::VIEW_AS_QUERY),
    )
    .await;
    let mut va = va.record(log, "view-as");
    va.rpc("update_agent_request", json!({"agentId": c, "title": "x"}))
        .await;
    e.m.refuse_once("send_turn", Refusal::new(ErrorCode::ViewAsNotRecorded, "no"));
    va.rpc(
        "send_agent_message_request",
        json!({"agentId": c, "text": "x", "messageId": "v1"}),
    )
    .await;
}

#[tokio::test]
async fn every_frame_shape_is_in_the_validated_corpus() {
    // every socket test checks its own frames (common/ws.rs); the tour sends every message type in both locales and
    // writes the corpus of its run for a reviewer to compare with the committed one
    let e = env().await;
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    tour(&e, &log).await;
    e.h.set_facts(|d| d.product.locale = "en-GB".into()).await;
    tour(&e, &log).await;
    let records = log.lock().unwrap().clone();
    let seen = common::corpus::lines(&records);
    assert!(seen.len() > 60, "{}", seen.len());
    assert!(
        records.iter().any(|r| r.1 == "in"),
        "client frames are in the corpus too"
    );
    let mut out = String::new();
    for line in seen.values() {
        out.push_str(&line.to_string());
        out.push('\n');
    }
    std::fs::write(concat!(env!("CARGO_TARGET_TMPDIR"), "/pigeon-frames.ndjson"), out).unwrap();
    common::corpus::check(&records);
    // every client message pigeon reads is in the corpus, so testing/paseo-contract checks its shape against every
    // client build's inbound schema
    let committed = std::fs::read_to_string(common::corpus::CORPUS).unwrap();
    let inbound: std::collections::BTreeSet<String> = committed
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["dir"] == "in")
        .map(|v| {
            let f = &v["frame"];
            let t = if f["type"] == "session" {
                &f["message"]["type"]
            } else {
                &f["type"]
            };
            t.as_str().unwrap().to_owned()
        })
        .collect();
    for kind in [
        "ping",
        "hello",
        "client_heartbeat",
        "subscription.release.request",
        "agent.timeline.set_subscription.request",
        "fetch_workspaces_request",
        "fetch_agents_request",
        "fetch_agent_request",
        "fetch_agent_history_request",
        "fetch_agent_timeline_request",
        "agent.timeline.list_prompts.request",
        "send_agent_message_request",
        "workspace.create.request",
        "creation.subscribe.request",
        "cancel_agent_request",
        "update_agent_request",
        "workspace.title.set.request",
        "archive_workspace_request",
        "archive_agent_request",
        "delete_agent_request",
        "refresh_agent_request",
        "agent_permission_response",
        "get_providers_snapshot_request",
        "project.list.request",
    ] {
        assert!(inbound.contains(kind), "no {kind} client frame in the corpus");
    }
}

/// Adds the frame shapes the other tests found missing (`target/tmp/pigeon-frames-missing/`) to the committed
/// corpus: run the socket tests, then this, then validate the corpus in `testing/paseo-contract`.
#[test]
#[ignore = "edits the committed corpus"]
fn merge_frame_corpus() {
    println!("{} shapes added", common::corpus::merge_missing());
}

/// Pins the calling thread to the CPUs `cpus` names (`taskset` on the thread's own id).
fn pin_thread(cpus: &str) {
    let me = std::fs::read_link("/proc/thread-self").unwrap();
    let tid = me.file_name().unwrap().to_string_lossy().into_owned();
    let out = std::process::Command::new("taskset")
        .args(["-pc", cpus, &tid])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// [`env`] with pigeon alone on one CPU (spec 8.6 and 13: a replica has a CPU limit of 1): its whole runtime (one
/// worker, its blocking threads, the listener, every socket and poller) runs on threads pinned to `cpu`, while the
/// in-memory magpie and the clients run on the test's own runtime, which the caller keeps off that CPU.
async fn env_pinned(cpu: &'static str) -> (Env, Pinned) {
    let m = Model::default();
    let served = model::serve(&m).await;
    let url = served.url();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        pin_thread(cpu);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .on_thread_start(move || pin_thread(cpu))
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let h = Harness::start_opts(Opts {
                tuning: fast(),
                magpie: Some(url),
                ..Opts::default()
            })
            .await;
            let _ = tx.send(h);
            let _ = stopped.await;
        });
        // pigeon's listener, sockets and pollers end with their runtime
        rt.shutdown_timeout(Duration::from_secs(5));
    });
    let h = rx.await.unwrap();
    (
        Env { h, m, _served: served },
        Pinned {
            stop: Some(stop),
            thread: Some(thread),
        },
    )
}

/// The pinned runtime of [`env_pinned`]: dropping it stops the runtime and joins its thread.
struct Pinned {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Pinned {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Spec 8.6's throughput target, recorded in the crate README: 5 000 open sockets on one replica (100 people, 50
/// sockets each, under the 64 per person), 1 000 of them (20 people) watching their person's conversation, a row
/// appended to each of the 20 conversations every 0.5 s for 10 s: 2 000 frames/s of fan-out. pigeon runs alone on
/// CPU 0 (`env_pinned`); the clients and magpie on the others. Run with
/// `taskset -c 1-15 cargo test --release -p pigeon --test socket bench_fanout -- --ignored --nocapture` (file
/// descriptor limit above 12 000).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark"]
async fn bench_fanout() {
    let (e, pinned) = env_pinned("0").await;
    let started = std::time::Instant::now();
    let mut idle = Vec::new();
    let mut readers = Vec::new();
    let mut convs = Vec::new();
    let rows = 20u64;
    for person in 0..100 {
        let email = format!("p{person}@example.org");
        let c = e.m.add(&email, None);
        let token = e.h.token(&email);
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..50 {
            let (url, token) = (e.h.url.clone(), token.clone());
            set.spawn(async move { Ws::hello(&url, &token, "").await.0 });
        }
        while let Some(ws) = set.join_next().await {
            let mut ws = ws.unwrap();
            if person < 20 {
                watch(&mut ws, &[c]).await;
                readers.push(tokio::spawn(async move {
                    let mut lat = Vec::new();
                    for _ in 0..rows {
                        let v = ws.wait_for("row", |v| is_stream(v, c)).await;
                        let text = v["message"]["payload"]["event"]["item"]["text"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        let sent_at: u128 = text.parse().unwrap();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_micros();
                        lat.push(now - sent_at);
                    }
                    lat
                }));
            } else {
                idle.push(ws);
            }
        }
        if person < 20 {
            convs.push(c);
        }
    }
    eprintln!("5000 sockets open in {:?}", started.elapsed());
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let push = std::time::Instant::now();
    for i in 0..rows {
        for c in &convs {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros();
            e.m.row(*c, None, assistant(&now.to_string()));
        }
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            push + Duration::from_millis(500 * (i + 1)),
        ))
        .await;
    }
    let mut all = Vec::new();
    for r in readers {
        all.extend(r.await.unwrap());
    }
    all.sort_unstable();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let p = |q: f64| all[((all.len() as f64 - 1.0) * q) as usize] as f64 / 1000.0;
    eprintln!(
        "{} frames to 1000 of 5000 sockets in {:?}: latency p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms",
        all.len(),
        push.elapsed(),
        p(0.5),
        p(0.99),
        p(1.0)
    );
    assert_eq!(all.len(), 20_000);
    let (change, timeline) = e.h.app.hub().unwrap().pollers();
    eprintln!("pollers: {change} change, {timeline} timeline");
    drop(idle);
    drop(e);
    // pigeon's runtime stops and its thread is joined
    drop(pinned);
}

/// A small guard on the fan-out path in CI: 200 sockets, 50 of them watching 5 conversations, 10 rows each, a row
/// every 50 ms to every conversation (500 frames/s): every frame arrives, and the 99th percentile latency stays under
/// 1 s on a debug build (the benchmark's own figures are in the README).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fan_out_latency_guard() {
    let e = env().await;
    let rows = 10u64;
    let mut readers = Vec::new();
    let mut idle = Vec::new();
    let mut convs = Vec::new();
    for person in 0..5 {
        let email = format!("g{person}@example.org");
        let c = e.m.add(&email, None);
        let token = e.h.token(&email);
        for i in 0..40 {
            let mut ws = Ws::hello(&e.h.url, &token, "").await.0;
            if i < 10 {
                watch(&mut ws, &[c]).await;
                readers.push(tokio::spawn(async move {
                    let mut lat = Vec::new();
                    for _ in 0..rows {
                        let v = ws.wait_for("row", |v| is_stream(v, c)).await;
                        let sent: u128 = v["message"]["payload"]["event"]["item"]["text"]
                            .as_str()
                            .unwrap()
                            .parse()
                            .unwrap();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_micros();
                        lat.push(now - sent);
                    }
                    lat
                }));
            } else {
                idle.push(ws);
            }
        }
        convs.push(c);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for _ in 0..rows {
        for c in &convs {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros();
            e.m.row(*c, None, assistant(&now.to_string()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut all = Vec::new();
    for r in readers {
        all.extend(r.await.unwrap());
    }
    all.sort_unstable();
    assert_eq!(all.len(), 500);
    let p99 = all[all.len() * 99 / 100 - 1];
    assert!(p99 < 1_000_000, "p99 {p99} us");
    drop(idle);
}

#[tokio::test]
async fn a_conversations_thinking_level_is_picked_checked_and_stored() {
    let e = env().await;
    let mut ws = e.ws(STAFF).await;
    let id = e.m.add(STAFF, Some("levels"));
    let agent = |p: &Value| (p["agent"]["model"].clone(), p["agent"]["thinkingOptionId"].clone());
    // the model list is read once, then cached
    ws.rpc("get_providers_snapshot_request", json!({"cwd": "/workspace"}))
        .await;
    tokio::time::sleep(model_cache()).await;
    // where people do not pick models, they pick no level either
    let (k, r) = ws
        .rpc(
            "set_agent_thinking_request",
            json!({"agentId": id, "thinkingOptionId": "max"}),
        )
        .await;
    assert_eq!(k, "set_agent_thinking_response");
    assert_eq!(r["accepted"], false);
    assert_eq!(e.m.calls_to("rename_conversation"), 0);
    e.h.set_facts(|d| d.conversations.people_pick_model = true).await;

    // a level of the conversation's model (the default while none is picked)
    let (_, r) = ws
        .rpc(
            "set_agent_thinking_request",
            json!({"agentId": id, "thinkingOptionId": "max"}),
        )
        .await;
    assert_eq!((r["accepted"].clone(), r["error"].clone()), (json!(true), Value::Null));
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
    assert_eq!(agent(&p), (json!("vendor/model-a"), json!("max")));
    assert_eq!(p["agent"]["effectiveThinkingOptionId"], "max");
    // one it does not have is refused, and magpie is not asked
    let before = e.m.calls_to("rename_conversation");
    let (_, r) = ws
        .rpc(
            "set_agent_thinking_request",
            json!({"agentId": id, "thinkingOptionId": "huge"}),
        )
        .await;
    assert_eq!(r["accepted"], false);
    assert_eq!(e.m.calls_to("rename_conversation"), before);

    // another model: a level it lacks goes back to none picked, one it has stays
    let (_, r) = ws
        .rpc(
            "set_agent_model_request",
            json!({"agentId": id, "modelId": "vendor/model-c"}),
        )
        .await;
    assert_eq!(r["accepted"], true);
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
    assert_eq!(agent(&p), (json!("vendor/model-c"), Value::Null), "model-c has no max");
    ws.rpc(
        "set_agent_thinking_request",
        json!({"agentId": id, "thinkingOptionId": "low"}),
    )
    .await;
    ws.rpc(
        "set_agent_model_request",
        json!({"agentId": id, "modelId": "vendor/model-a"}),
    )
    .await;
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
    assert_eq!(
        agent(&p),
        (json!("vendor/model-a"), json!("low")),
        "model-a has low too"
    );
    // a model with no levels takes none
    ws.rpc(
        "set_agent_model_request",
        json!({"agentId": id, "modelId": "vendor/model-b"}),
    )
    .await;
    let (_, r) = ws
        .rpc(
            "set_agent_thinking_request",
            json!({"agentId": id, "thinkingOptionId": "low"}),
        )
        .await;
    assert_eq!(r["accepted"], false);
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
    assert_eq!(agent(&p), (json!("vendor/model-b"), Value::Null));
    // null: none picked again
    ws.rpc(
        "set_agent_model_request",
        json!({"agentId": id, "modelId": "vendor/model-a"}),
    )
    .await;
    ws.rpc(
        "set_agent_thinking_request",
        json!({"agentId": id, "thinkingOptionId": "none"}),
    )
    .await;
    let (_, r) = ws
        .rpc(
            "set_agent_thinking_request",
            json!({"agentId": id, "thinkingOptionId": null}),
        )
        .await;
    assert_eq!(r["accepted"], true);
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await;
    assert_eq!(agent(&p), (json!("vendor/model-a"), Value::Null));

    // a draft's pick goes with the creation: the first turn runs on it; what the list does not hold is left out
    let create = |key: &str, config: Value| {
        json!({"idempotencyKey": key, "source": {"kind": "directory", "path": "/workspace", "projectId": "prj_conversations"},
               "agent": {"initialPrompt": "start", "clientMessageId": format!("{key}-m"), "config": config}})
    };
    let (_, r) = ws
        .rpc(
            "workspace.create.request",
            create("pick-1", json!({"provider": "claude", "cwd": "/workspace", "model": "vendor/model-c", "thinkingOptionId": "low"})),
        )
        .await;
    assert_eq!(agent(&r), (json!("vendor/model-c"), json!("low")));
    let (_, r) = ws
        .rpc(
            "workspace.create.request",
            create(
                "pick-2",
                json!({"provider": "claude", "cwd": "/workspace", "model": "vendor/other", "thinkingOptionId": "max"}),
            ),
        )
        .await;
    assert_eq!(
        agent(&r),
        (json!("vendor/model-a"), json!("max")),
        "an unknown model is left out; max is the default's"
    );
}
