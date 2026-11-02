//! The `/paseo-ws` socket (spec 7.1): the upgrade, the per-socket state, the ordered and bounded outbound queue,
//! heartbeats, the inbound limits, the open-socket re-checks and the close codes.
//!
//! Each socket runs a reader (this task) and a writer. Every frame to the client goes through one bounded queue
//! (8.5: 16 MiB or 4 096 frames; full = close 1013), so frames reach a socket in the order they were enqueued and
//! a slow socket never slows a poller or another socket. Request handlers run concurrently (a slow timeline fetch
//! never holds up a ping), each answering through the same queue.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::extract::Request;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use super::{SocketHandler, handshake};
use crate::App;
use crate::feeds::{Hub, LARGE_MESSAGE};
use crate::identity::{Access, Identity};
use hermit_api::people::Role;

/// Largest inbound message (7.1.1: the client's `MAX_WEBSOCKET_MESSAGE_BYTES`); larger is closed 1009.
pub const MAX_INBOUND: usize = 100 << 20;
/// The outbound queue's frame bound (8.5).
pub const QUEUE_FRAMES: usize = 4_096;
/// …and its byte bound.
pub const QUEUE_BYTES: usize = 16 << 20;
/// Inbound frame rate per socket, sustained and burst (13).
pub const RATE: f64 = 200.0;
/// See [`RATE`].
pub const BURST: f64 = 400.0;
/// Most conversations one socket watches (7.5.3).
pub const MAX_TIMELINE_AGENTS: usize = 64;
/// A queue entry that is not a frame but a close (`close_after_queued`): it cannot be a JSON text.
const CLOSE_MARKER: &str = "\u{0}close:";
/// The longest one frame write may take before the socket is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes received past the last whole message beyond which a message is arriving (the socket is not request-free;
/// above any read-ahead of small frames).
pub const ARRIVING: u64 = 256 << 10;

/// Which list a subscription is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListKind {
    /// `fetch_agents_request` with `subscribe`.
    Agents,
    /// `fetch_workspaces_request` with `subscribe`.
    Workspaces,
}

/// The admin of a view-as socket (6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// The admin's email.
    pub admin: String,
    /// The view session.
    pub session: String,
}

#[derive(Debug)]
struct ConnState {
    hello: bool,
    protocol: u64,
    client_type: String,
    app_version: String,
    agents_sub: Option<String>,
    workspaces_sub: Option<String>,
    projects_sub: Option<String>,
    timeline_sub: String,
    timeline_agents: HashSet<Uuid>,
    agents_floor: Option<u64>,
    workspaces_floor: Option<u64>,
    last_request: Instant,
}

/// One open socket (7.1.2): memory only, dies with the socket.
pub struct Conn {
    /// For logs.
    pub id: Uuid,
    /// The email whose data the socket sees.
    pub person: String,
    /// Set for a view-as socket.
    pub viewer: Option<Viewer>,
    out: mpsc::Sender<String>,
    out_bytes: AtomicUsize,
    close_tx: watch::Sender<Option<(u16, String)>>,
    closing: AtomicBool,
    st: Mutex<ConnState>,
    inflight: AtomicUsize,
    idle: tokio::sync::Notify,
    pong: AtomicBool,
    quiet: Duration,
    metrics: crate::metrics::Metrics,
    /// Bytes read from the socket's connection so far (`server::InboundBytes`), when the listener counts them.
    inbound: Option<Arc<AtomicU64>>,
    /// [`Self::inbound`] when the reader last had a whole message.
    consumed: AtomicU64,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn").field("id", &self.id).finish_non_exhaustive()
    }
}

/// A request in flight on a socket; the drain waits for none to be left.
#[derive(Debug)]
pub struct InFlight(Arc<Conn>);

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.0.inflight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl Conn {
    fn st(&self) -> std::sync::MutexGuard<'_, ConnState> {
        self.st.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Enqueues a frame of message type `kind`; skipped when the socket is closing. A full queue closes the socket
    /// 1013 (8.5).
    pub fn send(&self, kind: &str, frame: String) -> bool {
        if self.closing.load(Ordering::SeqCst) {
            return false;
        }
        self.metrics.frame_out(kind);
        let len = frame.len();
        if self.out_bytes.fetch_add(len, Ordering::SeqCst) + len > QUEUE_BYTES {
            self.out_bytes.fetch_sub(len, Ordering::SeqCst);
            self.slow();
            return false;
        }
        if self.out.try_send(frame).is_ok() {
            return true;
        }
        self.out_bytes.fetch_sub(len, Ordering::SeqCst);
        self.slow();
        false
    }

    fn slow(&self) {
        tracing::warn!(conn = %self.id, "slow socket closed");
        self.close(1013, "try again later");
    }

    /// Enqueues a session frame of `kind` whose payload is `{"subscriptionId": sub, …rest}`.
    pub fn send_sub(&self, kind: &str, sub: &str, rest: &str) -> bool {
        self.send(kind, super::messages::session_with_subscription(kind, sub, rest))
    }

    /// Enqueues a session reply.
    pub fn reply(&self, kind: &str, payload: &Value) -> bool {
        if kind == "rpc_error" {
            let code = payload.get("code").and_then(Value::as_str).unwrap_or("");
            // the request type is the client's: unknown ones are counted together
            let ty = if code == "unsupported" {
                "other"
            } else {
                payload.get("requestType").and_then(Value::as_str).unwrap_or("")
            };
            self.metrics.rpc_error(ty, code);
        }
        self.send(kind, super::messages::session(kind, payload))
    }

    /// Closes the socket with `code` (once; later calls do nothing). Frames already queued are dropped: for a slow
    /// socket (1013), one over its limits (1008, 1009) or a signed-out one; a close that must follow what was
    /// queued is [`Self::close_after_queued`].
    pub fn close(&self, code: u16, reason: &str) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            let _ = self.close_tx.send(Some((code, reason.to_owned())));
        }
    }

    /// Closes the socket with `code` after the frames already queued are written (a `hello.rejected`, the drain's
    /// 1012 after the replies it waited for, a resync).
    pub fn close_after_queued(&self, code: u16, reason: &str) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let marker = format!("{CLOSE_MARKER}{code}:{reason}");
        if self.out.try_send(marker).is_err() {
            self.close(code, reason);
            return;
        }
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Whether the socket is closing or closed.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    /// Marks a request in flight until the guard drops.
    #[must_use]
    pub fn in_flight(self: &Arc<Self>) -> InFlight {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        InFlight(self.clone())
    }

    /// Requests in flight.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.inflight.load(Ordering::SeqCst)
    }

    /// Whether a message is arriving: more than [`ARRIVING`] bytes came in since the reader last had a whole
    /// message (a large send in transfer).
    #[must_use]
    pub fn arriving(&self) -> bool {
        self.inbound.as_ref().is_some_and(|n| {
            n.load(Ordering::SeqCst)
                .saturating_sub(self.consumed.load(Ordering::SeqCst))
                > ARRIVING
        })
    }

    /// A request in flight or a message arriving.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.requests() > 0 || self.arriving()
    }

    /// Not [`Self::busy`] and nothing but heartbeats for the quiet time (5 s, DESIGN 9.3).
    #[must_use]
    pub fn request_free(&self) -> bool {
        !self.busy() && self.st().last_request.elapsed() >= self.quiet
    }

    /// Waits until no request is in flight and no message is arriving (its handler then counts as in flight), at
    /// most `bound`.
    pub async fn wait_requests(&self, bound: Duration) {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let idle = self.idle.notified();
            if !self.busy() {
                return;
            }
            let step = (tokio::time::Instant::now() + Duration::from_millis(50)).min(deadline);
            let _ = tokio::time::timeout_at(step, idle).await;
            if tokio::time::Instant::now() >= deadline {
                return;
            }
        }
    }

    fn received_message(&self) {
        if let Some(n) = &self.inbound {
            self.consumed.store(n.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }

    /// The socket's stable timeline subscription id.
    #[must_use]
    pub fn timeline_sub(&self) -> String {
        self.st().timeline_sub.clone()
    }

    /// The conversations this socket watches.
    #[must_use]
    pub fn timeline_agents(&self) -> HashSet<Uuid> {
        self.st().timeline_agents.clone()
    }

    /// Whether the socket watches `c`'s timeline.
    #[must_use]
    pub fn watches(&self, c: &Uuid) -> bool {
        self.st().timeline_agents.contains(c)
    }

    /// Whether the socket subscribes to a list.
    #[must_use]
    pub fn lists(&self) -> bool {
        let st = self.st();
        st.agents_sub.is_some() || st.workspaces_sub.is_some()
    }

    pub(crate) fn replace_timeline_agents(&self, agents: HashSet<Uuid>) -> HashSet<Uuid> {
        std::mem::replace(&mut self.st().timeline_agents, agents)
    }

    /// A list subscription's id.
    #[must_use]
    pub fn list_sub(&self, kind: ListKind) -> Option<String> {
        let st = self.st();
        match kind {
            ListKind::Agents => st.agents_sub.clone(),
            ListKind::Workspaces => st.workspaces_sub.clone(),
        }
    }

    /// The list subscription's id, made on first use.
    pub(crate) fn subscribe_list(&self, kind: ListKind) -> String {
        let mut st = self.st();
        let slot = match kind {
            ListKind::Agents => &mut st.agents_sub,
            ListKind::Workspaces => &mut st.workspaces_sub,
        };
        slot.get_or_insert_with(|| Uuid::new_v4().to_string()).clone()
    }

    pub(crate) fn subscribe_projects(&self) -> String {
        self.st()
            .projects_sub
            .get_or_insert_with(|| Uuid::new_v4().to_string())
            .clone()
    }

    /// The change seq a list subscription has seen up to (live changes at or below it are skipped).
    #[must_use]
    pub fn floor(&self, kind: ListKind) -> Option<u64> {
        let st = self.st();
        match kind {
            ListKind::Agents => st.agents_floor,
            ListKind::Workspaces => st.workspaces_floor,
        }
    }

    pub(crate) fn set_floor(&self, kind: ListKind, seq: u64) {
        let mut st = self.st();
        match kind {
            ListKind::Agents => st.agents_floor = Some(seq),
            ListKind::Workspaces => st.workspaces_floor = Some(seq),
        }
    }

    /// `subscription.release.request` (7.5.1): clears the subscription with that id.
    pub(crate) fn release(&self, hub: &Hub, conn: &Arc<Conn>, id: &str) {
        let timeline = {
            let mut st = self.st();
            if st.agents_sub.as_deref() == Some(id) {
                st.agents_sub = None;
                st.agents_floor = None;
            }
            if st.workspaces_sub.as_deref() == Some(id) {
                st.workspaces_sub = None;
                st.workspaces_floor = None;
            }
            if st.projects_sub.as_deref() == Some(id) {
                st.projects_sub = None;
            }
            st.timeline_sub == id
        };
        if timeline {
            hub.set_timeline(conn, &HashSet::new());
        }
    }

    pub(crate) fn hello(&self) -> bool {
        self.st().hello
    }

    pub(crate) fn set_hello(&self, protocol: u64, client_type: &str, app_version: &str) {
        let mut st = self.st();
        st.hello = true;
        st.protocol = protocol;
        client_type.clone_into(&mut st.client_type);
        app_version.clone_into(&mut st.app_version);
    }

    fn touched(&self) {
        self.st().last_request = Instant::now();
    }

    /// The person calls of this socket (`Aviary-Person`, and the viewer headers for view-as), one request id each.
    #[must_use]
    pub fn calls<'a>(&self, client: &'a magpie_api::client::Client) -> magpie_api::client::PersonCalls<'a> {
        let calls = match &self.viewer {
            Some(v) => client.viewing_as(&self.person, &v.admin, &v.session),
            None => client.person(&self.person),
        };
        calls.with_request_id(Uuid::now_v7().to_string())
    }
}

/// The socket package's [`SocketHandler`]: upgrades `/paseo-ws` and runs the socket on the replica's [`Hub`].
#[derive(Debug, Clone, Default)]
pub struct PaseoSocket {
    hub: Hub,
}

impl PaseoSocket {
    /// A handler on `hub`.
    #[must_use]
    pub fn new(hub: Hub) -> Self {
        Self { hub }
    }

    /// The hub.
    #[must_use]
    pub fn hub(&self) -> &Hub {
        &self.hub
    }
}

impl SocketHandler for PaseoSocket {
    fn upgrade(
        &self,
        app: App,
        req: Request,
        id: Identity,
    ) -> Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        let hub = self.hub.clone();
        Box::pin(async move {
            if hub.draining() || hub.full() {
                return crate::http::text(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable");
            }
            let (mut parts, _) = req.into_parts();
            let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                Ok(ws) => ws,
                Err(r) => return r.into_response(),
            };
            hub.set_app(&app);
            let inbound = parts
                .extensions
                .get::<crate::server::InboundBytes>()
                .map(|b| b.0.clone());
            ws.max_message_size(MAX_INBOUND)
                .max_frame_size(MAX_INBOUND)
                .on_upgrade(move |socket| run(app, hub, socket, id, inbound))
        })
    }

    fn drain(&self, jitter: Duration, rpc_wait: Duration) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let hub = self.hub.clone();
        Box::pin(async move { hub.drain(jitter, rpc_wait).await })
    }

    fn hub(&self) -> Option<Hub> {
        Some(self.hub.clone())
    }
}

fn person_of(id: &Identity) -> Option<(String, Option<Viewer>, i64)> {
    match id {
        Identity::Person(p) => Some((p.email.clone(), None, p.exp)),
        Identity::ViewAs(v) => Some((
            v.person.clone(),
            Some(Viewer {
                admin: v.admin.email.clone(),
                session: v.session.clone(),
            }),
            v.exp.min(v.admin.exp),
        )),
        _ => None,
    }
}

async fn run(app: App, hub: Hub, socket: WebSocket, id: Identity, inbound: Option<Arc<AtomicU64>>) {
    let Some((person, viewer, exp)) = person_of(&id) else {
        return;
    };
    let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
    let (close_tx, close_rx) = watch::channel(None);
    let conn = Arc::new(Conn {
        id: Uuid::new_v4(),
        person,
        viewer,
        out: tx,
        out_bytes: AtomicUsize::new(0),
        close_tx,
        closing: AtomicBool::new(false),
        st: Mutex::new(ConnState {
            hello: false,
            protocol: 0,
            client_type: String::new(),
            app_version: String::new(),
            agents_sub: None,
            workspaces_sub: None,
            projects_sub: None,
            timeline_sub: Uuid::new_v4().to_string(),
            timeline_agents: HashSet::new(),
            agents_floor: None,
            workspaces_floor: None,
            last_request: Instant::now(),
        }),
        inflight: AtomicUsize::new(0),
        idle: tokio::sync::Notify::new(),
        pong: AtomicBool::new(true),
        quiet: app.0.tuning.socket_quiet,
        metrics: app.0.metrics.clone(),
        consumed: AtomicU64::new(inbound.as_ref().map_or(0, |n| n.load(Ordering::SeqCst))),
        inbound,
    });
    hub.register(&conn);
    app.0.metrics.socket_opened(conn.viewer.is_some());
    tracing::info!(conn = %conn.id, viewas = conn.viewer.is_some(), "socket open");
    let (sink, stream) = socket.split();
    let mut writer = tokio::spawn(write_loop(
        conn.clone(),
        sink,
        rx,
        close_rx.clone(),
        app.0.tuning.socket_ping,
    ));
    let checks = tokio::spawn(recheck_loop(app.clone(), conn.clone(), exp, close_rx.clone()));
    let reader = read_loop(&app, &hub, &conn, stream, close_rx);
    tokio::pin!(reader);
    let code = tokio::select! {
        () = &mut reader => {
            conn.close(1000, "");
            writer.await.map_or(1006, |w| w.code)
        }
        w = &mut writer => match w {
            // a close the server sent: the reader waits (briefly) for the client's answer to it
            Ok(w) if w.sent_close => {
                reader.await;
                w.code
            }
            // the peer is gone (a missed ping, a write that failed or timed out): drop both halves now
            w => {
                conn.close(1006, "");
                w.map_or(1006, |w| w.code)
            }
        },
    };
    checks.abort();
    hub.unregister(&conn);
    app.0.metrics.socket_closed(conn.viewer.is_some(), code);
    let (client_type, app_version) = {
        let st = conn.st();
        (st.client_type.clone(), st.app_version.clone())
    };
    tracing::info!(conn = %conn.id, code, client_type, app_version, "socket closed");
}

/// How the writer ended.
struct WriterEnd {
    code: u16,
    /// A close frame was sent (else the peer is gone).
    sent_close: bool,
}

const GONE: WriterEnd = WriterEnd {
    code: 1006,
    sent_close: false,
};

/// Writes queued frames in order, a ping every `ping_every` (a socket that did not answer the previous ping is
/// dropped, 7.1.3), and the close frame when asked.
async fn write_loop(
    conn: Arc<Conn>,
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<String>,
    mut close_rx: watch::Receiver<Option<(u16, String)>>,
    ping_every: Duration,
) -> WriterEnd {
    let mut ping = tokio::time::interval(ping_every);
    ping.tick().await;
    loop {
        tokio::select! {
            biased;
            changed = close_rx.changed() => {
                let close = close_rx.borrow().clone();
                if let Some((code, reason)) = close {
                    if code == 1006 {
                        return GONE;
                    }
                    let frame = Message::Close(Some(CloseFrame { code, reason: reason.into() }));
                    let _ = tokio::time::timeout(Duration::from_secs(2), sink.send(frame)).await;
                    return WriterEnd { code, sent_close: true };
                }
                if changed.is_err() {
                    return GONE;
                }
            }
            frame = rx.recv() => {
                let Some(frame) = frame else { return GONE };
                if let Some(close) = frame.strip_prefix(CLOSE_MARKER) {
                    let (code, reason) = close.split_once(':').unwrap_or((close, ""));
                    let code: u16 = code.parse().unwrap_or(1000);
                    let frame = Message::Close(Some(CloseFrame { code, reason: reason.to_owned().into() }));
                    let _ = tokio::time::timeout(Duration::from_secs(2), sink.send(frame)).await;
                    let _ = conn.close_tx.send(Some((code, reason.to_owned())));
                    return WriterEnd { code, sent_close: true };
                }
                conn.out_bytes.fetch_sub(frame.len(), Ordering::SeqCst);
                let sent = tokio::time::timeout(WRITE_TIMEOUT, sink.send(Message::Text(frame.into()))).await;
                if !matches!(sent, Ok(Ok(()))) {
                    tracing::info!(conn = %conn.id, "socket write failed or timed out");
                    return GONE;
                }
            }
            _ = ping.tick() => {
                if !conn.pong.swap(false, Ordering::SeqCst) {
                    tracing::info!(conn = %conn.id, "socket missed a ping");
                    return GONE;
                }
                let sent = tokio::time::timeout(WRITE_TIMEOUT, sink.send(Message::Ping(Vec::new().into()))).await;
                if !matches!(sent, Ok(Ok(()))) {
                    return GONE;
                }
            }
        }
    }
}

/// 7.1.4: every recheck period the person (and the view's admin) is checked against hermit; past the token's
/// `exp` the socket closes 4401, no longer allowed 4403.
async fn recheck_loop(app: App, conn: Arc<Conn>, exp: i64, mut close_rx: watch::Receiver<Option<(u16, String)>>) {
    let every = app.0.tuning.socket_recheck;
    loop {
        let now = crate::identity::now_secs();
        let to_exp = Duration::from_secs(u64::try_from(exp.saturating_sub(now)).unwrap_or(0));
        let wait = every.min(to_exp);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = close_rx.changed() => return,
        }
        if crate::identity::now_secs() >= exp {
            conn.close(4401, "signed out");
            return;
        }
        let ids = app.identities();
        let allowed = match &conn.viewer {
            None => ids.access(&conn.person).await != Some(Access::NotAllowed),
            Some(v) => {
                let admin = ids.access(&v.admin).await;
                let person = ids.access(&conn.person).await;
                admin.is_none_or(|a| a == Access::Allowed(Role::Admin)) && person != Some(Access::NotAllowed)
            }
        };
        if !allowed {
            conn.close(4403, "not allowed");
            return;
        }
    }
}

struct Bucket {
    tokens: f64,
    at: Instant,
    warned: Option<Instant>,
}

impl Bucket {
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * RATE).min(BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

async fn read_loop(
    app: &App,
    hub: &Hub,
    conn: &Arc<Conn>,
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    mut close_rx: watch::Receiver<Option<(u16, String)>>,
) {
    let mut bucket = Bucket {
        tokens: BURST,
        at: Instant::now(),
        warned: None,
    };
    loop {
        let msg = tokio::select! {
            m = stream.next() => m,
            _ = close_rx.changed() => {
                // a close was asked for: give the client a moment to answer it
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    while let Some(Ok(m)) = stream.next().await {
                        if matches!(m, Message::Close(_)) { break; }
                    }
                }).await;
                return;
            }
        };
        let Some(msg) = msg else { return };
        // busy from the moment the message is in until its handler (which counts itself) has started: taken before
        // the arriving count resets, so the drain never sees the socket idle in between
        let busy = conn.in_flight();
        conn.received_message();
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Pong(_)) => {
                conn.pong.store(true, Ordering::SeqCst);
                continue;
            }
            Ok(Message::Close(_)) => return,
            Ok(Message::Ping(_) | Message::Binary(_)) => continue,
            Err(e) => {
                if too_big(e) {
                    conn.close(1009, "message too big");
                    let _ = tokio::time::timeout(Duration::from_secs(2), close_rx.changed()).await;
                }
                return;
            }
        };
        // any traffic shows the client is alive
        conn.pong.store(true, Ordering::SeqCst);
        if !bucket.take() {
            if bucket.warned.is_none_or(|w| w.elapsed() >= Duration::from_mins(1)) {
                tracing::warn!(conn = %conn.id, "inbound frames over the rate; dropped");
                bucket.warned = Some(Instant::now());
            }
            app.0.metrics.frame_dropped("rate");
            continue;
        }
        let large = text.len() > LARGE_MESSAGE;
        let permit = if large {
            // pauses reading this socket until one of the large-message slots is free (the socket stays busy)
            hub.large().acquire_owned().await.ok()
        } else {
            None
        };
        let Ok(frame) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        drop(text);
        if !handshake::is_heartbeat(&frame) {
            conn.touched();
        }
        handshake::frame(app, hub, conn, &frame, permit);
        drop(busy);
    }
}

/// An inbound message over the size limit (tungstenite's capacity error, by variant, not by its text).
fn too_big(e: axum::Error) -> bool {
    use tokio_tungstenite::tungstenite::Error;
    e.into_inner()
        .downcast::<Error>()
        .is_ok_and(|e| matches!(*e, Error::Capacity(_)))
}
