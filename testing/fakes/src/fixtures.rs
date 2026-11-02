//! The contract crates' golden fixtures, read from the repository (the fakes are test code and run in-tree), so a
//! test can script a fake straight from them.

use std::path::PathBuf;

use serde_json::Value;

use crate::script::Reply;

fn crate_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates")
        .join(name)
}

fn read(path: &PathBuf) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `crates/magpie-api/fixtures/routes/<route>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn magpie_route(route: &str) -> Value {
    read(
        &crate_dir("magpie-api")
            .join("fixtures/routes")
            .join(format!("{route}.json")),
    )
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
    read(
        &crate_dir("magpie-api")
            .join("fixtures")
            .join(dir)
            .join(format!("{name}.json")),
    )
}

/// `crates/hermit-api/fixtures/<name>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn hermit(name: &str) -> Value {
    read(&crate_dir("hermit-api").join("fixtures").join(format!("{name}.json")))
}

/// `crates/supervisor-api/fixtures/<name>.json`.
///
/// # Panics
/// When the fixture is missing or is not JSON.
#[must_use]
pub fn supervisor(name: &str) -> Value {
    read(
        &crate_dir("supervisor-api")
            .join("fixtures")
            .join(format!("{name}.json")),
    )
}
