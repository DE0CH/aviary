//! `aviary-conformance hermit` (hermit 24.1) against a real hermit replica with the fake fabric: every check runs and
//! passes, none skipped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use ::hermit::tokens::TokenFacts;
use aviary_conformance::hermit::{self as suite, Capabilities, Hooks, Options, Tokens};
use aviary_conformance::report::{Problem, Report};
use aviary_core::auth::Caller;
use common::Harness;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hermit_passes_the_conformance_suite() {
    // a new sandbox's row stays as its PUT left it long enough for the long-poll's timeout to be observed
    let timings = ::hermit::Timings {
        start_pause: Duration::from_millis(1500),
        ..common::timings()
    };
    let h = Harness::with("conformance", common::config(|_| {}), timings).await;
    let r = h.replica("hermit-a").await;
    let t = |c: Caller| Some(h.tokens[&c].clone());
    let tokens = Tokens {
        pigeon: t(Caller::Pigeon),
        magpie: t(Caller::Magpie),
        gateway: t(Caller::Gateway),
        tools: t(Caller::Tools),
        monitor: t(Caller::Monitor),
        admin: t(Caller::Admin),
    };
    let (c1, c2, c3) = (r.core().clone(), r.core().clone(), r.core().clone());
    let url = h.db.url.clone();
    let url2 = url.clone();
    let hooks = Hooks {
        token: Some(Box::new(move |sandbox, _cid| {
            let e = c1.live.get(sandbox)?;
            c1.keys.sandbox_token(
                u32::try_from(e.token_kid?).ok()?,
                &TokenFacts {
                    sandbox_id: e.id,
                    conversation_id: e.conversation_id,
                    ip: e.ip,
                    expires_at: e.hard_expires_at.unix_seconds(),
                    person: &e.person,
                },
            )
        })),
        // a requested (soft) release the busy supervisor has not taken yet: the next activity reply carries it
        pending_release: Some(Box::new(move |cid: Uuid| {
            let _ = &c2;
            let url = url.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let pool = aviary_pg::connect(&url, "hook", 1, Duration::from_secs(5)).unwrap();
                    pool.get()
                        .await
                        .unwrap()
                        .execute(
                            "update sandboxes set release_pending = jsonb_build_object('reason', 'requested', 'note', null,
                               'force', false, 'deadlineAt', to_char(now() at time zone 'utc' + interval '30 seconds',
                               'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), 'askedAt', to_char(now() at time zone 'utc',
                               'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), 'refusals', 0)
                             where conversation_id = $1 and status <> 'gone'",
                            &[&cid],
                        )
                        .await
                        .unwrap();
                });
            });
        })),
        expire_feed: Some(Box::new(move |seq: u64| {
            let _ = &c3;
            let url = url2.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let pool = aviary_pg::connect(&url, "hook", 1, Duration::from_secs(5)).unwrap();
                    let seq = i64::try_from(seq).unwrap();
                    pool.get()
                        .await
                        .unwrap()
                        .execute("delete from sandbox_events where seq < $1", &[&seq])
                        .await
                        .unwrap();
                });
            });
        })),
    };
    let report = Report::new("hermit", vec![], true);
    suite::run(
        Options {
            target: r.url.clone(),
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
}
