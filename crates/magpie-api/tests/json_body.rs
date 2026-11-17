//! A JSON body is at most [`magpie_api::server::JSON_BODY_MAX`]: it is read whole and parsed outside the service's
//! memory budget for buffered bodies, so the 64 MiB the service's router admits (for the raw-body routes) must not
//! apply to it. Before, eight sandboxes sending 64 MiB of JSON at once could hold a replica's whole gibibyte.
#![allow(clippy::unwrap_used)] // tests

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aviary_core::http::ApiError;
use magpie_api::live::LiveFramesRequest;
use magpie_api::server::{Ctx, JSON_BODY_MAX, Server, router};
use uuid::Uuid;

#[derive(Default)]
struct Counting {
    live: AtomicUsize,
}

impl Server for Counting {
    async fn live_frames(&self, _: Ctx, _: Uuid, _: LiveFramesRequest) -> Result<(), ApiError> {
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn serve(s: Arc<Counting>) -> String {
    // the service's own limit, as magpie sets it: 64 MiB for every route
    let app = aviary_core::http::standard(router(s), 64 << 20);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// `{"frames":[]}` padded with spaces to `len` bytes.
fn frames_body(len: usize) -> Vec<u8> {
    let mut b = br#"{"frames":[]"#.to_vec();
    b.resize(len - 1, b' ');
    b.push(b'}');
    b
}

#[tokio::test]
async fn a_json_body_over_the_limit_is_refused_and_a_raw_body_is_not() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let s = Arc::new(Counting::default());
    let base = serve(s.clone()).await;
    let http = reqwest::Client::new();
    let live = format!("{base}/v1/sandbox/turns/{}/live", Uuid::nil());

    // at the limit: taken
    let r = http.post(&live).body(frames_body(JSON_BODY_MAX)).send().await.unwrap();
    assert_eq!(r.status(), 204, "{}", r.text().await.unwrap());
    assert_eq!(s.live.load(Ordering::SeqCst), 1);

    // one byte over, with its length declared: 413 `too_large`, the handler never runs
    let r = http.post(&live).body(frames_body(JSON_BODY_MAX + 1)).send().await.unwrap();
    assert_eq!(r.status(), 413);
    let e: serde_json::Value = r.json().await.unwrap();
    assert_eq!(e["code"], "too_large", "{e}");
    assert_eq!(s.live.load(Ordering::SeqCst), 1);

    // the same without a declared length (sent in chunks): cut at the limit, 413
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        frames_body(JSON_BODY_MAX + 1).chunks(1 << 20).map(|c| Ok(c.to_vec())).collect();
    let r = http
        .post(&live)
        .body(reqwest::Body::wrap_stream(futures_util::stream::iter(chunks)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(s.live.load(Ordering::SeqCst), 1);

    // a raw-body route keeps the service's limit: an upload of the same size reaches its handler (501 here: this
    // server implements none), it is not refused for its size
    let r = http
        .post(format!("{base}/v1/uploads"))
        .header("aviary-upload-kind", "file")
        .header("aviary-upload-name", "a.bin")
        .body(vec![0u8; JSON_BODY_MAX + 1])
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), 413, "{}", r.text().await.unwrap());
}
