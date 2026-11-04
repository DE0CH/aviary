//! A fake old router with its children, for the switch window's tests (the LU contract rig of WP-50): the Paseo
//! socket at the old socket path, which takes an upgrade only with pigeon's pair token in the LU header, and answers
//! like the old bridge (`paseo-connection.ts`) for what pigeon relays: hello, the timeline subscription, timeline
//! and agent reads, sends (the user row and `turn_started` before the answer; a second send while a turn runs is
//! refused, and a send is never de-duplicated by its id; a write that reaches its commit point past its
//! `aviaryNotAfter` is refused, as the old ui's switch module does), Stop, rename, delete, restore; a create
//! de-duplicated by its idempotency key across the person's sockets; the old upload route (files
//! land in the person's old asset folder), the old warm and the share routes. A test can stream rows of a running
//! turn, end it, refuse upgrades with a status, or hold sends. Every name of the old stack comes from pigeon's own
//! constants, so this file names none.
#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc)] // tests

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures_util::{SinkExt as _, StreamExt as _};
use pigeon::legacy::{LU_HEADER, OLD_SOCKET, OLD_UPLOAD, OLD_VIEW_AS_HEADER, OLD_WARM};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

/// One conversation in an old child.
#[derive(Debug, Clone, Default)]
pub struct Agent {
    pub person: String,
    pub epoch: String,
    pub rows: Vec<Value>,
    pub running: bool,
    pub title: Option<String>,
    pub deleted: bool,
    /// Every send the child took: (clientMessageId, text, attachment paths).
    pub sends: Vec<(String, String, Vec<String>)>,
    pub cancels: usize,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // a fake's knobs
pub struct St {
    pub agents: HashMap<Uuid, Agent>,
    /// Open sockets: (id, person, sender, watched agents).
    sockets: Vec<(u64, String, mpsc::UnboundedSender<String>, HashSet<Uuid>)>,
    next: u64,
    /// Every upgrade and request: (token, pair header, view-as header).
    pub seen: Vec<(String, Option<String>, Option<String>)>,
    /// Files written into old asset folders: (path, bytes).
    pub files: Vec<(String, Bytes)>,
    pub warms: Vec<Value>,
    pub shares: Vec<(String, Uuid, bool)>,
    pub heartbeats: usize,
    /// Messages held (sends wait) until released.
    pub hold_sends: bool,
    /// A send's turn ends at once (the fuzz).
    pub auto_end: bool,
    /// Requests and upgrades without the pair token (old-path tabs).
    pub old_path_requests: usize,
    /// Conversations the old hub reports preparing or holding a pending message (its busy state).
    pub preparing: HashSet<Uuid>,
    pub pending: HashSet<Uuid>,
    /// The next send is dropped with the socket (the connection closes before the child sees it); once.
    pub drop_on_send: bool,
    /// Busy-state reads answered 500.
    pub busy_down: bool,
    pub busy_reads: usize,
    /// Conversations with tracked background work (a turn ended, its background shell runs): listed `running` in
    /// the busy state as the old route lists them, though the old hub takes a send (it refuses only a turn).
    pub background: HashSet<Uuid>,
    /// Timeline reads answered with an error.
    pub timeline_down: bool,
    /// A conversation's share-list reads answered 500.
    pub share_lists_down: bool,
    /// Sends refused because an attachment path names no file the child holds (a copy-at-send gone wrong).
    pub broken_sends: Vec<(String, Vec<String>)>,
    /// Writes (socket and HTTP) refused because they reached their commit point past their `aviaryNotAfter`.
    pub late_refused: usize,
    /// Creates by (person, idempotency key): the person's child de-duplicates a create across sockets.
    pub creations: HashMap<(String, String), Uuid>,
}

/// Whether a write's not-after instant (Unix ms) has passed: the old ui's switch module refuses it then.
fn past(not_after: Option<u64>) -> bool {
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    not_after.is_some_and(|t| now > t)
}

/// The fake. Cheap to clone.
#[derive(Clone)]
pub struct Old {
    st: Arc<Mutex<St>>,
    pair: String,
    /// Upgrades and requests answered with this status instead (0: served).
    pub refuse: Arc<AtomicU16>,
    pub upgrades: Arc<AtomicUsize>,
    released: Arc<tokio::sync::Notify>,
    /// Closes every open socket (an old ui pod restart, a bridge blip).
    kill: tokio::sync::broadcast::Sender<()>,
    pub url: String,
}

/// The email inside an Access token (the fake trusts the edge; pigeon checked the token).
pub fn email_of(token: &str) -> String {
    token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["email"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The old asset path of a file of `person` (`<home>/<key>/.claude/cloudcli-assets/<n>-<name>`).
pub fn old_path(person: &str, n: usize, name: &str) -> String {
    format!(
        "/home/{}/{}/{n}-{name}",
        aviary_core::log::person_key(person),
        magpie_api::legacy::LEGACY_ASSET_DIR
    )
}

/// The old hub's refusal of a send while a turn runs.
pub const BUSY: &str = "對話仍在進行中，請稍候。";

fn session(kind: &str, payload: &Value) -> String {
    json!({"type": "session", "message": {"type": kind, "payload": payload}}).to_string()
}

impl Old {
    pub async fn start(pair: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let old = Self {
            st: Arc::new(Mutex::new(St::default())),
            pair: pair.to_owned(),
            refuse: Arc::new(AtomicU16::new(0)),
            upgrades: Arc::new(AtomicUsize::new(0)),
            released: Arc::new(tokio::sync::Notify::new()),
            kill: tokio::sync::broadcast::channel(4).0,
            url,
        };
        let router = Router::new().fallback(handle).with_state(old.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        old
    }

    pub fn st(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap()
    }

    /// A conversation the old child of `person` has, with one assistant row.
    pub fn add(&self, id: Uuid, person: &str) {
        let mut st = self.st();
        st.agents.insert(
            id,
            Agent {
                person: person.to_owned(),
                epoch: format!("old-{}", &id.to_string()[..8]),
                rows: vec![json!({"type": "assistant_message", "text": "舊的回覆"})],
                ..Agent::default()
            },
        );
    }

    pub fn agent(&self, id: Uuid) -> Agent {
        self.st().agents[&id].clone()
    }

    /// Closes every open socket from the old side (the old ui pod restarted, or the bridge blipped).
    pub fn drop_sockets(&self) {
        let _ = self.kill.send(());
    }

    /// Sockets open now.
    pub fn sockets(&self) -> usize {
        self.st().sockets.len()
    }

    /// The old child restarted: its timeline is rebuilt from history, without the user rows' ids.
    pub fn forget_row_ids(&self, id: Uuid) {
        let mut st = self.st();
        for r in &mut st.agents.get_mut(&id).unwrap().rows {
            if let Some(o) = r.as_object_mut() {
                o.remove("id");
                o.remove("clientMessageId");
            }
        }
    }

    /// Lets held sends answer.
    pub fn release_sends(&self) {
        self.st().hold_sends = false;
        self.released.notify_waiters();
    }

    fn broadcast(&self, person: &str, agent: Uuid, kind: &str, payload: &Value) {
        let st = self.st();
        for (_, p, tx, watched) in &st.sockets {
            if p == person && (kind != "agent_stream" || watched.contains(&agent)) {
                let mut pl = payload.clone();
                pl["subscriptionId"] = json!("old-sub");
                let _ = tx.send(session(kind, &pl));
            }
        }
    }

    /// A row of the running turn, streamed to every socket of the person watching the conversation.
    #[allow(clippy::needless_pass_by_value)] // the tests' rows are built in place
    pub fn stream_row(&self, id: Uuid, item: Value) -> u64 {
        let (person, epoch, seq) = {
            let mut st = self.st();
            let a = st.agents.get_mut(&id).unwrap();
            a.rows.push(item.clone());
            (a.person.clone(), a.epoch.clone(), a.rows.len() as u64)
        };
        self.broadcast(
            &person,
            id,
            "agent_stream",
            &json!({"agentId": id, "event": {"type": "timeline", "provider": "claude", "item": item},
                    "timestamp": "2026-11-03T09:00:00.000Z", "seq": seq, "epoch": epoch}),
        );
        seq
    }

    /// The running turn ends.
    pub fn end_turn(&self, id: Uuid) {
        let person = {
            let mut st = self.st();
            let a = st.agents.get_mut(&id).unwrap();
            a.running = false;
            a.person.clone()
        };
        self.broadcast(
            &person,
            id,
            "agent_stream",
            &json!({"agentId": id, "event": {"type": "turn_completed", "provider": "claude", "turnId": "t"},
                    "timestamp": "2026-11-03T09:00:00.000Z"}),
        );
    }

    /// The sandbox chip of a conversation.
    pub fn chip(&self, id: Uuid, state: &str) {
        let person = self.st().agents[&id].person.clone();
        self.broadcast(
            &person,
            id,
            pigeon::paseo::compat::SANDBOX_STATUS,
            &json!({"agentId": id, "state": state}),
        );
    }

    /// The child takes a send (as when one arrives): its user row and `turn_started`, the turn running unless
    /// `auto_end`.
    pub fn take_send(&self, id: Uuid, cmid: &str, text: &str, paths: Vec<String>) {
        let person = {
            let mut st = self.st();
            let auto_end = st.auto_end;
            let a = st.agents.get_mut(&id).unwrap();
            a.sends.push((cmid.to_owned(), text.to_owned(), paths));
            a.running = !auto_end;
            a.person.clone()
        };
        self.stream_row(
            id,
            json!({"type": "user_message", "id": format!("paseo-user-{cmid}"), "clientMessageId": cmid,
                   "text": text}),
        );
        self.broadcast(
            &person,
            id,
            "agent_stream",
            &json!({"agentId": id, "event": {"type": "turn_started", "provider": "claude", "turnId": "t"},
                    "timestamp": "2026-11-03T09:00:00.000Z"}),
        );
    }

    fn snapshot(a: &Agent, id: Uuid) -> Value {
        json!({"id": id, "status": if a.running { "running" } else { "idle" }, "title": a.title,
               "archivedAt": if a.deleted { json!("2026-11-03T09:00:00.000Z") } else { Value::Null }})
    }

    async fn on_message(&self, sock: u64, person: &str, m: &Value) -> Option<String> {
        let kind = m["type"].as_str().unwrap_or("");
        let rid = m["requestId"].clone();
        let agent = m["agentId"]
            .as_str()
            .or_else(|| m["workspaceId"].as_str().and_then(|w| w.strip_prefix("wks_")))
            .and_then(|s| Uuid::parse_str(s).ok());
        let not_after = m[pigeon::legacy::OLD_NOT_AFTER].as_u64();
        let refused_late = |old: &Self| {
            old.st().late_refused += 1;
            Some(session(
                "rpc_error",
                &json!({"requestId": rid, "requestType": kind, "error": "too late", "code": "expired"}),
            ))
        };
        if kind != "send_agent_message_request" && past(not_after) {
            return refused_late(self);
        }
        match kind {
            "client_heartbeat" => {
                self.st().heartbeats += 1;
                None
            }
            "agent.timeline.set_subscription.request" => {
                let ids: HashSet<Uuid> = m["agentIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect();
                let mut st = self.st();
                if let Some(s) = st.sockets.iter_mut().find(|s| s.0 == sock) {
                    s.3 = ids;
                }
                Some(session(
                    "agent.timeline.set_subscription.response",
                    &json!({"requestId": rid, "agentIds": m["agentIds"], "subscriptionId": "old-sub"}),
                ))
            }
            "fetch_agent_timeline_request" => {
                if self.st().timeline_down {
                    return Some(session(
                        "rpc_error",
                        &json!({"requestId": rid, "requestType": kind, "error": "unavailable", "code": "failed"}),
                    ));
                }
                let st = self.st();
                let a = agent.and_then(|id| st.agents.get(&id).filter(|a| a.person == person));
                let Some(a) = a else {
                    return Some(session(
                        "fetch_agent_timeline_response",
                        &json!({"requestId": rid, "agentId": m["agentId"], "entries": [], "epoch": "",
                                "error": "Conversation not found"}),
                    ));
                };
                // the old hub's paging: the tail, or the page before a cursor, `limit` rows (0 or none: all)
                let n = a.rows.len();
                let limit = m["limit"]
                    .as_u64()
                    .filter(|l| *l > 0)
                    .map_or(n, |l| usize::try_from(l).unwrap());
                let before = m["cursor"]["seq"].as_u64().map(|s| usize::try_from(s).unwrap());
                let (direction, hi) = match (m["direction"].as_str(), before) {
                    (Some("before"), Some(seq)) => ("before", seq.saturating_sub(1).min(n)),
                    _ => ("tail", n),
                };
                let lo = hi.saturating_sub(limit);
                let entries: Vec<Value> = a.rows[lo..hi]
                    .iter()
                    .enumerate()
                    .map(|(i, r)| json!({"seqStart": lo + i + 1, "seqEnd": lo + i + 1, "item": r}))
                    .collect();
                let start = (lo < hi).then(|| json!({"epoch": a.epoch, "seq": lo + 1}));
                Some(session(
                    "fetch_agent_timeline_response",
                    &json!({"requestId": rid, "agentId": m["agentId"], "agent": Self::snapshot(a, agent.unwrap()),
                            "direction": direction, "epoch": a.epoch, "entries": entries, "startCursor": start,
                            "hasOlder": lo > 0, "error": null}),
                ))
            }
            "fetch_agent_request" => {
                let st = self.st();
                let a = agent.and_then(|id| st.agents.get(&id));
                Some(session(
                    "fetch_agent_response",
                    &json!({"requestId": rid, "agent": a.map(|a| Self::snapshot(a, agent.unwrap())),
                            "project": null, "error": if a.is_some() { Value::Null } else { json!("Conversation not found") }}),
                ))
            }
            "send_agent_message_request" => {
                let id = agent?;
                if std::mem::take(&mut self.st().drop_on_send) {
                    let _ = self.kill.send(());
                    return None;
                }
                loop {
                    let held = self.st().hold_sends;
                    if !held {
                        break;
                    }
                    let n = self.released.notified();
                    if !self.st().hold_sends {
                        break;
                    }
                    n.await;
                }
                // the commit point: the old hub's steps before the user row are done
                if past(not_after) {
                    self.st().late_refused += 1;
                    return Some(session(
                        "send_agent_message_response",
                        &json!({"requestId": rid, "agentId": id, "accepted": false, "error": "too late"}),
                    ));
                }
                let cmid = m["messageId"].as_str().unwrap_or("").to_owned();
                let text = m["text"].as_str().unwrap_or("").to_owned();
                let paths: Vec<String> = m["attachments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a["path"].as_str().map(str::to_owned))
                    .collect();
                let running = self.st().agents.get(&id).is_some_and(|a| a.running);
                if running {
                    return Some(session(
                        "send_agent_message_response",
                        &json!({"requestId": rid, "agentId": id, "accepted": false, "error": BUSY}),
                    ));
                }
                {
                    // the old child reads each attached file from its asset folder: a path it does not hold fails
                    let mut st = self.st();
                    if paths.iter().any(|p| !st.files.iter().any(|f| &f.0 == p)) {
                        st.broken_sends.push((cmid, paths));
                        return Some(session(
                            "send_agent_message_response",
                            &json!({"requestId": rid, "agentId": id, "accepted": false, "error": "附件不存在"}),
                        ));
                    }
                }
                self.take_send(id, &cmid, &text, paths);
                Some(session(
                    "send_agent_message_response",
                    &json!({"requestId": rid, "agentId": id, "accepted": true, "error": null}),
                ))
            }
            "cancel_agent_request" => {
                let id = agent?;
                let snap = {
                    let mut st = self.st();
                    let a = st.agents.get_mut(&id)?;
                    a.cancels += 1;
                    a.running = false;
                    Self::snapshot(a, id)
                };
                self.broadcast(
                    person,
                    id,
                    "agent_stream",
                    &json!({"agentId": id, "event": {"type": "turn_canceled", "provider": "claude", "turnId": "t",
                                                     "reason": "Stopped"}, "timestamp": "2026-11-03T09:00:00.000Z"}),
                );
                Some(session(
                    "cancel_agent_response",
                    &json!({"requestId": rid, "agentId": id, "agent": snap, "error": null}),
                ))
            }
            "update_agent_request" | "workspace.title.set.request" => {
                let id = agent?;
                let title = m["title"].as_str().unwrap_or("").trim().to_owned();
                self.st().agents.get_mut(&id)?.title = Some(title.clone());
                let reply = if kind == "update_agent_request" { "update_agent_response" } else { "workspace.title.set.response" };
                Some(session(
                    reply,
                    &json!({"requestId": rid, "agentId": id, "workspaceId": m["workspaceId"], "accepted": true,
                            "title": title, "error": null}),
                ))
            }
            "archive_agent_request" | "delete_agent_request" => {
                let id = agent?;
                self.st().agents.get_mut(&id)?.deleted = true;
                Some(session(
                    if kind == "archive_agent_request" { "agent_archived" } else { "agent_deleted" },
                    &json!({"requestId": rid, "agentId": id, "archivedAt": "2026-11-03T09:00:00.000Z"}),
                ))
            }
            "refresh_agent_request" => {
                let id = agent?;
                self.st().agents.get_mut(&id)?.deleted = false;
                Some(session("status", &json!({"status": "agent_refreshed", "agentId": id, "requestId": rid})))
            }
            "workspace.create.request" => {
                let key = m["idempotencyKey"].as_str().unwrap_or("").to_owned();
                let known = (!key.is_empty())
                    .then(|| self.st().creations.get(&(person.to_owned(), key.clone())).copied())
                    .flatten();
                if let Some(id) = known {
                    let snap = Self::snapshot(&self.agent(id), id);
                    return Some(session(
                        "workspace.create.response",
                        &json!({"requestId": rid, "workspace": {"id": format!("wks_{id}")}, "agent": snap,
                                "setupTerminalId": null, "error": null}),
                    ));
                }
                let id = Uuid::new_v4();
                self.add(id, person);
                if !key.is_empty() {
                    self.st().creations.insert((person.to_owned(), key), id);
                }
                let cmid = m["agent"]["clientMessageId"].as_str().unwrap_or("first").to_owned();
                let text = m["agent"]["initialPrompt"].as_str().unwrap_or("").to_owned();
                {
                    let mut st = self.st();
                    let a = st.agents.get_mut(&id).unwrap();
                    a.sends.push((cmid, text, Vec::new()));
                    a.running = true;
                }
                let snap = Self::snapshot(&self.agent(id), id);
                Some(session(
                    "workspace.create.response",
                    &json!({"requestId": rid, "workspace": {"id": format!("wks_{id}")}, "agent": snap,
                            "setupTerminalId": null, "error": null}),
                ))
            }
            _ => (!rid.is_null()).then(|| {
                session(
                    "rpc_error",
                    &json!({"requestId": rid, "requestType": kind, "error": "Not available here", "code": "unsupported"}),
                )
            }),
        }
    }
}

async fn socket(old: Old, ws: WebSocket, person: String) {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let id = {
        let mut st = old.st();
        st.next += 1;
        let id = st.next;
        st.sockets.push((id, person.clone(), tx.clone(), HashSet::new()));
        id
    };
    let writer = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if sink.send(Message::Text(f.into())).await.is_err() {
                break;
            }
        }
    });
    let mut hello = false;
    let mut kill = old.kill.subscribe();
    loop {
        let m = tokio::select! {
            m = stream.next() => m,
            _ = kill.recv() => break,
        };
        let Some(Ok(m)) = m else { break };
        let Message::Text(t) = m else {
            if matches!(m, Message::Close(_)) {
                break;
            }
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
            continue;
        };
        if v["type"] == "hello" {
            hello = true;
            let _ = tx.send(session(
                "status",
                &json!({"status": "server_info", "serverId": "srv_old"}),
            ));
            continue;
        }
        if v["type"] != "session" || !hello {
            continue;
        }
        let (old, tx, person, m) = (old.clone(), tx.clone(), person.clone(), v["message"].clone());
        // each message as its own task, as the bridge's async handlers interleave
        tokio::spawn(async move {
            if let Some(answer) = old.on_message(id, &person, &m).await {
                let _ = tx.send(answer);
            }
        });
    }
    old.st().sockets.retain(|s| s.0 != id);
    writer.abort();
}

fn header(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

async fn handle(State(old): State<Old>, req: Request) -> Response {
    let token = header(req.headers(), "cf-access-jwt-assertion").unwrap_or_default();
    let pair = header(req.headers(), LU_HEADER);
    let view = header(req.headers(), OLD_VIEW_AS_HEADER);
    old.st().seen.push((token.clone(), pair.clone(), view));
    let refuse = old.refuse.load(Ordering::SeqCst);
    if refuse != 0 {
        return StatusCode::from_u16(refuse).unwrap().into_response();
    }
    if token.is_empty() || pair.as_deref().is_some_and(|p| p != old.pair) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // a socket without the pair token is an old-path tab's (counted apart from the LU's)
    if pair.is_none() {
        old.st().old_path_requests += 1;
    }
    // a view's calls reach the viewed person's child, as the old router routes them
    let viewed = old.st().seen.last().and_then(|s| s.2.clone()).and_then(|v| {
        let first = v.split('.').next()?.to_owned();
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(first).ok()?;
        serde_json::from_slice::<Value>(&b).ok()?["u"]
            .as_str()
            .map(str::to_owned)
    });
    let person = viewed.unwrap_or_else(|| email_of(&token));
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    if path == OLD_SOCKET {
        old.upgrades.fetch_add(1, Ordering::SeqCst);
        let (mut parts, _) = req.into_parts();
        let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        return ws.on_upgrade(move |s| socket(old, s, person));
    }
    let not_after = header(req.headers(), pigeon::legacy::OLD_NOT_AFTER_HEADER).and_then(|v| v.parse().ok());
    let body = axum::body::to_bytes(req.into_body(), 64 << 20)
        .await
        .unwrap_or_default();
    if past(not_after) {
        old.st().late_refused += 1;
        return (
            StatusCode::PRECONDITION_FAILED,
            axum::Json(json!({"error": "too late"})),
        )
            .into_response();
    }
    if path == OLD_UPLOAD && method == axum::http::Method::POST {
        let (name, mime, bytes) = parse_one_file(&body);
        let n = old.st().files.len() + 1;
        let p = old_path(&person, n, &name);
        let size = bytes.len();
        old.st().files.push((p.clone(), bytes));
        return axum::Json(json!({"attachments": [{"path": p, "name": name, "size": size, "mimeType": mime}]}))
            .into_response();
    }
    if path == OLD_WARM {
        old.st()
            .warms
            .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
        return StatusCode::ACCEPTED.into_response();
    }
    if path.ends_with("/shares") {
        let conv = path
            .rsplit('/')
            .nth(1)
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_default();
        if method == axum::http::Method::POST {
            let id = format!("sh{}", old.st().shares.len() + 1);
            old.st().shares.push((id.clone(), conv, true));
            return (
                StatusCode::CREATED,
                axum::Json(json!({"id": id, "url": format!("/s/{id}"),
                                                            "createdAt": "2026-11-03T09:00:00.000Z"})),
            )
                .into_response();
        }
        if old.st().share_lists_down {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let list: Vec<Value> = old
            .st()
            .shares
            .iter()
            .filter(|s| s.1 == conv && s.2)
            .map(|s| json!({"id": s.0, "url": format!("/s/{}", s.0), "createdAt": "2026-11-03T09:00:00.000Z"}))
            .collect();
        return axum::Json(json!({"shares": list})).into_response();
    }
    if method == axum::http::Method::DELETE {
        let id = path.rsplit('/').next().unwrap_or("").to_owned();
        let mut st = old.st();
        if let Some(s) = st.shares.iter_mut().find(|s| s.0 == id) {
            s.2 = false;
            return StatusCode::NO_CONTENT.into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    if path == pigeon::legacy::OLD_BUSY {
        let mut st = old.st();
        st.busy_reads += 1;
        if st.busy_down {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let running: Vec<String> = st
            .agents
            .iter()
            .filter(|(id, a)| a.person == person && (a.running || st.background.contains(id)))
            .map(|(id, _)| id.to_string())
            .collect();
        let ids = |s: &HashSet<Uuid>| s.iter().map(Uuid::to_string).collect::<Vec<_>>();
        return axum::Json(
            json!({"running": running, "processes": [], "preparing": ids(&st.preparing),
                                 "pending": ids(&st.pending)}),
        )
        .into_response();
    }
    if path.contains("/files/raw") {
        return (StatusCode::OK, "old file bytes").into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

/// The one file part of a multipart body: (file name, type, bytes).
fn parse_one_file(body: &[u8]) -> (String, String, Bytes) {
    let text = String::from_utf8_lossy(body);
    let name = text
        .split("filename=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap_or("file")
        .to_owned();
    let mime = text
        .split("content-type: ")
        .nth(1)
        .and_then(|r| r.split("\r\n").next())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let start = body.windows(4).position(|w| w == b"\r\n\r\n").map_or(0, |i| i + 4);
    let end = body.windows(4).rposition(|w| w == b"\r\n--").unwrap_or(body.len());
    (name, mime, Bytes::copy_from_slice(&body[start..end.max(start)]))
}
