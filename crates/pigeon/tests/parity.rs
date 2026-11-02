//! Render parity of the tool-call mapper (WP-17 acceptance; cutover 10.2 item 6's input): every tool call the old
//! `CloudCLI` bridge rendered in its recorded frame log (`testing/paseo-contract/fixtures/cloudcli-bridge-*.ndjson`,
//! made from imported-conversation-like transcripts) is rebuilt from the transcript the recorder fed the bridge (the
//! `tool_use` name and input as written, the `tool_result` text and error flag:
//! `testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`) as the magpie row it becomes and projected by
//! pigeon: the item equals the bridge's, except MCP tools, which pigeon names without
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

/// The tool calls the recorder fed the bridge, as the transcript had them, by call id
/// (`testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`, from `record-cloudcli-bridge.ts`).
fn transcript_calls() -> std::collections::HashMap<String, Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json"
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["callId"].as_str().unwrap().to_owned(), c.clone()))
        .collect()
}

/// The row magpie keeps for a transcript's call (its `tool_use` input as written, its `tool_result` text), in the
/// state the bridge's frame shows (a running call has no result yet).
fn row_of(call: &Value, status: ToolStatus) -> ToolItem {
    let result = (status != ToolStatus::Running)
        .then(|| call.get("result").filter(|r| r.is_object()))
        .flatten()
        .map(|r| {
            let text = r["text"].as_str().unwrap().to_owned();
            ToolResult {
                bytes: text.len() as u64,
                text,
                truncated: false,
                blob_id: None,
                is_error: r["isError"].as_bool().unwrap(),
                images: None,
            }
        });
    ToolItem {
        call_id: call["callId"].as_str().unwrap().to_owned(),
        message_id: "msg".into(),
        block_index: 0,
        name: call["name"].as_str().unwrap().to_owned(),
        input: call["input"].clone(),
        status,
        result,
        sent_files: None,
        task: None,
        error: None,
    }
}

fn status_of(item: &Value) -> ToolStatus {
    match item["status"].as_str().unwrap() {
        "running" => ToolStatus::Running,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => ToolStatus::Canceled,
    }
}

#[test]
fn tool_calls_render_as_the_bridge_rendered_them() {
    let calls = bridge_tool_calls();
    let transcript = transcript_calls();
    assert!(calls.len() >= 20, "{}", calls.len());
    let mut compared = 0;
    let mut ids = std::collections::BTreeSet::new();
    let mut kinds = std::collections::BTreeSet::new();
    for bridge in &calls {
        let id = bridge["callId"].as_str().unwrap();
        let call = transcript
            .get(id)
            .unwrap_or_else(|| panic!("{id}: not in the recorder's transcript"));
        let name = bridge["name"].as_str().unwrap();
        assert_eq!(call["name"], name);
        let mut ours = tool_call_item(&row_of(call, status_of(bridge)), Locale::En).unwrap();
        // the bridge's SendUserFile cards came from its own snapshot; magpie's are a field of the row (A2.F21)
        if let Some(o) = ours.as_object_mut() {
            o.remove("metadata");
        }
        let mut theirs = bridge.clone();
        if let Some(o) = theirs.as_object_mut() {
            o.remove("metadata");
        }
        ids.insert(id.to_owned());
        if tool_name(name) == name {
            assert_eq!(ours, theirs, "{name}");
            kinds.insert(theirs["detail"]["type"].as_str().unwrap().to_owned());
            compared += 1;
        } else {
            // an MCP tool: named and mapped by its tool part, whatever the server
            assert_eq!(ours["name"], tool_name(name));
            assert_eq!(ours["status"], theirs["status"]);
            assert_eq!(ours["callId"], theirs["callId"]);
        }
    }
    assert!(compared >= 15, "{compared}");
    // every call of the transcript was rendered and compared
    assert_eq!(ids.len(), transcript.len(), "{ids:?}");
    // the detail kinds the bridge produced for these transcripts (the others, edit/fetch/plan/plain_text and the
    // rest of the ported mapper, are its unit tests in src/paseo/mapping.rs)
    assert!(kinds.len() >= 4, "{kinds:?}");
    // an imported call under the old server name and a new `mcp__tools__search` render the same (spec 18.1)
    let render = |id: &str| tool_call_item(&row_of(&transcript[id], ToolStatus::Completed), Locale::En).unwrap();
    let (a, b) = (render("t-search"), render("t-search2"));
    assert_eq!(a["detail"], b["detail"]);
    assert_eq!(a["name"], b["name"]);
}
