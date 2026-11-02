//! sandboxd against its contract crate (`sandboxd-api`): the golden fixtures parse with sandboxd's own parsers, and
//! what sandboxd writes parses as the contract's types. The answers of the whole running daemon are checked the same
//! way by `crates/sandboxd-api/tests/live.rs` (the soak job).
use crate::config::{Spec, parse_peer};
use crate::fixed::Buf;
use crate::json::{self, Val, W};
use crate::sandboxes::SendStats;
use crate::storage::SyncStats;
use serde_json::Value;

fn fixture(name: &str) -> String {
    let p = format!("{}/../sandboxd-api/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Equal JSON, numbers compared as f64 (sandboxd writes `1`, serde `1.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn written_bytes(f: impl FnOnce(&mut W)) -> Vec<u8> {
    let mut out = vec![0u8; 64 << 10];
    let mut b = Buf::new(&mut out);
    f(&mut W::new(&mut b));
    assert!(!b.full);
    let n = b.len;
    out.truncate(n);
    out
}

fn written(f: impl FnOnce(&mut W)) -> Value {
    serde_json::from_slice(&written_bytes(f)).unwrap()
}

#[test]
fn spec_fixtures_parse_and_write_back() {
    for name in ["spec.json", "spec-minimal.json", "spec-pids.json"] {
        let text = fixture(name);
        let s = Spec::parse(text.as_bytes(), "10.98.0.0/16").ok().unwrap();
        let bytes = written_bytes(|w| s.write(w));
        let back: Value = serde_json::from_slice(&bytes).unwrap();
        let want: Value = serde_json::from_str(&text).unwrap();
        assert!(same(&back, &want), "{name}: {back} vs {want}");
        // straight from the bytes: a serde_json Value would sort the keys
        let typed: sandboxd_api::Spec = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(typed.name, s.name.as_str());
        // sandboxd keeps the environment's order (the bundle follows it), as the contract's Env does
        let order: Vec<&str> = s.env_pairs().map(|(k, _)| k).collect();
        assert_eq!(order, typed.env.0.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>());
    }
}

#[test]
fn peer_fixtures_parse() {
    let text = fixture("peers-request.json");
    let typed: sandboxd_api::SetPeers = serde_json::from_str(&text).unwrap();
    let Ok(Val::Obj(o)) = json::parse(text.as_bytes()) else { panic!("not an object") };
    let mut got = Vec::new();
    json::object::<crate::util::Error>(o, |k, v| {
        if k.is("peers")
            && let Val::Arr(a) = v
        {
            json::array::<crate::util::Error>(a, |e| {
                let Val::Obj(po) = e else { panic!("peer") };
                got.push(parse_peer(po)?);
                Ok(())
            })?;
        }
        Ok(())
    })
    .ok()
    .unwrap();
    assert_eq!(got.len(), typed.peers.len());
    for (p, t) in got.iter().zip(&typed.peers) {
        assert_eq!(p.public_key.as_str(), t.public_key);
        assert_eq!(p.endpoint.as_str(), t.endpoint.as_deref().unwrap_or(""));
        assert_eq!(p.allowed.as_str(), t.allowed_ips.join(","));
    }
}

#[test]
fn answers_parse_as_the_contract() {
    let mut buf = [0u8; 256];
    let e = crate::server::error_body(&mut buf, "sbx-a is not here");
    let e: sandboxd_api::ErrorBody = serde_json::from_slice(e).unwrap();
    assert_eq!(e.error, "sbx-a is not here");
    let st = SendStats {
        ms: 1934,
        live_rounds: 2,
        live: SyncStats { files: 53, bytes: 230_686_720, deleted: 0, ms: 780 },
        last: SyncStats { files: 3, bytes: 20480, deleted: 1, ms: 130 },
        stopped_ms: 150,
    };
    let v = written(|w| st.write(w));
    let want: Value = serde_json::from_str(&fixture("send-response.json")).unwrap();
    assert!(same(&v, &want), "{v} vs {want}");
    let _: sandboxd_api::SendResult = serde_json::from_value(v).unwrap();
}

#[test]
fn names_and_constants_agree() {
    for n in ["sbx-0a1b2c", "sbx-", "sbx-ABC", "sbx-../x", "x-abc", &format!("sbx-{}", "a".repeat(41))] {
        assert_eq!(crate::config::valid_name(n), sandboxd_api::valid_name(n), "{n}");
    }
    assert_eq!(crate::server::CONTROLLER_CN, sandboxd_api::CLIENT_CN);
    let c = crate::config::HostConfig::parse(br#"{"hostCidr":"10.99.0.0/24","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","privateGateway":"10.97.0.1","name":"x","role":"hub","wgAddress":"10.99.0.1"}"#).ok().unwrap();
    assert_eq!((c.api_port, c.image_port), (sandboxd_api::PORT, sandboxd_api::IMAGE_PORT));
    assert_eq!(c.limits.max_body, sandboxd_api::MAX_BODY);
}
