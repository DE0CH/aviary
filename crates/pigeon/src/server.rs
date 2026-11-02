//! The 8080 listener (spec 5.13, 11.2): HTTP/1.1 with upgrades, keep-alive connections held up to 120 s idle, every
//! connection tracked so the drain can close the listener, let in-flight requests finish (`connection: close` on
//! their answers) and wait for transfers up to the transfer bound.
//!
//! hyper 1.12 runs its header-read timer whenever a connection waits for the next request head, idle keep-alive
//! included, so the two limits of spec 5.13 (30 s to read headers, 120 s idle) cannot be set apart: pigeon sets one
//! 120 s bound, the one that matters (cloudflared reuses a pooled connection for up to 90 s and must never find it
//! closed under it). A recorded deviation.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};
use tower::Service as _;

/// The idle keep-alive and header-read bound (spec 5.13; see the module docs).
pub const IDLE: Duration = Duration::from_secs(120);

/// Transfers in flight (uploads and downloads); the drain waits for them. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Transfers(Arc<TransfersInner>);

#[derive(Debug, Default)]
struct TransfersInner {
    active: AtomicUsize,
    idle: Notify,
}

/// Held while a transfer runs.
#[derive(Debug)]
pub struct TransferGuard(Transfers);

impl Drop for TransferGuard {
    fn drop(&mut self) {
        if self.0.0.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.0.idle.notify_waiters();
        }
    }
}

impl Transfers {
    /// Counts a transfer until the guard drops.
    #[must_use]
    pub fn guard(&self) -> TransferGuard {
        self.0.active.fetch_add(1, Ordering::SeqCst);
        TransferGuard(self.clone())
    }

    /// Transfers running now.
    #[must_use]
    pub fn active(&self) -> usize {
        self.0.active.load(Ordering::SeqCst)
    }

    /// Resolves when no transfer runs.
    pub async fn idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// The WebSocket frames arriving on the connection a request came on (a request extension). The listener reads every
/// byte through [`Counted`], which, once the socket upgrade has [started](Self::start) the tracking, follows the
/// client's frame headers (opcode, FIN, payload length): so the drain knows a message is arriving from its first
/// bytes on, before tungstenite has the whole message (spec 11.2 step 3: a deploy never cuts a send in the middle).
#[derive(Debug, Clone, Default)]
pub struct InboundBytes(pub Arc<std::sync::Mutex<WsFrames>>);

impl InboundBytes {
    fn lock(&self) -> std::sync::MutexGuard<'_, WsFrames> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// From now on the bytes read are WebSocket frames (called by the upgrade: the request head has been read and a
    /// client sends no frame before the 101).
    pub fn start(&self) {
        self.lock().on = true;
    }

    /// Data messages whose last byte has been read.
    #[must_use]
    pub fn complete(&self) -> u64 {
        self.lock().complete
    }

    /// Whether a data message has begun and its last byte has not been read yet.
    #[must_use]
    pub fn open(&self) -> bool {
        self.lock().open
    }

    /// Bytes of frames read so far (for tests and logs).
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.lock().bytes
    }
}

/// A parser of the client's frame headers (RFC 6455 5.2) that skips the payloads: it counts whole data messages and
/// says whether one is part-way in. Control frames (ping, pong, close) may come between a message's fragments; they
/// count as nothing.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // a parser's state flags
pub struct WsFrames {
    on: bool,
    hdr: [u8; 14],
    hdr_len: usize,
    left: u64,
    data: bool,
    fin: bool,
    open: bool,
    complete: u64,
    bytes: u64,
}

impl WsFrames {
    /// Feeds bytes read from the connection.
    pub fn feed(&mut self, mut b: &[u8]) {
        if !self.on {
            return;
        }
        self.bytes += b.len() as u64;
        while !b.is_empty() {
            if self.left > 0 {
                let n = usize::try_from(self.left).unwrap_or(usize::MAX).min(b.len());
                self.left -= n as u64;
                b = &b[n..];
                if self.left == 0 {
                    self.frame_done();
                }
                continue;
            }
            self.hdr[self.hdr_len] = b[0];
            self.hdr_len += 1;
            b = &b[1..];
            if self.hdr_len < 2 {
                continue;
            }
            let len7 = self.hdr[1] & 0x7f;
            let ext = match len7 {
                126 => 2,
                127 => 8,
                _ => 0,
            };
            let mask = if self.hdr[1] & 0x80 == 0 { 0 } else { 4 };
            if self.hdr_len < 2 + ext + mask {
                continue;
            }
            let len = match ext {
                2 => u64::from(u16::from_be_bytes([self.hdr[2], self.hdr[3]])),
                8 => u64::from_be_bytes([
                    self.hdr[2],
                    self.hdr[3],
                    self.hdr[4],
                    self.hdr[5],
                    self.hdr[6],
                    self.hdr[7],
                    self.hdr[8],
                    self.hdr[9],
                ]),
                _ => u64::from(len7),
            };
            let opcode = self.hdr[0] & 0x0f;
            self.fin = self.hdr[0] & 0x80 != 0;
            self.data = opcode < 8;
            if self.data {
                self.open = true;
            }
            self.hdr_len = 0;
            self.left = len;
            if len == 0 {
                self.frame_done();
            }
        }
    }

    fn frame_done(&mut self) {
        if self.data && self.fin {
            self.open = false;
            self.complete += 1;
        }
    }
}

/// A connection's IO that feeds the bytes read to its [`InboundBytes`].
struct Counted<T> {
    io: T,
    read: InboundBytes,
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.io).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.read.lock().feed(&buf.filled()[before..]);
        }
        r
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

/// The running listener: stop it with [`Listener::drain`].
#[derive(Debug)]
pub struct Listener {
    stop: watch::Sender<bool>,
    done: tokio::task::JoinHandle<()>,
    open: Arc<AtomicUsize>,
}

impl Listener {
    /// Connections open now.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }

    /// Spec 11.2 steps 2 and 4: closes the listener (stragglers connect to another replica), asks every connection
    /// to finish (idle ones close at once, busy ones answer with `connection: close`) and waits for them, at most
    /// `bound`.
    pub async fn drain(self, bound: Duration) {
        let started = Instant::now();
        let _ = self.stop.send(true);
        if tokio::time::timeout(bound, self.done).await.is_err() {
            tracing::warn!(
                connections = self.open.load(Ordering::SeqCst),
                "drain: connections still open at the transfer bound"
            );
        }
        tracing::info!(
            ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "listener drained"
        );
    }
}

/// Serves `router` on `listener` until [`Listener::drain`].
#[must_use]
pub fn serve(listener: TcpListener, router: Router) -> Listener {
    let (stop, mut stopped) = watch::channel(false);
    let open = Arc::new(AtomicUsize::new(0));
    let open2 = open.clone();
    let done = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let _ = stream.set_nodelay(true);
                        let router = router.clone();
                        let stop = stopped.clone();
                        let open = open2.clone();
                        open.fetch_add(1, Ordering::SeqCst);
                        conns.spawn(async move {
                            connection(stream, router, stop).await;
                            open.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    // EMFILE and the like: back off and keep serving
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                },
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() {
                        break;
                    }
                }
            }
        }
        // the listener closes here: new connections go to another replica
        drop(listener);
        while conns.join_next().await.is_some() {}
    });
    Listener { stop, done, open }
}

async fn connection(stream: tokio::net::TcpStream, router: Router, mut stop: watch::Receiver<bool>) {
    let inbound = InboundBytes::default();
    let io = Counted {
        io: stream,
        read: inbound.clone(),
    };
    let svc = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
        let mut router = router.clone();
        let mut req: Request = req.map(Body::new);
        req.extensions_mut().insert(inbound.clone());
        async move { Ok::<_, Infallible>(router.call(req).await.unwrap_or_else(|e| match e {})) }
    });
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(IDLE)
        .keep_alive(true)
        .serve_connection(TokioIo::new(io), svc)
        .with_upgrades();
    tokio::pin!(conn);
    let mut stopping = *stop.borrow();
    if stopping {
        conn.as_mut().graceful_shutdown();
    }
    loop {
        tokio::select! {
            r = conn.as_mut() => {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "connection ended");
                }
                break;
            }
            changed = stop.changed(), if !stopping => {
                if changed.is_err() || *stop.borrow() {
                    stopping = true;
                    conn.as_mut().graceful_shutdown();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WsFrames;

    fn header(fin: bool, opcode: u8, len: u64) -> Vec<u8> {
        let mut h = vec![if fin { 0x80 } else { 0 } | opcode];
        if len < 126 {
            h.push(0x80 | u8::try_from(len).unwrap_or(0));
        } else if len <= 0xffff {
            h.push(0x80 | 0x7e);
            h.extend_from_slice(&u16::try_from(len).unwrap_or(0).to_be_bytes());
        } else {
            h.push(0x80 | 0x7f);
            h.extend_from_slice(&len.to_be_bytes());
        }
        h.extend_from_slice(&[1, 2, 3, 4]);
        h
    }

    fn on() -> WsFrames {
        WsFrames {
            on: true,
            ..WsFrames::default()
        }
    }

    #[test]
    fn a_message_is_open_from_its_first_header_byte_to_its_last_payload_byte() {
        let mut f = on();
        let h = header(true, 1, 70_000);
        f.feed(&h[..1]);
        assert!(!f.open, "one byte is not a header yet");
        f.feed(&h[1..]);
        assert!(f.open);
        f.feed(&vec![0u8; 69_999]);
        assert!(f.open);
        assert_eq!(f.complete, 0);
        f.feed(&[0u8; 1]);
        assert!(!f.open);
        assert_eq!(f.complete, 1);
    }

    #[test]
    fn small_messages_in_one_read_count_one_each_and_pings_count_nothing() {
        let mut f = on();
        let mut b = Vec::new();
        for _ in 0..3 {
            b.extend(header(true, 1, 5));
            b.extend_from_slice(b"hello");
        }
        b.extend(header(true, 0xA, 0)); // a pong
        b.extend(header(true, 0x9, 2)); // a ping
        b.extend_from_slice(b"hi");
        f.feed(&b);
        assert_eq!(f.complete, 3);
        assert!(!f.open);
    }

    #[test]
    fn a_fragmented_message_with_a_control_frame_between_its_fragments_stays_open_until_its_final_fragment() {
        let mut f = on();
        let mut b = header(false, 1, 3);
        b.extend_from_slice(b"abc");
        b.extend(header(true, 0xA, 0));
        f.feed(&b);
        assert!(f.open);
        let mut b = header(true, 0, 2);
        b.extend_from_slice(b"de");
        f.feed(&b);
        assert!(!f.open);
        assert_eq!(f.complete, 1);
    }

    #[test]
    fn bytes_before_the_upgrade_are_not_frames() {
        let mut f = WsFrames::default();
        f.feed(b"GET /paseo-ws HTTP/1.1\r\n\r\n");
        assert!(!f.open);
        assert_eq!(f.bytes, 0);
    }
}
