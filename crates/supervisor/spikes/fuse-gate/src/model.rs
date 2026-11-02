//! Mock Anthropic Messages API, scripted by `GATE:` lines in the conversation's last prompt:
//!   GATE: tool NAME {json input}   one tool call per model response, in order
//!   GATE: text N DELAY_MS          the final answer in N chunks, DELAY_MS apart (each chunk carries a unique token)
//!   GATE: failonce ID              the first response streams a text block, then an overloaded error mid-stream
//!   GATE: trunconce ID             the first response streams a text block, then the stream ends without
//!                                  message_stop (Claude Code tombstones the orphaned message and retries)
//!                                  (how: close, reset, stall, garbage, refusal, refusalcat = a refusal with
//!                                  stop_details category cyber; several trunconce lines fire one per request)
//!   GATE: thinkfail ID [ERRTYPE]   the first response streams one complete thinking block, then an `error` event of
//!                                  ERRTYPE (default api_error) mid-stream: with --fallback-model, Claude Code 2.1.304
//!                                  moves to the fallback model and tombstones the thinking-only message
//!                                  (abandonAttempt("chain_advance"))
//! Every chunk's send time is logged, so token gaps can be measured against Claude Code's stream-json output.

use crate::http::{self, Reply, Request};
use crate::magpie::now_us;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static REQ: AtomicU64 = AtomicU64::new(0);

fn text_of(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter(|b| b["type"] == "text")
            .map(|b| b["text"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// "@@BIG:n" in a tool input string becomes n KiB of text lines (grows the transcript cheaply).
fn big(v: &mut Value) {
    match v {
        Value::String(s) => {
            if let Some(n) = s.strip_prefix("@@BIG:").and_then(|n| n.parse::<usize>().ok()) {
                let line = "0123456789abcdef".repeat(63) + "\n";
                *s = line.repeat(n * 1024 / line.len() + 1);
            }
        }
        Value::Object(m) => m.values_mut().for_each(big),
        Value::Array(a) => a.iter_mut().for_each(big),
        _ => {}
    }
}

enum Plan {
    Tool(String, Value),
    Text(usize, u64),
    Fail(usize, u64),
    Trunc(usize, u64, String),
    ThinkFail(String),
}

fn plan(body: &Value, failed: &Mutex<HashSet<String>>) -> Plan {
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    let Some(k) = msgs
        .iter()
        .rposition(|m| m["role"] == "user" && text_of(&m["content"]).contains("GATE:"))
    else {
        return Plan::Text(3, 0);
    };
    let after = msgs[k + 1..].iter().filter(|m| m["role"] == "assistant").count();
    let text = text_of(&msgs[k]["content"]);
    let mut tools = Vec::new();
    let (mut n, mut d) = (5usize, 0u64);
    let mut fail = None;
    let mut trunc = Vec::new();
    let mut think = None;
    for l in text.lines() {
        let Some(rest) = l.trim().strip_prefix("GATE:") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(t) = rest.strip_prefix("tool ") {
            let (name, js) = t.split_once(' ').unwrap_or((t, "{}"));
            let mut v: Value = serde_json::from_str(js).unwrap_or(json!({}));
            big(&mut v);
            tools.push((name.to_string(), v));
        } else if let Some(t) = rest.strip_prefix("text ") {
            let mut it = t.split_whitespace();
            n = it.next().and_then(|x| x.parse().ok()).unwrap_or(5);
            d = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        } else if let Some(t) = rest.strip_prefix("failonce ") {
            fail = Some(t.trim().to_string());
        } else if let Some(t) = rest.strip_prefix("thinkfail ") {
            let mut it = t.split_whitespace();
            think = Some((
                it.next().unwrap_or("").to_string(),
                it.next().unwrap_or("api_error").to_string(),
            ));
        } else if let Some(t) = rest.strip_prefix("trunconce ") {
            let mut it = t.split_whitespace();
            trunc.push((
                it.next().unwrap_or("").to_string(),
                it.next().unwrap_or("close").to_string(),
            ));
        }
    }
    if let Some(id) = fail
        && failed.lock().unwrap().insert(id)
    {
        return Plan::Fail(n, d);
    }
    if let Some((id, kind)) = think
        && failed.lock().unwrap().insert(id)
    {
        return Plan::ThinkFail(kind);
    }
    // several trunconce lines fire one per request, in order (e.g. a refusal and then a refused continuation)
    for (id, how) in trunc {
        if failed.lock().unwrap().insert(id) {
            return Plan::Trunc(n, d, how);
        }
    }
    if after < tools.len() {
        let (a, b) = tools.swap_remove(after);
        return Plan::Tool(a, b);
    }
    Plan::Text(n, d)
}

fn ev(w: &mut TcpStream, name: &str, data: &Value) -> std::io::Result<()> {
    w.write_all(format!("event: {name}\ndata: {data}\n\n").as_bytes())?;
    w.flush()
}

pub fn run(listen: &str, logp: &str) -> std::io::Result<()> {
    let log = Mutex::new(OpenOptions::new().create(true).append(true).open(logp)?);
    let log: &'static Mutex<std::fs::File> = Box::leak(Box::new(log));
    let failed: &'static Mutex<HashSet<String>> = Box::leak(Box::new(Mutex::new(HashSet::new())));
    eprintln!("mock model on {listen}");
    http::serve(listen, move |req: Request, w: &mut TcpStream| {
        let lg = |s: String| {
            let _ = log.lock().unwrap().write_all(format!("{} {s}\n", now_us()).as_bytes());
        };
        if req.path.starts_with("/v1/models") {
            return Reply::Full(200, "application/json", json!({"data": [{"id": "claude-sonnet-4-5", "type": "model", "display_name": "x", "created_at": "2025-01-01T00:00:00Z"}, {"id": "claude-haiku-4-5", "type": "model", "display_name": "y", "created_at": "2025-01-01T00:00:00Z"}], "has_more": false}).to_string().into_bytes());
        }
        if req.path.starts_with("/v1/messages/count_tokens") {
            return Reply::Full(200, "application/json", b"{\"input_tokens\":100}".to_vec());
        }
        if !req.path.starts_with("/v1/messages") {
            lg(format!("unrouted {} {}", req.method, req.path));
            return Reply::Full(404, "application/json", b"{}".to_vec());
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
        let id = REQ.fetch_add(1, Ordering::SeqCst);
        let model = body["model"].as_str().unwrap_or("claude-sonnet-4-5").to_string();
        let p = plan(&body, failed);
        let stream = body["stream"].as_bool().unwrap_or(false);
        let nmsg = body["messages"].as_array().map_or(0, Vec::len);
        let tools: Vec<&str> = body["tools"]
            .as_array()
            .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
            .unwrap_or_default();
        lg(format!("req={id} tools={}", tools.join(",")));
        let usage = json!({"input_tokens": 100, "output_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0});
        match &p {
            Plan::Tool(n, _) => lg(format!(
                "req={id} recv msgs={nmsg} model={model} stream={stream} plan=tool:{n}"
            )),
            Plan::Text(n, d) => lg(format!(
                "req={id} recv msgs={nmsg} model={model} stream={stream} plan=text:{n}x{d}ms"
            )),
            Plan::Fail(..) => lg(format!(
                "req={id} recv msgs={nmsg} model={model} stream={stream} plan=failonce"
            )),
            Plan::Trunc(..) => lg(format!(
                "req={id} recv msgs={nmsg} model={model} stream={stream} plan=trunconce"
            )),
            Plan::ThinkFail(k) => lg(format!(
                "req={id} recv msgs={nmsg} model={model} stream={stream} plan=thinkfail:{k}"
            )),
        }
        if !stream {
            let content = match &p {
                Plan::Tool(n, i) => {
                    json!([{"type": "tool_use", "id": format!("toolu_{id:08}"), "name": n, "input": i}])
                }
                _ => json!([{"type": "text", "text": format!("ok r{id}")}]),
            };
            let stop = if matches!(p, Plan::Tool(..)) {
                "tool_use"
            } else {
                "end_turn"
            };
            let m = json!({"id": format!("msg_{id:08}"), "type": "message", "role": "assistant", "model": model, "content": content, "stop_reason": stop, "stop_sequence": null, "usage": usage});
            return Reply::Full(200, "application/json", m.to_string().into_bytes());
        }
        let r = (|| -> std::io::Result<()> {
            w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
            ev(
                w,
                "message_start",
                &json!({"type": "message_start", "message": {"id": format!("msg_{id:08}"), "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null, "usage": usage}}),
            )?;
            let stop = match &p {
                Plan::ThinkFail(kind) => {
                    ev(
                        w,
                        "content_block_start",
                        &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
                    )?;
                    ev(
                        w,
                        "content_block_delta",
                        &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": format!("gate thinking r{id}")}}),
                    )?;
                    ev(
                        w,
                        "content_block_delta",
                        &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "Z2F0ZS1zaWduYXR1cmU="}}),
                    )?;
                    ev(
                        w,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": 0}),
                    )?;
                    thread::sleep(Duration::from_millis(300));
                    ev(
                        w,
                        "error",
                        &json!({"type": "error", "error": {"type": kind, "message": "gate thinkfail"}}),
                    )?;
                    lg(format!("req={id} thinkfail ({kind}) after a thinking block"));
                    return Ok(());
                }
                Plan::Tool(n, input) => {
                    ev(
                        w,
                        "content_block_start",
                        &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": format!("toolu_{id:08}"), "name": n, "input": {}}}),
                    )?;
                    ev(
                        w,
                        "content_block_delta",
                        &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
                    )?;
                    ev(
                        w,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": 0}),
                    )?;
                    "tool_use"
                }
                Plan::Text(n, d) | Plan::Fail(n, d) | Plan::Trunc(n, d, _) => {
                    ev(
                        w,
                        "content_block_start",
                        &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                    )?;
                    for i in 0..*n {
                        if *d > 0 && i > 0 {
                            thread::sleep(Duration::from_millis(*d));
                        }
                        let tok = format!("tk{id}x{i} ");
                        ev(
                            w,
                            "content_block_delta",
                            &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": tok}}),
                        )?;
                        lg(format!("req={id} chunk={i} token={}", tok.trim()));
                    }
                    ev(
                        w,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": 0}),
                    )?;
                    if let Plan::Trunc(_, _, how) = &p {
                        thread::sleep(Duration::from_millis(300));
                        match how.as_str() {
                            "reset" => {
                                use std::os::fd::AsRawFd;
                                let l = libc::linger {
                                    l_onoff: 1,
                                    l_linger: 0,
                                };
                                // SAFETY: setsockopt on our own socket; the close sends RST.
                                unsafe {
                                    libc::setsockopt(
                                        w.as_raw_fd(),
                                        libc::SOL_SOCKET,
                                        libc::SO_LINGER,
                                        (&raw const l).cast(),
                                        std::mem::size_of::<libc::linger>() as u32,
                                    );
                                }
                                let _ = w.shutdown(std::net::Shutdown::Read);
                            }
                            "stall" => thread::sleep(Duration::from_secs(20)),
                            "refusalcat" => {
                                ev(
                                    w,
                                    "message_delta",
                                    &json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_sequence": null, "stop_details": {"type": "refusal", "category": "cyber", "explanation": "gate refusal"}}, "usage": {"output_tokens": 10}}),
                                )?;
                                ev(w, "message_stop", &json!({"type": "message_stop"}))?;
                            }
                            "refusal" => {
                                ev(
                                    w,
                                    "message_delta",
                                    &json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_sequence": null}, "usage": {"output_tokens": 10}}),
                                )?;
                                ev(w, "message_stop", &json!({"type": "message_stop"}))?;
                            }
                            "garbage" => {
                                w.write_all(b"event: content_block_delta\ndata: {not json\n\n")?;
                            }
                            _ => {}
                        }
                        lg(format!("req={id} truncated ({how}, no message_stop)"));
                        return Ok(());
                    }
                    if matches!(p, Plan::Fail(..)) {
                        thread::sleep(Duration::from_millis(300));
                        ev(
                            w,
                            "error",
                            &json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded (gate failonce)"}}),
                        )?;
                        lg(format!("req={id} failed mid-stream"));
                        return Ok(());
                    }
                    "end_turn"
                }
            };
            ev(
                w,
                "message_delta",
                &json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 10}}),
            )?;
            ev(w, "message_stop", &json!({"type": "message_stop"}))?;
            lg(format!("req={id} done stop={stop}"));
            Ok(())
        })();
        if let Err(e) = r {
            lg(format!("req={id} write error {e}"));
        }
        Reply::Streamed
    })
}
