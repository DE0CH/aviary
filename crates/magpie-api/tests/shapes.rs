//! The fixtures carry what the consumers expect: pigeon 14.1's shapes and the supervisor's (supervisor 10-11) are
//! covered by the same fixtures magpie's types round-trip, and the wire stays camelCase.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::implicit_clone
)] // tests

mod common;

use common::*;
use serde_json::Value;

/// Keys a fixture object must carry.
fn has_keys(v: &Value, keys: &[&str], what: &str) {
    let o = v.as_object().unwrap_or_else(|| panic!("{what}: not an object"));
    for k in keys {
        assert!(o.contains_key(*k), "{what}: missing `{k}`");
    }
}

fn body(id: &str, status: u64) -> Value {
    route_fixture(id)["responses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["status"] == status)
        .unwrap_or_else(|| panic!("{id}: no {status} response"))["body"]
        .clone()
}

#[test]
fn pigeon_14_1_shapes() {
    // 14.1.0 Conversation (plus magpie's additive fields, which pigeon ignores)
    let conversation = body("get_conversation", 200);
    has_keys(
        &conversation,
        &[
            "id",
            "title",
            "titleSource",
            "createdAt",
            "updatedAt",
            "lastUserMessageAt",
            "deletedAt",
            "purgeAt",
            "claudeSessionId",
            "turn",
            "lastTurn",
            "sandbox",
            "asks",
            "migrating",
        ],
        "Conversation",
    );
    has_keys(
        &conversation["turn"],
        &["id", "startedAt", "origin"],
        "Conversation.turn",
    );
    has_keys(
        &conversation["lastTurn"],
        &["id", "outcome", "endedAt", "error"],
        "Conversation.lastTurn",
    );
    has_keys(&conversation["lastTurn"]["error"], &["code", "message"], "ErrorInfo");
    has_keys(
        &conversation["asks"][0],
        &["id", "toolName", "toolUseId", "input", "createdAt"],
        "Ask",
    );
    // origins are R30's (DESIGN 12 D4)
    for (name, v) in fixture_files("events") {
        if v["kind"] == "turn.started" {
            let o = v["data"]["origin"].as_str().unwrap();
            assert!(["person", "agent", "import"].contains(&o), "{name}: origin {o}");
        }
    }
    // Row: 14.1.0's `seqs` and magpie 7.3's `sourceSeqRanges`/`updated`
    for (name, row) in fixture_files("rows") {
        has_keys(
            &row,
            &[
                "seqStart",
                "seqEnd",
                "seqs",
                "sourceSeqRanges",
                "updated",
                "timestamp",
                "item",
            ],
            &name,
        );
    }
    // M1, M9, M12, M13, M16 envelopes
    has_keys(
        &body("list_conversations", 200),
        &["conversations", "nextCursor", "list"],
        "M1",
    );
    has_keys(
        &body("list_conversations", 200)["list"],
        &["generation", "headSeq"],
        "M1 list",
    );
    has_keys(
        &body("timeline", 200),
        &["epoch", "minSeq", "maxSeq", "hasOlder", "hasNewer", "rows"],
        "M9",
    );
    has_keys(
        &body("prompts", 200)["prompts"][0],
        &["seq", "timestamp", "preview"],
        "M10",
    );
    has_keys(&body("changes", 200), &["generation", "headSeq", "changes"], "M12");
    for c in body("changes", 200)["changes"].as_array().unwrap() {
        has_keys(c, &["seq", "at", "type", "conversationId"], "Change");
    }
    has_keys(
        &body("upload", 201),
        &["uploadId", "path", "name", "size", "mimeType"],
        "M13",
    );
    has_keys(
        &body("get_share", 200),
        &["id", "title", "ownerName", "isOwner", "createdAt", "epoch", "rows"],
        "M16",
    );
    // M6 delete answers the Conversation (with deletedAt and purgeAt), pigeon 14.1
    has_keys(&body("delete_conversation", 200), &["id", "deletedAt", "purgeAt"], "M6");
    // the migration refusal carries retryAfterMs, and route in the switch window (R37)
    has_keys(
        &body("rename_conversation", 409),
        &["code", "message", "retryAfterMs"],
        "409 migrating",
    );
    has_keys(&body("send_turn", 409).clone(), &["code", "message"], "409");
}

#[test]
fn supervisor_10_11_shapes() {
    let commands = body("commands", 200);
    let start = commands["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "start_turn")
        .unwrap();
    has_keys(start, &["cmdSeq", "type", "turnId", "payload"], "command");
    has_keys(
        &start["payload"],
        &["text", "files", "images", "resume", "sentAt"],
        "start_turn",
    );
    has_keys(
        &start["payload"]["files"][0],
        &["uploadId", "name", "mimeType", "size", "workspacePath", "blob"],
        "start_turn.files",
    );
    has_keys(
        &start["payload"]["images"][0],
        &[
            "uploadId",
            "mimeType",
            "workspacePath",
            "blob",
            "modelBlob",
            "modelMime",
        ],
        "start_turn.images",
    );
    has_keys(&start["payload"]["resume"], &["providerSessionId"], "start_turn.resume");
    let answer = commands["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "answer_ask")
        .unwrap();
    has_keys(
        &answer["payload"],
        &["askId", "allow", "updatedInput", "message"],
        "answer_ask",
    );
    let title = commands["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "title")
        .unwrap();
    has_keys(&title["payload"], &["userPrompt", "maxTokens"], "title");

    let hb = route_fixture("heartbeat")["request"].clone();
    has_keys(
        &hb,
        &["turnId", "state", "tasks", "lastTurn", "gen", "supervisor", "cc"],
        "heartbeat",
    );
    has_keys(
        &hb["supervisor"],
        &["version", "bootId", "restarts"],
        "heartbeat.supervisor",
    );
    has_keys(
        &hb["cc"],
        &["quotaExceeded", "blockedWrites", "oldestBlockedMs"],
        "heartbeat.cc",
    );
    has_keys(&hb["lastTurn"], &["id", "outcome", "error"], "heartbeat.lastTurn");

    let state = route_fixture("turn_state")["request"].clone();
    has_keys(
        &state,
        &[
            "state",
            "at",
            "exitCode",
            "held",
            "tasks",
            "lastUuid",
            "usage",
            "durationMs",
        ],
        "turn state",
    );
    has_keys(
        &state["usage"],
        &[
            "inputTokens",
            "outputTokens",
            "cacheReadInputTokens",
            "cacheCreationInputTokens",
        ],
        "usage",
    );
    has_keys(
        &state["tasks"][0],
        &["taskId", "toolUseId", "type", "description", "startedAt"],
        "task",
    );

    let lease = body("lease", 200);
    has_keys(
        &lease,
        &[
            "leaseId",
            "epoch",
            "conversationId",
            "providerSessionId",
            "mainTranscript",
            "resync",
        ],
        "lease",
    );
    let tree = body("cc_tree", 200);
    has_keys(&tree, &["files", "ccBytes", "quotaBytes"], "tree");
    has_keys(
        &tree["files"][0],
        &["path", "kind", "size", "mtimeNs", "mode", "version"],
        "tree entry",
    );
    let frames = route_fixture("live_frames")["request"]["frames"].clone();
    has_keys(
        &frames[0],
        &["n", "messageId", "index", "op", "block", "parentToolUseId", "at"],
        "live start",
    );
    has_keys(
        &frames[1],
        &["n", "messageId", "index", "op", "offset", "text"],
        "live delta",
    );
    has_keys(
        &frames[4],
        &["n", "op", "status", "attempt", "retryInMs"],
        "live status",
    );
    let tasks = route_fixture("tasks")["request"]["events"][1].clone();
    has_keys(
        &tasks,
        &[
            "id",
            "taskId",
            "toolUseId",
            "event",
            "status",
            "summary",
            "outputFile",
            "at",
        ],
        "task event",
    );
    has_keys(
        &route_fixture("sent_files")["request"]["files"][0],
        &["path", "size", "sha256", "mimeType"],
        "sent file",
    );
    has_keys(&body("sent_files_commit", 200), &["kept", "seq"], "commit");
    has_keys(
        &route_fixture("ask_open")["request"],
        &["askId", "toolName", "toolUseId", "input"],
        "ask",
    );
}

/// Keys whose values are not magpie's API but data magpie carries as is: Claude Code's tool inputs and ask inputs,
/// and the Messages API response of a title ack.
const OPAQUE_KEYS: &[&str] = &["input", "updatedInput", "response"];

fn snake_keys(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            let cc_event = o
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|k| k.starts_with("cc."));
            for (k, child) in o {
                if k.contains('_') {
                    out.push(format!("{path}.{k}"));
                }
                if OPAQUE_KEYS.contains(&k.as_str()) || (cc_event && k == "data") {
                    continue;
                }
                snake_keys(child, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                snake_keys(child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn no_snake_case_key_outside_claude_code_data() {
    let mut found = Vec::new();
    for dir in ["routes", "events", "rows", "changes", "commands", "live"] {
        for (name, v) in fixture_files(dir) {
            snake_keys(&v, &format!("{dir}/{name}"), &mut found);
        }
    }
    assert!(found.is_empty(), "snake_case keys: {found:#?}");
}

#[test]
fn claude_code_data_keeps_its_own_keys() {
    // the rule above is not vacuous: Claude Code's lines do carry snake_case keys, inside `data`
    let mut found = Vec::new();
    for (name, v) in fixture_files("events") {
        if v["kind"].as_str().is_some_and(|k| k.starts_with("cc.")) {
            snake_keys(&v["data"], &name, &mut found);
        }
    }
    assert!(
        found
            .iter()
            .any(|k| k.ends_with(".stop_reason") || k.ends_with(".tool_use_id")),
        "{found:?}"
    );
}
