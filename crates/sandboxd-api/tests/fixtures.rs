//! Every golden fixture parses as its route's type and serializes back to the same JSON.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

mod common;

use common::{fixture, same};
use sandboxd_api::{
    Deleted, EgressTargets, EnsureImage, ErrorBody, Host, ImageDigest, Listed, Peers, ReceiveResult, Receiving, Role,
    Sandboxes, SendRequest, SendResult, SetPeers, Spec,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let text = fixture(name);
    let v: T = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let back = serde_json::to_value(&v).unwrap();
    let want: Value = serde_json::from_str(&text).unwrap();
    assert!(same(&back, &want), "{name}: {back} vs {want}");
    v
}

#[test]
fn host() {
    let h: Host = round_trip("host.json");
    assert_eq!((h.role, h.gateway, h.jfs_mounted), (Role::Burst, false, Some(true)));
    assert_eq!(h.sandboxes.len(), 1);
    let g: Host = round_trip("host-gateway.json");
    assert_eq!(
        (g.role, g.gateway, g.jfs_mounted, g.public_ipv6),
        (Role::Hub, true, None, None)
    );
}

#[test]
fn peers_egress_images() {
    let p: SetPeers = round_trip("peers-request.json");
    assert_eq!(p.peers[1].endpoint, None);
    let _: Peers = round_trip("peers-response.json");
    let _: EgressTargets = round_trip("egress.json");
    let i: EnsureImage = round_trip("images-request.json");
    assert!(i.reference.is_some_and(|r| r.contains("@sha256:")));
    let _: ImageDigest = round_trip("images-response.json");
}

#[test]
fn sandboxes() {
    let s: Spec = round_trip("spec.json");
    assert_eq!(s.env.0[0].0, "HTTP_PROXY", "env keeps its order");
    assert_eq!(s.env.0.last().map(|(k, _)| k.as_str()), Some("AVIARY_SANDBOX"));
    let m: Spec = round_trip("spec-minimal.json");
    assert_eq!((m.image_ref, m.workspace), (None, None));
    let _: Listed = round_trip("listed.json");
    let a: Listed = round_trip("listed-alive.json");
    assert_eq!((a.alive, a.sending, a.scratch_used), (Some(false), false, None));
    let s: Listed = round_trip("listed-sending.json");
    assert_eq!(
        (s.alive, s.sending, s.scratch_used),
        (Some(true), true, Some(5_033_164_800))
    );
    let p: Spec = round_trip("spec-pids.json");
    assert_eq!(p.pids, Some(4096));
    let _: Sandboxes = round_trip("sandboxes.json");
    let _: Deleted = round_trip("deleted.json");
}

#[test]
fn migration() {
    let _: Receiving = round_trip("receive-response.json");
    assert!(matches!(
        round_trip("receive-running.json"),
        ReceiveResult::Running { stats: Some(_) }
    ));
    assert_eq!(
        round_trip::<ReceiveResult>("receive-receiving.json"),
        ReceiveResult::Receiving
    );
    assert!(matches!(round_trip("receive-failed.json"), ReceiveResult::Failed { error } if error == "not here"));
    assert_eq!(round_trip::<ReceiveResult>("receive-none.json"), ReceiveResult::None);
    let s: SendRequest = round_trip("send-request.json");
    assert_eq!(s.max_downtime_ms, Some(500));
    let m: SendRequest = round_trip("send-request-minimal.json");
    assert_eq!((m.downtime_ms, m.max_rounds, m.max_downtime_ms), (None, None, None));
    let _: SendResult = round_trip("send-response.json");
    let _: ErrorBody = round_trip("error.json");
}

#[test]
fn answers_of_a_newer_sandboxd_still_parse() {
    // expand-only (lib.rs): a field or state an older client does not know is ignored, not an error
    let mut listed: Value = serde_json::from_str(&fixture("listed.json")).unwrap();
    listed["alive"] = Value::Bool(true);
    listed["state"] = Value::from("paused");
    let listed: Listed = serde_json::from_value(listed).unwrap();
    assert_eq!(listed.state, sandboxd_api::SandboxState::Unknown);
    let mut host: Value = serde_json::from_str(&fixture("host.json")).unwrap();
    host["newFact"] = Value::from(1);
    host["sandboxd"]["memory"]["newArena"] = Value::from(2);
    host["sandboxes"][0]["alive"] = Value::Bool(false);
    let _: Host = serde_json::from_value(host).unwrap();
    let result: ReceiveResult = serde_json::from_value(serde_json::json!({"state": "moving", "why": "x"})).unwrap();
    assert_eq!(result, ReceiveResult::Unknown);
    let mut sent: Value = serde_json::from_str(&fixture("send-response.json")).unwrap();
    sent["scratch"]["extra"] = Value::from(0);
    let _: SendResult = serde_json::from_value(sent).unwrap();
}

#[test]
fn debug_hides_env_values() {
    let s: Spec = serde_json::from_str(&fixture("spec.json")).unwrap();
    let (k, v) = s.env.0[0].clone();
    let shown = format!("{s:?}");
    assert!(shown.contains(&k), "{shown}");
    assert!(!shown.contains(&v), "{shown}");
}
