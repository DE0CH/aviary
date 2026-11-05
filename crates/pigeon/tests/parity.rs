//! Render parity of the tool-call mapper (WP-17 acceptance; cutover 10.2 item 6's input): every tool call the old
//! `CloudCLI` bridge rendered in its recorded frame log (`testing/paseo-contract/fixtures/cloudcli-bridge-*.ndjson`,
//! made from imported-conversation-like transcripts) is rebuilt from the transcript the recorder fed the bridge (the
//! `tool_use` name and input as written, the `tool_result` text and error flag:
//! `testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`) as the magpie row it becomes and projected by
//! pigeon: the item equals the bridge's, MCP tools included (named `mcp__<server>__<tool>` as written, as the old
//! bridge named them: cutover 10.2 R6, what staff see today).
//!
//! The second test projects conversation A's whole history the same way, row by row through `PaseoProjector` (the
//! projection pigeon serves): the user row with an image and a file, the reasoning row, every tool row across its two
//! seqs, the assistant rows and the error row, each compared, envelope and item, with the entry the bridge served at
//! the same seq. The differences that are pigeon's by design are listed in the test (image URLs, the error sentence).
//! Real imported conversations (Claude Code transcripts such as `crates/magpie/tests/fixtures/claude-code-*`) are
//! not compared here: their rows come from magpie's importer (`crates/magpie-import`, WP-29), and render parity over
//! the imported set is cutover 10.2 item 6 (WP-41), which compares the two stacks' renders of the same conversations.
#![allow(clippy::unwrap_used, clippy::too_many_lines)] // tests

use std::collections::BTreeMap;

use magpie_api::rows::{
    AssistantItem, ErrorItem, ReasoningItem, Row, RowFile, RowImage, RowItem, ToolItem, ToolResult, ToolStatus,
    UserItem,
};
use pigeon::paseo::mapping::{PaseoProjector, tool_call_item};
use pigeon::paseo::strings::Locale;
use pigeon::paseo::{ProjectCtx, RowProjector};
use serde_json::{Value, json};

/// Every frame of the bridge logs (`fixtures/cloudcli-bridge-*.ndjson`).
fn bridge_frames() -> Vec<Value> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testing/paseo-contract/fixtures");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cloudcli-bridge-")
        {
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                out.push(serde_json::from_str(line).unwrap());
            }
        }
    }
    out
}

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

fn recorded() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The tool calls the recorder fed the bridge, as the transcript had them, by call id
/// (`testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`, from `record-cloudcli-bridge.ts`).
fn transcript_calls() -> std::collections::HashMap<String, Value> {
    let v = recorded();
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
        assert_eq!(ours, theirs, "{name}");
        kinds.insert(theirs["detail"]["type"].as_str().unwrap().to_owned());
        compared += 1;
    }
    assert!(compared >= 15, "{compared}");
    // every call of the transcript was rendered and compared
    assert_eq!(ids.len(), transcript.len(), "{ids:?}");
    // the detail kinds the bridge produced for these transcripts (the others, edit/fetch/plan/plain_text and the
    // rest of the ported mapper, are its unit tests in src/paseo/mapping.rs)
    assert!(kinds.len() >= 4, "{kinds:?}");
    // an imported call under the old server name and a new `mcp__tools__search` keep their own names, as the bridge
    // showed them
    let render = |id: &str| tool_call_item(&row_of(&transcript[id], ToolStatus::Completed), Locale::En).unwrap();
    let (a, b) = (render("t-search"), render("t-search2"));
    assert_eq!(a["name"], transcript["t-search"]["name"]);
    assert_eq!(b["name"], transcript["t-search2"]["name"]);
}

/// Conversation A's entries as the bridge served them (its `fetch_agent_timeline_response` pages), by seq.
fn bridge_entries() -> BTreeMap<u64, Value> {
    let a = "0a5e5b9e-1c2d-4e3f-8a9b-000000000001";
    let mut out = BTreeMap::new();
    for f in bridge_frames() {
        let frame = &f["frame"];
        if f["dir"] != "out" || frame["message"]["type"] != "fetch_agent_timeline_response" {
            continue;
        }
        let p = &frame["message"]["payload"];
        if p["agentId"] != a {
            continue;
        }
        for e in p["entries"].as_array().unwrap() {
            out.entry(e["seqStart"].as_u64().unwrap()).or_insert_with(|| e.clone());
        }
    }
    out
}

/// The magpie rows the recorder's history makes: one row per message, a tool call's result joining its row (the
/// row's second seq), times as the bridge stamped them.
fn history_rows(entries: &BTreeMap<u64, Value>) -> Vec<Row> {
    let rec = recorded();
    let calls = transcript_calls();
    let mut rows: Vec<Row> = Vec::new();
    let mut by_call: BTreeMap<String, usize> = BTreeMap::new();
    for h in rec["history"].as_array().unwrap() {
        let seq = h["seq"].as_u64().unwrap();
        let id = h["messageId"].as_str().unwrap().to_owned();
        let text = h["content"].as_str().unwrap_or("").to_owned();
        let item = match (h["kind"].as_str().unwrap(), h["role"].as_str()) {
            ("tool_result", _) => {
                let call = h["callId"].as_str().unwrap();
                let row = &mut rows[by_call[call]];
                let full = row_of(&calls[call], ToolStatus::Completed);
                let failed = full.result.as_ref().is_some_and(|r| r.is_error);
                row.item = RowItem::Tool(ToolItem {
                    status: if failed {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Completed
                    },
                    ..full
                });
                row.seq_end = seq;
                row.seqs.push(seq);
                row.updated = true;
                continue;
            }
            ("tool_use", _) => {
                let call = h["callId"].as_str().unwrap();
                by_call.insert(call.to_owned(), rows.len());
                RowItem::Tool(row_of(&calls[call], ToolStatus::Running))
            }
            ("text", Some("user")) => RowItem::User(UserItem {
                message_id: id,
                client_message_id: None,
                text,
                images: h["images"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|_| RowImage {
                                blob_id: "0".repeat(64),
                                mime_type: "image/png".into(),
                                width: None,
                                height: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                files: h["files"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| RowFile {
                                // magpie keeps a user file's path relative to /workspace
                                path: f["path"].as_str().unwrap().trim_start_matches("/workspace/").to_owned(),
                                name: f["name"].as_str().unwrap().to_owned(),
                                // an imported attachment may have no type: pigeon types it by name, as the bridge did
                                mime_type: String::new(),
                                size: f["size"].as_u64().unwrap(),
                                missing: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                viewer: None,
            }),
            ("text", _) => RowItem::Assistant(AssistantItem {
                message_id: id,
                block_index: 0,
                text,
                error: None,
            }),
            ("thinking", _) => RowItem::Reasoning(ReasoningItem {
                message_id: id,
                block_index: 0,
                text,
            }),
            ("error", _) => RowItem::Error(ErrorItem {
                code: magpie_api::errors::DataCode::ModelUnavailable,
                message: text,
                detail: None,
            }),
            (k, _) => panic!("history kind {k}"),
        };
        let at = entries
            .range(..=seq)
            .next_back()
            .map_or_else(|| json!("2026-10-29T09:00:00.000Z"), |(_, e)| e["timestamp"].clone());
        rows.push(Row {
            seq_start: seq,
            seq_end: seq,
            seqs: vec![seq],
            updated: false,
            timestamp: serde_json::from_value(at).unwrap(),
            turn_id: None,
            item,
        });
    }
    rows
}

#[test]
fn every_history_row_projects_as_the_bridge_served_it() {
    let entries = bridge_entries();
    let rows = history_rows(&entries);
    let signer = pigeon::blobs::Signer::new(&pigeon::KeyList(vec![[5; 32]]));
    let ctx = ProjectCtx {
        zh: false,
        signer: &signer,
    };
    assert_eq!(
        rows.len(),
        entries.len(),
        "one bridge entry per row: {:?}",
        entries.keys()
    );
    let mut kinds = std::collections::BTreeSet::new();
    for row in &rows {
        let mut ours = PaseoProjector.project(row, &ctx).unwrap();
        let mut theirs = entries[&row.seq_start].clone();
        let kind = theirs["item"]["type"].as_str().unwrap().to_owned();
        kinds.insert(kind.clone());
        // what is pigeon's by design:
        match kind.as_str() {
            // images: magpie's blobs behind signed URLs (spec 5.9) with a thumbnail, not the bridge's data URLs
            "user_message" if theirs["item"].get("images").is_some() => {
                let (a, b) = (ours["item"]["images"].take(), theirs["item"]["images"].take());
                assert_eq!(a.as_array().unwrap().len(), b.as_array().unwrap().len());
                for (x, y) in a.as_array().unwrap().iter().zip(b.as_array().unwrap()) {
                    assert_eq!(x["mimeType"], y["mimeType"]);
                    assert!(x["url"].as_str().unwrap().starts_with("/api/"), "{x}");
                    assert!(x["thumbnailUrl"].is_string(), "{x}");
                }
            }
            // a failed turn's error: pigeon's sentence for magpie's code (spec 7.9), not the old provider's text
            "error" => {
                assert_eq!(
                    ours["item"]["message"],
                    Locale::En.error_sentence(&magpie_api::errors::ErrorInfo::new(
                        magpie_api::errors::DataCode::ModelUnavailable,
                        String::new()
                    ))
                );
                ours["item"]["message"].take();
                theirs["item"]["message"].take();
            }
            "tool_call" => {
                // SendUserFile's card: the bridge's came from its own snapshot (see the first test)
                ours["item"].as_object_mut().unwrap().remove("metadata");
                theirs["item"].as_object_mut().unwrap().remove("metadata");
            }
            _ => {}
        }
        // the seqs a row covers: pigeon joins adjacent seqs into one range ({3, 4}), the bridge listed one range per
        // event ({3, 3}, {4, 4}); the app reads both as the same coverage
        let covered = |v: &mut Value| -> Vec<u64> {
            let ranges = v["sourceSeqRanges"].take();
            ranges
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|r| r["startSeq"].as_u64().unwrap()..=r["endSeq"].as_u64().unwrap())
                .collect()
        };
        assert_eq!(covered(&mut ours), covered(&mut theirs), "seq {}", row.seq_start);
        assert_eq!(ours, theirs, "seq {}", row.seq_start);
    }
    for k in ["user_message", "reasoning", "tool_call", "assistant_message", "error"] {
        assert!(kinds.contains(k), "{k} compared: {kinds:?}");
    }
}
