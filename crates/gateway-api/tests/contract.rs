//! The gateway contract end to end: fixtures round-trip with camelCase keys; a token minted with hermit's codec,
//! carried as the proxy credential, parses, verifies and yields the identity headers; the models route client
//! reads the R18 shape.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use gateway_api::credential::{boot_env, parse_proxy_authorization, proxy_authorization, token_from_proxy_url};
use gateway_api::models::Models;
use gateway_api::{Claims, FixedName, KeySet, TokenKeys, headers};
use hermit_api::ErrorBody;
use hermit_api::token::MasterKey;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let want: Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap()).unwrap();
    let typed: T = serde_json::from_value(want.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), want, "{name}");
    typed
}

const NAMES: &[&str] = &[
    "models",
    "error-not-allowed",
    "error-upstream-unreachable",
    "error-unavailable",
    "error-too-large",
];

#[test]
fn fixtures_round_trip_and_are_camel_case() {
    let m: Models = round_trip("models");
    assert!(m.models.iter().any(|x| x.id == m.default));
    for name in &NAMES[1..] {
        let e: ErrorBody = round_trip(name);
        assert!(e.fields.is_empty());
    }
    let files: BTreeSet<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, NAMES.iter().map(|n| format!("{n}.json")).collect());
    for name in NAMES {
        let text = std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let mut stack = vec![v];
        while let Some(v) = stack.pop() {
            match v {
                Value::Object(m) => {
                    for (k, v) in m {
                        assert!(!k.contains('_'), "{name}: {k}");
                        stack.push(v);
                    }
                }
                Value::Array(a) => stack.extend(a),
                _ => {}
            }
        }
    }
}

fn claims() -> Claims {
    Claims {
        sandbox_id: "0190f6c2-5b1e-7a3c-9d4e-1f2a3b4c5d6e".parse().unwrap(),
        conversation_id: "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f".parse().unwrap(),
        source: Ipv4Addr::new(10, 203, 4, 9),
        expires_at: 1_900_000_000,
        person: "staff@example.org".into(),
    }
}

#[test]
fn a_sandbox_request_from_boot_environment_to_identity_headers() {
    let key = MasterKey::new(1, [42; 32]);
    let signing = key.signing_key().unwrap();
    let token = signing.sign(&claims()).unwrap();

    // hermit puts the token only in the proxy URL; the supervisor reads it back from there (D1)
    let env = boot_env(&token, "192.0.2.1:3128");
    let https_proxy = &env.iter().find(|(k, _)| *k == "HTTPS_PROXY").unwrap().1;
    let mine = token_from_proxy_url(https_proxy).unwrap();
    assert_eq!(mine, token);

    // the client sends it as the proxy credential; the gateway parses and verifies it locally
    let header = proxy_authorization(&mine);
    let presented = parse_proxy_authorization(&header).unwrap();
    let set = KeySet::from_keys(&TokenKeys {
        keys: vec![signing.public_key()],
    });
    let verified = set
        .verify(&presented, 1_800_000_000, IpAddr::V4(Ipv4Addr::new(10, 203, 4, 9)))
        .unwrap();
    assert_eq!(verified, claims());

    // routed to a fixed name with the identity headers from the token
    let host = FixedName::from_host("hermit").unwrap();
    assert!(host.allows("GET", "/v1/sandbox/self"));
    let ids = headers::identity(&verified);
    assert_eq!(ids[0].1, "0190f6c2-5b1e-7a3c-9d4e-1f2a3b4c5d6e");
    assert_eq!(ids[2].1, "staff@example.org");
    assert!(headers::is_stripped("Proxy-Authorization"));
}

#[tokio::test]
async fn the_models_client() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = axum::Router::new().route(
        gateway_api::models::MODELS,
        axum::routing::get(|h: axum::http::HeaderMap| async move {
            assert_eq!(h["authorization"], "Bearer pigeon-token");
            assert_eq!(h["aviary-protocol"], "gateway=1");
            let v: Value = serde_json::from_str(&std::fs::read_to_string(dir().join("models.json")).unwrap()).unwrap();
            axum::Json(v)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let m = gateway_api::models::fetch(
        &reqwest::Client::new(),
        &format!("http://{addr}/"),
        Some("pigeon-token"),
    )
    .await
    .unwrap();
    assert_eq!(m, round_trip::<Models>("models"));
    let e = gateway_api::models::fetch(&reqwest::Client::new(), &format!("http://{addr}/x"), None)
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(404));
}

proptest! {
    #[test]
    fn credential_round_trip(token in "[A-Za-z0-9_.-]{1,500}") {
        prop_assert_eq!(parse_proxy_authorization(&proxy_authorization(&token)), Ok(token.clone()));
        let url = gateway_api::credential::proxy_url(&token, "192.0.2.1:3128");
        prop_assert_eq!(token_from_proxy_url(&url), Some(token));
    }

    #[test]
    fn no_path_escapes_the_sandbox_prefix(segs in prop::collection::vec("[a-z.%2eEfF/]{0,6}", 1..6)) {
        let path = format!("/v1/sandbox/{}", segs.join("/"));
        if FixedName::Hermit.allows("GET", &path) {
            // whatever is allowed has no dot segment and no encoded dot or slash
            let lower = path.to_ascii_lowercase();
            prop_assert!(!lower.contains("%2e") && !lower.contains("%2f"));
            prop_assert!(!path.split('/').any(|s| s == ".." || s == "."));
        }
    }
}
