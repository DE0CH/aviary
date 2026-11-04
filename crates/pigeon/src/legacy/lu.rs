//! One legacy upstream socket (switch-final 4.1, DESIGN 9.1): pigeon's WebSocket to the old router's `/paseo-ws`
//! for one native socket, as its person. Requests go out with a request id of their own and their answer comes
//! back to the caller; the old child's live frames of the legacy conversations the tab watches (`agent_stream`, the
//! sandbox chip, an ask and its resolution) go to the tab, `agent_stream` with the tab's own subscription id. A `client_heartbeat` every 15 s also keeps the old
//! child from its idle stop. The socket ends with its tab, or once it has streamed nothing to the tab and carried no
//! call for [`crate::Tuning::legacy_lu_idle`] (the old child then idles out, and the old stack's LU count reaches zero
//! with the tab still open: DESIGN 9.2, A5; the tab's next legacy request opens a new one); one that ends while its tab
//! stays is handled by [`Legacy::lu_ended`] (reopened with a replacement per watched conversation, or the tab
//! resumes).
//!
//! A write is never applied by the old child late. pigeon's own leg cannot promise it: bridge B1 is a load balancer
//! that acknowledges bytes itself, and the old router relays frames to the child on a socket of its own, so bytes
//! either holds can still arrive after a partition heals, whatever pigeon does with its connection. So each write
//! ([`Lu::write`]) carries its bound as an instant ([`OLD_NOT_AFTER`]), and the old ui's switch module refuses a write
//! that reaches its commit point later: a write pigeon gave up on is applied within its bound (plus the clocks' skew,
//! [`super::OLD_CLOCK_SKEW_MAX`]) or never. pigeon's leg also fails fast: a frame's write to the kernel is bounded (a
//! call's bound; the heartbeat period for the heartbeat and the subscription), the connection's `TCP_USER_TIMEOUT`
//! ([`LU_TCP_USER_TIMEOUT`]) aborts it once bytes stay unacknowledged that long, and a socket that ends with a call
//! unanswered, a write past its bound or a write call that timed out is reset (`SO_LINGER` 0).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;

use super::{CONNECT_TIMEOUT, LU_HEADER, LU_TCP_USER_TIMEOUT, Legacy, OLD_NOT_AFTER, OLD_SOCKET, OLD_VIEW_AS_HEADER};
use crate::paseo::compat;
use crate::paseo::transport::Conn;

/// Why an LU call has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuError {
    /// The old router answered 401 (or 403): the token is no longer good. Nothing was sent.
    SignedOut,
    /// The old router answered 503. Nothing was sent.
    Unavailable,
    /// The socket could not be opened (or there is no token). Nothing was sent.
    NotSent,
    /// The request went out and the socket closed before its answer.
    Lost,
    /// The request went out and no answer came within the call's bound.
    Timeout,
}

impl LuError {
    /// Whether the request may have reached the old child.
    #[must_use]
    pub fn maybe_sent(self) -> bool {
        matches!(self, Self::Lost | Self::Timeout)
    }
}

/// An answer: the session message type and its payload.
pub type Answer = Result<(String, Value), LuError>;

/// A frame for the socket and the moment past which it must not go out (`None`: whenever).
type Frame = (String, Option<tokio::time::Instant>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Open,
    Dead(LuError),
}

struct St {
    pending: HashMap<String, oneshot::Sender<Answer>>,
    agents: HashSet<Uuid>,
    /// Requests holding the LU across their steps ([`Lu::hold`]): it is not closed as unused meanwhile.
    holders: usize,
    /// When it was last handed out, called or held.
    last_use: Instant,
    /// Closed as unused: a new one is opened for the tab's next legacy request, under no reopen rule.
    idle: bool,
}

/// The socket. Its task owns the connection; this is the handle.
pub struct Lu {
    conn: Weak<Conn>,
    conn_id: Uuid,
    person: String,
    /// Frames for the socket, each with the moment past which it is dropped unsent (a call's bound).
    out: mpsc::UnboundedSender<Frame>,
    st: Mutex<St>,
    phase: watch::Sender<Phase>,
    next: AtomicU64,
    /// When it opened, and when it ended.
    up: Mutex<(Option<Instant>, Option<Instant>)>,
    /// A write call went unanswered past its bound: the socket is reset.
    abort: tokio::sync::Notify,
}

/// A request's hold on an LU ([`Lu::hold`]).
#[derive(Debug)]
pub struct Hold(Arc<Lu>);

impl Drop for Hold {
    fn drop(&mut self) {
        let mut st = self.0.st();
        st.holders = st.holders.saturating_sub(1);
        st.last_use = Instant::now();
    }
}

impl std::fmt::Debug for Lu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lu")
            .field("conn", &self.conn_id)
            .finish_non_exhaustive()
    }
}

impl Lu {
    /// Opens the LU of `conn`, its timeline set to `agents` once open.
    pub(crate) fn open(legacy: Legacy, conn: &Arc<Conn>, agents: HashSet<Uuid>) -> Arc<Self> {
        let (out, rx) = mpsc::unbounded_channel();
        let lu = Arc::new(Self {
            conn: Arc::downgrade(conn),
            conn_id: conn.id,
            person: conn.person.clone(),
            out,
            st: Mutex::new(St {
                pending: HashMap::new(),
                agents,
                holders: 0,
                last_use: Instant::now(),
                idle: false,
            }),
            phase: watch::channel(Phase::Connecting).0,
            next: AtomicU64::new(0),
            up: Mutex::new((None, None)),
            abort: tokio::sync::Notify::new(),
        });
        let me = lu.clone();
        let conn = conn.clone();
        tokio::spawn(async move { run(legacy, me, conn, rx).await });
        lu
    }

    fn st(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the socket ended (or never opened).
    #[must_use]
    pub fn is_dead(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Dead(_))
    }

    /// Why it ended (`None` while it has not).
    #[must_use]
    pub fn ended_with(&self) -> Option<LuError> {
        match *self.phase.borrow() {
            Phase::Dead(e) => Some(e),
            _ => None,
        }
    }

    /// How long it was open before it ended (`None`: it never opened).
    #[must_use]
    pub fn up_for(&self) -> Option<Duration> {
        let up = *self.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match up {
            (Some(open), Some(end)) => Some(end.saturating_duration_since(open)),
            (Some(open), None) => Some(open.elapsed()),
            _ => None,
        }
    }

    /// The native socket it serves.
    #[must_use]
    pub fn conn_id(&self) -> Uuid {
        self.conn_id
    }

    /// Whether it was closed as unused (not a drop: the tab's next legacy request opens a new one).
    #[must_use]
    pub fn closed_idle(&self) -> bool {
        self.st().idle
    }

    /// It is being used (handed out for a request); `false` when it was already closed as unused (a new one is
    /// opened instead: one closing must not take a request or a watch).
    pub(crate) fn touch(&self) -> bool {
        let mut st = self.st();
        st.last_use = Instant::now();
        !st.idle
    }

    /// Keeps it from closing as unused until the hold is dropped (a ticketed write's steps between its calls).
    #[must_use]
    pub fn hold(self: &Arc<Self>) -> Hold {
        let mut st = self.st();
        st.holders += 1;
        st.last_use = Instant::now();
        drop(st);
        Hold(self.clone())
    }

    /// Whether it has been unused for `idle`: nothing watched, no call in flight, no holder. When so it is closed as
    /// unused and dead at once, under the same lock as those checks, before its Close goes out: [`Legacy::lu_for`]
    /// hands out a new one from then and a call through this one is not sent ([`Self::call`]).
    fn idle_now(&self, idle: Duration) -> bool {
        let mut st = self.st();
        if !st.idle && st.agents.is_empty() && st.pending.is_empty() && st.holders == 0 && st.last_use.elapsed() >= idle
        {
            st.idle = true;
            self.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 = Some(Instant::now());
            self.phase.send_replace(Phase::Dead(LuError::NotSent));
        }
        st.idle
    }

    /// Whether its native socket is still there.
    #[must_use]
    pub fn conn_alive(&self) -> bool {
        self.conn.upgrade().is_some_and(|c| !c.is_closing())
    }

    fn rid(&self) -> String {
        format!("lu-{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// Waits until the socket is open (at most [`CONNECT_TIMEOUT`]); why not, when it does not open.
    ///
    /// # Errors
    /// The socket did not open.
    pub async fn ready(&self) -> Result<(), LuError> {
        let mut phase = self.phase.subscribe();
        let opened = tokio::time::timeout(CONNECT_TIMEOUT, phase.wait_for(|p| *p != Phase::Connecting)).await;
        match opened.map(|p| p.map(|p| *p)) {
            Ok(Ok(Phase::Open)) => Ok(()),
            Ok(Ok(Phase::Dead(e))) => Err(if e.maybe_sent() { LuError::NotSent } else { e }),
            _ => Err(LuError::NotSent),
        }
    }

    /// Sends a session message (its `requestId` replaced by the LU's own) and waits up to `bound` for the answer:
    /// the message whose payload names that request id (the response or `rpc_error`; the old child's
    /// `agent_refreshed` status too). The frame never goes out past `bound`: one still queued then is dropped, and
    /// one whose write to the socket has not finished by then ends the socket (a ticket's settle time counts on a
    /// write that has not reached the old child by its bound never reaching it).
    ///
    /// # Errors
    /// Why there is no answer ([`LuError`]).
    pub async fn call(&self, mut message: Value, bound: Duration) -> Answer {
        self.ready().await?;
        let rid = self.rid();
        if let Some(m) = message.as_object_mut() {
            m.insert("requestId".into(), json!(rid));
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.st();
            if st.idle {
                // closed as unused just now: nothing sent
                return Err(LuError::NotSent);
            }
            st.pending.insert(rid.clone(), tx);
            st.last_use = Instant::now();
        }
        let frame = json!({"type": "session", "message": message}).to_string();
        let deadline = tokio::time::Instant::now() + bound;
        if self.out.send((frame, Some(deadline))).is_err() {
            self.st().pending.remove(&rid);
            return Err(LuError::NotSent);
        }
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(a)) => a,
            Ok(Err(_)) => Err(LuError::Lost),
            Err(_) => {
                self.st().pending.remove(&rid);
                Err(LuError::Timeout)
            }
        }
    }

    /// [`Self::call`] for a write: it carries `bound` as the instant past which the old child refuses it
    /// ([`OLD_NOT_AFTER`]), and when it times out the socket is reset (its bytes may sit unacknowledged in the kernel
    /// through a stalled leg).
    ///
    /// # Errors
    /// Why there is no answer ([`LuError`]).
    pub async fn write(&self, mut message: Value, bound: Duration) -> Answer {
        if let Some(m) = message.as_object_mut() {
            m.insert(OLD_NOT_AFTER.into(), json!(super::not_after(bound)));
        }
        let a = self.call(message, bound).await;
        if matches!(a, Err(LuError::Timeout)) {
            self.abort.notify_one();
        }
        a
    }

    /// The tab now watches a legacy conversation: the old child streams it. `false` when it was closed as unused
    /// meanwhile (nothing watched through it: the caller opens a new one).
    pub fn watch(&self, conversation: Uuid) -> bool {
        let mut st = self.st();
        if st.idle {
            return false;
        }
        if st.agents.insert(conversation) {
            drop(st);
            self.send_agents();
        }
        true
    }

    /// …and no longer does (or it was released).
    pub fn unwatch(&self, conversation: Uuid) {
        if self.st().agents.remove(&conversation) {
            self.send_agents();
        }
    }

    /// The conversations the old child streams to this LU.
    #[must_use]
    pub fn watched(&self) -> HashSet<Uuid> {
        self.st().agents.clone()
    }

    fn send_agents(&self) {
        if *self.phase.borrow() != Phase::Open {
            // sent once the socket opens
            return;
        }
        let ids: Vec<String> = self.st().agents.iter().map(Uuid::to_string).collect();
        let frame = json!({"type": "session", "message": {"type": "agent.timeline.set_subscription.request",
                                                          "agentIds": ids, "requestId": self.rid()}});
        let _ = self.out.send((frame.to_string(), None));
    }

    fn deliver(&self, rid: &str, answer: Answer) -> bool {
        let tx = self.st().pending.remove(rid);
        match tx {
            Some(tx) => {
                let _ = tx.send(answer);
                true
            }
            None => false,
        }
    }

    fn end(&self, why: LuError) {
        self.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 = Some(Instant::now());
        self.phase.send_replace(Phase::Dead(why));
        let pending: Vec<_> = self.st().pending.drain().collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(if why == LuError::NotSent {
                LuError::NotSent
            } else {
                LuError::Lost
            }));
        }
    }
}

type Stream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Who the LU is: the token holder's newest token, and a view's claims signed with the old key.
fn identity(legacy: &Legacy, conn: &Conn) -> Option<(String, Option<String>)> {
    match &conn.viewer {
        None => legacy.0.tokens.newest(&conn.person).map(|(t, _)| (t, None)),
        Some(v) => {
            let (t, _) = legacy.0.tokens.newest(&v.admin)?;
            let old = legacy.0.tokens.old_token(&crate::viewas::Claims {
                a: v.admin.clone(),
                u: conn.person.clone(),
                s: v.session.clone(),
                e: v.exp,
            });
            Some((t, Some(old)))
        }
    }
}

/// A TCP connection to the old router (the bridge Service) whose unacknowledged bytes are given up after
/// [`LU_TCP_USER_TIMEOUT`].
async fn dial(base: &str) -> std::io::Result<tokio::net::TcpStream> {
    let url = reqwest::Url::parse(base).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let host = url
        .host_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let tcp = tokio::net::TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    socket2::SockRef::from(&tcp).set_tcp_user_timeout(Some(LU_TCP_USER_TIMEOUT))?;
    Ok(tcp)
}

async fn connect(legacy: &Legacy, conn: &Conn) -> Result<Stream, LuError> {
    let Some((token, view)) = identity(legacy, conn) else {
        // no unexpired token of the holder on this replica: the tab outlived its Access token (switch-final C17), so
        // the signed-out answer, never a write error
        legacy.0.metrics.lu_401.inc();
        return Err(LuError::SignedOut);
    };
    let url = format!("{}{OLD_SOCKET}", legacy.0.base.replacen("http://", "ws://", 1));
    let mut req = url.into_client_request().map_err(|_| LuError::NotSent)?;
    let h = req.headers_mut();
    let header = |v: &str| v.parse().map_err(|_| LuError::NotSent);
    h.insert("cf-access-jwt-assertion", header(&token)?);
    h.insert(LU_HEADER, header(&legacy.0.pair)?);
    h.insert("user-agent", header(&legacy.0.user_agent)?);
    if let Some(v) = view {
        h.insert(OLD_VIEW_AS_HEADER, header(&v)?);
    }
    let open = async {
        let tcp = dial(&legacy.0.base)
            .await
            .map_err(tokio_tungstenite::tungstenite::Error::Io)?;
        tokio_tungstenite::client_async(req, tokio_tungstenite::MaybeTlsStream::Plain(tcp)).await
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, open).await {
        Ok(Ok((s, _))) => Ok(s),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => {
            let status = r.status().as_u16();
            tracing::warn!(status, "LU upgrade refused");
            Err(match status {
                401 | 403 => {
                    legacy.0.metrics.lu_401.inc();
                    LuError::SignedOut
                }
                503 => {
                    legacy.0.metrics.lu_503.inc();
                    LuError::Unavailable
                }
                _ => LuError::NotSent,
            })
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "LU not opened");
            Err(LuError::NotSent)
        }
        Err(_) => {
            tracing::warn!("LU upgrade timed out");
            Err(LuError::NotSent)
        }
    }
}

/// The old child's hello: `status` / `server_info`.
async fn hello(stream: &mut Stream, conn: &Conn) -> Result<(), LuError> {
    let hello = json!({"type": "hello", "clientId": format!("lu-{}", conn.id), "clientType": "browser",
                       "protocolVersion": 1, "capabilities": {}, "appVersion": conn.app_version()});
    stream
        .send(Message::Text(hello.to_string().into()))
        .await
        .map_err(|_| LuError::NotSent)?;
    let wait = async {
        while let Some(m) = stream.next().await {
            let Ok(Message::Text(t)) = m else {
                if m.is_err() {
                    return Err(LuError::NotSent);
                }
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&t) else {
                continue;
            };
            if v["type"] == "session" && v["message"]["type"] == "status" {
                return Ok(());
            }
        }
        Err(LuError::NotSent)
    };
    tokio::time::timeout(CONNECT_TIMEOUT, wait)
        .await
        .unwrap_or(Err(LuError::NotSent))
}

async fn run(legacy: Legacy, lu: Arc<Lu>, conn: Arc<Conn>, mut rx: mpsc::UnboundedReceiver<Frame>) {
    let stream = match connect(&legacy, &conn).await {
        Ok(mut s) => match hello(&mut s, &conn).await {
            Ok(()) => s,
            Err(e) => {
                lu.end(e);
                legacy.lu_ended(Some(conn), &lu);
                return;
            }
        },
        Err(e) => {
            lu.end(e);
            legacy.lu_ended(Some(conn), &lu);
            return;
        }
    };
    drop(conn);
    legacy.0.metrics.lu_sockets.inc();
    tracing::info!(conn = %lu.conn_id, "LU open");
    lu.up.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0 = Some(Instant::now());
    lu.phase.send_replace(Phase::Open);
    if !lu.st().agents.is_empty() {
        lu.send_agents();
    }
    let (mut sink, mut stream) = stream.split();
    let hb = legacy.0.heartbeat;
    let mut beat = tokio::time::interval(hb);
    beat.tick().await;
    let mut alive = tokio::time::interval(Duration::from_millis(250));
    let end = loop {
        tokio::select! {
            f = rx.recv() => {
                let Some((f, deadline)) = f else { break End::Closed };
                // the heartbeat period bounds a frame with no caller waiting (the subscription)
                let d = deadline.unwrap_or_else(|| tokio::time::Instant::now() + hb);
                if deadline.is_some() && tokio::time::Instant::now() >= d {
                    // its caller has given up: it must not reach the old child later
                    tracing::warn!(conn = %lu.conn_id, "an LU frame past its bound dropped unsent");
                    continue;
                }
                match tokio::time::timeout_at(d, sink.send(Message::Text(f.into()))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => break End::Closed,
                    // a stalled socket: reset, so the frame's bytes never go out after its bound
                    Err(_) => break End::Stalled,
                }
            }
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => inbound(&legacy, &lu, &t),
                Some(Ok(Message::Close(_)) | Err(_)) | None => break End::Closed,
                Some(Ok(_)) => {}
            },
            _ = beat.tick() => {
                let f = json!({"type": "session", "message": {"type": "client_heartbeat"}});
                match tokio::time::timeout(hb, sink.send(Message::Text(f.to_string().into()))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => break End::Closed,
                    Err(_) => break End::Stalled,
                }
            }
            () = lu.abort.notified() => break End::Stalled,
            _ = alive.tick() => {
                if lu.conn.upgrade().is_none_or(|c| c.is_closing()) {
                    break End::TabGone;
                }
                if lu.idle_now(legacy.0.lu_idle) {
                    break End::Idle;
                }
            }
        }
    };
    let unanswered = !lu.st().pending.is_empty();
    if end == End::Stalled || unanswered {
        // what the kernel holds for the old side is discarded, not delivered after the caller gave up on it
        tracing::warn!(conn = %lu.conn_id, ?end, unanswered, "LU reset");
        reset(sink, stream);
    } else if matches!(end, End::TabGone | End::Idle) {
        let _ = tokio::time::timeout(Duration::from_secs(2), sink.send(Message::Close(None))).await;
    }
    legacy.0.metrics.lu_sockets.dec();
    tracing::info!(conn = %lu.conn_id, ?end, "LU closed");
    lu.end(LuError::Lost);
    legacy.lu_ended(lu.conn.upgrade(), &lu);
}

/// Why the socket loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Its tab went.
    TabGone,
    /// Unused for the idle time.
    Idle,
    /// The old side closed it, or it failed.
    Closed,
    /// A write did not reach the kernel within its bound, or a write call went unanswered past its bound.
    Stalled,
}

/// Ends the connection with a reset (`SO_LINGER` 0): the kernel drops the bytes it has not delivered.
fn reset(sink: futures_util::stream::SplitSink<Stream, Message>, stream: futures_util::stream::SplitStream<Stream>) {
    if let Ok(ws) = sink.reunite(stream)
        && let tokio_tungstenite::MaybeTlsStream::Plain(tcp) = ws.get_ref()
        && let Err(e) = tcp.set_zero_linger()
    {
        tracing::warn!(error = %e, "LU reset not set");
    }
}

/// A frame from the old child: an answer to a call, or a live frame for the tab.
fn inbound(legacy: &Legacy, lu: &Lu, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if v["type"] != "session" {
        return;
    }
    let kind = v["message"]["type"].as_str().unwrap_or("").to_owned();
    let payload = v["message"]["payload"].clone();
    if let Some(rid) = payload.get("requestId").and_then(Value::as_str)
        && lu.deliver(rid, Ok((kind.clone(), payload.clone())))
    {
        return;
    }
    let Some(conn) = lu.conn.upgrade() else { return };
    let Some(agent) = payload
        .get("agentId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return;
    };
    // only while the conversation is legacy: after its release the native feed serves it
    if legacy.is_legacy(&lu.person, &agent) != Some(true) {
        return;
    }
    match kind.as_str() {
        "agent_stream" => {
            if !conn.watches(&agent) {
                return;
            }
            let mut p = payload;
            if let Some(o) = p.as_object_mut() {
                o.insert("subscriptionId".into(), json!(conn.timeline_sub()));
            }
            conn.reply("agent_stream", &p);
        }
        k if k == compat::SANDBOX_STATUS => {
            conn.reply(k, &payload);
        }
        // an ask of the old turn and its end (answered, or the turn stopped): what the native feed sends for an
        // owned conversation (7.5.9), in the same shape, to a tab that watches the conversation or lists it; without
        // the resolution a stopped turn's question stays on the moved tab, and without the request an ask made after
        // the move is never shown (WP-54's ask persona)
        "agent_permission_request" | "agent_permission_resolved" if conn.watches(&agent) || conn.lists() => {
            conn.reply(&kind, &payload);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An LU with no socket task (the state machine alone).
    fn detached() -> Lu {
        Lu {
            conn: Weak::new(),
            conn_id: Uuid::new_v4(),
            person: "staff@example.com".into(),
            out: mpsc::unbounded_channel().0,
            st: Mutex::new(St {
                pending: HashMap::new(),
                agents: HashSet::new(),
                holders: 0,
                last_use: Instant::now(),
                idle: false,
            }),
            phase: watch::channel(Phase::Open).0,
            next: AtomicU64::new(0),
            up: Mutex::new((Some(Instant::now()), None)),
            abort: tokio::sync::Notify::new(),
        }
    }

    /// The idle decision makes the LU dead at once, before its Close goes out: it is not handed out again, takes no
    /// watch and sends no call, so nothing can land on a socket that is closing.
    #[tokio::test]
    async fn an_lu_closed_as_unused_is_dead_from_that_moment() {
        let lu = detached();
        assert!(lu.touch(), "open and in use");
        assert!(!lu.idle_now(Duration::from_secs(60)), "used just now");
        assert!(lu.idle_now(Duration::ZERO));
        assert!(lu.is_dead() && lu.closed_idle());
        assert!(!lu.touch(), "not handed out once idle");
        assert!(!lu.watch(Uuid::new_v4()), "no watch on a closing LU");
        assert!(lu.watched().is_empty());
        assert_eq!(
            lu.call(json!({"type": "fetch_agent_request"}), Duration::from_secs(1))
                .await,
            Err(LuError::NotSent)
        );
    }

    /// A watched conversation, a call in flight or a holder keeps it from closing as unused.
    #[test]
    fn what_keeps_an_lu_in_use() {
        let lu = detached();
        assert!(lu.watch(Uuid::new_v4()));
        assert!(!lu.idle_now(Duration::ZERO), "watched");
        let lu = Arc::new(detached());
        let held = lu.hold();
        assert!(!lu.idle_now(Duration::ZERO), "held");
        drop(held);
        assert!(lu.idle_now(Duration::ZERO));
    }
}
