//! `aviary-conformance hermit` against fake-hermit (aviary-fakes), with every hook, so no check is skipped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::Ipv4Addr;
use std::time::Duration;

use aviary_conformance::hermit::{self, Capabilities, Hooks, Options, Tokens};
use aviary_conformance::report::{Problem, Report};
use aviary_core::auth::Caller;
use aviary_fakes::FakeHermit;
use aviary_fakes::hermit::HermitConfig;
use hermit_api::sandbox::ReleaseReason;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fake_hermit_passes() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fake = FakeHermit::with_config(HermitConfig {
        // long enough to observe a long-poll timing out while starting
        ready_after: Some(Duration::from_secs(2)),
        ..HermitConfig::default()
    });
    let running = fake.serve().await.unwrap();
    let t = |c| fake.token(c);
    let tokens = Tokens {
        pigeon: t(Caller::Pigeon),
        magpie: t(Caller::Magpie),
        gateway: t(Caller::Gateway),
        tools: t(Caller::Tools),
        monitor: t(Caller::Monitor),
        admin: t(Caller::Admin),
    };
    let (f1, f2, f3) = (fake.clone(), fake.clone(), fake.clone());
    let hooks = Hooks {
        token: Some(Box::new(move |_sandbox, cid| f1.mint_token(cid, Ipv4Addr::UNSPECIFIED))),
        pending_release: Some(Box::new(move |cid| f2.request_release(cid, ReleaseReason::Idle, false))),
        expire_feed: Some(Box::new(move |seq| f3.expire_events_before(seq))),
    };
    let report = Report::new("hermit", vec![], true);
    hermit::run(
        Options {
            target: running.url(),
            tokens,
            capabilities: Capabilities::default(),
            hooks,
            ready_within: Duration::from_secs(30),
        },
        &report,
    )
    .await;
    assert!(report.passed(), "failures: {:#?}", report.failures());
    let skipped: Vec<_> = report
        .results()
        .into_iter()
        .filter(|r| matches!(r.outcome, Err(Problem::Skip(_))))
        .map(|r| r.name)
        .collect();
    assert!(skipped.is_empty(), "skipped: {skipped:?}");
    assert!(report.junit().contains("<testsuite name=\"hermit\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hermit_answering_pause_while_claiming_none_fails() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fake = FakeHermit::new();
    let running = fake.serve().await.unwrap();
    let tokens = Tokens {
        magpie: fake.token(Caller::Magpie),
        gateway: fake.token(Caller::Gateway),
        ..Tokens::default()
    };
    // the fake answers 501 to pause; a suite told the implementation claims pause must fail it
    let report = Report::new("hermit", vec!["sandbox.capabilities".into()], true);
    hermit::run(
        Options {
            target: running.url(),
            tokens,
            capabilities: Capabilities {
                pause: true,
                snapshot: false,
            },
            hooks: Hooks::default(),
            ready_within: Duration::from_secs(30),
        },
        &report,
    )
    .await;
    assert!(!report.passed(), "{:#?}", report.results());
}
