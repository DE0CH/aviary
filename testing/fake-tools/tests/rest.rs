//! fake-tools serves the tools REST API (decision 15) with both tools, refuses a wrong bearer, and records each call
//! with the person the gateway named.
#![allow(clippy::unwrap_used)] // tests

use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TOKEN: &str = "tools-pair-token";

async fn serve(tools: &std::sync::Arc<fake_tools::FakeTools>) -> String {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = tools.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

async fn post(http: &reqwest::Client, url: &str, token: &str, body: &str) -> reqwest::Response {
    http.post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .header("aviary-sandbox-person", "staff@example.org")
        .header("aviary-sandbox-conversation", "0190f6c2-0000-7000-8000-000000000002")
        .body(body.to_owned())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn list_call_openapi_and_recorded_calls() {
    let tools = fake_tools::FakeTools::new(TOKEN);
    let base = serve(&tools).await;
    let http = reqwest::Client::new();
    let list: Value = http
        .get(format!("{base}/v1/tools"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["echo", "slow_echo"]);
    assert_eq!(list[0]["inputSchema"]["required"], json!(["text"]));
    assert!(list[0]["title"].is_string() && list[0]["description"].is_string());

    let r = post(&http, &format!("{base}/v1/tools/echo"), TOKEN, r#"{"text": "回聲"}"#).await;
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/plain"));
    assert_eq!(r.text().await.unwrap(), "回聲");

    let t0 = Instant::now();
    let r = post(
        &http,
        &format!("{base}/v1/tools/slow_echo"),
        TOKEN,
        r#"{"text": "s", "delayMs": 300}"#,
    )
    .await;
    assert_eq!(r.status(), 200);
    assert!(t0.elapsed() >= Duration::from_millis(300));

    let calls = tools.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].tool, "echo");
    assert_eq!(calls[0].arguments, json!({"text": "回聲"}));
    assert_eq!(calls[0].person.as_deref(), Some("staff@example.org"));
    assert_eq!(
        calls[0].conversation.as_deref(),
        Some("0190f6c2-0000-7000-8000-000000000002")
    );
    let recorded: Value = http
        .get(format!("{base}/__calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recorded[1]["tool"], "slow_echo");

    let api: Value = http
        .get(format!("{base}/v1/openapi.json"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(api["openapi"], "3.1.0");
    assert!(api["paths"]["/v1/tools/echo"]["post"].is_object());

    assert_eq!(http.post(format!("{base}/__reset")).send().await.unwrap().status(), 204);
    assert_eq!(tools.calls().len(), 0);
}

#[tokio::test]
async fn bad_arguments_unknown_tools_and_wrong_bearers() {
    let tools = fake_tools::FakeTools::new(TOKEN);
    let base = serve(&tools).await;
    let http = reqwest::Client::new();
    let echo = format!("{base}/v1/tools/echo");
    for body in ["[1]", "not json", r#"{"text": 1}"#, "{}"] {
        let r = post(&http, &echo, TOKEN, body).await;
        assert_eq!(r.status(), 400, "{body}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["code"], "bad_request");
        assert!(v["message"].is_string());
    }
    let r = post(&http, &format!("{base}/v1/tools/slow_echo"), TOKEN, r#"{"text": "s"}"#).await;
    assert_eq!(r.status(), 400);
    let r = post(&http, &format!("{base}/v1/tools/nope"), TOKEN, "{}").await;
    assert_eq!(r.status(), 404);
    let r = post(&http, &echo, "wrong", r#"{"text": "x"}"#).await;
    assert_eq!(r.status(), 401);
    assert!(r.headers().contains_key("www-authenticate"));
    assert_eq!(http.get(format!("{base}/v1/tools")).send().await.unwrap().status(), 401);
    assert_eq!(
        http.post(format!("{base}/mcp"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(tools.calls().len(), 0);
}

#[tokio::test]
async fn a_token_rotation_accepts_both_tokens_then_only_the_new_one() {
    let tools = fake_tools::FakeTools::with_tokens(&["old-token", "next-token"]);
    let base = serve(&tools).await;
    let http = reqwest::Client::new();
    let echo = format!("{base}/v1/tools/echo");
    let body = r#"{"text": "x"}"#;
    assert_eq!(post(&http, &echo, "old-token", body).await.status(), 200);
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
    tools.set_tokens(&["next-token"]);
    assert_eq!(post(&http, &echo, "old-token", body).await.status(), 401);
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
}

#[tokio::test]
async fn a_token_file_changed_in_place_changes_the_accepted_tokens() {
    let dir = std::env::temp_dir().join(format!("fake-tools-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tokens");
    // a mounted Secret is swapped by a rename, as the kubelet's `..data` link is
    let swap = |text: &str| {
        let tmp = dir.join("tokens.new");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, &file).unwrap();
    };
    swap("old-token\n");
    assert!(fake_tools::FakeTools::from_token_file(&dir.join("absent")).is_err());
    let tools = fake_tools::FakeTools::from_token_file(&file).unwrap();
    let base = serve(&tools).await;
    let http = reqwest::Client::new();
    let echo = format!("{base}/v1/tools/echo");
    let body = r#"{"text": "x"}"#;
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 401);
    // the overlap: both
    swap("old-token\nnext-token\n");
    assert_eq!(post(&http, &echo, "old-token", body).await.status(), 200);
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
    // the old one dropped
    swap("next-token\n");
    assert_eq!(post(&http, &echo, "old-token", body).await.status(), 401);
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
    // an empty or missing file keeps the last set
    swap("\n");
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(post(&http, &echo, "next-token", body).await.status(), 200);
    assert_eq!(post(&http, &echo, "old-token", body).await.status(), 401);
    std::fs::remove_dir_all(&dir).unwrap();
}
