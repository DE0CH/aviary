//! `aviary-conformance model` (platform 8.3): the models route (R18) and the Messages API a deployment's model
//! route serves (non-streaming, streaming, `count_tokens`), as Claude Code and pigeon read them.
//!
//! `--target` is called directly with pigeon's bearer; with `--proxy` and `--sandbox-token` the Messages API calls
//! go through a gateway's proxy as `http://model/…`, the way a sandbox makes them.

use std::time::Duration;

use gateway_api::credential::proxy_authorization;
use gateway_api::models::{COUNT_TOKENS, MESSAGES, MODELS, Models};
use serde_json::{Value, json};

use crate::proxy::{self, Conn, full};
use crate::report::{Outcome, Problem, Report};
use crate::{ensure, fail};

/// The suite's inputs.
pub struct Options {
    pub target: String,
    /// pigeon's pair token (the in-cluster bearer).
    pub token: String,
    /// The Messages API through a proxy: its address and a sandbox token.
    pub proxy: Option<(std::net::SocketAddr, String)>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("target", &self.target)
            .field("proxy", &self.proxy.as_ref().map(|p| p.0))
            .finish_non_exhaustive()
    }
}

struct Suite {
    o: Options,
    http: reqwest::Client,
}

impl Suite {
    /// One call: status, content type, body.
    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, String, Vec<u8>), Problem> {
        if let (Some((addr, token)), true) = (&self.o.proxy, path != MODELS) {
            let mut c = Conn::open(*addr).await?;
            let b = body
                .map(|b| serde_json::to_vec(b).unwrap_or_default())
                .unwrap_or_default();
            let req = proxy::request(
                method,
                &format!("http://model{path}"),
                Some(&proxy_authorization(token)),
                &[
                    ("content-type", "application/json"),
                    ("anthropic-version", "2023-06-01"),
                ],
                full(b),
            );
            let res = c.send(req).await?;
            let st = res.status().as_u16();
            let ct = content_type(res.headers());
            let read = proxy::read_body(res.into_body(), Duration::from_secs(120)).await;
            if let Some(e) = read.error {
                fail!("{path}: {e}");
            }
            return Ok((st, ct, read.bytes));
        }
        let url = format!("{}{path}", self.o.target.trim_end_matches('/'));
        let m: reqwest::Method = method.parse().map_err(|_| Problem::Fail(method.into()))?;
        let mut rb = self
            .http
            .request(m, url)
            .bearer_auth(&self.o.token)
            .header("anthropic-version", "2023-06-01")
            .timeout(Duration::from_secs(120));
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let res = rb.send().await.map_err(|e| Problem::Fail(format!("{path}: {e}")))?;
        let st = res.status().as_u16();
        let ct = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let b = res.bytes().await.map_err(|e| Problem::Fail(e.to_string()))?.to_vec();
        Ok((st, ct, b))
    }
}

fn content_type(h: &hyper::HeaderMap) -> String {
    h.get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Runs the model suite, recording into `report`.
pub async fn run(o: Options, report: &Report) {
    let s = Suite {
        o,
        http: reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .no_proxy()
            .build()
            .unwrap_or_default(),
    };
    let models = match models(&s).await {
        Ok(m) => {
            report.record("models", Ok(()), Duration::ZERO);
            Some(m)
        }
        Err(p) => {
            report.record("models", Err(p), Duration::ZERO);
            None
        }
    };
    let model = models.map_or_else(|| "conformance".to_owned(), |m| m.default);
    report.check("messages", messages(&s, &model)).await;
    report.check("messages.stream", stream(&s, &model)).await;
    report.check("count-tokens", count_tokens(&s, &model)).await;
}

async fn models(s: &Suite) -> Result<Models, Problem> {
    let (st, _, b) = s.call("GET", MODELS, None).await?;
    ensure!(st == 200, "GET {MODELS}: {st} {}", String::from_utf8_lossy(&b));
    let m: Models = serde_json::from_slice(&b).map_err(|e| Problem::Fail(format!("not the R18 shape: {e}")))?;
    ensure!(
        m.models.iter().any(|x| x.id == m.default),
        "default {:?} not in models",
        m.default
    );
    ensure!(!m.title.is_empty() && !m.small.is_empty(), "small or title empty");
    ensure!(m.context_tokens > 0, "contextTokens 0");
    Ok(m)
}

fn ask(model: &str, stream: bool) -> Value {
    json!({"model": model, "max_tokens": 32, "stream": stream,
        "messages": [{"role": "user", "content": "aviary-conformance: answer with one word"}]})
}

async fn messages(s: &Suite, model: &str) -> Outcome {
    let (st, ct, b) = s.call("POST", MESSAGES, Some(&ask(model, false))).await?;
    ensure!(st == 200, "{st} {}", String::from_utf8_lossy(&b));
    ensure!(ct.starts_with("application/json"), "content-type {ct:?}");
    let v: Value = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    ensure!(v["type"] == "message" && v["role"] == "assistant", "not a message: {v}");
    ensure!(
        v["content"].as_array().is_some_and(|c| !c.is_empty()),
        "no content: {v}"
    );
    ensure!(
        v["usage"]["input_tokens"].is_u64() && v["usage"]["output_tokens"].is_u64(),
        "no usage: {v}"
    );
    ensure!(v.get("stop_reason").is_some(), "no stop_reason: {v}");
    Ok(())
}

/// The `event:`/`data:` pairs of an SSE body.
fn events(body: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for block in body.split("\n\n") {
        let mut event = None;
        let mut data = String::new();
        for line in block.lines() {
            if let Some(e) = line.strip_prefix("event:") {
                event = Some(e.trim().to_owned());
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim_start());
            }
        }
        if let Some(e) = event {
            out.push((e, serde_json::from_str(&data).unwrap_or(Value::Null)));
        }
    }
    out
}

async fn stream(s: &Suite, model: &str) -> Outcome {
    let (st, ct, b) = s.call("POST", MESSAGES, Some(&ask(model, true))).await?;
    ensure!(st == 200, "{st} {}", String::from_utf8_lossy(&b));
    ensure!(ct.starts_with("text/event-stream"), "content-type {ct:?}");
    let evs = events(&String::from_utf8_lossy(&b));
    let names: Vec<&str> = evs.iter().map(|(e, _)| e.as_str()).filter(|e| *e != "ping").collect();
    ensure!(
        names.first() == Some(&"message_start"),
        "first event {:?}",
        names.first()
    );
    ensure!(names.last() == Some(&"message_stop"), "last event {:?}", names.last());
    ensure!(names.contains(&"message_delta"), "no message_delta");
    let mut open: Option<u64> = None;
    for (e, d) in &evs {
        ensure!(d.is_object(), "{e}: data is not JSON");
        ensure!(d["type"] == e.as_str(), "{e}: data.type {}", d["type"]);
        match e.as_str() {
            "content_block_start" => {
                ensure!(open.is_none(), "a block started inside another");
                open = d["index"].as_u64();
            }
            "content_block_delta" => ensure!(
                open == d["index"].as_u64() && open.is_some(),
                "a delta outside its block"
            ),
            "content_block_stop" => {
                ensure!(open == d["index"].as_u64(), "a stop of a block that is not open");
                open = None;
            }
            "message_delta" => ensure!(open.is_none(), "message_delta inside a block"),
            _ => {}
        }
    }
    ensure!(names.contains(&"content_block_delta"), "no content");
    Ok(())
}

async fn count_tokens(s: &Suite, model: &str) -> Outcome {
    let body = json!({"model": model, "messages": [{"role": "user", "content": "aviary-conformance"}]});
    let (st, _, b) = s.call("POST", COUNT_TOKENS, Some(&body)).await?;
    ensure!(st == 200, "{st} {}", String::from_utf8_lossy(&b));
    let v: Value = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    ensure!(v["input_tokens"].is_u64(), "no input_tokens: {v}");
    Ok(())
}
