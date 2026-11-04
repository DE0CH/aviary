//! pigeon's in-memory model of magpie's switch-window tickets (`common/model.rs`, which the LU contract rig of
//! `tests/legacy.rs` runs on) passes the shared ticket suite that magpie itself passes (`aviary_conformance::tickets`,
//! run against magpie in `crates/magpie/tests/legacy.rs`): the exactly-once tests rest on magpie's rules, not on a
//! model that drifted from them.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use aviary_conformance::report::Report;
use aviary_conformance::tickets::{self, Hooks, Options};
use common::STAFF;
use common::model::{self, Model};

#[tokio::test]
async fn the_model_keeps_magpies_ticket_rules() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let m = Model::default();
    let served = model::serve(&m).await;
    let client = magpie_api::client::Client::new(reqwest::Client::new(), served.url(), "pigeon");
    let (a, b, c, d, e, f) = (m.clone(), m.clone(), m.clone(), m.clone(), m.clone(), m.clone());
    let hooks = Hooks {
        legacy_conversation: Box::new(move || {
            let m = a.clone();
            Box::pin(async move { m.add_legacy(STAFF, Some("舊對話")) })
        }),
        age_ticket: Box::new(move |t, by| {
            let m = b.clone();
            Box::pin(async move { m.age_ticket(t, by) })
        }),
        age_claim: Box::new(move |t, by| {
            let m = e.clone();
            Box::pin(async move { m.age_claim(t, by) })
        }),
        expire_settled: Box::new(move |conv| {
            let m = f.clone();
            Box::pin(async move {
                m.expire_settled(conv);
                m.st()
                    .tickets
                    .iter()
                    .filter(|t| t.conversation == conv && t.expired)
                    .map(|t| t.id)
                    .collect()
            })
        }),
        outcome: Box::new(move |t| {
            let m = c.clone();
            Box::pin(async move { m.st().tickets.iter().find(|x| x.id == t).and_then(|x| x.outcome) })
        }),
        release: Box::new(move |conv| {
            let m = d.clone();
            Box::pin(async move { assert!(m.try_release(conv)) })
        }),
    };
    let report = Report::new("tickets", vec![], true);
    tickets::run(
        Options {
            client,
            person: STAFF.into(),
            hooks,
        },
        &report,
    )
    .await;
    assert!(report.passed(), "failures: {:#?}", report.failures());
    assert_eq!(report.results().len(), 11);
}
