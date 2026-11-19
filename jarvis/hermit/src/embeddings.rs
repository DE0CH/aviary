//! pigeon's query embeddings for the conversation search (Deyao, 2026-11-17): `POST :8082/v1/embeddings` relayed to
//! OpenRouter's embeddings API with the deployment's key (`HERMIT_OPENROUTER_KEY`). The messages' own vectors are
//! made on each session's machine (its gateway, `machine.rs`); only a search's query is embedded here.

use std::time::Duration;

use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};

/// A query's embedding is one short text: the search waits at most this long for it.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// A request body larger than this is no query.
pub const BODY_MAX: usize = 64 * 1024;

/// The JSON refusal of the gateway contract.
#[must_use]
pub fn refusal(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({"code": code, "message": message})),
    )
        .into_response()
}

/// Relays `body` to `<base>/v1/embeddings` with `key`; OpenRouter's answer as it came. No key: 503 (the search then
/// matches words only).
pub async fn relay(http: &reqwest::Client, base: &str, key: Option<&str>, body: Bytes) -> Response {
    let Some(key) = key else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "no OpenRouter key (HERMIT_OPENROUTER_KEY)",
        );
    };
    if body.len() > BODY_MAX {
        return refusal(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "a query's embedding is one short text",
        );
    }
    let sent = http
        .post(format!(
            "{}{}",
            base.trim_end_matches('/'),
            gateway_api::models::EMBEDDINGS
        ))
        .bearer_auth(key)
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(TIMEOUT)
        .body(body)
        .send()
        .await;
    let res = match sent {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "query embedding: OpenRouter unreachable");
            return refusal(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "OpenRouter is unreachable",
            );
        }
    };
    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    match res.bytes().await {
        Ok(b) => (status, [(header::CONTENT_TYPE, "application/json")], b).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "query embedding: the answer broke off");
            refusal(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "OpenRouter's answer broke off",
            )
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The query reaches OpenRouter's embeddings API with the key and its body as it came, and the answer comes back
    /// unchanged; with no key nothing is sent.
    #[tokio::test]
    async fn a_query_is_relayed_with_the_key() {
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let s = seen.clone();
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |h: axum::http::HeaderMap, b: Bytes| {
                let s = s.clone();
                async move {
                    let auth = h
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    s.lock()
                        .unwrap()
                        .push((auth, String::from_utf8(b.to_vec()).unwrap()));
                    axum::Json(serde_json::json!({"data": [{"index": 0, "embedding": [0.5]}]}))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let http = crate::central::http().unwrap();
        let body = r#"{"model":"qwen/qwen3-embedding-8b","input":["bus"],"dimensions":1024}"#;
        let r = relay(&http, &base, Some("or-key"), Bytes::from(body)).await;
        assert_eq!(r.status(), StatusCode::OK);
        let answer = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&answer).unwrap()["data"][0]["embedding"]
                [0],
            0.5
        );
        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![("Bearer or-key".to_owned(), body.to_owned())]
        );
        let r = relay(&http, &base, None, Bytes::from(body)).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
