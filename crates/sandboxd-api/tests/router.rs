//! The router serves a `Server` with sandboxd's routes and error shape, and the client speaks to it.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)
#![allow(clippy::unused_async_trait_impl)] // the fake answers at once; the trait is async

mod common;

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::fixture;
use sandboxd_api::{
    Answer, ApiError, Client, Deleted, EgressTargets, EnsureImage, Error, Host, ImageDigest, Listed, Peer, Peers,
    ReceiveResult, Receiving, Sandboxes, SendRequest, SendResult, Server, SetPeers, Spec, router,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(&fixture(name)).unwrap()
}

/// Answers with the fixtures and records what it was asked.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn saw(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
}

impl Server for Fake {
    async fn host(&self) -> Answer<Host> {
        Ok(load("host.json"))
    }
    async fn set_peers(&self, req: SetPeers) -> Answer<Peers> {
        self.saw(format!("peers {}", req.peers.len()));
        Ok(load("peers-response.json"))
    }
    async fn set_egress(&self, req: EgressTargets) -> Answer<EgressTargets> {
        Err(ApiError::new(
            400,
            format!("only the hub translates the egress address ({})", req.targets.len()),
        ))
    }
    async fn ensure_image(&self, req: EnsureImage) -> Answer<ImageDigest> {
        Ok(ImageDigest {
            digest: req.digest.unwrap_or_default(),
        })
    }
    async fn list(&self) -> Answer<Sandboxes> {
        Ok(load("sandboxes.json"))
    }
    async fn create(&self, spec: Spec) -> Answer<Listed> {
        self.saw(format!("create {} env {}", spec.name, spec.env.0.len()));
        Ok(load("listed.json"))
    }
    async fn delete(&self, name: String) -> Answer<Deleted> {
        Ok(Deleted { deleted: name })
    }
    async fn spec(&self, name: String) -> Answer<Spec> {
        Err(ApiError::new(404, format!("{name} is not here")))
    }
    async fn receive(&self, spec: Spec) -> Answer<Receiving> {
        self.saw(format!("receive {}", spec.name));
        Ok(load("receive-response.json"))
    }
    async fn receive_result(&self, name: String, wait_ms: u64) -> Answer<ReceiveResult> {
        self.saw(format!("result {name} {wait_ms}"));
        Ok(ReceiveResult::Receiving)
    }
    async fn send(&self, name: String, req: SendRequest) -> Answer<SendResult> {
        self.saw(format!("send {name} {}", req.to));
        Ok(load("send-response.json"))
    }
}

async fn serve() -> (Arc<Fake>, Client, String) {
    // for the plain reqwest client of the raw checks below
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fake = Arc::new(Fake::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = router(fake.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (fake, Client::plain(&base).unwrap(), base)
}

#[tokio::test]
async fn every_route_through_the_client() {
    let (fake, c, _) = serve().await;
    assert_eq!(c.host().await.unwrap().name, "aws-3f9a1c");
    let peers = vec![Peer {
        public_key: "k".into(),
        endpoint: None,
        allowed_ips: vec!["10.99.0.1/32".into()],
    }];
    assert_eq!(c.set_peers(peers).await.unwrap().peers.len(), 2);
    let d = c
        .ensure_image(&EnsureImage {
            reference: None,
            digest: Some("d".into()),
        })
        .await
        .unwrap();
    assert_eq!(d.digest, "d");
    assert_eq!(c.list().await.unwrap().sandboxes.len(), 2);
    let spec: Spec = load("spec.json");
    assert_eq!(c.create(&spec).await.unwrap().name, "sbx-0a1b2c");
    assert_eq!(c.delete("sbx-0a1b2c").await.unwrap().deleted, "sbx-0a1b2c");
    assert_eq!(c.receive(&spec).await.unwrap().port, 7142);
    assert_eq!(
        c.receive_result("sbx-0a1b2c", 500_000).await.unwrap(),
        ReceiveResult::Receiving
    );
    let sent = c.send("sbx-0a1b2c", &load("send-request.json")).await.unwrap();
    assert_eq!(sent.scratch.stopped_ms, 150);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            "peers 1",
            "create sbx-0a1b2c env 3",
            "receive sbx-0a1b2c",
            "result sbx-0a1b2c 90000", // the client asks for at most RECEIVE_WAIT_MS
            "send sbx-0a1b2c 10.99.0.11:7142",
        ]
    );
}

#[tokio::test]
async fn errors_have_sandboxd_shape() {
    let (_, c, base) = serve().await;
    match c.spec("sbx-nope").await {
        Err(Error::Status { status: 404, body }) => assert_eq!(body, "sbx-nope is not here"),
        other => panic!("{other:?}"),
    }
    match c.set_egress(vec!["10.244.0.5".into()]).await {
        Err(Error::Status { status: 400, body }) => assert!(body.starts_with("only the hub")),
        other => panic!("{other:?}"),
    }
    match c.delete("BAD").await {
        Err(Error::Status { status: 400, body }) => assert_eq!(body, "bad sandbox name"),
        other => panic!("{other:?}"),
    }
    let http = reqwest::Client::new();
    let r = http.get(format!("{base}/v1/nope")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
    assert_eq!(r.text().await.unwrap(), r#"{"error":"not found"}"#);
    let r = http
        .post(format!("{base}/v1/sandboxes"))
        .header("content-type", "application/json")
        .body("{")
        .send();
    assert_eq!(r.await.unwrap().status().as_u16(), 400);
    let big = "x".repeat(70 << 10);
    let r = http
        .post(format!("{base}/v1/sandboxes"))
        .header("content-type", "application/json")
        .body(big)
        .send();
    assert_eq!(r.await.unwrap().status().as_u16(), 413);
    // another method on a known path: sandboxd answers 404 `not found` (crates/sandboxd/src/api.rs, the soak checks it)
    for (m, p) in [
        (reqwest::Method::PATCH, "/v1/host"),
        (reqwest::Method::GET, "/v1/egress"),
        (reqwest::Method::PUT, "/v1/sandboxes/sbx-0a1b2c"),
    ] {
        let r = http.request(m.clone(), format!("{base}{p}")).send().await.unwrap();
        assert_eq!(
            (r.status().as_u16(), r.text().await.unwrap()),
            (404, r#"{"error":"not found"}"#.to_owned()),
            "{m} {p}"
        );
    }
    let mut spec: Spec = load("spec.json");
    spec.name = "sbx-other".into();
    let r = http
        .post(format!("{base}/v1/sandboxes/sbx-0a1b2c/receive"))
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(
        (r.status().as_u16(), r.text().await.unwrap()),
        (400, r#"{"error":"name mismatch"}"#.to_owned())
    );
}

#[tokio::test]
async fn receive_wait_is_read_as_sandboxd_reads_it() {
    // the first `wait=` as a number, clamped to [0, 120000]; anything else is 0, never a 400
    let (fake, _, base) = serve().await;
    let http = reqwest::Client::new();
    for q in [
        "wait=abc",
        "",
        "wait=",
        "wait=-5",
        "wait=inf",
        "wait=1e9",
        "x=1&wait=250.7&wait=9",
    ] {
        let r = http
            .get(format!("{base}/v1/sandboxes/sbx-0a1b2c/receive?{q}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            (r.status().as_u16(), r.text().await.unwrap()),
            (200, r#"{"state":"receiving"}"#.to_owned()),
            "{q}"
        );
    }
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            "result sbx-0a1b2c 0",
            "result sbx-0a1b2c 0",
            "result sbx-0a1b2c 0",
            "result sbx-0a1b2c 0",
            "result sbx-0a1b2c 0",
            "result sbx-0a1b2c 120000",
            "result sbx-0a1b2c 250",
        ]
    );
}

#[tokio::test]
async fn bodies_are_read_as_sandboxd_reads_them() {
    // the content type is ignored, an empty body or a JSON value that is not an object is `{}`, chunked is 411
    let (_, _, base) = serve().await;
    let http = reqwest::Client::new();
    for (ct, b, want) in [
        (None, "", r#"{"digest":""}"#),
        (None, " \n\t", r#"{"digest":""}"#),
        (None, "[1,2]", r#"{"digest":""}"#),
        (Some("text/plain"), "null", r#"{"digest":""}"#),
        (
            Some("text/plain"),
            r#"{"digest":"sha256:ab"}"#,
            r#"{"digest":"sha256:ab"}"#,
        ),
    ] {
        let mut r = http.post(format!("{base}/v1/images")).body(b);
        if let Some(ct) = ct {
            r = r.header("content-type", ct);
        }
        let r = r.send().await.unwrap();
        assert_eq!(
            (r.status().as_u16(), r.text().await.unwrap()),
            (200, want.to_owned()),
            "{ct:?} {b:?}"
        );
    }
    let r = http
        .post(format!("{base}/v1/images"))
        .body("{nope")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    let mut s = tokio::net::TcpStream::connect(base.trim_start_matches("http://"))
        .await
        .unwrap();
    s.write_all(
        b"POST /v1/images HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
    )
    .await
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 411"), "{out}");
    assert!(out.ends_with(r#"{"error":"send a content-length"}"#), "{out}");
}
