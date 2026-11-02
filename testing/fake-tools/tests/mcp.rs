//! fake-tools speaks the MCP Streamable-HTTP handshake and both tools, refuses a wrong bearer, and records each
//! call with the person the gateway named.
#![allow(clippy::unwrap_used)] // tests

use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TOKEN: &str = "tools-pair-token";

async fn rig() -> (String, reqwest::Client, std::sync::Arc<fake_tools::FakeTools>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tools = fake_tools::FakeTools::new(TOKEN);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let router = tools.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, reqwest::Client::new(), tools)
}

async fn rpc(http: &reqwest::Client, url: &str, token: &str, session: Option<&str>, body: Value) -> reqwest::Response {
    let mut rb = http
        .post(url)
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .header("aviary-sandbox-person", "staff@example.org")
        .json(&body);
    if let Some(s) = session {
        rb = rb.header("mcp-session-id", s);
    }
    rb.send().await.unwrap()
}

#[tokio::test]
async fn handshake_tools_and_recorded_calls() {
    let (url, http, tools) = rig().await;
    let init = rpc(
        &http,
        &url,
        TOKEN,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
    )
    .await;
    assert_eq!(init.status(), 200);
    let session = init.headers()["mcp-session-id"].to_str().unwrap().to_owned();
    let init: Value = init.json().await.unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "fake-tools");
    let note = rpc(
        &http,
        &url,
        TOKEN,
        Some(&session),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(note.status(), 202);
    let list: Value = rpc(
        &http,
        &url,
        TOKEN,
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await
    .json()
    .await
    .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["echo", "slow_echo"]);
    let echo: Value = rpc(
        &http,
        &url,
        TOKEN,
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "echo", "arguments": {"text": "你好"}}}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(echo["result"]["content"][0]["text"], "你好");
    let t = Instant::now();
    let slow: Value = rpc(
        &http,
        &url,
        TOKEN,
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "slow_echo", "arguments": {"text": "late", "delayMs": 300}}}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert!(t.elapsed() >= Duration::from_millis(300));
    assert_eq!(slow["result"]["content"][0]["text"], "late");
    let unknown: Value = rpc(
        &http,
        &url,
        TOKEN,
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "nope", "arguments": {}}}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32602);

    let calls = tools.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].person.as_deref(), Some("staff@example.org"));
    assert_eq!(calls[1].tool, "slow_echo");
    assert_eq!(calls[0].session.as_deref(), Some(session.as_str()));
    let recorded: Value = http
        .get(url.replace("/mcp", "/__calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recorded[0]["person"], "staff@example.org");
}

#[tokio::test]
async fn a_wrong_or_missing_bearer_is_refused() {
    let (url, http, tools) = rig().await;
    let r = rpc(
        &http,
        &url,
        "wrong",
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
    )
    .await;
    assert_eq!(r.status(), 401);
    let r = http
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(tools.calls(), vec![], "nothing recorded");
}
