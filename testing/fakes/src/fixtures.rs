//! The contract crates' golden fixtures, embedded at build time (`build.rs`), so a test can script a fake straight
//! from them, in-tree or from a built binary or image outside the checkout.

use serde_json::Value;

use crate::script::Reply;

include!(concat!(env!("OUT_DIR"), "/fixtures.rs"));

fn read(krate: &str, path: &str) -> Value {
    let (_, _, text) = FIXTURES
        .iter()
        .find(|(k, p, _)| *k == krate && *p == path)
        .unwrap_or_else(|| panic!("crates/{krate}/fixtures/{path}: no such fixture"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("crates/{krate}/fixtures/{path}: {e}"))
}

/// `crates/magpie-api/fixtures/routes/<route>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn magpie_route(route: &str) -> Value {
    read("magpie-api", &format!("routes/{route}.json"))
}

/// The responses of a magpie route fixture as scripted answers, in fixture order.
///
/// # Panics
/// When the fixture is missing or broken.
#[must_use]
pub fn magpie_replies(route: &str) -> Vec<Reply> {
    magpie_route(route)["responses"]
        .as_array()
        .unwrap_or_else(|| panic!("{route}: no responses"))
        .iter()
        .map(Reply::from_fixture)
        .collect()
}

/// A magpie fixture of another kind: `crates/magpie-api/fixtures/<dir>/<name>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn magpie(dir: &str, name: &str) -> Value {
    read("magpie-api", &format!("{dir}/{name}.json"))
}

/// `crates/hermit-api/fixtures/<name>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn hermit(name: &str) -> Value {
    read("hermit-api", &format!("{name}.json"))
}

/// `crates/supervisor-api/fixtures/<name>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn supervisor(name: &str) -> Value {
    read("supervisor-api", &format!("{name}.json"))
}

/// Every embedded fixture of `krate` (`magpie-api`, `hermit-api`, `supervisor-api`) as (path under its fixtures
/// directory, contents).
pub fn all(krate: &str) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    FIXTURES
        .iter()
        .filter(move |(k, _, _)| *k == krate)
        .map(|(_, p, t)| (*p, *t))
}
