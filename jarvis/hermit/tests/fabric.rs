//! hermit-jarvis and the fabric hermit (src/fabric.rs): the fabric hermit's control hashes for a fabric machine's
//! sandbox, read as caller gateway with the identity headers its guard wants.

#![allow(clippy::unwrap_used)]

use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use jarvis_aviary::fabric::FabricHermit;
use serde_json::json;
use uuid::Uuid;

const SANDBOX: &str = "0190f6c2-5b1e-7a3c-9d4e-1f2a3b4c5d6e";
const CONVERSATION: &str = "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f";

fn gateway(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer gw-token")
}

async fn fake(live: bool) -> String {
    let app = axum::Router::new()
        .route(
            "/v1/sandbox-tokens/{id}",
            get(move |h: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>| async move {
                if !gateway(&h) || h.get("aviary-protocol").and_then(|v| v.to_str().ok()) != Some("hermit-core=1") {
                    return (StatusCode::FORBIDDEN, axum::Json(json!({"code": "wrong_caller"})));
                }
                if id != SANDBOX {
                    return (StatusCode::NOT_FOUND, axum::Json(json!({"code": "no_sandbox"})));
                }
                (
                    StatusCode::OK,
                    axum::Json(json!({"sandboxId": SANDBOX, "conversationId": CONVERSATION, "person": "deyao@example.org",
                                      "state": "starting", "live": live, "expiresAt": "2027-01-01T00:00:00.000Z"})),
                )
            }),
        )
        .route(
            "/v1/sandbox/self",
            get(|h: HeaderMap| async move {
                let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
                if !gateway(&h)
                    || hv("aviary-protocol") != "hermit-sandbox=1"
                    || hv("aviary-sandbox-id") != SANDBOX
                    || hv("aviary-sandbox-conversation") != CONVERSATION
                    || hv("aviary-sandbox-person") != "deyao@example.org"
                {
                    return (StatusCode::NOT_FOUND, axum::Json(json!({"code": "no_sandbox"})));
                }
                (StatusCode::OK, axum::Json(json!({"control": {"port": 7070, "tokenSha256": ["a".repeat(64), "b".repeat(64)]}})))
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

#[tokio::test]
async fn the_fabric_hermits_control_hashes_for_a_fabric_machine() {
    let http = jarvis_aviary::central::http().unwrap();
    let f = FabricHermit {
        url: fake(true).await,
        gateway_token: "gw-token".into(),
    };
    let sandbox =
        jarvis_aviary::fabric::sandbox_of_machine("x0190f6c25b1e7a3c9d4e1f2a3b4c5d6e").unwrap();
    assert_eq!(
        f.control_hashes(&http, sandbox).await.unwrap(),
        ["a".repeat(64), "b".repeat(64)]
    );
    // a sandbox the fabric hermit does not know, a wrong token, a sandbox that is gone: no hashes, the boot fails
    assert!(f.control_hashes(&http, Uuid::new_v4()).await.is_err());
    let wrong = FabricHermit {
        gateway_token: "other".into(),
        ..f.clone()
    };
    assert!(wrong.control_hashes(&http, sandbox).await.is_err());
    let gone = FabricHermit {
        url: fake(false).await,
        gateway_token: "gw-token".into(),
    };
    assert!(gone.control_hashes(&http, sandbox).await.is_err());
}
