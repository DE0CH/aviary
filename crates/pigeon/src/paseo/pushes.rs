//! The payloads of the server pushes (spec 7.6). A push that carries the socket's subscription id is built once
//! without it and spliced per socket ([`super::messages::session_with_subscription`]), so a poller serialises a row
//! once for every socket it reaches (8.5).

use magpie_api::conversations::{Ask, Conversation};
use magpie_api::errors::ErrorInfo;
use magpie_api::turns::{AgentPermissionResponse, TurnPhase};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::mapping::{
    PROVIDER, View, agent_snapshot, permission_request, project_placement, workspace_descriptor, workspace_id,
};
use super::strings::Locale;

/// `agent_stream` of one projected row (7.6.1), without `subscriptionId`.
#[must_use]
pub fn timeline_row(agent: &Uuid, row: &Value, epoch: &str) -> Value {
    let mut event = Map::new();
    event.insert("type".into(), json!("timeline"));
    event.insert("provider".into(), json!(PROVIDER));
    event.insert("item".into(), row.get("item").cloned().unwrap_or(Value::Null));
    if let Some(t) = row.get("turnId") {
        event.insert("turnId".into(), t.clone());
    }
    json!({"agentId": agent, "event": event, "timestamp": row.get("timestamp").cloned().unwrap_or(Value::Null),
           "seq": row.get("seqEnd").cloned().unwrap_or(Value::Null), "epoch": epoch})
}

/// `agent_stream` of live text (magpie 7.5): a chunk of the answer or of the thinking the running turn is writing,
/// without `subscriptionId`. It has no `seq` (it is no row: the block's row comes when the block is complete), and
/// the app appends it to the message (`messageId`) or the thought it is showing.
#[must_use]
pub fn live_text(agent: &Uuid, turn: &Uuid, message_id: &str, thinking: bool, text: &str) -> Value {
    let item = if thinking {
        json!({"type": "reasoning", "text": text})
    } else {
        json!({"type": "assistant_message", "text": text, "messageId": message_id})
    };
    json!({"agentId": agent,
           "event": {"type": "timeline", "provider": PROVIDER, "item": item, "turnId": turn},
           "timestamp": super::messages::now_iso()})
}

/// `agent_stream` of a turn boundary (7.6.2), without `subscriptionId`.
#[must_use]
pub fn turn_event(agent: &Uuid, phase: TurnPhase, turn: &Uuid, error: Option<&ErrorInfo>, locale: Locale) -> Value {
    let mut event = Map::new();
    let kind = match phase {
        TurnPhase::Started => "turn_started",
        TurnPhase::Completed => "turn_completed",
        TurnPhase::Failed => "turn_failed",
        TurnPhase::Canceled => "turn_canceled",
    };
    event.insert("type".into(), json!(kind));
    event.insert("provider".into(), json!(PROVIDER));
    event.insert("turnId".into(), json!(turn));
    match phase {
        TurnPhase::Failed => {
            let sentence = error.map_or_else(|| locale.turn_failed().to_owned(), |e| locale.error_sentence(e));
            event.insert("error".into(), json!(sentence));
            if let Some(e) = error {
                event.insert("code".into(), json!(e.code.as_str()));
            }
        }
        TurnPhase::Canceled => {
            event.insert("reason".into(), json!("Stopped"));
        }
        TurnPhase::Started | TurnPhase::Completed => {}
    }
    json!({"agentId": agent, "event": event, "timestamp": super::messages::now_iso()})
}

/// `agent.timeline.replacement` (7.6.3), without `subscriptionId`.
#[must_use]
pub fn replacement(agent: &Uuid, epoch: &str) -> Value {
    json!({"agentId": agent, "epoch": epoch})
}

/// `agent_update` upsert (7.6.4), without `subscriptionId`.
#[must_use]
pub fn agent_upsert(c: &Conversation, view: &View<'_>, generation: &str, seq: u64) -> Value {
    json!({"kind": "upsert", "agent": agent_snapshot(c, view), "project": project_placement(c, view),
           "generation": generation, "seq": seq})
}

/// `workspace_update` upsert (7.6.4), without `subscriptionId`.
#[must_use]
pub fn workspace_upsert(c: &Conversation, view: &View<'_>, generation: &str, seq: u64) -> Value {
    let mut w = workspace_descriptor(c, view);
    if let Some(o) = w.as_object_mut() {
        o.insert("syncSeq".into(), json!(seq));
    }
    json!({"kind": "upsert", "workspace": w, "generation": generation, "seq": seq})
}

/// `agent_update` remove (7.6.4), without `subscriptionId`.
#[must_use]
pub fn agent_remove(id: &Uuid, generation: &str, seq: u64) -> Value {
    json!({"kind": "remove", "agentId": id, "generation": generation, "seq": seq})
}

/// `workspace_update` remove (7.6.4), without `subscriptionId`.
#[must_use]
pub fn workspace_remove(id: &Uuid, generation: &str, seq: u64) -> Value {
    json!({"kind": "remove", "id": workspace_id(id), "generation": generation, "seq": seq})
}

/// The sandbox chip message (`compat::SANDBOX_STATUS`, 7.6.5).
#[must_use]
pub fn sandbox_status(agent: &Uuid, state: &str) -> Value {
    json!({"agentId": agent, "state": state})
}

/// `agent_permission_request` (7.5.9).
#[must_use]
pub fn permission(agent: &Uuid, ask: &Ask) -> Value {
    json!({"agentId": agent, "request": permission_request(ask)})
}

/// `agent_permission_resolved` (7.5.9).
#[must_use]
pub fn permission_resolved(agent: &Uuid, ask_id: &str, resolution: &AgentPermissionResponse) -> Value {
    json!({"agentId": agent, "requestId": ask_id, "resolution": resolution})
}
