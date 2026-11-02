//! Serving a fake on a local port. The fakes' own accept loop (hyper HTTP/1.1, one task per connection) gives every
//! request a [`ConnKill`] for its connection, so the `Drop` fault can close the connection without an answer, which
//! `axum::serve` cannot.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tower::Service;

/// Closes the connection a request arrived on (request extension).
#[derive(Clone, Debug, Default)]
pub struct ConnKill(Arc<Notify>);

impl ConnKill {
    /// Close the connection now (the request in flight gets no answer).
    pub fn kill(&self) {
        self.0.notify_one();
    }
}

/// A fake listening on `127.0.0.1`; stops when dropped.
#[derive(Debug)]
pub struct Served {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Served {
    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serve `router` on a fresh port of `127.0.0.1`.
///
/// # Errors
/// When no port can be bound.
pub async fn serve(router: Router) -> std::io::Result<Served> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    serve_on(listener, router)
}

/// Serve `router` on `listener` (a fixed port for a standalone fake).
///
/// # Errors
/// When the listener's address cannot be read.
pub fn serve_on(listener: TcpListener, router: Router) -> std::io::Result<Served> {
    let addr = listener.local_addr()?;
    let task = tokio::spawn(accept(listener, router));
    Ok(Served { addr, task })
}

async fn accept(listener: TcpListener, router: Router) {
    let mut conns = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        conns.spawn(connection(stream, router.clone()));
                    }
                    // EMFILE and the like: back off instead of spinning, and keep serving
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

async fn connection(stream: tokio::net::TcpStream, router: Router) {
    let kill = ConnKill::default();
    let k = kill.clone();
    let svc = hyper::service::service_fn(move |mut req: hyper::Request<Incoming>| {
        req.extensions_mut().insert(k.clone());
        let mut router = router.clone();
        let req: Request = req.map(Body::new);
        async move { Ok::<_, Infallible>(router.call(req).await.unwrap_or_else(|e| match e {})) }
    });
    let conn = hyper::server::conn::http1::Builder::new()
        .keep_alive(true)
        .serve_connection(TokioIo::new(stream), svc);
    tokio::select! {
        _ = conn => {}
        () = kill.0.notified() => {}
    }
}
