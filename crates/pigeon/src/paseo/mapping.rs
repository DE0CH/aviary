//! The pure projections of spec 7.7-7.8: magpie's `Conversation` → the Paseo objects (`WorkspaceDescriptor`,
//! `AgentSnapshot`, `ProjectPlacement`, `ProjectDescriptor`), and magpie's timeline `Row` → a Paseo row, with the
//! tool-call detail of the Paseo mapper ported from the vendored TypeScript.
//!
//! The tool-call mapper is a port of Paseo's `claude-tool-call-mapper.ts`, `claude-tool-call-detail-parser.ts`,
//! `tool-call-detail-primitives.ts` and `tool-call-mapper-utils.ts` as vendored in DE0CH/claudecodeui
//! `server/modules/paseo/vendor/` (from getpaseo/paseo `packages/server/src/server/agent/`):
//!
//! ```text
//! Copyright Paseo contributors. Licensed under the Apache License, Version 2.0 (the "License"); you may not use
//! this code except in compliance with the License. You may obtain a copy of the License at
//! http://www.apache.org/licenses/LICENSE-2.0. Unless required by applicable law or agreed to in writing, software
//! distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND,
//! either express or implied. See the License for the specific language governing permissions and limitations
//! under the License. Modified: ported to Rust; outputs are the text magpie keeps (a string or nothing).
//! ```
//!
//! magpie hands pigeon a tool's result as its text preview (`result.text`), never Claude Code's structured
//! `toolUseResult`, so the mapper's output schemas see a string or nothing: the object-only output branches of the
//! TypeScript (Grep's and Glob's structured results, `WebFetch`'s and `WebSearch`'s objects, the Codex shell
//! envelopes' object forms) can never match and are not ported; their string behaviour (the branch fails and the
//! detail falls back to `unknown`) is kept, as the old bridge showed for transcripts without `toolUseResult`.

use magpie_api::conversations::Conversation;
use magpie_api::rows::{Row, RowItem, ToolItem, ToolStatus, seq_ranges};
use magpie_api::uploads::BlobVariant;
use serde_json::{Map, Value, json};

use super::strings::Locale;
use super::{ProjectCtx, RowProjector, safe_path};

/// The one host's server id (7.4): cached hosts and replica rows are keyed by it, so it never changes.
pub const SERVER_ID: &str = "srv_conversations";
/// The one project's id and key (7.4).
pub const PROJECT_ID: &str = "prj_conversations";
/// Every conversation's working directory as the app sees it (7.4).
pub const CWD: &str = "/workspace";
/// The workspace id of a conversation: `wks_<id>`.
pub const WORKSPACE_PREFIX: &str = "wks_";
/// The provider of every agent.
pub const PROVIDER: &str = "claude";
/// A projected frame is never larger than this (7.8).
pub const MAX_PUSH_BYTES: usize = 16 << 20;
/// …and a string field of an oversized frame is cut to this.
pub const CUT_FIELD_BYTES: usize = 1 << 20;

/// What the object projections need besides the conversation.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// The product name (`product.name`).
    pub product: &'a str,
    /// The strings.
    pub locale: Locale,
    /// The default model's id, when the model list is known (a conversation whose person picked one shows its own).
    pub model: Option<&'a str>,
}

/// `wks_<id>`.
#[must_use]
pub fn workspace_id(id: &uuid::Uuid) -> String {
    format!("{WORKSPACE_PREFIX}{id}")
}

fn name_of(c: &Conversation, v: &View<'_>) -> String {
    c.title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(|| v.locale.untitled().to_owned(), str::to_owned)
}

/// `ProjectDescriptor` (7.5.1).
#[must_use]
pub fn project_descriptor(v: &View<'_>) -> Value {
    json!({
        "projectId": PROJECT_ID, "projectKey": PROJECT_ID, "projectDisplayName": v.product,
        "projectCustomName": null, "projectCustomIconRevision": null, "projectIconRevision": "automatic:none:v1",
        "projectRootPath": CWD, "projectKind": "directory",
    })
}

/// `ProjectPlacement(c)` (7.7).
#[must_use]
pub fn project_placement(c: &Conversation, v: &View<'_>) -> Value {
    json!({
        "projectKey": PROJECT_ID, "projectName": v.product, "workspaceName": name_of(c, v),
        "checkout": {"cwd": CWD, "isGit": false, "currentBranch": null, "remoteUrl": null, "worktreeRoot": null,
                     "isPaseoOwnedWorktree": false, "mainRepoRoot": null},
    })
}

/// `WorkspaceDescriptor(c)` (7.7).
#[must_use]
pub fn workspace_descriptor(c: &Conversation, v: &View<'_>) -> Value {
    let running = c.turn.is_some();
    let last_user = c.last_user_message_at.unwrap_or(c.updated_at);
    json!({
        "id": workspace_id(&c.id), "projectId": PROJECT_ID, "projectDisplayName": v.product,
        "projectCustomName": null, "projectCustomIconRevision": null, "projectRootPath": CWD,
        "workspaceDirectory": CWD, "setupStatus": null, "projectKind": "directory", "workspaceKind": "directory",
        "name": name_of(c, v), "title": c.title, "pinnedAt": null, "background": false, "archivingAt": null,
        "status": if running { "running" } else { "done" }, "statusEnteredAt": c.updated_at,
        "activityAt": c.updated_at, "createdAt": c.created_at, "lastUserMessageAt": last_user, "diffStat": null,
        "scripts": [], "project": project_placement(c, v),
    })
}

/// The ask kind the app shows (7.5.9).
#[must_use]
pub fn ask_kind(tool: &str) -> &'static str {
    match tool {
        "AskUserQuestion" => "question",
        "ExitPlanMode" => "plan",
        _ => "tool",
    }
}

/// An open ask as a Paseo `AgentPermissionRequest` (7.5.9).
#[must_use]
pub fn permission_request(ask: &magpie_api::conversations::Ask) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(ask.id));
    m.insert("provider".into(), json!(PROVIDER));
    m.insert("name".into(), json!(ask.tool_name));
    m.insert("kind".into(), json!(ask_kind(&ask.tool_name)));
    if let Some(t) = &ask.title {
        m.insert("title".into(), json!(t));
    }
    if let Some(d) = &ask.description {
        m.insert("description".into(), json!(d));
    }
    m.insert("input".into(), Value::Object(question_input(ask)));
    Value::Object(m)
}

/// An ask's input as the app's card reads it. Claude Code's own `AskUserQuestion` always lets the person type an
/// answer of their own beside the options ("Other"), and says so nowhere in the input; the app's question card shows
/// its text field only where a question says `allowOther`. So every question with options says it here.
fn question_input(ask: &magpie_api::conversations::Ask) -> Map<String, Value> {
    let mut input = ask.input.clone();
    if ask.tool_name != "AskUserQuestion" {
        return input;
    }
    if let Some(questions) = input.get_mut("questions").and_then(Value::as_array_mut) {
        for q in questions.iter_mut().filter_map(Value::as_object_mut) {
            if !q.contains_key("allowOther") && !q.contains_key("isOther") {
                q.insert("allowOther".into(), json!(true));
            }
        }
    }
    input
}

/// The `startedAt` the working-time counter runs from (7.7, DECISIONS 17): the turn's start moved on by the time its
/// answered questions waited, and none while a question is open, so the counter stands still while the person
/// decides and goes on from where it was after the answer.
fn working_since(c: &Conversation, t: &magpie_api::conversations::TurnRef) -> Option<magpie_api::Timestamp> {
    if !c.asks.is_empty() {
        return None;
    }
    let wait = time::Duration::milliseconds(i64::try_from(t.ask_wait_ms).unwrap_or(i64::MAX));
    Some(magpie_api::Timestamp(t.started_at.0.saturating_add(wait)))
}

/// `AgentSnapshot(c)` (7.7).
#[must_use]
pub fn agent_snapshot(c: &Conversation, v: &View<'_>) -> Value {
    let running = c.turn.is_some();
    let last_user = c.last_user_message_at.unwrap_or(c.updated_at);
    let asks: Vec<Value> = c.asks.iter().map(permission_request).collect();
    let attention = c.asks.first();
    // the model the person picked for this conversation, else the deployment's default
    let model = c.picked_model.as_deref().or(v.model);
    // the conversation's queue (magpie's `pendingMessages`): messages sent during the reply that have not reached it
    // yet. `held`: it waits for the reply to end and can still be taken back.
    let queue: Vec<Value> = c
        .pending_messages
        .iter()
        .map(|m| {
            json!({"messageId": m.client_message_id, "text": m.text, "attachments": m.attachments,
                   "sentAt": m.sent_at, "held": m.held})
        })
        .collect();
    json!({
        "id": c.id, "provider": PROVIDER, "cwd": CWD, "workspaceId": workspace_id(&c.id), "model": model,
        "pendingMessages": queue,
        // the deployment closed the conversation to messages: the sentence the app shows in place of its message box
        "closed": c.closed,
        "thinkingOptionId": null, "effectiveThinkingOptionId": null,
        "runtimeInfo": {"provider": PROVIDER, "sessionId": c.claude_session_id, "model": model, "modeId": null},

        "createdAt": c.created_at, "updatedAt": c.updated_at, "lastUserMessageAt": last_user,
        "status": if running { "running" } else { "idle" },
        "activeTurn": c.turn.as_ref().map(|t| json!({"turnId": t.id, "startedAt": working_since(c, t)})),
        "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsSessionListing": false,
            "supportsDynamicModes": false, "supportsMcpServers": false, "supportsReasoningStream": true,
            "supportsToolInvocations": true, "supportsRewindConversation": false, "supportsRewindFiles": false,
            "supportsRewindBoth": false},
        "currentModeId": null, "availableModes": [], "features": [], "pendingPermissions": asks, "persistence": null,
        "title": name_of(c, v), "labels": {}, "archivedAt": c.deleted_at,
        "requiresAttention": attention.is_some(),
        "attentionReason": attention.map(|_| "permission"),
        "attentionTimestamp": attention.map(|a| a.created_at),
    })
}

// ------------------------------------------------------------------------------------------------ the tool mapper

fn non_empty(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// zod `.optional()` of a string: absent is fine, present must be a string (null included: zod's optional is not
/// nullable).
fn opt_str<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a str>, ()> {
    match o.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(()),
    }
}

fn opt_num(o: &Map<String, Value>, k: &str) -> Result<Option<f64>, ()> {
    match o.get(k) {
        None => Ok(None),
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

fn req_str<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str)
}

/// `CommandValueSchema` then the shell input's transform.
fn command_value(v: &Value) -> Result<Option<String>, ()> {
    match v {
        Value::String(s) => Ok(non_empty(Some(&Value::String(s.clone())))),
        Value::Array(a) => {
            let mut tokens = Vec::with_capacity(a.len());
            for t in a {
                tokens.push(t.as_str().ok_or(())?.trim());
            }
            let joined = tokens
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            Ok(if joined.is_empty() { None } else { Some(joined) })
        }
        _ => Err(()),
    }
}

struct ShellInput {
    command: Option<String>,
    cwd: Option<String>,
}

fn shell_input(v: &Value) -> Result<ShellInput, ()> {
    let o = v.as_object().ok_or(())?;
    let cwd = opt_str(o, "cwd")?;
    let directory = opt_str(o, "directory")?;
    let command = match (o.get("command"), o.get("cmd")) {
        (Some(c), _) if command_value(c).is_ok() => command_value(c)?,
        (_, Some(c)) => command_value(c)?,
        _ => return Err(()),
    };
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .or(directory.filter(|s| !s.is_empty()))
        .map(str::to_owned);
    Ok(ShellInput { command, cwd })
}

/// `extractCodexShellOutput`: a Codex exec envelope's body, else the text.
fn codex_shell_output(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let first = lines.first().map(|l| l.trim().to_lowercase()).unwrap_or_default();
    let header: Vec<String> = lines.iter().take(8).map(|l| l.trim().to_lowercase()).collect();
    let envelope = first.starts_with("chunk id:")
        && header.iter().any(|l| l.starts_with("wall time:"))
        && header.iter().any(|l| l.starts_with("process exited with code"));
    if !envelope {
        return Some(text.to_owned());
    }
    if let Some(i) = lines.iter().position(|l| l.trim() == "Output:") {
        let body = lines[i + 1..].join("\n");
        return if body.is_empty() { None } else { Some(body) };
    }
    let is_header = |l: &str| {
        let n = l.trim().to_lowercase();
        [
            "chunk id:",
            "wall time:",
            "process exited with code",
            "original token count:",
        ]
        .iter()
        .any(|p| n.starts_with(p))
    };
    let start = (1..lines.len()).find(|&i| !is_header(lines[i]))?;
    let body = lines[start..].join("\n");
    if body.is_empty() { None } else { Some(body) }
}

struct PathInput {
    file_path: String,
}

/// `ToolPathInputSchema`: `path`, `file_path` or `filePath` (the first present string).
fn path_input(o: &Map<String, Value>) -> Result<PathInput, ()> {
    for k in ["path", "file_path", "filePath"] {
        if let Some(s) = req_str(o, k) {
            return Ok(PathInput {
                file_path: s.to_owned(),
            });
        }
    }
    Err(())
}

struct ReadInput {
    file_path: String,
    offset: Option<f64>,
    limit: Option<f64>,
}

fn read_input(v: &Value) -> Result<ReadInput, ()> {
    let o = v.as_object().ok_or(())?;
    // each branch requires its key and checks offset/limit; they share offset/limit, so one check serves all
    let offset = opt_num(o, "offset")?;
    let limit = opt_num(o, "limit")?;
    let p = path_input(o)?;
    Ok(ReadInput {
        file_path: p.file_path,
        offset,
        limit,
    })
}

struct ReadOutput {
    file_path: Option<String>,
    content: Option<String>,
}

fn xml_tag<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let lower = s.to_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = start + lower[start..].find(&close)?;
    s.get(start..end)
}

fn decode_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") && e.len() > 2 && e[2..].chars().all(|c| c.is_ascii_hexdigit()) => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') && e.len() > 1 && e[1..].chars().all(|c| c.is_ascii_digit()) => {
                e[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(c) = decoded {
            out.push(c);
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn trim_outer_line_breaks(s: &str) -> &str {
    let s = s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s);
    s.strip_suffix("\r\n").or_else(|| s.strip_suffix('\n')).unwrap_or(s)
}

/// `ToolReadOutputSchema` on a string.
fn read_output(text: &str) -> ReadOutput {
    let trimmed = text.trim();
    if trimmed.starts_with("<path>") && trimmed.to_lowercase().contains("<content>") {
        let file_path = xml_tag(trimmed, "path").map(str::trim).filter(|s| !s.is_empty());
        let content = xml_tag(trimmed, "content");
        if file_path.is_some() || content.is_some() {
            return ReadOutput {
                file_path: file_path.map(decode_xml),
                content: content.map(|c| decode_xml(trim_outer_line_breaks(c))),
            };
        }
    }
    ReadOutput {
        file_path: None,
        content: if text.is_empty() { None } else { Some(text.to_owned()) },
    }
}

/// `stripReadLineNumberGutter`: `cat -n` content as raw source, with the first line number.
fn strip_gutter(content: &str) -> Option<(String, Option<u64>)> {
    if content.is_empty() {
        return None;
    }
    let normalized = content.replace("\r\n", "\n");
    let mut stripped: Vec<&str> = Vec::new();
    let (mut non_empty, mut matched) = (0usize, 0usize);
    let mut start_line: Option<u64> = None;
    let mut prev: Option<u64> = None;
    let mut sequential = true;
    let mut first_matched = false;
    let mut saw = false;
    for line in normalized.split('\n') {
        if line.is_empty() {
            stripped.push(line);
            continue;
        }
        non_empty += 1;
        // ^\s*(\d+)\t(.*)$
        let body = line.trim_start();
        let digits = body.chars().take_while(char::is_ascii_digit).count();
        let gutter = digits > 0 && body[digits..].starts_with('\t');
        if !gutter {
            if !saw {
                return None;
            }
            stripped.push(line);
            continue;
        }
        if !saw {
            first_matched = true;
        }
        saw = true;
        matched += 1;
        let n: u64 = body[..digits].parse().unwrap_or(u64::MAX);
        if start_line.is_none() {
            start_line = Some(n);
        }
        if prev.is_some_and(|p| p.checked_add(1) != Some(n)) {
            sequential = false;
        }
        prev = Some(n);
        stripped.push(&body[digits + 1..]);
    }
    #[allow(clippy::cast_precision_loss)] // a ratio of line counts
    let ratio = matched as f64 / non_empty.max(1) as f64;
    if !first_matched || !sequential || non_empty == 0 || ratio < 0.5 {
        return None;
    }
    Some((stripped.join("\n"), start_line))
}

fn normalize_path(p: Option<&str>) -> Option<String> {
    p.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn truncate_diff(text: Option<String>) -> Option<String> {
    const MAX: usize = 12_000;
    let text = text?;
    // JavaScript counts UTF-16 code units; this counts chars, which agrees for the text a diff holds
    let n = text.chars().count();
    if n <= MAX {
        return Some(text);
    }
    let head: String = text.chars().take(MAX).collect();
    Some(format!("{head}\n...[truncated {} chars]", n - MAX))
}

fn first_non_empty(o: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| non_empty(o.get(*k)))
}

const EDIT_TEXT_KEYS: [&str; 13] = [
    "old_string",
    "old_str",
    "oldContent",
    "old_content",
    "new_string",
    "new_str",
    "newContent",
    "new_content",
    "content",
    "patch",
    "diff",
    "unified_diff",
    "unifiedDiff",
];

fn detail_shell(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    let input = input.map(shell_input).transpose()?;
    let out = output.map(codex_shell_output);
    let Some(command) = input.as_ref().and_then(|i| i.command.clone()) else {
        return Ok(None);
    };
    let mut m = Map::new();
    m.insert("type".into(), json!("shell"));
    m.insert("command".into(), json!(command));
    if let Some(cwd) = input.and_then(|i| i.cwd) {
        m.insert("cwd".into(), json!(cwd));
    }
    if let Some(Some(o)) = out {
        m.insert("output".into(), json!(o));
    }
    Ok(Some(Value::Object(m)))
}

fn detail_read(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    let input = input.map(read_input).transpose()?;
    let out = output.map(read_output);
    let path = input
        .as_ref()
        .map(|i| i.file_path.as_str())
        .or_else(|| out.as_ref().and_then(|o| o.file_path.as_deref()));
    let Some(file_path) = normalize_path(path) else {
        return Ok(None);
    };
    let out_content = out.as_ref().and_then(|o| o.content.clone());
    let stripped = out_content.as_deref().and_then(strip_gutter);
    let content = stripped.as_ref().map(|(c, _)| c.clone()).or(out_content);
    let mut m = Map::new();
    m.insert("type".into(), json!("read"));
    m.insert("filePath".into(), json!(file_path));
    if let Some(c) = content.filter(|c| !c.is_empty()) {
        m.insert("content".into(), json!(c));
    }
    let offset = input.as_ref().and_then(|i| i.offset);
    if let Some(o) = offset {
        m.insert("offset".into(), json!(o));
    } else if let Some(Some(n)) = stripped.map(|(_, n)| n) {
        m.insert("offset".into(), json!(n));
    }
    if let Some(l) = input.as_ref().and_then(|i| i.limit) {
        m.insert("limit".into(), json!(l));
    }
    Ok(Some(Value::Object(m)))
}

fn write_content(o: &Map<String, Value>) -> Result<Option<String>, ()> {
    for k in ["content", "new_content", "newContent"] {
        opt_str(o, k)?;
    }
    Ok(first_non_empty(o, &["content", "new_content", "newContent"]))
}

fn detail_write(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    let input = match input {
        None => None,
        Some(v) => {
            let o = v.as_object().ok_or(())?;
            let p = path_input(o)?;
            Some((p.file_path, write_content(o)?))
        }
    };
    // a string output parses to nothing (ToolWriteOutputSchema's string branch)
    let _ = output;
    let Some(file_path) = normalize_path(input.as_ref().map(|(p, _)| p.as_str())) else {
        return Ok(None);
    };
    let mut m = Map::new();
    m.insert("type".into(), json!("write"));
    m.insert("filePath".into(), json!(file_path));
    if let Some(c) = input.and_then(|(_, c)| c) {
        m.insert("content".into(), json!(c));
    }
    Ok(Some(Value::Object(m)))
}

fn detail_edit(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    // ToolEditOutputSchema has no string branch: a result text fails the branch
    if output.is_some() {
        return Err(());
    }
    let Some(v) = input else {
        return Ok(None);
    };
    let o = v.as_object().ok_or(())?;
    let p = path_input(o)?;
    for k in EDIT_TEXT_KEYS {
        opt_str(o, k)?;
    }
    let Some(file_path) = normalize_path(Some(&p.file_path)) else {
        return Ok(None);
    };
    let old = first_non_empty(o, &["old_string", "old_str", "oldContent", "old_content"]);
    let new = first_non_empty(o, &["new_string", "new_str", "newContent", "new_content", "content"]);
    let diff = truncate_diff(first_non_empty(o, &["patch", "diff", "unified_diff", "unifiedDiff"]));
    let mut m = Map::new();
    m.insert("type".into(), json!("edit"));
    m.insert("filePath".into(), json!(file_path));
    if let Some(s) = old {
        m.insert("oldString".into(), json!(s));
    }
    if let Some(s) = new {
        m.insert("newString".into(), json!(s));
    }
    if let Some(s) = diff {
        m.insert("unifiedDiff".into(), json!(s));
    }
    Ok(Some(Value::Object(m)))
}

fn search_query(v: &Value) -> Result<String, ()> {
    let o = v.as_object().ok_or(())?;
    for k in ["query", "q", "pattern"] {
        if let Some(s) = req_str(o, k) {
            return Ok(s.to_owned());
        }
    }
    Err(())
}

/// `toSearchToolDetail`; `output_ok` is whether the branch's output schema accepts what it got.
fn detail_search(input: Option<&Value>, output_ok: bool, tool_name: &'static str) -> Result<Option<Value>, ()> {
    if !output_ok {
        return Err(());
    }
    let query = input.map(search_query).transpose()?;
    let Some(query) = query.filter(|q| !q.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(json!({"type": "search", "query": query, "toolName": tool_name})))
}

fn detail_fetch(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    // ToolWebFetchOutputSchema is an object: a result text fails the branch
    if output.is_some() {
        return Err(());
    }
    let Some(v) = input else {
        return Ok(None);
    };
    let o = v.as_object().ok_or(())?;
    let url = req_str(o, "url").ok_or(())?;
    let prompt = req_str(o, "prompt").ok_or(())?;
    if url.is_empty() {
        return Ok(None);
    }
    let mut m = Map::new();
    m.insert("type".into(), json!("fetch"));
    m.insert("url".into(), json!(url));
    if !prompt.is_empty() {
        m.insert("prompt".into(), json!(prompt));
    }
    Ok(Some(Value::Object(m)))
}

fn detail_skill(input: Option<&Value>, output: Option<&str>) -> Result<Option<Value>, ()> {
    let skill = match input {
        None => None,
        Some(v) => Some(req_str(v.as_object().ok_or(())?, "skill").ok_or(())?.to_owned()),
    };
    let Some(label) = skill.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let mut m = Map::new();
    m.insert("type".into(), json!("plain_text"));
    m.insert("label".into(), json!(label));
    m.insert("icon".into(), json!("sparkles"));
    if let Some(t) = output.filter(|t| !t.is_empty()) {
        m.insert("text".into(), json!(t));
    }
    Ok(Some(Value::Object(m)))
}

fn detail_speak(input: Option<&Value>) -> Result<Option<Value>, ()> {
    let text = match input {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => Some(req_str(o, "text").ok_or(())?.to_owned()),
        Some(_) => return Err(()),
    };
    let text = text.map(|t| t.trim().to_owned()).unwrap_or_default();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({"type": "unknown", "input": text, "output": null})))
}

/// `deriveClaudeToolDetail(name, input, output)`: the detail for the branch named `name`, or `unknown` with the
/// input and output as given when no branch takes them.
#[must_use]
pub fn derive_detail(name: &str, input: &Value, output: Option<&str>) -> Value {
    let inp = if input.is_null() { None } else { Some(input) };
    let derived = match name {
        "Bash" | "bash" | "shell" | "exec_command" => detail_shell(inp, output),
        "Read" | "read" | "read_file" | "view_file" => detail_read(inp, output),
        "Write" | "write" | "write_file" | "create_file" => detail_write(inp, output),
        "Edit" | "MultiEdit" | "multi_edit" | "edit" | "apply_patch" | "apply_diff" | "str_replace_editor" => {
            detail_edit(inp, output)
        }
        "WebSearch" | "web_search" => detail_search(inp, output.is_none(), "web_search"),
        "search" => detail_search(inp, true, "search"),
        "Grep" | "grep" => detail_search(inp, output.is_none(), "grep"),
        "Glob" | "glob" => detail_search(inp, output.is_none(), "glob"),
        "WebFetch" | "web_fetch" | "WebFetchTool" | "web_fetch_tool" | "webfetch" => detail_fetch(inp, output),
        "Skill" => detail_skill(inp, output),
        "speak" => detail_speak(inp),
        _ => Err(()),
    };
    match derived {
        Ok(Some(d)) => d,
        _ => json!({"type": "unknown", "input": input, "output": output}),
    }
}

/// The tool kinds of `resolveClaudeToolKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Shell,
    Read,
    Write,
    Edit,
    Search,
    Fetch,
    Speak,
    Unknown,
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Bash" | "bash" | "shell" | "exec_command" => ToolKind::Shell,
        "Read" | "read" | "read_file" | "view_file" => ToolKind::Read,
        "Write" | "write" | "write_file" | "create_file" => ToolKind::Write,
        "Edit" | "MultiEdit" | "multi_edit" | "edit" | "apply_patch" | "apply_diff" | "str_replace_editor" => {
            ToolKind::Edit
        }
        "WebSearch" | "web_search" | "search" | "Grep" | "grep" | "Glob" | "glob" => ToolKind::Search,
        "WebFetch" | "web_fetch" | "WebFetchTool" | "web_fetch_tool" | "webfetch" => ToolKind::Fetch,
        n if is_speak(n) => ToolKind::Speak,
        _ => ToolKind::Unknown,
    }
}

fn is_speak(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
        .rfind(|t| !t.is_empty())
        == Some("speak")
}

fn detail_name(kind: ToolKind, name: &str) -> &str {
    match kind {
        ToolKind::Shell => "shell",
        ToolKind::Read => "read_file",
        ToolKind::Write => "write_file",
        ToolKind::Edit => "apply_patch",
        ToolKind::Speak => "speak",
        ToolKind::Search | ToolKind::Fetch | ToolKind::Unknown => name,
    }
}

fn status_name(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Running => "running",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Canceled => "canceled",
    }
}

/// A tool row's Paseo item (`mapClaude*ToolCall`), or `None` when the mapper drops it (no call id).
#[must_use]
pub fn tool_call_item(t: &ToolItem, locale: Locale) -> Option<Value> {
    let call_id = t.call_id.trim();
    if call_id.is_empty() || t.name.trim().is_empty() {
        return None;
    }
    let call_id = t.call_id.clone();
    let mut metadata: Option<Map<String, Value>> = t.sent_files.as_ref().map(|sent| {
        let files: Vec<Value> = sent
            .iter()
            .filter(|f| safe_path(&f.path))
            .map(|f| {
                json!({"path": f.path, "name": f.name, "size": f.size, "mediaType": f.mime_type,
                       "isImage": f.is_image, "sent": t.call_id})
            })
            .collect();
        let mut m = Map::new();
        m.insert("sentFiles".into(), Value::Array(files));
        m
    });
    if let Some(at) = &t.answered_at {
        // when the person answered the question this call asked: the app leaves the wait out of the turn's working
        // time (DECISIONS 17)
        metadata
            .get_or_insert_with(Map::new)
            .insert("answeredAt".into(), json!(at));
    }
    let shown = t.result.as_ref().map(|r| {
        if r.truncated {
            format!("{}\n{}", r.text, locale.output_truncated())
        } else {
            r.text.clone()
        }
    });
    let output = shown.as_deref();
    // the name as Claude Code wrote it, trimmed (the mapper's `raw.name.trim()`): an MCP tool keeps its
    // `mcp__<server>__` prefix, as the old stack showed it (cutover 10.2 R6)
    let trimmed = t.name.trim();
    if trimmed == "ExitPlanMode"
        && let Some(plan) = t.input.get("plan").and_then(Value::as_str)
    {
        let name = if t.status == ToolStatus::Running {
            "ExitPlanMode"
        } else {
            "plan_approval"
        };
        let status = match t.status {
            ToolStatus::Failed => ToolStatus::Completed,
            s => s,
        };
        if matches!(t.status, ToolStatus::Completed | ToolStatus::Failed) {
            metadata
                .get_or_insert_with(Map::new)
                .insert("approved".into(), json!(t.status == ToolStatus::Completed));
        }
        let mut m = Map::new();
        m.insert("type".into(), json!("tool_call"));
        m.insert("callId".into(), json!(call_id));
        m.insert("name".into(), json!(name));
        m.insert("status".into(), json!(status_name(status)));
        m.insert("error".into(), Value::Null);
        m.insert("detail".into(), json!({"type": "plan", "text": plan}));
        m.insert("metadata".into(), Value::Object(metadata.unwrap_or_default()));
        return Some(Value::Object(m));
    }
    let kind = tool_kind(trimmed);
    let name = if kind == ToolKind::Speak { "speak" } else { trimmed };
    let detail = derive_detail(detail_name(kind, name), &t.input, output);
    let error = if t.status == ToolStatus::Failed {
        let message = match (&t.result, &t.error) {
            (Some(r), _) if r.is_error && !r.text.is_empty() => r.text.clone(),
            (_, Some(e)) => locale.error_sentence(e),
            _ => "Tool call failed".to_owned(),
        };
        json!({"message": message})
    } else {
        Value::Null
    };
    let mut m = Map::new();
    m.insert("type".into(), json!("tool_call"));
    m.insert("callId".into(), json!(call_id));
    m.insert("name".into(), json!(name));
    m.insert("detail".into(), detail);
    m.insert("status".into(), json!(status_name(t.status)));
    m.insert("error".into(), error);
    if let Some(md) = metadata {
        m.insert("metadata".into(), Value::Object(md));
    }
    Some(Value::Object(m))
}

/// The Paseo `item` of a row (7.8), or `None` for a kind that shows nothing.
#[must_use]
pub fn row_item(row: &Row, ctx: &ProjectCtx<'_>) -> Option<Value> {
    let locale = Locale::from_zh(ctx.zh);
    Some(match &row.item {
        RowItem::User(u) => {
            let mut m = Map::new();
            m.insert("type".into(), json!("user_message"));
            m.insert("text".into(), json!(u.text));
            m.insert("messageId".into(), json!(u.message_id));
            if let Some(c) = &u.client_message_id {
                m.insert("clientMessageId".into(), json!(c));
            }
            if !u.images.is_empty() {
                let images: Vec<Value> = u
                    .images
                    .iter()
                    .map(|i| {
                        json!({
                            "url": ctx.signer.url(&i.blob_id, BlobVariant::Display),
                            "mimeType": i.mime_type,
                            "thumbnailUrl": ctx.signer.url(&i.blob_id, BlobVariant::Thumb),
                        })
                    })
                    .collect();
                m.insert("images".into(), Value::Array(images));
            }
            let files: Vec<Value> = u
                .files
                .iter()
                .filter(|f| safe_path(&f.path))
                .map(|f| {
                    let mime = if f.mime_type.is_empty() {
                        magpie_api::mime::from_name(&f.name).to_owned()
                    } else {
                        f.mime_type.clone()
                    };
                    json!({"type": "uploaded_file", "id": f.path, "fileName": f.name, "mimeType": mime,
                           "size": f.size, "path": f.path})
                })
                .collect();
            if !files.is_empty() {
                m.insert("attachments".into(), Value::Array(files));
            }
            Value::Object(m)
        }
        RowItem::Assistant(a) => {
            let text = a
                .error
                .as_ref()
                .map_or_else(|| a.text.clone(), |e| locale.error_sentence(e));
            if text.is_empty() {
                return None;
            }
            json!({"type": "assistant_message", "text": text, "messageId": a.message_id})
        }
        RowItem::Reasoning(r) => json!({"type": "reasoning", "text": r.text}),
        RowItem::Tool(t) => tool_call_item(t, locale)?,
        RowItem::Error(e) => {
            let info = magpie_api::errors::ErrorInfo::new(e.code.clone(), e.message.clone());
            json!({"type": "error", "message": locale.error_sentence(&info)})
        }
        RowItem::Unknown => return None,
    })
}

/// Cuts the largest string fields of `v` to [`CUT_FIELD_BYTES`] with the truncation line until its JSON is at most
/// `limit` bytes (7.8), and says whether it cut anything.
pub fn cap_size(v: &mut Value, limit: usize, line: &str) -> bool {
    let mut cut = false;
    loop {
        let size = serde_json::to_vec(v).map_or(0, |b| b.len());
        if size <= limit {
            return cut;
        }
        let Some(s) = largest_string(v) else {
            return cut;
        };
        // a field already cut (or short) is not cut again: the loop ends when nothing is left to cut
        if s.len() <= CUT_FIELD_BYTES + line.len() + 1 {
            return cut;
        }
        let mut end = CUT_FIELD_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('\n');
        s.push_str(line);
        cut = true;
    }
}

fn largest_string(v: &mut Value) -> Option<&mut String> {
    fn walk<'a>(v: &'a mut Value, best: &mut Option<&'a mut String>) {
        match v {
            Value::String(s) => {
                if best.as_ref().is_none_or(|b| s.len() > b.len()) {
                    *best = Some(s);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, best)),
            Value::Object(o) => o.values_mut().for_each(|x| walk(x, best)),
            _ => {}
        }
    }
    let mut best = None;
    walk(v, &mut best);
    best
}

/// The projection pigeon serves (7.8): every row kind, the ported tool-call mapper, the 7.9 sentences, the 16 MiB
/// cap.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaseoProjector;

impl RowProjector for PaseoProjector {
    fn project(&self, row: &Row, ctx: &ProjectCtx<'_>) -> Option<Value> {
        let mut item = row_item(row, ctx)?;
        cap_size(
            &mut item,
            MAX_PUSH_BYTES - 64 * 1024,
            Locale::from_zh(ctx.zh).output_truncated(),
        );
        let mut out = Map::new();
        out.insert("provider".into(), json!(PROVIDER));
        out.insert("item".into(), item);
        if let Some(t) = row.turn_id {
            out.insert("turnId".into(), json!(t));
        }
        out.insert("timestamp".into(), json!(row.timestamp));
        out.insert("seqStart".into(), json!(row.seq_start));
        out.insert("seqEnd".into(), json!(row.seq_end));
        out.insert("sourceSeqRanges".into(), json!(seq_ranges(&row.seqs)));
        let collapsed: Vec<&str> = if row.seqs.len() > 1 {
            vec!["tool_lifecycle"]
        } else {
            vec![]
        };
        out.insert("collapsed".into(), json!(collapsed));
        Some(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DECISIONS 17: the working-time counter stands still while a question is open and leaves out the time
    /// answered questions waited.
    #[test]
    fn the_working_time_leaves_out_the_questions_waits() {
        let mut c: Conversation = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(), "title": null, "titleSource": null, "createdAt": "2026-11-05T08:00:00Z",
            "updatedAt": "2026-11-05T08:00:00Z", "lastUserMessageAt": null, "deletedAt": null, "purgeAt": null,
            "claudeSessionId": null, "lastTurn": null, "sandbox": null, "model": null, "contextTokens": null,
            "turn": {"id": uuid::Uuid::nil(), "startedAt": "2026-11-05T08:00:00Z", "origin": "person", "askWaitMs": 90000},
            "run": {"state": "running", "background": []}, "origin": {"kind": "new"}}))
        .unwrap();
        let t = c.turn.clone().unwrap();
        assert_eq!(
            serde_json::to_value(working_since(&c, &t)).unwrap(),
            json!("2026-11-05T08:01:30.000Z")
        );
        c.asks.push(magpie_api::conversations::Ask {
            id: "ask-1".into(),
            tool_name: "AskUserQuestion".into(),
            tool_use_id: Some("toolu_1".into()),
            input: Map::new(),
            title: None,
            description: None,
            created_at: t.started_at,
        });
        assert_eq!(working_since(&c, &t), None, "no counter while the question is open");
    }

    /// A conversation whose person picked a model shows it instead of the deployment's default.
    #[test]
    fn the_agent_shows_the_picked_model() {
        let base = json!({"id": uuid::Uuid::nil(), "title": null, "titleSource": null,
            "createdAt": "2026-11-12T04:00:00Z", "updatedAt": "2026-11-12T04:00:00Z", "lastUserMessageAt": null,
            "deletedAt": null, "purgeAt": null, "claudeSessionId": null, "lastTurn": null, "sandbox": null,
            "model": "claude-fable-5-2", "contextTokens": null, "turn": null,
            "run": {"state": "idle", "background": []}, "origin": {"kind": "new"}});
        let view = View {
            product: "Chat",
            locale: Locale::En,
            model: Some("claude-fable-5-2"),
        };
        let plain: Conversation = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(agent_snapshot(&plain, &view)["model"], "claude-fable-5-2");
        let mut picked = base;
        picked["pickedModel"] = json!("claude-opus-5-5");
        picked["pendingMessages"] = json!([
            {"clientMessageId": "m2", "text": "also this", "attachments": 0, "sentAt": "2026-11-12T04:01:00.000Z",
             "held": false},
            {"clientMessageId": "m3", "text": "and this file", "attachments": 1,
             "sentAt": "2026-11-12T04:01:05.000Z", "held": true}]);
        let picked: Conversation = serde_json::from_value(picked).unwrap();
        let a = agent_snapshot(&picked, &view);
        assert_eq!(a["model"], "claude-opus-5-5");
        assert_eq!(a["runtimeInfo"]["model"], "claude-opus-5-5");
        // the conversation's queue goes to the app with the agent
        assert_eq!(agent_snapshot(&plain, &view)["pendingMessages"], json!([]));
        assert_eq!(
            a["pendingMessages"],
            json!([
                {"messageId": "m2", "text": "also this", "attachments": 0, "sentAt": "2026-11-12T04:01:00.000Z",
                 "held": false},
                {"messageId": "m3", "text": "and this file", "attachments": 1,
                 "sentAt": "2026-11-12T04:01:05.000Z", "held": true}])
        );
    }

    /// A question offers the person's own answer beside its options, as Claude Code's own does: the app shows its
    /// text field only for a question that says `allowOther`. Another tool's input goes as it is.
    #[test]
    fn a_question_offers_an_answer_of_the_persons_own() {
        let ask = |tool: &str, input: Value| magpie_api::conversations::Ask {
            id: "ask-1".into(),
            tool_name: tool.into(),
            tool_use_id: Some("toolu_1".into()),
            input: input.as_object().cloned().unwrap(),
            title: None,
            description: None,
            created_at: serde_json::from_value(json!("2026-11-12T04:00:00Z")).unwrap(),
        };
        let input = json!({"questions": [
            {"question": "Red or Blue?", "header": "Colour", "multiSelect": false,
             "options": [{"label": "Red", "description": ""}, {"label": "Blue", "description": ""}]},
            {"question": "Which sizes?", "header": "Size", "multiSelect": true, "allowOther": false,
             "options": [{"label": "S", "description": ""}]}]});
        let r = permission_request(&ask("AskUserQuestion", input.clone()));
        assert_eq!(r["kind"], "question");
        assert_eq!(r["input"]["questions"][0]["allowOther"], json!(true), "{r}");
        assert_eq!(r["input"]["questions"][0]["options"], input["questions"][0]["options"]);
        assert_eq!(
            r["input"]["questions"][1]["allowOther"],
            json!(false),
            "what the input says stands"
        );
        let other = permission_request(&ask("Bash", input.clone()));
        assert_eq!(other["input"], input, "another tool's input is not touched");
    }

    /// DECISIONS 17: an answered question's tool call says when it was answered (`metadata.answeredAt`), for the
    /// app's completed-turn working time; an unanswered one says nothing.
    #[test]
    fn an_answered_question_carries_its_answer_time() {
        let input = json!({"questions": [{"question": "Red or Blue?", "header": "Colour", "multiSelect": false,
            "options": [{"label": "Red", "description": ""}, {"label": "Blue", "description": ""}]}]});
        let mut t = tool("AskUserQuestion", input, ToolStatus::Running, None);
        let open = tool_call_item(&t, Locale::En).unwrap();
        assert!(open.get("metadata").is_none(), "{open}");
        t.status = ToolStatus::Completed;
        t.result = tool(
            "AskUserQuestion",
            json!({}),
            ToolStatus::Completed,
            Some("The user answered: Blue"),
        )
        .result;
        t.answered_at = Some(serde_json::from_value(json!("2026-11-05T08:42:57.000Z")).unwrap());
        let done = tool_call_item(&t, Locale::En).unwrap();
        assert_eq!(
            done["metadata"]["answeredAt"],
            json!("2026-11-05T08:42:57.000Z"),
            "{done}"
        );
        let mut plan = tool(
            "ExitPlanMode",
            json!({"plan": "1. do it"}),
            ToolStatus::Completed,
            Some("User has approved your plan."),
        );
        plan.answered_at = t.answered_at;
        let plan = tool_call_item(&plan, Locale::En).unwrap();
        assert_eq!(
            plan["metadata"]["answeredAt"],
            json!("2026-11-05T08:42:57.000Z"),
            "{plan}"
        );
        assert_eq!(plan["metadata"]["approved"], json!(true), "{plan}");
    }

    fn tool(name: &str, input: Value, status: ToolStatus, result: Option<&str>) -> ToolItem {
        ToolItem {
            call_id: "toolu_1".into(),
            message_id: "msg_1".into(),
            block_index: 0,
            name: name.into(),
            input,
            status,
            result: result.map(|t| magpie_api::rows::ToolResult {
                text: t.into(),
                truncated: false,
                bytes: t.len() as u64,
                blob_id: None,
                is_error: status == ToolStatus::Failed,
                images: None,
            }),
            sent_files: None,
            task: None,
            error: None,
            answered_at: None,
        }
    }

    fn detail(name: &str, input: Value, status: ToolStatus, result: Option<&str>) -> Value {
        tool_call_item(&tool(name, input, status, result), Locale::En).unwrap()["detail"].clone()
    }

    #[test]
    fn shell_read_write_edit_search_fetch() {
        assert_eq!(
            detail(
                "Bash",
                json!({"command": "ls uploads"}),
                ToolStatus::Completed,
                Some("q3.xlsx\n")
            ),
            json!({"type": "shell", "command": "ls uploads", "output": "q3.xlsx\n"})
        );
        assert_eq!(
            detail(
                "Bash",
                json!({"command": ["git", " status ", ""]}),
                ToolStatus::Running,
                None
            ),
            json!({"type": "shell", "command": "git status"})
        );
        assert_eq!(
            detail(
                "Read",
                json!({"file_path": "/workspace/a.py"}),
                ToolStatus::Completed,
                Some("     3\timport os\n     4\tprint(1)")
            ),
            json!({"type": "read", "filePath": "/workspace/a.py", "content": "import os\nprint(1)", "offset": 3})
        );
        assert_eq!(
            detail(
                "Write",
                json!({"file_path": "/workspace/s.md", "content": "# S\n"}),
                ToolStatus::Completed,
                Some("ok")
            ),
            json!({"type": "write", "filePath": "/workspace/s.md", "content": "# S\n"})
        );
        // an edit's result text fails its output schema (the old bridge showed unknown too)
        let edit_in = json!({"file_path": "/workspace/n.md", "old_string": "a", "new_string": "b"});
        assert_eq!(
            detail("Edit", edit_in.clone(), ToolStatus::Running, None),
            json!({"type": "edit", "filePath": "/workspace/n.md", "oldString": "a", "newString": "b"})
        );
        assert_eq!(
            detail("Edit", edit_in.clone(), ToolStatus::Completed, Some("updated")),
            json!({"type": "unknown", "input": edit_in, "output": "updated"})
        );
        assert_eq!(
            detail("Glob", json!({"pattern": "**/*.xlsx"}), ToolStatus::Running, None),
            json!({"type": "search", "query": "**/*.xlsx", "toolName": "glob"})
        );
        assert_eq!(
            detail(
                "WebFetch",
                json!({"url": "https://e.x/r", "prompt": "p"}),
                ToolStatus::Running,
                None
            ),
            json!({"type": "fetch", "url": "https://e.x/r", "prompt": "p"})
        );
        assert_eq!(
            detail("Skill", json!({"skill": "pdf"}), ToolStatus::Completed, Some("done")),
            json!({"type": "plain_text", "label": "pdf", "icon": "sparkles", "text": "done"})
        );
        assert_eq!(
            detail("TodoWrite", json!({"todos": []}), ToolStatus::Running, None)["type"],
            "unknown"
        );
    }

    /// The mapper's name is the call's own (trimmed): an MCP tool keeps its server prefix and is an `unknown`
    /// detail, as the old bridge's TypeScript mapper made it (cutover 10.2 R6: what staff see today).
    #[test]
    fn mcp_names_keep_their_server() {
        for name in ["mcp__team__web_search", " mcp__tools__web_search "] {
            let item = tool_call_item(
                &tool(name, json!({"query": "hsi"}), ToolStatus::Completed, Some("r")),
                Locale::En,
            )
            .unwrap();
            assert_eq!(item["name"], name.trim());
            assert_eq!(item["detail"]["type"], "unknown");
        }
        let bare = tool_call_item(
            &tool("search", json!({"query": "hsi"}), ToolStatus::Completed, Some("r")),
            Locale::En,
        )
        .unwrap();
        assert_eq!(bare["detail"]["type"], "search");
    }

    #[test]
    fn plan_failed_and_truncated() {
        let plan = tool_call_item(
            &tool("ExitPlanMode", json!({"plan": "1. do"}), ToolStatus::Failed, Some("no")),
            Locale::En,
        )
        .unwrap();
        assert_eq!(plan["name"], "plan_approval");
        assert_eq!(plan["status"], "completed");
        assert_eq!(plan["metadata"], json!({"approved": false}));
        let failed = tool_call_item(
            &tool(
                "Read",
                json!({"file_path": "/x"}),
                ToolStatus::Failed,
                Some("File does not exist."),
            ),
            Locale::En,
        )
        .unwrap();
        assert_eq!(failed["error"], json!({"message": "File does not exist."}));
        let mut t = tool(
            "Bash",
            json!({"command": "cat big"}),
            ToolStatus::Completed,
            Some("abc"),
        );
        t.result.as_mut().unwrap().truncated = true;
        let item = tool_call_item(&t, Locale::Zh).unwrap();
        assert_eq!(item["detail"]["output"], "abc\n…（輸出過長，只顯示開頭部分）");
    }

    #[test]
    fn xml_read_output_and_gutter_guard() {
        let d = detail(
            "Read",
            Value::Null,
            ToolStatus::Completed,
            Some("<path>/w/a&amp;b.txt</path><content>\nx &lt; y\n</content>"),
        );
        assert_eq!(d, json!({"type": "read", "filePath": "/w/a&b.txt", "content": "x < y"}));
        assert!(strip_gutter("1\ta\n3\tb").is_none(), "not sequential");
        assert!(strip_gutter("x\n1\ta").is_none(), "first line has no gutter");
    }

    #[test]
    fn oversized_frames_are_cut() {
        let mut v = json!({"a": "x".repeat(3 << 20), "b": "y".repeat(2 << 20), "c": "small"});
        assert!(cap_size(&mut v, (5 << 20) / 2, "LINE"));
        assert!(v["a"].as_str().unwrap().ends_with("\nLINE"));
        assert!(serde_json::to_vec(&v).unwrap().len() <= (5 << 20) / 2);
        // nothing more to cut: it stops even above the limit
        assert!(!cap_size(&mut v, 1 << 20, "LINE"));
        assert_eq!(v["c"], "small");
    }
}
