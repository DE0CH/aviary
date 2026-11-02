//! The answers of a running sandboxd parse as this crate's types. `crates/sandboxd/tests/soak.py --dump DIR` records
//! every exchange of its functional part (one JSON file each: method, path, status, request, response); the soak job
//! then runs `cargo test -p sandboxd-api --test live -- --ignored` with `SANDBOXD_DUMP=DIR`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

mod common;

use common::{same, same_keys};
use sandboxd_api::{
    Deleted, EgressTargets, ErrorBody, Host, ImageDigest, Listed, Peers, ReceiveResult, Receiving, Sandboxes,
    SendResult, Spec,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Parses `v` as `T` and serializes it back: the same keys at every level (the types ignore unknown fields, so this
/// is where a field sandboxd writes but the types lack is caught), and with `exact` the same values too (every type
/// but `Host`).
fn check<T: DeserializeOwned + Serialize>(what: &str, v: &Value, exact: bool) {
    let t: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{what}: {e}: {v}"));
    let back = serde_json::to_value(&t).unwrap();
    assert!(same_keys(&back, v), "{what}: keys differ: {back} vs {v}");
    if exact {
        assert!(same(&back, v), "{what}: {back} vs {v}");
    }
}

#[test]
#[ignore = "needs SANDBOXD_DUMP from the soak job"]
fn answers_of_a_real_sandboxd() {
    #[allow(clippy::disallowed_methods)] // test input, not configuration
    let dir = std::env::var("SANDBOXD_DUMP").expect("SANDBOXD_DUMP");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let mut seen = std::collections::BTreeSet::new();
    for f in &files {
        let x: Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        let (m, p, status, res) = (
            x["method"].as_str().unwrap(),
            x["path"].as_str().unwrap(),
            x["status"].as_u64().unwrap(),
            &x["response"],
        );
        let what = format!("{} {m} {p} {status}", f.display());
        if res.is_null() {
            continue; // no JSON body (a 413 from the HTTP layer)
        }
        if status >= 400 {
            check::<ErrorBody>(&what, res, true);
            continue;
        }
        let parts: Vec<&str> = p
            .split('?')
            .next()
            .unwrap()
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let route = match (m, parts.as_slice()) {
            ("GET", ["v1", "host"]) => {
                check::<Host>(&what, res, false);
                "host"
            }
            ("PUT", ["v1", "peers"]) => {
                check::<Peers>(&what, res, true);
                "peers"
            }
            ("PUT", ["v1", "egress"]) => {
                check::<EgressTargets>(&what, res, true);
                "egress"
            }
            ("POST", ["v1", "images"]) => {
                check::<ImageDigest>(&what, res, true);
                "images"
            }
            ("GET", ["v1", "sandboxes"]) => {
                check::<Sandboxes>(&what, res, true);
                "list"
            }
            ("POST", ["v1", "sandboxes"]) => {
                check::<Listed>(&what, res, true);
                "create"
            }
            ("DELETE", ["v1", "sandboxes", _]) => {
                check::<Deleted>(&what, res, true);
                "delete"
            }
            ("GET", ["v1", "sandboxes", _, "spec"]) => {
                check::<Spec>(&what, res, true);
                "spec"
            }
            ("POST", ["v1", "sandboxes", _, "receive"]) => {
                check::<Receiving>(&what, res, true);
                "receive"
            }
            ("GET", ["v1", "sandboxes", _, "receive"]) => {
                check::<ReceiveResult>(&what, res, true);
                "receive result"
            }
            ("POST", ["v1", "sandboxes", _, "send"]) => {
                check::<SendResult>(&what, res, true);
                "send"
            }
            _ => panic!("{what}: not a sandboxd route"),
        };
        seen.insert(route);
    }
    let want = [
        "create",
        "delete",
        "egress",
        "host",
        "images",
        "list",
        "peers",
        "receive",
        "receive result",
        "send",
        "spec",
    ];
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        want,
        "every route answered at least once ({} exchanges)",
        files.len()
    );
}
