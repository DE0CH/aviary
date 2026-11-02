//! A sandbox client of a gateway's proxy, one TCP connection at a time, so a check controls exactly which requests
//! share a kept-alive connection and sees when the gateway closes it. Requests go out in absolute form
//! (`GET http://magpie/v1/sandbox/x`), as every sandbox client sends them (platform 3.11.1).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1;
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::sync::watch;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = BoxBody<Bytes, BoxError>;

/// An empty request body.
#[must_use]
pub fn empty() -> Body {
    Empty::new().map_err(|e| match e {}).boxed()
}

/// A request body of these bytes.
#[must_use]
pub fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).map_err(|e| match e {}).boxed()
}

/// One connection to the proxy.
#[derive(Debug)]
pub struct Conn {
    sender: http1::SendRequest<Body>,
    closed: watch::Receiver<bool>,
}

impl Conn {
    /// Connects to the proxy.
    ///
    /// # Errors
    /// When the TCP connection or the HTTP/1.1 handshake fails.
    pub async fn open(addr: SocketAddr) -> Result<Self, String> {
        let stream = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| format!("connect to {addr} timed out"))?
            .map_err(|e| format!("connect to {addr}: {e}"))?;
        let _ = stream.set_nodelay(true);
        let (sender, conn) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| format!("handshake: {e}"))?;
        let (tx, closed) = watch::channel(false);
        tokio::spawn(async move {
            let _ = conn.await;
            let _ = tx.send(true);
        });
        Ok(Self { sender, closed })
    }

    /// Sends one request and waits for the response head.
    ///
    /// # Errors
    /// When the connection is gone or fails.
    pub async fn send(&mut self, req: Request<Body>) -> Result<Response<Incoming>, String> {
        self.sender
            .ready()
            .await
            .map_err(|e| format!("connection not usable: {e}"))?;
        self.sender
            .send_request(req)
            .await
            .map_err(|e| format!("request failed: {e}"))
    }

    /// Whether the gateway has closed the connection.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow() || self.sender.is_closed()
    }

    /// Waits up to `timeout` for the connection to close; true if it did.
    pub async fn wait_closed(&mut self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.closed.wait_for(|c| *c))
            .await
            .is_ok()
            || self.sender.is_closed()
    }
}

/// A proxy-form request: `url` is absolute (`http://magpie/v1/sandbox/x`); `credential` is the full
/// `Proxy-Authorization` value, if any.
///
/// # Panics
/// Never for the absolute URLs and header names the suites build (a programming error otherwise).
#[must_use]
pub fn request(
    method: &str,
    url: &str,
    credential: Option<&str>,
    headers: &[(&str, &str)],
    body: Body,
) -> Request<Body> {
    let uri: hyper::Uri = url.parse().unwrap_or_else(|e| panic!("{url}: {e}"));
    let host = uri.authority().map(|a| a.as_str().to_owned()).unwrap_or_default();
    let mut req = Request::new(body);
    *req.method_mut() = method.parse().unwrap_or_else(|e| panic!("{method}: {e}"));
    *req.uri_mut() = uri;
    let h = req.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&host) {
        h.insert(header::HOST, v);
    }
    if let Some(c) = credential
        && let Ok(v) = HeaderValue::from_str(c)
    {
        h.insert(header::PROXY_AUTHORIZATION, v);
    }
    for (k, v) in headers {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            h.append(k, v);
        }
    }
    req
}

/// A `CONNECT host:port` request.
///
/// # Panics
/// As [`request`].
#[must_use]
pub fn connect(authority: &str, credential: Option<&str>) -> Request<Body> {
    let mut req = Request::new(empty());
    *req.method_mut() = hyper::Method::CONNECT;
    *req.uri_mut() = authority.parse().unwrap_or_else(|e| panic!("{authority}: {e}"));
    if let Ok(v) = HeaderValue::from_str(authority) {
        req.headers_mut().insert(header::HOST, v);
    }
    if let Some(c) = credential
        && let Ok(v) = HeaderValue::from_str(c)
    {
        req.headers_mut().insert(header::PROXY_AUTHORIZATION, v);
    }
    req
}

/// A whole response body, read frame by frame, with the time the first data byte arrived.
#[derive(Debug)]
pub struct Read {
    pub bytes: Vec<u8>,
    pub first_byte: Option<Instant>,
    /// The body ended cleanly (not cut).
    pub complete: bool,
    pub error: Option<String>,
}

/// Reads a response body to its end (or `timeout`).
pub async fn read_body(body: Incoming, timeout: Duration) -> Read {
    let mut body = body;
    let mut out = Read {
        bytes: Vec::new(),
        first_byte: None,
        complete: false,
        error: None,
    };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, body.frame()).await {
            Err(_) => {
                out.error = Some("timed out".into());
                return out;
            }
            Ok(None) => {
                out.complete = true;
                return out;
            }
            Ok(Some(Err(e))) => {
                out.error = Some(e.to_string());
                return out;
            }
            Ok(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref()
                    && !d.is_empty()
                {
                    out.first_byte.get_or_insert_with(Instant::now);
                    out.bytes.extend_from_slice(d);
                }
            }
        }
    }
}

/// Resolves `http://host:port` (or `host:port`) to one socket address.
///
/// # Errors
/// When the name does not resolve.
pub async fn resolve(url: &str) -> Result<SocketAddr, String> {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    let with_port = if authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    tokio::net::lookup_host(&with_port)
        .await
        .map_err(|e| format!("{with_port}: {e}"))?
        .next()
        .ok_or_else(|| format!("{with_port}: no address"))
}
