//! Render parity of the tool-call mapper (WP-17 acceptance; cutover 10.2 item 6's input): every tool call the old
//! `CloudCLI` bridge rendered in its recorded frame log (`testing/paseo-contract/fixtures/cloudcli-bridge-*.ndjson`,
//! made from imported-conversation-like transcripts) is rebuilt as the magpie row it becomes (name, input, status,
//! result text) and projected by pigeon: the item equals the bridge's, except MCP tools, which pigeon names without
//! their server so that history written under the old server name and `mcp__tools__*` calls render the same
//! (spec 7.8, 18.1).
#![allow(clippy::unwrap_used)] // tests

use magpie_api::rows::{ToolItem, ToolResult, ToolStatus};
use pigeon::paseo::mapping::{tool_call_item, tool_name};
use pigeon::paseo::strings::Locale;
use serde_json::Value;

fn bridge_tool_calls() -> Vec<Value> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testing/paseo-contract/fixtures");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cloudcli-bridge-")
        {
            continue;
        }
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            collect(&v, &mut out);
        }
    }
    out
}

fn collect(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("tool_call") {
                out.push(v.clone());
            }
            o.values().for_each(|x| collect(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// The row magpie keeps for the call the bridge rendered: the input and the result text as the transcript had them.
fn row_of(item: &Value) -> ToolItem {
    let detail = &item["detail"];
    let s = |k: &str| detail.get(k).and_then(Value::as_str).map(str::to_owned);
    let (input, output) = match detail["type"].as_str().unwrap() {
        "unknown" => (detail["input"].clone(), detail["output"].as_str().map(str::to_owned)),
        "shell" => (serde_json::json!({"command": s("command")}), s("output")),
        "read" => (serde_json::json!({"file_path": s("filePath")}), s("content")),
        "write" => (
            serde_json::json!({"file_path": s("filePath"), "content": s("content")}),
            None,
        ),
        "search" => (serde_json::json!({"pattern": s("query")}), None),
        other => panic!("detail {other} not in the bridge log"),
    };
    let status = match item["status"].as_str().unwrap() {
        "running" => ToolStatus::Running,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => ToolStatus::Canceled,
    };
    let error_text = item["error"]["message"].as_str().map(str::to_owned);
    ToolItem {
        call_id: item["callId"].as_str().unwrap().to_owned(),
        message_id: "msg".into(),
        block_index: 0,
        name: item["name"].as_str().unwrap().to_owned(),
        input,
        status,
        result: output.or(error_text.clone()).map(|text| ToolResult {
            bytes: text.len() as u64,
            text,
            truncated: false,
            blob_id: None,
            is_error: status == ToolStatus::Failed,
            images: None,
        }),
        sent_files: None,
        task: None,
        error: None,
    }
}

#[test]
fn tool_calls_render_as_the_bridge_rendered_them() {
    let calls = bridge_tool_calls();
    assert!(calls.len() >= 20, "{}", calls.len());
    let mut compared = 0;
    for bridge in &calls {
        let name = bridge["name"].as_str().unwrap();
        let mut ours = tool_call_item(&row_of(bridge), Locale::En).unwrap();
        // the bridge's SendUserFile cards came from its own snapshot; magpie's are a field of the row (A2.F21)
        if let Some(o) = ours.as_object_mut() {
            o.remove("metadata");
        }
        let mut theirs = bridge.clone();
        if let Some(o) = theirs.as_object_mut() {
            o.remove("metadata");
        }
        if tool_name(name) == name {
            assert_eq!(ours, theirs, "{name}");
            compared += 1;
        } else {
            // an MCP tool: named and mapped by its tool part, whatever the server
            assert_eq!(ours["name"], tool_name(name));
            assert_eq!(ours["status"], theirs["status"]);
            assert_eq!(ours["callId"], theirs["callId"]);
        }
    }
    assert!(compared >= 15, "{compared}");
    // an imported call under the old server name and a new `mcp__tools__search` render the same (spec 18.1)
    let pick = |n: &str| calls.iter().find(|c| c["name"] == n).unwrap();
    let old = calls
        .iter()
        .find(|c| {
            let n = c["name"].as_str().unwrap();
            n.starts_with("mcp__") && !n.starts_with("mcp__tools__") && n.ends_with("__search")
        })
        .unwrap();
    let a = tool_call_item(&row_of(old), Locale::En).unwrap();
    let b = tool_call_item(&row_of(pick("mcp__tools__search")), Locale::En).unwrap();
    assert_eq!(a["detail"], b["detail"]);
    assert_eq!(a["name"], b["name"]);
}
