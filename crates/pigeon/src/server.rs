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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

/// The bytes read so far from the connection a request came on (a request extension): a socket upgraded on it keeps
/// counting, so the drain sees a large message arriving before its last byte.
#[derive(Debug, Clone, Default)]
pub struct InboundBytes(pub Arc<AtomicU64>);

/// A connection's IO that counts the bytes read.
struct Counted<T> {
    io: T,
    read: Arc<AtomicU64>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.io).poll_read(cx, buf);
        let n = buf.filled().len().saturating_sub(before);
        if n > 0 {
            self.read.fetch_add(n as u64, Ordering::SeqCst);
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
        read: inbound.0.clone(),
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
