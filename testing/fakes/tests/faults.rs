//! The fault knobs, each on its own: latency, a burst of 503, holding requests until released, and dropping the
//! connection without an answer; by route name and by path template, on each of the three fakes.
#![allow(clippy::unwrap_used)] // tests

use std::time::{Duration, Instant};

use aviary_core::auth::Caller;
use aviary_fakes::{FakeHermit, FakeMagpie, FakeSupervisor, FaultKind, Hold};
use magpie_api::client::{Client, ClientError};
use magpie_api::sandbox::CommandsQuery;
use uuid::Uuid;

const CONV: Uuid = Uuid::from_u128(0x0190_f6c2_4a0d_7b2c_8e1f_0a1b_2c3d_4e5f);

fn setup() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A magpie client speaking as the gateway for a sandbox of `CONV` (the identity headers the gateway would add).
fn sandbox_http() -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("aviary-sandbox-id", "sbx-1".parse().unwrap());
    h.insert("aviary-sandbox-conversation", CONV.to_string().parse().unwrap());
    h.insert("aviary-sandbox-person", "staff@example.org".parse().unwrap());
    reqwest::Client::builder().default_headers(h).build().unwrap()
}

async fn magpie() -> (aviary_fakes::magpie::Running, Client) {
    setup();
    let running = FakeMagpie::new().serve().await.unwrap();
    let client = running.client(sandbox_http(), Caller::Gateway);
    (running, client)
}

#[tokio::test]
async fn latency_delays_only_its_route() {
    let (running, c) = magpie().await;
    running
        .fake
        .faults()
        .on("cc_tree", FaultKind::Latency(Duration::from_millis(400)));
    let t = Instant::now();
    c.sandbox(None).cc_tree().await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(400), "{:?}", t.elapsed());
    let t = Instant::now();
    c.sandbox(None).lease().await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(300), "{:?}", t.elapsed());
}

#[tokio::test]
async fn a_burst_of_503_then_service() {
    let (running, c) = magpie().await;
    let faults = running.fake.faults();
    faults.times("lease", FaultKind::Unavailable { retry_after_secs: 2 }, 3);
    for _ in 0..3 {
        match c.sandbox(None).lease().await {
            Err(ClientError::Api(e)) => {
                assert_eq!(e.status, 503);
                assert_eq!(serde_json::to_value(&e).unwrap()["code"], "unavailable");
            }
            other => panic!("{other:?}"),
        }
    }
    let lease = c.sandbox(None).lease().await.unwrap();
    assert_eq!(lease.epoch, 1, "the 503s reached nothing");
    assert_eq!(faults.hits(), 3);
    // the header is the contract's retry-after
    faults.times("lease", FaultKind::Unavailable { retry_after_secs: 2 }, 1);
    let res = sandbox_http()
        .post(format!("{}/v1/sandbox/lease", running.url()))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    assert_eq!(res.headers()["retry-after"], "2");
}

#[tokio::test]
async fn an_outage_everywhere_until_removed() {
    let (running, c) = magpie().await;
    let id = running
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 1 });
    assert!(c.sandbox(None).lease().await.is_err());
    assert!(c.sandbox(None).cc_tree().await.is_err());
    running.fake.faults().remove(id);
    c.sandbox(None).lease().await.unwrap();
}

#[tokio::test]
async fn hold_keeps_requests_until_released() {
    let (running, c) = magpie().await;
    let lease = c.sandbox(None).lease().await.unwrap();
    let hold = Hold::new();
    running.fake.faults().on("commands", FaultKind::Hold(hold.clone()));
    running.fake.push_command(
        CONV,
        None,
        magpie_api::sandbox::CommandBody::StopTasks(magpie_api::Empty {}),
    );
    let c2 = c.clone();
    let task = tokio::spawn(async move {
        c2.sandbox(Some(lease.lease_id))
            .commands(&CommandsQuery {
                after: 0,
                wait_ms: None,
            })
            .await
    });
    assert!(hold.wait_held(1, Duration::from_secs(5)).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!task.is_finished(), "a held request is not answered");
    assert!(
        running.fake.script().calls_to("commands").is_empty(),
        "a held request has not reached the fake"
    );
    hold.release();
    let got = task.await.unwrap().unwrap();
    assert_eq!(got.commands.len(), 1);
    assert_eq!(hold.waiting(), 0);
}

#[tokio::test]
async fn drop_closes_the_connection_without_an_answer() {
    let (running, c) = magpie().await;
    running.fake.faults().times("lease", FaultKind::Drop, 1);
    match c.sandbox(None).lease().await {
        Err(ClientError::Http(e)) => assert!(!e.is_status(), "{e}"),
        other => panic!("expected a transport error, got {other:?}"),
    }
    assert!(running.fake.script().calls_to("lease").is_empty());
    // the next request on a new connection is served
    assert_eq!(c.sandbox(None).lease().await.unwrap().epoch, 1);
}

#[tokio::test]
async fn faults_match_by_path_template_and_on_hermit() {
    setup();
    let running = FakeHermit::new().serve().await.unwrap();
    let c = running.client(reqwest::Client::new(), Caller::Pigeon);
    running.fake.faults().times(
        hermit_api::routes::DEPLOYMENT,
        FaultKind::Unavailable { retry_after_secs: 1 },
        1,
    );
    assert_eq!(c.deployment().await.unwrap_err().status(), Some(503));
    c.deployment().await.unwrap();
    running.fake.faults().times("deployment", FaultKind::Drop, 1);
    assert!(matches!(
        c.deployment().await,
        Err(hermit_api::ClientError::Transport(_))
    ));
    c.deployment().await.unwrap();
    running
        .fake
        .faults()
        .times("person", FaultKind::Latency(Duration::from_millis(300)), 1);
    let t = Instant::now();
    c.person("staff@example.org").await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn faults_on_the_supervisor() {
    setup();
    let running = FakeSupervisor::new("tok").serve().await.unwrap();
    let c = supervisor_api::client::ControlClient::new(reqwest::Client::new(), running.url(), "tok");
    let hold = Hold::new();
    running.fake.faults().times("health", FaultKind::Hold(hold.clone()), 1);
    let c2 = c.clone();
    let task = tokio::spawn(async move { c2.health().await });
    assert!(hold.wait_held(1, Duration::from_secs(5)).await);
    hold.release();
    assert!(task.await.unwrap().unwrap().ready);
    running
        .fake
        .faults()
        .times("state", FaultKind::Unavailable { retry_after_secs: 1 }, 2);
    assert!(c.state().await.is_err());
    assert!(c.state().await.is_err());
    c.state().await.unwrap();
}

#[tokio::test]
async fn drop_outside_the_fakes_accept_loop_answers_500_instead_of_hanging() {
    setup();
    let fake = FakeSupervisor::new("tok");
    fake.faults().on("health", FaultKind::Drop);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = fake.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let c = supervisor_api::client::ControlClient::new(reqwest::Client::new(), format!("http://{addr}"), "tok");
    let e = tokio::time::timeout(Duration::from_secs(5), c.health())
        .await
        .unwrap_or_else(|_| panic!("a Drop fault without aviary_fakes::serve must not hang"))
        .unwrap_err();
    assert_eq!((e.status(), e.code()), (Some(500), Some("internal")));
    server.abort();
}

#[tokio::test]
async fn an_outage_everywhere_covers_unmatched_paths() {
    setup();
    let running = FakeHermit::new().serve().await.unwrap();
    let id = running
        .fake
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 2 });
    let r = reqwest::get(format!("{}/no/such/path", running.url())).await.unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers()["retry-after"], "2");
    running.fake.faults().remove(id);
    let r = reqwest::get(format!("{}/no/such/path", running.url())).await.unwrap();
    assert_eq!(r.status(), 404);
}
