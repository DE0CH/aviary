//! Every `:7070` and local-socket type round-trips its golden fixture; no serialised key contains `_`; every
//! fixture file is claimed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use supervisor_api::control::{
    Health, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, RunSummary, StateReport,
};
use supervisor_api::local::{SendUserFileReply, SendUserFileRequest};
use supervisor_api::{ReleaseReason, SecretValue};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let text = std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap();
    let want: Value = serde_json::from_str(&text).unwrap();
    let typed: T = serde_json::from_value(want.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        want,
        "{name} does not round-trip"
    );
    typed
}

macro_rules! fixtures {
    ($($name:literal => $ty:ty),* $(,)?) => {
        const NAMES: &[&str] = &[$($name),*];
        #[test]
        fn every_fixture_round_trips() {
            $( round_trip::<$ty>($name); )*
        }
    };
}

fixtures! {
    "health" => Health,
    "health-starting" => Health,
    "state" => StateReport,
    "state-booting" => StateReport,
    "prepare-snapshot" => PrepareRequest,
    "prepare-pause" => PrepareRequest,
    "prepare-answer" => PrepareAnswer,
    "resumed" => Resumed,
    "release-idle" => ReleaseRequest,
    "release-host-lost" => ReleaseRequest,
    "release-answer" => ReleaseAnswer,
    "error-busy" => aviary_core_error::ErrorBody,
    "error-not-ready" => aviary_core_error::ErrorBody,
    "error-not-granted" => aviary_core_error::ErrorBody,
    "send-user-file" => SendUserFileRequest,
    "send-user-file-reply" => SendUserFileReply,
    "secret" => SecretValue,
}

mod aviary_core_error {
    pub use hermit_api::ErrorBody;
}

#[test]
fn every_fixture_file_is_claimed() {
    let files: BTreeSet<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    let claimed: BTreeSet<String> = NAMES.iter().map(|n| format!("{n}.json")).collect();
    assert_eq!(files, claimed);
}

fn underscore_keys(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                if k.contains('_') {
                    out.push(format!("{path}.{k}"));
                }
                underscore_keys(v, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                underscore_keys(v, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn no_underscore_in_any_key() {
    let mut found = Vec::new();
    for name in NAMES {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap()).unwrap();
        underscore_keys(&v, name, &mut found);
    }
    assert!(found.is_empty(), "keys with `_`: {found:?}");
}

#[test]
fn release_requests_use_the_shared_reasons() {
    for name in ["release-idle", "release-host-lost"] {
        let r: ReleaseRequest = round_trip(name);
        assert_ne!(r.reason, ReleaseReason::Unknown);
        assert_eq!(r.reason.code(), Some(r.code), "{name}");
    }
}

#[test]
fn the_busy_answer_carries_a_run_summary() {
    let body: hermit_api::ErrorBody = round_trip("error-busy");
    let run: RunSummary = serde_json::from_value(body.fields["run"].clone()).unwrap();
    assert!(run.state.is_busy());
    let made = supervisor_api::control::busy(run);
    assert_eq!(made.body().fields["run"], body.fields["run"]);
}
