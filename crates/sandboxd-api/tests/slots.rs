//! The client's per-host slots: long calls never make the heartbeat wait, and a call's timeout covers its wait for a
//! slot.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)
#![allow(clippy::unused_async_trait_impl)] // the fake answers at once; the trait is async

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::fixture;
use sandboxd_api::{
    Answer, ApiError, Client, Deleted, EgressTargets, EnsureImage, Error, HEARTBEAT_SLOTS, HOST_TIMEOUT, Host,
    ImageDigest, Listed, MAX_CONNECTIONS, Peers, ReceiveResult, Receiving, Sandboxes, SendRequest, SendResult, Server,
    SetPeers, Spec, router,
};
use tokio::sync::Notify;

/// Creates and image pulls hang until `release`; the heartbeat answers at once, or hangs when `hang_heartbeat`.
#[derive(Default)]
struct Slow {
    release: Notify,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    hang_heartbeat: bool,
}

impl Slow {
    async fn hold(&self) {
        let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        self.release.notified().await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

fn no() -> ApiError {
    ApiError::new(500, "not in this test")
}

impl Server for Slow {
    async fn host(&self) -> Answer<Host> {
        if self.hang_heartbeat {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        Ok(serde_json::from_str(&fixture("host.json")).unwrap())
    }
    async fn set_peers(&self, _: SetPeers) -> Answer<Peers> {
        Err(no())
    }
    async fn set_egress(&self, _: EgressTargets) -> Answer<EgressTargets> {
        Err(no())
    }
    async fn ensure_image(&self, _: EnsureImage) -> Answer<ImageDigest> {
        self.hold().await;
        Ok(ImageDigest { digest: "d".into() })
    }
    async fn list(&self) -> Answer<Sandboxes> {
        Ok(Sandboxes { sandboxes: vec![] })
    }
    async fn create(&self, _: Spec) -> Answer<Listed> {
        self.hold().await;
        Ok(serde_json::from_str(&fixture("listed.json")).unwrap())
    }
    async fn delete(&self, name: String) -> Answer<Deleted> {
        Ok(Deleted { deleted: name })
    }
    async fn spec(&self, _: String) -> Answer<Spec> {
        Err(no())
    }
    async fn receive(&self, _: Spec) -> Answer<Receiving> {
        Err(no())
    }
    async fn receive_result(&self, _: String, _: u64) -> Answer<ReceiveResult> {
        Err(no())
    }
    async fn send(&self, _: String, _: SendRequest) -> Answer<SendResult> {
        Err(no())
    }
}

async fn serve(slow: Slow) -> (Arc<Slow>, Client) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let s = Arc::new(slow);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = router(s.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (s, Client::plain(&base).unwrap())
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn heartbeat_answers_while_every_other_slot_is_held() {
    let (s, c) = serve(Slow::default()).await;
    let spec: Spec = serde_json::from_str(&fixture("spec.json")).unwrap();
    let long = MAX_CONNECTIONS - HEARTBEAT_SLOTS;
    // every shared slot held by a long call, and two more calls queued behind them
    let mut calls = Vec::new();
    for i in 0..long + 2 {
        let (c, spec) = (c.clone(), spec.clone());
        calls.push(tokio::spawn(async move {
            if i % 2 == 0 {
                c.create(&spec).await.map(|_| ())
            } else {
                c.ensure_image(&EnsureImage::default()).await.map(|_| ())
            }
        }));
    }
    until("the long calls reach the server", || {
        s.in_flight.load(Ordering::SeqCst) == long
    })
    .await;
    for _ in 0..3 {
        let t = Instant::now();
        let h = tokio::time::timeout(HOST_TIMEOUT, c.host()).await;
        assert!(h.is_ok_and(|h| h.is_ok()), "heartbeat within {HOST_TIMEOUT:?}");
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }
    // the queued calls never got past the client: at most MAX_CONNECTIONS requests at once
    assert_eq!(s.peak.load(Ordering::SeqCst), long);
    while calls.iter().any(|c| !c.is_finished()) {
        s.release.notify_waiters();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for c in calls {
        c.await.unwrap().unwrap();
    }
    assert_eq!(s.peak.load(Ordering::SeqCst), long);
}

#[tokio::test]
async fn a_hung_heartbeat_fails_within_its_timeout() {
    let (_, c) = serve(Slow {
        hang_heartbeat: true,
        ..Slow::default()
    })
    .await;
    // the first holds the heartbeat slot until it times out; the second waits for the slot, and that wait counts
    let start = Instant::now();
    let (first, second) = tokio::join!(c.host(), c.host());
    let took = start.elapsed();
    assert!(
        matches!(first, Err(Error::Timeout(after)) if after == HOST_TIMEOUT),
        "{first:?}"
    );
    assert!(
        matches!(second, Err(Error::Timeout(after)) if after == HOST_TIMEOUT),
        "{second:?}"
    );
    assert!(took < HOST_TIMEOUT + Duration::from_secs(1), "{took:?}");
}
