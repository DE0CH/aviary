//! `aviary-conformance gateway` against the reference gateway (`test-gateway`) in-process: it passes, and each seeded
//! fault of `test_gateway::Faults` makes the check that guards against it fail (WP-09 acceptance).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use aviary_conformance::fakes::Fakes;
use aviary_conformance::gateway::{self, Durations, Options, Tokens};
use aviary_conformance::report::{Problem, Report};
use aviary_core::auth::{Caller, Callers};
use hermit_api::token::MasterKey;
use test_gateway::{Config, Credentials, DrainBounds, Faults, Running, Upstreams};
use uuid::Uuid;

fn token(name: &str) -> String {
    format!("{name}-{}", Uuid::new_v4().simple())
}

/// Runs the suite (only `only`, all when empty) against a test-gateway with `faults`.
async fn run(faults: Faults, only: &[&str]) -> Report {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tokens = Tokens {
        gateway_to_magpie: token("magpie"),
        gateway_to_hermit: token("hermit"),
        gateway_to_tools: token("tools"),
        gateway_to_model: token("model"),
        pigeon_to_gateway: token("pigeon"),
        hermit_to_gateway: token("hermit-in"),
    };
    let mut secret = [0u8; 32];
    secret[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    let key = MasterKey::new(1, secret);
    let fakes = Fakes::new(tokens.expected(), vec![gateway::public_key(&key).unwrap()]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = format!("http://{}", listener.local_addr().unwrap());
    let served = fakes.serve(listener).unwrap();

    let cfg = Config {
        proxy_listen: "127.0.0.1:0".parse().unwrap(),
        api_listen: "127.0.0.1:0".parse().unwrap(),
        upstreams: Upstreams {
            magpie: up.clone(),
            hermit: up.clone(),
            tools: up.clone(),
            model: up.clone(),
        },
        credentials: Credentials {
            magpie: tokens.gateway_to_magpie.clone(),
            hermit: tokens.gateway_to_hermit.clone(),
            tools: tokens.gateway_to_tools.clone(),
            model: tokens.gateway_to_model.clone(),
        },
        callers: Callers::new([
            (Caller::Pigeon, vec![tokens.pigeon_to_gateway.clone()]),
            (Caller::Hermit, vec![tokens.hermit_to_gateway.clone()]),
        ])
        .unwrap(),
        self_hosts: vec!["gateway.conformance.invalid".into(), "192.0.2.10".into()],
        ops_summary: test_gateway::default_ops_summary(),
        drain: DrainBounds::default(),
        upload_cap: test_gateway::UPLOAD_CAP,
        faults,
    };
    let mut registry = prometheus_client::registry::Registry::default();
    let running: Arc<Running> = Arc::new(test_gateway::start(cfg, &mut registry).await.unwrap());
    let r = running.clone();
    let o = Options {
        proxy: running.proxy_addr(),
        api: format!("http://{}", running.api_addr()),
        sandbox_ip: "127.0.0.1".parse().unwrap(),
        tokens,
        signing_key: key,
        self_hosts: vec!["gateway.conformance.invalid".into(), "192.0.2.10".into()],
        sigterm: Some(Box::new(move || {
            let r = r.clone();
            Box::pin(async move {
                tokio::spawn(async move { r.drain().await });
                Ok(())
            })
        })),
        durations: Durations::quick(),
    };
    let report = Report::new("gateway", only.iter().map(|s| (*s).to_owned()).collect(), true);
    gateway::run(o, fakes, &report).await;
    drop(served);
    report
}

fn failed(report: &Report, check: &str) -> bool {
    report
        .results()
        .iter()
        .any(|r| r.name == check && matches!(r.outcome, Err(Problem::Fail(_))))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reference_gateway_passes() {
    let report = run(Faults::default(), &[]).await;
    let results = report.results();
    assert!(report.passed(), "failures: {:#?}", report.failures());
    // every check ran: none was skipped
    let skipped: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Err(Problem::Skip(_))))
        .map(|r| r.name.clone())
        .collect();
    assert!(skipped.is_empty(), "skipped: {skipped:?}");
    assert!(results.len() >= 27, "{} checks", results.len());
    let junit = report.junit();
    assert!(junit.contains("<testsuite name=\"gateway\""), "{junit}");
    assert!(junit.contains("failures=\"0\""));
    assert!(junit.contains("name=\"drain\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stripped_header_kept_fails() {
    let report = run(
        Faults {
            keep_client_aviary_headers: true,
            ..Faults::default()
        },
        &["route.magpie", "route.hermit"],
    )
    .await;
    assert!(failed(&report, "route.magpie"), "{:#?}", report.results());
    assert!(failed(&report, "route.hermit"), "{:#?}", report.results());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hermit_outage_turning_into_403_fails() {
    let report = run(
        Faults {
            hermit_failure_refuses: true,
            ..Faults::default()
        },
        &["liveness.hermit-outage"],
    )
    .await;
    assert!(failed(&report, "liveness.hermit-outage"), "{:#?}", report.results());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_cap_counting_fixed_names_fails() {
    let report = run(
        Faults {
            upload_cap_counts_fixed_names: true,
            ..Faults::default()
        },
        &["limits.upload-cap"],
    )
    .await;
    assert!(failed(&report, "limits.upload-cap"), "{:#?}", report.results());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drain_cutting_a_stream_fails() {
    let report = run(
        Faults {
            drain_cuts_streams: true,
            ..Faults::default()
        },
        &["drain"],
    )
    .await;
    assert!(failed(&report, "drain"), "{:#?}", report.results());
}
