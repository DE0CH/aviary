//! Every request, response and event type round-trips its golden fixture; no serialised key contains `_`
//! (platform 3.6); every fixture file is claimed by a type.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hermit_api::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use hermit_api::deployment::Deployment;
use hermit_api::error::ErrorBody;
use hermit_api::exec::{ExecRequest, ExecResult};
use hermit_api::ops::SnapshotQuery;
use hermit_api::people::{People, Person, WarmReply, WarmRequest};
use hermit_api::sandbox::{
    PutSandboxRequest, ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents, SandboxEventsQuery,
    SandboxQuery, SnapshotReply,
};
use hermit_api::sandbox_surface::{ActivityReply, ActivityReport, SandboxSelf, SecretKeys, SecretValue};
use hermit_api::token::{KeySet, TokenKeys, TokenLiveness};
use hermit_api::workspaces::{CloneRequest, Entries, EntriesQuery, FilesQuery, PutWorkspaceRequest, Workspace};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Loads a fixture, decodes it as `T`, encodes it again and requires the same JSON.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let text = std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap();
    let want: Value = serde_json::from_str(&text).unwrap();
    let typed: T = serde_json::from_value(want.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let got = serde_json::to_value(&typed).unwrap();
    assert_eq!(got, want, "{name} does not round-trip");
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
    "deployment" => Deployment,
    "deployment-no-app" => Deployment,
    "person-staff" => Person,
    "person-admin" => Person,
    "person-not-allowed" => Person,
    "people" => People,
    "warm-request" => WarmRequest,
    "warm-request-no-conversation" => WarmRequest,
    "warm-reply" => WarmReply,
    "put-sandbox-request" => PutSandboxRequest,
    "sandbox-starting" => Sandbox,
    "sandbox-ready" => Sandbox,
    "sandbox-gone" => Sandbox,
    "sandbox-never-ready" => Sandbox,
    "release-sandbox-request" => ReleaseSandboxRequest,
    "release-sandbox-reply" => ReleaseSandboxReply,
    "release-sandbox-reply-none" => ReleaseSandboxReply,
    "snapshot-reply" => SnapshotReply,
    "sandbox-events" => SandboxEvents,
    "sandbox-events-empty" => SandboxEvents,
    "workspace" => Workspace,
    "entries" => Entries,
    "entries-last-page" => Entries,
    "clone-request" => CloneRequest,
    "token-keys" => TokenKeys,
    "token-liveness" => TokenLiveness,
    "token-liveness-gone" => TokenLiveness,
    "sandbox-self" => SandboxSelf,
    "secret-keys" => SecretKeys,
    "secret-keys-granted" => SecretKeys,
    "secret-value" => SecretValue,
    "activity-report" => ActivityReport,
    "activity-report-idle" => ActivityReport,
    "activity-reply-ready" => ActivityReply,
    "activity-reply-releasing" => ActivityReply,
    "exec-request" => ExecRequest,
    "exec-result" => ExecResult,
    "admin-host-patch" => HostPatch,
    "admin-host-patch-uncap" => HostPatch,
    "admin-host-patch-retire" => HostPatch,
    "admin-sandboxes-enabled" => SandboxesEnabled,
    "admin-fresh-host" => FreshHost,
    "admin-release" => AdminRelease,
    "admin-recycle" => Recycle,
    "admin-purge-request" => PurgeRequest,
    "admin-purge-reply" => PurgeReply,
    "error-sandboxes-disabled" => ErrorBody,
    "error-not-granted" => ErrorBody,
    "error-person-mismatch" => ErrorBody,
    "error-cursor-expired" => ErrorBody,
    "put-workspace-request" => PutWorkspaceRequest,
    // query strings, as their JSON objects (the HTTP tests cover the URL form)
    "query-sandbox" => SandboxQuery,
    "query-sandbox-events" => SandboxEventsQuery,
    "query-sandbox-events-seq" => SandboxEventsQuery,
    "query-entries" => EntriesQuery,
    "query-files" => FilesQuery,
    "query-admin-sandboxes" => AdminSandboxesQuery,
    "query-admin-events" => AdminEventsQuery,
    "query-ops-snapshot" => SnapshotQuery,
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

/// Keys of these objects are data (environment variable names), not field names.
const DATA_MAPS: &[&str] = &["env"];

fn underscore_keys(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            let data = DATA_MAPS.iter().any(|d| path.ends_with(&format!(".{d}")));
            for (k, v) in map {
                if !data && k.contains('_') {
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
fn the_descriptor_etag_is_its_hash() {
    let d: SandboxSelf = round_trip("sandbox-self");
    assert_eq!(d.etag, d.compute_etag());
    let mut changed = d.clone();
    changed.claude.managed_claude_md.push('!');
    assert_ne!(changed.compute_etag(), d.etag);
    assert!(serde_json::to_vec(&d).unwrap().len() <= hermit_api::sandbox_surface::SELF_MAX_BYTES);
}

#[test]
fn the_published_keys_decode() {
    let keys: TokenKeys = round_trip("token-keys");
    let set = KeySet::from_keys(&keys);
    assert!(set.contains(1) && set.contains(2));
}

#[test]
fn release_codes_agree_with_reasons() {
    for name in ["sandbox-gone", "sandbox-never-ready"] {
        let s: Sandbox = round_trip(name);
        let r = s.release.unwrap();
        assert_eq!(r.reason.code(), Some(r.code), "{name}");
    }
    let reply: ActivityReply = round_trip("activity-reply-releasing");
    let r = reply.release.unwrap();
    assert_eq!(r.reason.call_code(), Some(r.code));
}
