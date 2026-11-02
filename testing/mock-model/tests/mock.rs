//! mock-model streams SSE byte-identically to recorded real responses (`recorded/*.sse`, recorded from the
//! upstream YYC's gateway relays to, see `recorded/README.md`), and its script, request log, models route,
//! `count_tokens` and error paths work as platform 7.5 states.
#![allow(clippy::unwrap_used)] // tests

use std::time::{Duration, Instant};

use base64::Engine as _;
use mock_model::MockModel;
use serde_json::{Value, json};

struct Rig {
    url: String,
    http: reqwest::Client,
}

impl Rig {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mock = MockModel::with_default_models();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, mock.router()).await.unwrap() });
        Self {
            url,
            http: reqwest::Client::new(),
        }
    }

    async fn script(&self, s: Value) {
        let res = self
            .http
            .post(format!("{}/__script", self.url))
            .json(&s)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 204, "{}", res.text().await.unwrap());
    }

    async fn messages(&self, body: Value, conversation: Option<&str>) -> reqwest::Response {
        let mut rb = self.http.post(format!("{}/v1/messages", self.url)).json(&body);
        if let Some(c) = conversation {
            rb = rb.header("aviary-sandbox-conversation", c);
        }
        rb.send().await.unwrap()
    }
}

fn recorded(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/recorded/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn request(text: &str, stream: bool) -> Value {
    json!({
        "model": "deepseek/deepseek-v4.1-flash-1017",
        "max_tokens": 200,
        "stream": stream,
        "messages": [{"role": "user", "content": text}]
    })
}

#[tokio::test]
async fn a_thinking_and_text_stream_is_byte_identical_to_the_recording() {
    let rig = Rig::new().await;
    rig.script(json!({
        "match": {"textContains": "Hello from the recording"},
        "message": {
            "id": "msg_vCBDz68alEIhnNqbYr4M", "provider": "Wafer",
            "inputTokens": 39, "outputTokens": 18, "thinkingTokens": 11, "cacheReadInputTokens": 0,
            "cost": "0.00001407", "promptCost": "0.00000507", "completionsCost": "0.000009"
        },
        "reply": [
            {"thinking": "We need answer exactly. Need comply. Just string.",
             "chunks": ["We", " need answer exactly", ". Need comply", ". Just", " string", "."]},
            {"text": "Hello from the recording.", "chunks": ["Hello from the", " recording."]}
        ]
    }))
    .await;
    let res = rig
        .messages(request("Reply with exactly: Hello from the recording.", true), None)
        .await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let got = res.bytes().await.unwrap();
    let want = recorded("openrouter-thinking-text.sse");
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "the stream differs from the recording"
    );
    assert_eq!(&got[..], &want[..]);
}

#[tokio::test]
async fn a_tool_use_stream_is_byte_identical_to_the_recording() {
    let rig = Rig::new().await;
    rig.script(json!({
        "message": {
            "id": "msg_I2mxpnF38OHoP7umfySc", "provider": "DigitalOcean",
            "inputTokens": 301, "outputTokens": 63, "thinkingTokens": 17,
            "cost": "0.00009954", "promptCost": "0.00005418", "completionsCost": "0.00004536"
        },
        "reply": [
            {"thinking": "The user wants me to run a simple command. Let me do it and verify.",
             "chunks": ["The", " user wants me to run a", " simple command. Let me do", " it and", " verify."]},
            {"toolUse": {"id": "call_221231a84c454661845ee0a0", "name": "Bash",
                         "input": {"command": "echo hi > /workspace/a.txt"}}}
        ]
    }))
    .await;
    let got = rig
        .messages(
            request("Use the Bash tool to run: echo hi > /workspace/a.txt", true),
            None,
        )
        .await
        .bytes()
        .await
        .unwrap();
    let want = recorded("openrouter-thinking-tool-use.sse");
    assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want));
}

#[tokio::test]
async fn unmatched_title_non_streaming_count_tokens_and_models() {
    let rig = Rig::new().await;
    // unmatched: the text "ok", streamed in the same framing
    let body = rig
        .messages(request("anything", true), None)
        .await
        .text()
        .await
        .unwrap();
    assert!(body.starts_with("event: message_start\n"));
    assert!(body.contains(r#""delta":{"type":"text_delta","text":"ok"}"#), "{body}");
    assert!(body.contains(r#""stop_reason":"end_turn""#));
    assert!(body.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    // the title model answers the fixed title, here as one JSON message
    let models: Value = rig
        .http
        .get(format!("{}/v1/aviary/models", rig.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let parsed: gateway_api::models::Models = serde_json::from_value(models.clone()).unwrap();
    let mut req = request("title please", false);
    req["model"] = json!(parsed.title);
    let msg: Value = rig.messages(req, None).await.json().await.unwrap();
    assert_eq!(msg["content"][0]["text"], mock_model::TITLE_TEXT);
    assert_eq!(msg["type"], "message");
    assert_eq!(msg["model"], json!(parsed.title));
    // count_tokens: a fixed estimate
    let n: Value = rig
        .http
        .post(format!("{}/v1/messages/count_tokens", rig.url))
        .json(&request("abc", false))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(n["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn scripts_match_by_conversation_and_nth_and_errors_and_pacing() {
    let rig = Rig::new().await;
    let conv = "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f";
    rig.script(json!({"match": {"conversation": conv, "nth": 2},
                      "reply": [{"error": {"status": 529, "type": "overloaded_error"}}]}))
        .await;
    rig.script(json!({"match": {"conversation": conv}, "times": 0,
                      "reply": [{"text": "a b c d", "chunkMs": 100}, {"error": {"status": 500, "type": "api_error"}}]}))
        .await;
    // 1st: the always-entry, paced: three pauses of 100 ms between four deltas, then an error event mid-stream
    let t = Instant::now();
    let body = rig.messages(request("x", true), Some(conv)).await.text().await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(300), "{:?}", t.elapsed());
    assert_eq!(body.matches("text_delta").count(), 4);
    assert!(body.contains("event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\""));
    assert!(!body.contains("message_stop"));
    // 2nd: the 529 as the HTTP answer
    let res = rig.messages(request("x", true), Some(conv)).await;
    assert_eq!(res.status(), 529);
    let e: Value = res.json().await.unwrap();
    assert_eq!(e["error"]["type"], "overloaded_error");
    // another conversation is not matched
    let other = rig.messages(request("x", true), None).await.text().await.unwrap();
    assert!(other.contains(r#""text":"ok""#));
    // a broken script is refused
    let res = rig
        .http
        .post(format!("{}/__script", rig.url))
        .json(&json!({"reply": [{"text": "a", "sleepMs": 1}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn the_request_log_has_images_tools_and_the_system_hash() {
    let rig = Rig::new().await;
    let conv = "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f";
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&2000u32.to_be_bytes());
    png.extend_from_slice(&1333u32.to_be_bytes());
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let req = json!({
        "model": "example/model-large-1017", "max_tokens": 10, "stream": false,
        "system": [{"type": "text", "text": "You are the deployment's assistant."}],
        "tools": [{"name": "Bash", "input_schema": {}}, {"name": "mcp__tools__search", "input_schema": {}}],
        "messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}},
            {"type": "text", "text": "what is this"}
        ]}]
    });
    assert_eq!(rig.messages(req, Some(conv)).await.status(), 200);
    let log: Value = rig
        .http
        .get(format!("{}/__requests?conversation={conv}", rig.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = &log[0];
    assert_eq!(r["messages"], 1);
    assert_eq!(
        r["images"],
        json!([{"width": 2000, "height": 1333, "bytes": png.len()}])
    );
    assert_eq!(r["tools"], json!(["Bash", "mcp__tools__search"]));
    assert_eq!(r["system"].as_str().unwrap().len(), 64);
    assert_eq!(r["text"], "what is this");
    let none: Value = rig
        .http
        .get(format!(
            "{}/__requests?conversation=0190f6c2-0000-7000-8000-000000000000",
            rig.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(none, json!([]));
    rig.http.post(format!("{}/__reset", rig.url)).send().await.unwrap();
    let all: Value = rig
        .http
        .get(format!("{}/__requests", rig.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all, json!([]));
}
