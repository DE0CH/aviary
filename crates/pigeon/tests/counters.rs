//! The counter wiring check for pigeon's switch-window counters (WP-54, DESIGN 9.9): a pigeon built as the binary
//! builds it (`App::new` on the `pigeon` registry, the test harness) scrapes every `pigeon` line of
//! `testing/switch/zero-counters.txt`, present, at 0 before anything happened. (`Harness::metric` reads an absent
//! series as 0, so presence is checked here once for all of them.)
#![allow(clippy::unwrap_used, clippy::too_many_lines)] // tests (the shared harness has long builders)

mod common;

use common::Harness;

const LIST: &str = include_str!("../../../testing/switch/zero-counters.txt");

#[tokio::test]
async fn every_zero_counter_of_pigeon_is_exported_at_zero() {
    let h = Harness::start().await;
    let text = h.metrics();
    let series: Vec<&str> = LIST
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (series, who, state) = (w.next()?, w.next()?, w.next()?);
            (who == "pigeon" && state == "exported").then_some(series)
        })
        .collect();
    assert!(series.len() >= 5, "the list names pigeon's counters");
    for s in series {
        let value = text
            .lines()
            .find_map(|l| l.strip_prefix(s).and_then(|r| r.strip_prefix(' ')));
        assert_eq!(value, Some("0"), "{s} is exported at 0 on a fresh pigeon:\n{text}");
    }
}
