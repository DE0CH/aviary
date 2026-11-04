//! `aviary-conformance model` against mock-model directly, and through the reference gateway's proxy with
//! fake-hermit's keys and liveness.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use aviary_conformance::model::{self, Options};
use aviary_conformance::report::Report;
use aviary_core::auth::{Caller, Callers};
use aviary_fakes::FakeHermit;
use hermit_api::sandbox::PutSandboxRequest;
use uuid::Uuid;

async fn mock_model() -> String {
    let mock = mock_model::MockModel::with_default_models();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock.router()).await.unwrap() });
    url
}

#[tokio::test]
async fn mock_model_passes() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let target = mock_model().await;
    let report = Report::new("model", vec![], true);
    model::run(
        Options {
            target,
            token: "pigeon-token".into(),
            proxy: None,
        },
        &report,
    )
    .await;
    assert!(report.passed(), "failures: {:#?}", report.failures());
    assert_eq!(report.results().len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mock_model_through_the_reference_gateway_passes() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let model_url = mock_model().await;
    let hermit = FakeHermit::new();
    let hermit_running = hermit.serve().await.unwrap();
    let pigeon = format!("pigeon-{}", Uuid::new_v4().simple());
    let cfg = test_gateway::Config {
        proxy_listen: "127.0.0.1:0".parse().unwrap(),
        api_listen: "127.0.0.1:0".parse().unwrap(),
        upstreams: test_gateway::Upstreams {
            magpie: "http://127.0.0.1:9".into(),
            hermit: hermit_running.url(),
            tools: "http://127.0.0.1:9".into(),
            model: model_url,
        },
        credentials: test_gateway::Credentials {
            magpie: "unused".into(),
            hermit: hermit.token(Caller::Gateway).unwrap(),
            tools: "unused".into(),
            model: "model-key".into(),
        },
        routes: Vec::new(),
        callers: Callers::new([
            (Caller::Pigeon, vec![pigeon.clone()]),
            (Caller::Hermit, vec!["h".to_owned()]),
        ])
        .unwrap(),
        self_hosts: vec![],
        ops_summary: test_gateway::default_ops_summary(),
        drain: test_gateway::DrainBounds::default(),
        upload_cap: test_gateway::UPLOAD_CAP,
        upstream_idle: gateway_api::limits::IDLE_TIMEOUT,
        faults: test_gateway::Faults::default(),
    };
    let mut registry = prometheus_client::registry::Registry::default();
    let gw = Arc::new(test_gateway::start(cfg, &mut registry).await.unwrap());
    tokio::time::timeout(Duration::from_secs(10), gw.keys_loaded())
        .await
        .unwrap();

    // a live sandbox in fake-hermit and its token, presented from 127.0.0.1
    let cid = Uuid::now_v7();
    hermit_running
        .client(reqwest::Client::new(), Caller::Magpie)
        .put_sandbox(
            cid,
            &PutSandboxRequest {
                person: "staff@example.org".into(),
            },
        )
        .await
        .unwrap();
    let token = hermit.mint_token(cid, Ipv4Addr::LOCALHOST).unwrap();

    let report = Report::new("model", vec![], true);
    model::run(
        Options {
            target: format!("http://{}", gw.api_addr()),
            token: pigeon,
            proxy: Some((gw.proxy_addr(), token)),
        },
        &report,
    )
    .await;
    assert!(report.passed(), "failures: {:#?}", report.failures());
}
