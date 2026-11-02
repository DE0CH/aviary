//! pigeon spec 18.3 (the HTTP part; the socket closes are the socket package's) and 11.1: readiness waits for the
//! deployment facts and every team's keys and never for magpie or hermit later; a drain closes the listener, lets
//! in-flight requests and transfers finish, and ends at once when nothing is moving.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use std::time::{Duration, Instant};

use aviary_fakes::{FaultKind, Hold, Reply};
use common::*;
use pigeon::paseo::compat;
use serde_json::json;

#[tokio::test]
async fn readiness_waits_for_facts_and_keys_only() {
    let mut h = Harness::start().await;
    // a second pigeon on the same fakes, started while hermit is down
    let fault = h
        .hermit
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    let app = h.app.clone();
    let readiness = aviary_core::ops::Readiness::detached();
    app.start(&readiness);
    readiness.started();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        readiness.why().iter().any(|w| w == "deployment"),
        "{:?}",
        readiness.why()
    );
    assert!(readiness.why().iter().any(|w| w == "jwks"), "{:?}", readiness.why());
    h.hermit.fake.faults().remove(fault);
    let started = Instant::now();
    while !readiness.why().is_empty() && started.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(readiness.why().is_empty(), "{:?}", readiness.why());
    // later outages do not close the gates again
    h.hermit
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    h.magpie
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(readiness.why().is_empty(), "{:?}", readiness.why());
    // and a draining pigeon answers what is in flight (below)
    h.listener.take().unwrap().drain(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn a_drain_closes_the_listener_and_finishes_what_is_in_flight() {
    let mut h = Harness::start().await;
    let cid = uuid::Uuid::new_v4();
    h.magpie.fake.script().always(
        "files_raw",
        Reply::bytes(200, "done").with_header("content-type", "text/plain"),
    );
    let hold = Hold::new();
    h.magpie.fake.faults().on("files_raw", FaultKind::Hold(hold.clone()));
    let url = format!("{}{}{cid}/files/raw?path=a.txt", h.url, compat::SESSIONS);
    let token = h.token(STAFF);
    let client = h.http.clone();
    let inflight = tokio::spawn(async move {
        client
            .get(url)
            .header("cf-access-jwt-assertion", token)
            .send()
            .await
            .unwrap()
    });
    let started = Instant::now();
    while hold.waiting() == 0 && started.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hold.waiting(), 1, "the request is in flight at magpie");
    let listener = h.listener.take().unwrap();
    let drain = tokio::spawn(listener.drain(Duration::from_secs(30)));
    tokio::time::sleep(Duration::from_millis(200)).await;
    // the listener is closed: a new connection is refused
    let fresh = reqwest::Client::new();
    let refused = fresh.get(format!("{}/", h.url)).send().await;
    assert!(refused.is_err(), "a new connection after the drain began: {refused:?}");
    assert!(!drain.is_finished(), "the drain waits for the request in flight");
    hold.release();
    let r = inflight.await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["connection"], "close", "the answer closes its connection");
    assert_eq!(r.text().await.unwrap(), "done");
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_drain_with_nothing_moving_ends_at_once() {
    let mut h = Harness::start().await;
    // a kept-alive idle connection does not hold the drain
    assert_eq!(h.get("/").send().await.unwrap().status(), 200);
    let started = Instant::now();
    h.listener.take().unwrap().drain(Duration::from_secs(30)).await;
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    tokio::time::timeout(Duration::from_secs(1), h.app.transfers().idle())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_upload_across_a_drain_completes() {
    // spec 18.3 scaled for CI: a throttled upload (4 MiB at about 2 MiB/s) with the drain starting 0.5 s in
    let mut h = Harness::start().await;
    h.magpie.fake.script().always(
        "upload",
        Reply::json(
            201,
            json!({"uploadId": uuid::Uuid::new_v4(), "path": "uploads/1/big.bin", "name": "big.bin",
                   "size": 4_194_304, "mimeType": "application/octet-stream"}),
        ),
    );
    let boundary = "b0undary";
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"big.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    let chunk = vec![b'z'; 256 * 1024];
    let mut parts: Vec<Vec<u8>> = vec![head.into_bytes()];
    for _ in 0..16 {
        parts.push(chunk.clone());
    }
    parts.push(tail.into_bytes());
    let stream = futures_util::stream::unfold(parts.into_iter(), |mut it| async move {
        let next = it.next()?;
        tokio::time::sleep(Duration::from_millis(120)).await;
        Some((Ok::<_, std::io::Error>(next), it))
    });
    let url = format!("{}/api/assets/files", h.url);
    let token = h.token(STAFF);
    let client = h.http.clone();
    let upload = tokio::spawn(async move {
        client
            .post(url)
            .header("cf-access-jwt-assertion", token)
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.app.transfers().active(), 1, "the upload is a transfer");
    let drain_started = Instant::now();
    let drain = tokio::spawn(h.listener.take().unwrap().drain(Duration::from_secs(60)));
    let r = upload.await.unwrap();
    assert_eq!(r.status(), 201);
    let call = h.magpie.fake.script().calls_to("upload").pop().unwrap();
    assert_eq!(call.bytes.unwrap().len(), 4_194_304, "every byte reached magpie");
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
    assert!(
        drain_started.elapsed() > Duration::from_millis(800),
        "the drain waited for the upload"
    );
    assert_eq!(h.app.transfers().active(), 0);
}
