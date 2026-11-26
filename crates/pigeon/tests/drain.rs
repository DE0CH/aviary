//! pigeon spec 18.3 (the HTTP part; the socket closes are the socket package's) and 11.1: readiness waits for the
//! deployment facts and every team's keys and never for magpie or hermit later; a drain closes the listener, lets
//! in-flight requests and transfers finish, and ends at once when nothing is moving.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use std::time::Duration;

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
    let faulted = h.hermit.fake.faults().hits();
    app.start(&readiness);
    readiness.started();
    until("its first facts read failed", || {
        h.hermit.fake.faults().hits() > faulted
    })
    .await;
    assert!(
        readiness.why().iter().any(|w| w == "deployment"),
        "{:?}",
        readiness.why()
    );
    assert!(readiness.why().iter().any(|w| w == "jwks"), "{:?}", readiness.why());
    h.hermit.fake.faults().remove(fault);
    h.advance_until(h.tuning.start_retry, "ready", || readiness.why().is_empty())
        .await;
    // later outages do not close the gates again
    h.hermit
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    h.magpie
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    let before = h.hermit.fake.faults().hits();
    h.advance_until(h.tuning.facts_every, "a facts re-read in the outage", || {
        h.hermit.fake.faults().hits() > before
    })
    .await;
    assert!(readiness.why().is_empty(), "{:?}", readiness.why());
    // and a draining pigeon answers what is in flight (below)
    h.listener
        .take()
        .unwrap()
        .drain(Duration::from_secs(1), aviary_core::clock::Clock::real())
        .await;
}

#[tokio::test]
async fn readiness_waits_for_every_teams_keys() {
    // a fresh pigeon whose second team's certs fail: the facts load, the jwks gate stays closed
    let h = Harness::start_opts(Opts {
        certs_down: vec![1],
        wait_ready: false,
        ..Opts::default()
    })
    .await;
    until("the facts", || h.app.identities().facts().is_some()).await;
    h.advance_until(h.tuning.start_retry, "the failing team retried", || {
        h.teams[1].jwks_hits.load(std::sync::atomic::Ordering::SeqCst) >= 2
    })
    .await;
    let why = h.readiness.why();
    assert!(!why.iter().any(|w| w == "deployment"), "{why:?}");
    assert!(why.iter().any(|w| w == "jwks"), "one team has no keys: {why:?}");
    assert!(
        h.teams[1].jwks_hits.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "the failing team is retried"
    );
    // its certs answer: the gate opens
    h.teams[1].certs_up.store(true, std::sync::atomic::Ordering::SeqCst);
    h.advance_until(h.tuning.start_retry, "ready", || h.readiness.why().is_empty())
        .await;
    assert_eq!(h.get_as(compat::ME, STAFF).await.status(), 200);
    let t2 = h.teams[1].token(STAFF);
    assert_eq!(h.as_token(h.get(compat::ME), &t2).send().await.unwrap().status(), 200);
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
    until("the request in flight at magpie", || hold.waiting() == 1).await;
    let listener = h.listener.take().unwrap();
    // the drain's 30 s bound is on pigeon's clock, which this test does not move: only the request ends it
    let drain = tokio::spawn(listener.drain(Duration::from_secs(30), h.app.clock()));
    // the listener is closed: a new connection is refused
    refused_soon(&h.url).await;
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
    // the 30 s bound is on the clock, which does not move: a drain that waited for it would never end
    tokio::time::timeout(
        Duration::from_secs(20),
        h.listener.take().unwrap().drain(Duration::from_secs(30), h.app.clock()),
    )
    .await
    .unwrap_or_else(|_| panic!("the drain ends by itself"));
    tokio::time::timeout(Duration::from_secs(1), h.app.transfers().idle())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_upload_across_a_drain_completes() {
    // spec 18.3 scaled for CI: an upload part-way through its body when the drain starts; the rest is sent only then
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
    let (go, gate) = tokio::sync::watch::channel(false);
    let stream = futures_util::stream::unfold((parts.into_iter().enumerate(), gate), |(mut it, mut gate)| async move {
        let (i, next) = it.next()?;
        if i == 5 {
            let _ = gate.wait_for(|g| *g).await;
        }
        Some((Ok::<_, std::io::Error>(next), (it, gate)))
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
    until("the upload is a transfer", || h.app.transfers().active() == 1).await;
    let drain = tokio::spawn(h.listener.take().unwrap().drain(Duration::from_secs(60), h.app.clock()));
    refused_soon(&h.url).await;
    assert!(!drain.is_finished(), "the drain waits for the upload");
    go.send_replace(true);
    let r = upload.await.unwrap();
    assert_eq!(r.status(), 201);
    let call = h.magpie.fake.script().calls_to("upload").pop().unwrap();
    assert_eq!(call.bytes.unwrap().len(), 4_194_304, "every byte reached magpie");
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.app.transfers().active(), 0);
}

#[tokio::test]
async fn sigterm_through_mains_wiring_turns_unready_closes_and_exits_at_once() {
    // spec 18.3's unit case through `App::drain` (what `main` runs): SIGTERM -> /readyz 503, the listener closes,
    // and with nothing transferring the drain ends well within 20 s
    use aviary_core::drain::{Outcome, Signal, Signals};
    let mut h = Harness::start().await;
    let ops = aviary_core::ops::Ops::new(
        aviary_core::version::Build::new("pigeon"),
        aviary_core::ops::registry(&aviary_core::version::Build::new("pigeon")),
    );
    let ops_addr = ops.spawn("127.0.0.1:0".parse().unwrap()).await.unwrap();
    h.app.start(ops.readiness());
    ops.readiness().started();
    let readyz = format!("http://{ops_addr}/readyz");
    until("pigeon ready", || {
        h.readiness.why().is_empty() && ops.readiness().why().is_empty()
    })
    .await;
    assert_eq!(reqwest::get(&readyz).await.unwrap().status(), 200);
    // a kept-alive idle connection does not hold the drain
    assert_eq!(h.get("/").send().await.unwrap().status(), 200);
    let d = h.app.drain(ops.readiness(), h.listener.take().unwrap());
    let (tx, mut signals) = Signals::channel();
    let run = tokio::spawn(async move { d.run(&mut signals).await });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!run.is_finished(), "nothing happens before the signal");
    tx.send(Signal::Term).unwrap();
    // the transfer bound is on pigeon's clock, which does not move: a drain that waited for it would not end
    let outcome = tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome, Outcome::Drained);
    assert_eq!(
        reqwest::get(&readyz).await.unwrap().status(),
        503,
        "/readyz after SIGTERM"
    );
    let refused = reqwest::Client::new().get(format!("{}/", h.url)).send().await;
    assert!(refused.is_err(), "the listener is closed: {refused:?}");
}

/// Waits until a new connection to `url` is refused (the listener closed): looked at again and again, never after a
/// time the test assumes; 30 s only stops a test that would hang.
async fn refused_soon(url: &str) {
    let addr = url.trim_start_matches("http://").to_owned();
    let wait = async {
        while tokio::net::TcpStream::connect(&addr).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .unwrap_or_else(|_| panic!("the listener never closed"));
}
