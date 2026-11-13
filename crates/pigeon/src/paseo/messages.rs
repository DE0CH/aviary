//! The Paseo wire messages pigeon reads and writes (spec 7.2-7.5.1): the session envelope, the top-level frames,
//! the handshake's `server_info`, and the replies of the static and developer-machine requests.
//!
//! Outbound payloads are built as JSON in the exact shapes of the app's `WSOutboundMessageSchema`; the contract suite
//! records every frame pigeon sends and validates the corpus against every client build (spec 18.1). Inbound
//! messages are read field by field: the app's own schema is loose about what pigeon ignores, and a field of the
//! wrong type is treated as absent.

use serde_json::{Map, Value, json};

use super::compat;
use super::mapping::{PROVIDER, SERVER_ID, project_descriptor};

/// The protocol versions this build accepts in `hello` (7.2; the rename of 1.2 adds 2).
pub const PROTOCOLS: &[u64] = &[1];
/// The version pigeon names when the app's own is not a plain `x.y.z` (7.2).
pub const DEFAULT_VERSION: &str = "0.13.2";

/// `{"type":"session","message":{"type":…,"payload":…}}` as text.
#[must_use]
pub fn session(kind: &str, payload: &Value) -> String {
    json!({"type": "session", "message": {"type": kind, "payload": payload}}).to_string()
}

/// A session frame whose payload starts with `subscriptionId`, built from a payload serialised once for every
/// socket (8.5): `rest` is that payload's JSON object without its opening brace (`"agentId":…}`), or `}` when empty.
#[must_use]
pub fn session_with_subscription(kind: &str, subscription: &str, rest: &str) -> String {
    let sep = if rest.starts_with('}') { "" } else { "," };
    format!(
        r#"{{"type":"session","message":{{"type":{},"payload":{{"subscriptionId":{}{sep}{rest}}}}}"#,
        Value::from(kind),
        Value::from(subscription)
    )
}

/// A payload's JSON without its opening brace, for [`session_with_subscription`].
#[must_use]
pub fn payload_rest(payload: &Value) -> String {
    let s = payload.to_string();
    s.strip_prefix('{').map_or(s.clone(), str::to_owned)
}

/// The raw `{"type":"pong"}` (7.1.3).
pub const PONG: &str = r#"{"type":"pong"}"#;

/// `hello.rejected` for an unknown protocol (7.2).
#[must_use]
pub fn hello_rejected() -> String {
    json!({"type": "hello.rejected", "reason": "incompatible_protocol", "accepts": []}).to_string()
}

/// The handshake's `status` payload (7.2).
#[must_use]
pub fn server_info(product: &str, app_version: Option<&str>, protocol: u64) -> Value {
    let version = app_version.filter(|v| is_plain_version(v)).unwrap_or(DEFAULT_VERSION);
    json!({
        "status": "server_info", "protocolVersion": protocol, "serverId": SERVER_ID, "hostname": product,
        "version": version, "sourceId": "owner", "permissions": ["daemon.read", "workspace.read", "workspace.write"],
        "desktopManaged": false,
        "capabilities": {"voice": {"dictation": {"enabled": false, "reason": ""}, "voice": {"enabled": false, "reason": ""}}},
        "features": {"ownedSubscriptions": true, "explicitEventSubscriptions": true, "selectiveAgentTimeline": true,
            "providersSnapshot": true, "providersSnapshotCwd": true, "directorySync": true, "workspaceMultiplicity": true,
            "canonicalSubmittedPrompts": true, "agentTimelinePromptIndex": true, "workspaceSortTimes": true,
            "stableProjectIdentity": true, "projectList": true, "creationLifecycle": true, "agentRequestReceipts": true,
            "workspaceRequestReceipts": true},
    })
}

/// `^\d+\.\d+\.\d+$`.
#[must_use]
pub fn is_plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The daemon settings the app reads (`paseo-connection.ts` `DAEMON_CONFIG`): nothing is configurable.
#[must_use]
pub fn daemon_config() -> Value {
    json!({
        "relay": {"enabled": false, "requireProtectedSecurity": false},
        "mcp": {"enabled": false, "injectIntoAgents": false},
        "hostnames": [], "cors": {"allowedOrigins": []}, "trustedProxies": ["loopback"],
        "git": {"maxProcessesPerSecond": 1, "maxProcessConcurrency": 1}, "app": {"baseUrl": ""},
        "browserTools": {"enabled": false}, "providers": {}, "metadataGeneration": {"providers": []},
        "autoArchiveAfterMerge": false, "enableTerminalAgentHooks": false, "idleRuntimeCleanup": true,
        "appendSystemPrompt": "", "skills": {}, "pluginsEnabled": false, "plugins": {},
        "features": {
            "dictation": {"enabled": false, "stt": {"provider": "local"}},
            "voiceMode": {"enabled": false, "stt": {"provider": "local"}, "tts": {"provider": "local"}},
        },
        "envOverrides": {},
    })
}

/// A string field, `None` when absent or not a string.
#[must_use]
pub fn str_field<'a>(m: &'a Value, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

/// A field as `true` only when it is the JSON `true`.
#[must_use]
pub fn bool_field(m: &Value, k: &str) -> bool {
    m.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// The `subscribe` of a request: `true` (the create and creation requests) or an object, `{}` or
/// `{"subscriptionId": …}` (the list requests, as the apps send them); `Some` with the id the client chose, if any.
#[must_use]
pub fn subscribe(m: &Value) -> Option<Option<String>> {
    match m.get("subscribe")? {
        Value::Bool(true) => Some(None),
        Value::Object(o) => Some(
            o.get("subscriptionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        ),
        _ => None,
    }
}

/// A non-empty `sync` object (the client holds a cursor).
#[must_use]
pub fn has_sync(m: &Value) -> bool {
    m.get("sync").and_then(Value::as_object).is_some_and(|o| !o.is_empty())
}

/// The `sync` of a snapshot reply (7.5.2).
#[must_use]
pub fn sync_snapshot(generation: &str, head_seq: u64, client_sync: bool) -> Value {
    json!({"generation": generation, "headSeq": head_seq, "mode": "snapshot",
           "reason": if client_sync { "cursor_expired" } else { "no_cursor" }, "removals": []})
}

/// Now, ISO-8601 UTC with milliseconds.
#[must_use]
pub fn now_iso() -> String {
    magpie_api::Timestamp::now().to_wire()
}

/// The reply to a request the socket answers without an upstream (7.5.1), `None` for a type that is not one of them.
/// `product` names the one host and project; `generation` is the person's list generation when known.
#[must_use]
pub fn static_reply(
    kind: &str,
    m: &Value,
    request_id: &str,
    product: &str,
    generation: &str,
) -> Option<(&'static str, Value)> {
    let rid = request_id;
    let new_id = || uuid::Uuid::new_v4().to_string();
    Some(match kind {
        "session.events.set_subscription.request" => (
            "session.events.set_subscription.response",
            json!({"requestId": rid, "subscriptionId": new_id()}),
        ),
        "workspace.label.list.request" => {
            let mut p = Map::new();
            p.insert("requestId".into(), json!(rid));
            if let Some(id) = subscribe(m) {
                p.insert("subscriptionId".into(), json!(id.unwrap_or_else(new_id)));
            }
            p.insert("labels".into(), json!([]));
            p.insert(
                "sync".into(),
                json!({"mode": if has_sync(m) { "changes" } else { "snapshot" }, "generation": generation,
                       "headSeq": 0, "removals": []}),
            );
            ("workspace.label.list.response", Value::Object(p))
        }
        "hosts.list.request" => (
            "hosts.list.response",
            json!({"requestId": rid, "subscriptionId": new_id(),
                   "directory": {"self": {"serverId": SERVER_ID, "label": product}, "hosts": []}}),
        ),
        "usage.list_reports.request" => ("usage.list_reports.response", json!({"requestId": rid, "error": null})),
        "plugin.catalog.get.request" => ("plugin.catalog.get.response", json!({"requestId": rid, "plugins": []})),
        "project.icon.get.request" => (
            "project.icon.get.response",
            json!({"requestId": rid, "projectId": m.get("projectId").cloned().unwrap_or(Value::Null),
                   "icon": null, "error": null}),
        ),
        "project_icon_request" => (
            "project_icon_response",
            json!({"requestId": rid, "cwd": str_field(m, "cwd").unwrap_or(""), "icon": null, "error": null}),
        ),
        "list_provider_features_request" => (
            "list_provider_features_response",
            json!({"requestId": rid, "provider": PROVIDER, "features": [], "error": null, "fetchedAt": now_iso()}),
        ),
        "get_daemon_config_request" => (
            "get_daemon_config_response",
            json!({"requestId": rid, "config": daemon_config()}),
        ),
        "checkout_status_request" => (
            "checkout_status_response",
            json!({"requestId": rid, "cwd": str_field(m, "cwd").unwrap_or(""), "isGit": false, "repoRoot": null,
                   "currentBranch": null, "isDirty": null, "baseRef": null, "aheadBehind": null, "upstreamRef": null,
                   "aheadOfOrigin": null, "behindOfOrigin": null, "hasRemote": false, "remoteUrl": null,
                   "isPaseoOwnedWorktree": false, "error": null}),
        ),
        "checkout_pr_status_request" => (
            "checkout_pr_status_response",
            json!({"requestId": rid, "cwd": str_field(m, "cwd").unwrap_or(""), "status": null,
                   "githubFeaturesEnabled": false, "authState": "no_remote", "error": null}),
        ),
        "subscribe_terminals_request" => (
            "terminals_changed",
            json!({"requestId": rid, "cwd": str_field(m, "cwd").unwrap_or(""),
                   "workspaceId": str_field(m, "workspaceId").unwrap_or(""), "terminals": [],
                   "subscriptionId": new_id()}),
        ),
        "list_terminals_request" => (
            "list_terminals_response",
            json!({"requestId": rid, "cwd": str_field(m, "cwd").unwrap_or(""), "terminals": []}),
        ),
        "workspace.script.list.request" => (
            "workspace.script.list.response",
            json!({"requestId": rid, "workspaceId": str_field(m, "workspaceId").unwrap_or(""), "scripts": [],
                   "error": null}),
        ),
        "agent.provider_subagents.list.request" => (
            "agent.provider_subagents.list.response",
            json!({"requestId": rid, "parentAgentId": str_field(m, "parentAgentId").unwrap_or(""),
                   "subagents": [], "error": null}),
        ),
        _ => return None,
    })
}

/// `get_providers_snapshot_response` (7.5.1) from the model list (`{"OPTIONS", "DEFAULT"}`, 5.5), or with no model
/// when `model` cannot be reached.
#[must_use]
pub fn providers_snapshot(request_id: &str, cwd: Option<&str>, models: Option<&Value>) -> Value {
    let default = models.and_then(|m| m.get("DEFAULT")).and_then(Value::as_str);
    let label = default.and_then(|d| {
        models?
            .get("OPTIONS")?
            .as_array()?
            .iter()
            .find(|o| o.get("value").and_then(Value::as_str) == Some(d))?
            .get("label")?
            .as_str()
    });
    let label = label.or(default).unwrap_or("");
    // every model of the list, the default marked; a model with thinking levels points at its set (the levels as
    // options, their ids as labels: the app words them; the model's own level is the set's default)
    let mut thinking_sets: Vec<Value> = Vec::new();
    let model_list: Vec<Value> = models
        .and_then(|m| m.get("OPTIONS"))
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|o| {
                    let id = o.get("value").and_then(Value::as_str)?;
                    let mut model = Map::new();
                    model.insert("id".into(), json!(id));
                    model.insert(
                        "label".into(),
                        json!(o.get("label").and_then(Value::as_str).unwrap_or(id)),
                    );
                    model.insert("description".into(), json!(""));
                    model.insert("isDefault".into(), json!(Some(id) == default));
                    let levels: Vec<&str> = o
                        .get("thinking")
                        .and_then(Value::as_array)
                        .map(|l| l.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    if !levels.is_empty() {
                        let own = o
                            .get("reasoning")
                            .and_then(Value::as_str)
                            .filter(|r| levels.contains(r));
                        let options: Vec<Value> = levels
                            .iter()
                            .map(|l| json!({"id": l, "label": l, "isDefault": Some(*l) == own}))
                            .collect();
                        let mut set = Map::new();
                        set.insert("options".into(), json!(options));
                        if let Some(own) = own {
                            set.insert("defaultOptionId".into(), json!(own));
                        }
                        let set = Value::Object(set);
                        let index = thinking_sets.iter().position(|s| *s == set).unwrap_or_else(|| {
                            thinking_sets.push(set);
                            thinking_sets.len() - 1
                        });
                        model.insert("thinkingSet".into(), json!(index));
                    }
                    // a model that runs in another harness than Claude Code says so: the app asks before a
                    // conversation moves between harnesses (it is carried over as a summary)
                    if let Some(h) = o.get("harness").and_then(Value::as_str) {
                        model.insert("metadata".into(), json!({"harness": h}));
                    }
                    Some(Value::Object(model))
                })
                .collect()
        })
        .unwrap_or_default();
    let hash = serde_json::to_string(&(default, &model_list, &thinking_sets)).unwrap_or_default();
    let now = now_iso();
    let mut p = Map::new();
    p.insert("requestId".into(), json!(request_id));
    if let Some(c) = cwd {
        p.insert("cwd".into(), json!(c));
    }
    p.insert("generatedAt".into(), json!(now));
    p.insert("entries".into(), json!([]));
    p.insert("snapshotHash".into(), json!(format!("models:{hash}")));
    p.insert("fetchedAt".into(), json!({PROVIDER: now}));
    p.insert(
        "compactSnapshot".into(),
        json!({"thinkingSets": thinking_sets, "entries": [{"provider": PROVIDER, "status": "ready", "enabled": true,
            "source": "builtin", "label": label, "description": "", "defaultModeId": null, "models": model_list,
            "modes": []}]}),
    );
    Value::Object(p)
}

/// `project.list.response` (7.5.1).
#[must_use]
pub fn project_list(
    request_id: &str,
    view: &super::mapping::View<'_>,
    generation: &str,
    head: u64,
    sync: bool,
) -> Value {
    json!({"requestId": request_id, "projects": [project_descriptor(view)],
           "sync": sync_snapshot(generation, head, sync)})
}

/// `rpc_error` (7.3).
#[must_use]
pub fn rpc_error(request_id: &str, request_type: &str, error: &str, code: &str) -> Value {
    json!({"requestId": request_id, "requestType": request_type, "error": error, "code": code})
}

/// The message type the app listens to for the sandbox chip (7.6.5), for this protocol.
#[must_use]
pub fn sandbox_status_type() -> &'static str {
    compat::SANDBOX_STATUS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model of another harness names it in its `metadata`; a Claude Code model reads as it always did.
    #[test]
    fn a_snapshot_names_the_harness_of_a_model_that_is_not_claude_codes() {
        let models = json!({"DEFAULT": "vendor/a", "OPTIONS": [
            {"value": "vendor/a", "label": "A"},
            {"value": "vendor/video", "label": "Video", "harness": "qwen"}]});
        let snap = providers_snapshot("r1", None, Some(&models));
        let listed = &snap["compactSnapshot"]["entries"][0]["models"];
        assert_eq!(listed[0].get("metadata"), None);
        assert_eq!(listed[1]["metadata"], json!({"harness": "qwen"}));
    }

    #[test]
    fn subscription_splice_is_valid_json() {
        let p = json!({"agentId": "a", "seq": 3});
        let f = session_with_subscription("agent_stream", "sub-1", &payload_rest(&p));
        let v: Value = serde_json::from_str(&f).unwrap();
        assert_eq!(
            v["message"]["payload"],
            json!({"subscriptionId": "sub-1", "agentId": "a", "seq": 3})
        );
        let f = session_with_subscription("x", "s", &payload_rest(&json!({})));
        let v: Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["message"]["payload"], json!({"subscriptionId": "s"}));
    }

    #[test]
    fn versions() {
        assert!(is_plain_version("0.13.2"));
        assert!(!is_plain_version("0.13"));
        assert!(!is_plain_version("1.2.x"));
        assert_eq!(server_info("P", Some("1.0.0-beta"), 1)["version"], DEFAULT_VERSION);
        assert_eq!(server_info("P", Some("2.1.0"), 1)["version"], "2.1.0");
    }
}
