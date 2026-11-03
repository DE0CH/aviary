//! A Paseo socket client for the tests: connects to `/paseo-ws` with an Access token, says hello, sends session
//! messages and waits for the frames it expects. Every frame both ways is recorded, and on drop checked against the
//! frame corpus (`corpus.rs`): a frame of a shape every client has not been checked to accept fails the test.
#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc)] // tests

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Stream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One recorded frame: (case, "in" | "out", frame).
pub type Record = (String, &'static str, Value);

pub struct Ws {
    stream: Stream,
    pending: VecDeque<Value>,
    /// Set when the server closed: (code, reason).
    pub closed: Option<(u16, String)>,
    pub log: Option<(Arc<Mutex<Vec<Record>>>, String)>,
    /// Everything this socket sent and received (frames sent with [`Ws::raw_invalid`] aside).
    seen: Vec<Record>,
    next_id: u64,
    /// Checked against the frame corpus on drop (off for a socket fed through the switch window's legacy upstream:
    /// those frames are the old child's own shapes, relayed).
    check: bool,
}

impl Drop for Ws {
    fn drop(&mut self) {
        if self.check && !std::thread::panicking() {
            super::corpus::check(&self.seen);
        }
    }
}

pub const WAIT: Duration = Duration::from_secs(10);

impl Ws {
    /// Opens `/paseo-ws` (with `query`, e.g. a view-as token) as the holder of `token`; the HTTP status when the
    /// upgrade is refused.
    pub async fn open(base: &str, token: &str, query: &str) -> Result<Self, u16> {
        let url = format!("{}/paseo-ws{query}", base.replacen("http://", "ws://", 1));
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("cf-access-jwt-assertion", token.parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((stream, _)) => Ok(Self {
                stream,
                pending: VecDeque::new(),
                closed: None,
                log: None,
                seen: Vec::new(),
                next_id: 0,
                check: true,
            }),
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
            Err(e) => panic!("connect: {e}"),
        }
    }

    /// Opens and says hello (protocol 1); returns the socket and the `server_info` payload.
    pub async fn hello(base: &str, token: &str, query: &str) -> (Self, Value) {
        let mut ws = Self::open(base, token, query).await.unwrap();
        ws.raw(
            json!({"type": "hello", "clientId": "cid-test", "clientType": "browser", "protocolVersion": 1,
                      "capabilities": {}, "appVersion": "0.13.2"}),
        )
        .await;
        let info = ws.next_session("status").await;
        (ws, info)
    }

    /// No corpus check on drop: a socket served through the legacy upstream, whose frames are the old child's.
    #[must_use]
    pub fn relayed(mut self) -> Self {
        self.check = false;
        self
    }

    pub fn record(mut self, log: &Arc<Mutex<Vec<Record>>>, case: &str) -> Self {
        self.log = Some((log.clone(), case.to_owned()));
        self
    }

    pub fn set_case(&mut self, case: &str) {
        if let Some((_, c)) = &mut self.log {
            case.clone_into(c);
        }
    }

    fn keep(&mut self, dir: &'static str, v: &Value) {
        let case = self.log.as_ref().map_or_else(String::new, |(_, c)| c.clone());
        if let Some((log, _)) = &self.log {
            log.lock().unwrap().push((case.clone(), dir, v.clone()));
        }
        // a large frame's shape is kept without its text (the corpus cuts long texts anyway)
        self.seen.push((case, dir, super::corpus::normalise(v)));
    }

    /// Sends a top-level frame that is deliberately not a valid client message (it is not recorded).
    pub async fn raw_invalid(&mut self, v: Value) {
        self.stream.send(Message::Text(v.to_string().into())).await.unwrap();
    }

    /// Sends a session message that is deliberately not valid for the clients' schema (an unknown type, a field
    /// missing): not recorded.
    pub async fn session_invalid(&mut self, message: Value) {
        self.raw_invalid(json!({"type": "session", "message": message})).await;
    }

    /// [`Ws::rpc`] with a request no client sends (an unknown type): not recorded.
    pub async fn rpc_invalid(&mut self, kind: &str, mut message: Value) -> (String, Value) {
        let rid = self.rid();
        message["type"] = json!(kind);
        message["requestId"] = json!(rid);
        self.session_invalid(message).await;
        self.answer(kind, rid).await
    }

    /// Sends a top-level frame.
    pub async fn raw(&mut self, v: Value) {
        self.keep("in", &v);
        self.stream.send(Message::Text(v.to_string().into())).await.unwrap();
    }

    /// Writes raw bytes to the socket's TCP stream (hand-built frames: a header only, a message in slow chunks).
    pub async fn raw_bytes(&mut self, b: &[u8]) {
        use tokio::io::AsyncWriteExt;
        match self.stream.get_mut() {
            tokio_tungstenite::MaybeTlsStream::Plain(s) => {
                s.write_all(b).await.unwrap();
                s.flush().await.unwrap();
            }
            _ => panic!("not a plain TCP stream"),
        }
    }

    /// The header of a masked (all-zero key: the payload goes as is) final text frame of `len` bytes.
    pub fn text_header(len: u64) -> Vec<u8> {
        let mut h = vec![0x81u8];
        if len < 126 {
            h.push(0x80 | u8::try_from(len).unwrap());
        } else if len <= 0xffff {
            h.push(0x80 | 0x7e);
            h.extend_from_slice(&u16::try_from(len).unwrap().to_be_bytes());
        } else {
            h.push(0x80 | 0x7f);
            h.extend_from_slice(&len.to_be_bytes());
        }
        h.extend_from_slice(&[0, 0, 0, 0]);
        h
    }

    /// Sends a text frame as is.
    pub async fn text(&mut self, s: &str) {
        self.stream.send(Message::Text(s.to_owned().into())).await.unwrap();
    }

    /// Sends a session message.
    pub async fn session(&mut self, message: Value) {
        self.raw(json!({"type": "session", "message": message})).await;
    }

    /// A fresh request id.
    pub fn rid(&mut self) -> String {
        self.next_id += 1;
        format!("req-{}", self.next_id)
    }

    /// The next frame from the server (pings are answered by tungstenite and skipped); `None` once closed.
    pub async fn next_raw(&mut self, wait: Duration) -> Option<Value> {
        if let Some(v) = self.pending.pop_front() {
            return Some(v);
        }
        self.read(wait).await
    }

    async fn read(&mut self, wait: Duration) -> Option<Value> {
        // one deadline for the whole read: pings (answered and skipped) do not extend it
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let m = tokio::time::timeout_at(deadline, self.stream.next()).await.ok()??;
            match m {
                Ok(Message::Text(t)) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap();
                    self.keep("out", &v);
                    return Some(v);
                }
                Ok(Message::Close(c)) => {
                    self.closed = Some(c.map_or((1005, String::new()), |c| (c.code.into(), c.reason.to_string())));
                    return None;
                }
                Ok(_) => {}
                Err(_) => {
                    self.closed.get_or_insert((1006, String::new()));
                    return None;
                }
            }
        }
    }

    /// Waits for the first frame matching `pred`, keeping the others for later reads.
    pub async fn wait_for(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let mut skipped = VecDeque::new();
        // look at what is already pending first
        while let Some(v) = self.pending.pop_front() {
            if pred(&v) {
                self.pending = skipped.into_iter().chain(self.pending.drain(..)).collect();
                return v;
            }
            skipped.push_back(v);
        }
        loop {
            let Some(v) = self.read(WAIT).await else {
                panic!("no {what}: closed {:?}; skipped {skipped:?}", self.closed);
            };
            if pred(&v) {
                self.pending = skipped;
                return v;
            }
            skipped.push_back(v);
        }
    }

    /// The payload of the next session message of `kind`.
    pub async fn next_session(&mut self, kind: &str) -> Value {
        let k = kind.to_owned();
        let v = self
            .wait_for(kind, move |v| {
                v["type"] == "session" && v["message"]["type"] == k.as_str()
            })
            .await;
        v["message"]["payload"].clone()
    }

    /// Sends a request and returns the payload of the session message answering it (by `requestId`).
    pub async fn rpc(&mut self, kind: &str, mut message: Value) -> (String, Value) {
        let rid = self.rid();
        message["type"] = json!(kind);
        message["requestId"] = json!(rid);
        self.session(message).await;
        self.answer(kind, rid).await
    }

    async fn answer(&mut self, kind: &str, rid: String) -> (String, Value) {
        let r = rid.clone();
        let v = self
            .wait_for(kind, move |v| {
                v["type"] == "session" && v["message"]["payload"]["requestId"] == r.as_str()
            })
            .await;
        (
            v["message"]["type"].as_str().unwrap().to_owned(),
            v["message"]["payload"].clone(),
        )
    }

    /// Frames that arrive within `wait` (none expected: asserts in the caller).
    pub async fn drain_for(&mut self, wait: Duration) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending.drain(..).collect();
        while let Some(v) = self.read(wait).await {
            out.push(v);
        }
        out
    }

    /// Waits for the server to close the socket; its close code.
    pub async fn closed_code(&mut self, wait: Duration) -> Option<u16> {
        let deadline = tokio::time::Instant::now() + wait;
        while self.closed.is_none() {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            if self.read(left).await.is_none() && self.closed.is_none() {
                return None;
            }
        }
        self.closed.as_ref().map(|c| c.0)
    }

    /// Closes from the client side.
    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }

    /// The frames this socket recorded.
    pub fn seen(&self) -> &[Record] {
        &self.seen
    }
}
