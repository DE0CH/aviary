//! A Paseo socket client: `/paseo-ws` with an Access token, the hello, session requests answered by `requestId`, and
//! the pushes kept in arrival order for the scenario to read. Every frame both ways is kept (the frame log of 7.8's
//! artifacts, printed when a scenario fails).

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context as _, bail};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

type Stream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// How long a request waits for its answer (the app's RPC timeout is 60 s).
pub const RPC_WAIT: Duration = Duration::from_secs(60);

/// One socket.
pub struct Ws {
    stream: Stream,
    pending: VecDeque<Value>,
    /// Every frame: ("in" | "out", frame).
    pub log: Vec<(&'static str, Value)>,
    next_id: u64,
    pub closed: Option<u16>,
}

impl std::fmt::Debug for Ws {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ws")
            .field("frames", &self.log.len())
            .finish_non_exhaustive()
    }
}

impl Ws {
    /// Opens `<base>/paseo-ws` and says hello; the socket and the `server_info` payload.
    pub async fn open(base: &str, token: &str) -> anyhow::Result<(Self, Value)> {
        Self::open_query(base, token, "").await
    }

    /// [`Ws::open`] with a query on the socket's URL (a view-as token: `?jarvis_view_as=…`).
    pub async fn open_query(base: &str, token: &str, query: &str) -> anyhow::Result<(Self, Value)> {
        let url = format!("{}/paseo-ws{query}", base.replacen("http://", "ws://", 1));
        let mut req = url.into_client_request()?;
        req.headers_mut().insert("cf-access-token", token.parse()?);
        let (stream, _) = tokio_tungstenite::connect_async(req).await.context("socket upgrade")?;
        let mut ws = Self {
            stream,
            pending: VecDeque::new(),
            log: Vec::new(),
            next_id: 0,
            closed: None,
        };
        ws.send(
            json!({"type": "hello", "clientId": format!("e2e-{}", uuid::Uuid::new_v4()),
            "clientType": "browser", "protocolVersion": 1, "capabilities": {}, "appVersion": "0.13.2"}),
        )
        .await?;
        let info = ws
            .wait_for("server_info", RPC_WAIT, |v| {
                v["type"] == "session"
                    && v["message"]["type"] == "status"
                    && v["message"]["payload"]["status"] == "server_info"
            })
            .await?;
        Ok((ws, info["message"]["payload"].clone()))
    }

    /// Sends a top-level frame.
    pub async fn send(&mut self, v: Value) -> anyhow::Result<()> {
        self.log.push(("in", v.clone()));
        self.stream.send(Message::Text(v.to_string().into())).await?;
        Ok(())
    }

    async fn read(&mut self, wait: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let m = tokio::time::timeout_at(deadline, self.stream.next()).await.ok()??;
            match m {
                Ok(Message::Text(t)) => {
                    let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
                        continue;
                    };
                    self.log.push(("out", v.clone()));
                    return Some(v);
                }
                Ok(Message::Close(c)) => {
                    self.closed = Some(c.map_or(1005, |c| c.code.into()));
                    return None;
                }
                Ok(_) => {}
                Err(_) => {
                    self.closed.get_or_insert(1006);
                    return None;
                }
            }
        }
    }

    /// The first frame matching `pred` within `wait`; the others stay for later reads.
    pub async fn wait_for(
        &mut self,
        what: &str,
        wait: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> anyhow::Result<Value> {
        if let Some(i) = self.pending.iter().position(&pred) {
            return Ok(self.pending.remove(i).unwrap_or_default());
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Some(v) = self.read(left).await else {
                bail!("no {what} within {wait:?} (closed: {:?})", self.closed);
            };
            if pred(&v) {
                return Ok(v);
            }
            self.pending.push_back(v);
        }
    }

    /// Everything that arrived within `wait`, the kept frames first.
    pub async fn drain(&mut self, wait: Duration) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending.drain(..).collect();
        while let Some(v) = self.read(wait).await {
            out.push(v);
        }
        out
    }

    /// Sends a session request and returns (answer type, payload).
    pub async fn rpc(&mut self, kind: &str, mut message: Value) -> anyhow::Result<(String, Value)> {
        self.next_id += 1;
        let rid = format!("e2e-{}", self.next_id);
        message["type"] = json!(kind);
        message["requestId"] = json!(rid);
        self.send(json!({"type": "session", "message": message})).await?;
        let v = self
            .wait_for(kind, RPC_WAIT, |v| {
                v["type"] == "session" && v["message"]["payload"]["requestId"] == rid.as_str()
            })
            .await?;
        Ok((
            v["message"]["type"].as_str().unwrap_or_default().to_owned(),
            v["message"]["payload"].clone(),
        ))
    }

    /// The last `n` frames, one per line (for a failure's report).
    #[must_use]
    pub fn tail(&self, n: usize) -> String {
        let mut s = String::new();
        for (dir, v) in self.log.iter().rev().take(n).rev() {
            let mut t = v.to_string();
            if t.len() > 600 {
                t.truncate(600);
                t.push('…');
            }
            s.push_str(dir);
            s.push(' ');
            s.push_str(&t);
            s.push('\n');
        }
        s
    }

    /// Closes the socket.
    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }
}
