//! The client drops an idle pooled connection before sandboxd closes it (sandboxd advertises `keep-alive: timeout=5`).
//!
//! The fake server answers keep-alive and treats a connection idle for 5 s as closed: a request that arrives on it is
//! dropped without an answer, which is what a request written onto a connection sandboxd is closing gets (the close
//! itself is not sent first, so the client cannot see it coming). A call 5.5 s after the previous one must open a new
//! connection and succeed.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sandboxd_api::{Client, POOL_IDLE};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SERVER_IDLE: Duration = Duration::from_secs(5);

async fn connection(mut s: TcpStream) {
    let mut last = Instant::now();
    let mut buf = Vec::new();
    loop {
        let mut chunk = [0u8; 1024];
        let Ok(n) = s.read(&mut chunk).await else { return };
        if n == 0 {
            return;
        }
        if last.elapsed() >= SERVER_IDLE {
            return; // the request raced the close: no answer
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            buf.drain(..end + 4);
            let body = br#"{"sandboxes":[]}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: keep-alive\r\nkeep-alive: timeout=5\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body).await.unwrap();
            last = Instant::now();
        }
    }
}

async fn serve() -> (Arc<AtomicUsize>, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let a = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            a.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(connection(s));
        }
    });
    (accepted, base)
}

#[tokio::test]
async fn a_call_after_sandboxds_idle_close_opens_a_new_connection() {
    assert!(POOL_IDLE < SERVER_IDLE);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (accepted, base) = serve().await;
    let c = Client::plain(&base).unwrap();
    c.list().await.unwrap();
    // within the pool's idle time the connection is reused
    tokio::time::sleep(Duration::from_secs(1)).await;
    c.list().await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    // past sandboxd's keep-alive: a pooled connection would get no answer
    tokio::time::sleep(Duration::from_millis(5_500)).await;
    c.list().await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}
