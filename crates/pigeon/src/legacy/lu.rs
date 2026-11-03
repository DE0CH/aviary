//! One legacy upstream socket (switch-final 4.1, DESIGN 9.1): pigeon's WebSocket to the old router's `/paseo-ws`
//! for one native socket, as its person. Requests go out with a request id of their own and their answer comes
//! back to the caller; the old child's live frames of the legacy conversations the tab watches (`agent_stream`, the
//! sandbox chip) go to the tab with the tab's own subscription id. A `client_heartbeat` every 15 s also keeps the old
//! child from its idle stop. The socket ends with its tab; one that ends while its tab stays is handled by
//! [`Legacy::lu_ended`] (reopened with a replacement per watched conversation, or the tab resumes).

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

use super::{CONNECT_TIMEOUT, LU_HEADER, Legacy, OLD_SOCKET, OLD_VIEW_AS_HEADER};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Open,
    Dead(LuError),
}

struct St {
    pending: HashMap<String, oneshot::Sender<Answer>>,
    agents: HashSet<Uuid>,
}

/// The socket. Its task owns the connection; this is the handle.
pub struct Lu {
    conn: Weak<Conn>,
    conn_id: Uuid,
    person: String,
    out: mpsc::UnboundedSender<String>,
    st: Mutex<St>,
    phase: watch::Sender<Phase>,
    next: AtomicU64,
    /// When it opened, and when it ended.
    up: Mutex<(Option<Instant>, Option<Instant>)>,
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
            }),
            phase: watch::channel(Phase::Connecting).0,
            next: AtomicU64::new(0),
            up: Mutex::new((None, None)),
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
    /// `agent_refreshed` status too).
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
        self.st().pending.insert(rid.clone(), tx);
        let frame = json!({"type": "session", "message": message}).to_string();
        if self.out.send(frame).is_err() {
            self.st().pending.remove(&rid);
            return Err(LuError::NotSent);
        }
        match tokio::time::timeout(bound, rx).await {
            Ok(Ok(a)) => a,
            Ok(Err(_)) => Err(LuError::Lost),
            Err(_) => {
                self.st().pending.remove(&rid);
                Err(LuError::Timeout)
            }
        }
    }

    /// The tab now watches a legacy conversation: the old child streams it.
    pub fn watch(&self, conversation: Uuid) {
        if self.st().agents.insert(conversation) {
            self.send_agents();
        }
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
        let _ = self.out.send(frame.to_string());
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

async fn connect(legacy: &Legacy, conn: &Conn) -> Result<Stream, LuError> {
    let (token, view) = identity(legacy, conn).ok_or(LuError::NotSent)?;
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
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req)).await {
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

async fn run(legacy: Legacy, lu: Arc<Lu>, conn: Arc<Conn>, mut rx: mpsc::UnboundedReceiver<String>) {
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
    let mut beat = tokio::time::interval(legacy.0.heartbeat);
    beat.tick().await;
    let mut alive = tokio::time::interval(Duration::from_millis(250));
    let gone = loop {
        tokio::select! {
            f = rx.recv() => {
                let Some(f) = f else { break false };
                if sink.send(Message::Text(f.into())).await.is_err() {
                    break false;
                }
            }
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => inbound(&legacy, &lu, &t),
                Some(Ok(Message::Close(_)) | Err(_)) | None => break false,
                Some(Ok(_)) => {}
            },
            _ = beat.tick() => {
                let f = json!({"type": "session", "message": {"type": "client_heartbeat"}});
                if sink.send(Message::Text(f.to_string().into())).await.is_err() {
                    break false;
                }
            }
            _ = alive.tick() => {
                if lu.conn.upgrade().is_none_or(|c| c.is_closing()) {
                    break true;
                }
            }
        }
    };
    if gone {
        let _ = tokio::time::timeout(Duration::from_secs(2), sink.send(Message::Close(None))).await;
    }
    legacy.0.metrics.lu_sockets.dec();
    tracing::info!(conn = %lu.conn_id, tab_gone = gone, "LU closed");
    lu.end(LuError::Lost);
    legacy.lu_ended(lu.conn.upgrade(), &lu);
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
        _ => {}
    }
}
