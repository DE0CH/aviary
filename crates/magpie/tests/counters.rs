//! The counter wiring check for magpie's switch-window counters (WP-54, DESIGN 9.9): on the registry magpie and the
//! import Deployment build (`ops::registry` of the `magpie` build plus `magpie::legacy::Metrics::register`, as
//! `magpie-import`'s `serve` and `connect_with` do), every `magpie` line of `testing/switch/zero-counters.txt` is
//! scraped, present, at 0 before anything happened.
#![allow(clippy::unwrap_used)] // tests

const LIST: &str = include_str!("../../../testing/switch/zero-counters.txt");

/// The `exported` series of `exporter` in the list.
fn exported(exporter: &str) -> Vec<&'static str> {
    LIST.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (series, who, state) = (w.next()?, w.next()?, w.next()?);
            (who == exporter && state == "exported").then_some(series)
        })
        .collect()
}

#[test]
fn every_zero_counter_of_magpie_is_exported_at_zero() {
    let build = aviary_core::version::Build::new("magpie");
    let mut registry = aviary_core::ops::registry(&build);
    let _metrics = magpie::legacy::Metrics::register(&mut registry);
    let mut text = String::new();
    prometheus_client::encoding::text::encode(&mut text, &registry).unwrap();
    let series = exported("magpie");
    assert!(series.len() >= 3, "the list names magpie's counters");
    for s in series {
        let value = text
            .lines()
            .find_map(|l| l.strip_prefix(s).and_then(|r| r.strip_prefix(' ')));
        assert_eq!(value, Some("0"), "{s} is exported at 0 on a fresh registry:\n{text}");
    }
}
