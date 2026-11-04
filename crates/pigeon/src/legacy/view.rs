//! A legacy conversation's agent state on a socket (DESIGN 9.3 Lists, 9.5 Asks; found by WP-54's ask persona).
//!
//! The list is native from T0 (magpie's imported rows), but magpie does not hold an old turn's live state: its
//! snapshot of a legacy conversation has no active turn and no ask. Before this, an ask made after the move showed no
//! question and no Stop on the moved tab (every native upsert, the running mirror's among them, said `activeTurn:
//! null`), and after Stop the question stayed (the old answer carries the snapshot from before the resolution, and
//! the old child's later `agent_update` never reached the tab). The old stack says what that state is, so the tab
//! is shown what it says, until the release:
//!
//! - the LU subscribes to the old child's list ([`super::lu`]), and every snapshot of a legacy conversation the old
//!   child sends on it (the list answer, its `agent_update`s, the `agent` of an answer) is kept here, per socket, as
//!   that conversation's old state: the fields of [`STATE`];
//! - every agent snapshot pigeon sends the socket of a legacy conversation (a list page, a change-feed upsert, an
//!   answer through the LU) is magpie's projection, the last one sent being kept as the socket's base, with the old
//!   state over it: one shape for the conversation whichever path a frame takes, magpie's `updatedAt` and title
//!   (the client keeps a snapshot only when its `updatedAt` is not older than the one it has), the old stack's turn
//!   and asks;
//! - a change of the old state is pushed as one `agent_update` upsert of the base with the new state, under the
//!   socket's own list subscription and with no `generation` or `seq`: it is not a list change, so the client's list
//!   cursor (magpie's seq, spec 7.6.4) is left as it is and magpie's changes keep their seqs. No seq is invented
//!   and none is reused;
//! - each frame is sent under the socket's lock, built with the state it carries, so of two frames the later one
//!   always carries the newer old state;
//! - a conversation magpie says is not legacy (released, or never legacy) loses its entry: magpie's state from
//!   then. An LU that ends drops what it said (its successor's list answer says it again).
//!
//! The rejected alternatives: passing the old child's `agent_update` through with the subscription id rewritten
//! leaves magpie's own upserts of the conversation (the running mirror every 5 s, a title catch-up) saying "no turn"
//! after it, and needs a seq for a frame the list never had; mirroring the old turn and asks into magpie (the
//! importer's 5 s running mirror extended) would show an ask up to 5 s late, write magpie's ask rows for a turn
//! magpie does not run, and still not carry the old turn's id that the LU's `turn_*` events name.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::paseo::transport::{Conn, ListKind};

/// The snapshot fields that are the agent's live state (spec 7.7): the old stack's for a legacy conversation.
pub const STATE: [&str; 6] = [
    "status",
    "activeTurn",
    "pendingPermissions",
    "requiresAttention",
    "attentionReason",
    "attentionTimestamp",
];

#[derive(Debug, Default)]
struct Slot {
    /// The last native `{agent, project}` sent to the socket.
    base: Option<Value>,
    /// The old stack's state, as the LU last said it.
    old: Option<Map<String, Value>>,
}

/// One socket's legacy conversations (a field of [`Conn`]; it dies with the socket).
#[derive(Debug, Default)]
pub struct Agents(Mutex<HashMap<Uuid, Slot>>);

impl Agents {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Slot>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The conversation is not legacy (any more): magpie's state from now.
    pub fn forget(&self, conversation: &Uuid) {
        let mut m = self.lock();
        if !m.is_empty() {
            m.remove(conversation);
        }
    }

    /// The LU ended: what it said is no longer kept up to date.
    pub fn lu_ended(&self) {
        for s in self.lock().values_mut() {
            s.old = None;
        }
    }

    /// Whether the old state of a conversation is known.
    #[cfg(test)]
    #[must_use]
    pub fn knows(&self, conversation: &Uuid) -> bool {
        self.lock().get(conversation).is_some_and(|s| s.old.is_some())
    }
}

fn id_of(agent: &Value) -> Option<Uuid> {
    agent
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// The [`STATE`] fields of an old snapshot.
fn state_of(agent: &Value) -> Option<Map<String, Value>> {
    let o = agent.as_object()?;
    let mut m = Map::new();
    for k in STATE {
        m.insert(k.to_owned(), o.get(k).cloned().unwrap_or(Value::Null));
    }
    // the old bridge omits nothing, but a snapshot without its status is not one
    o.contains_key("status").then_some(m)
}

/// Whether two states differ in anything but the attention time (the old bridge stamps it anew on each snapshot).
fn differs(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    STATE
        .iter()
        .filter(|k| **k != "attentionTimestamp")
        .any(|k| a.get(*k) != b.get(*k))
}

/// `agent` with `old` over it.
fn over(agent: &mut Value, old: &Map<String, Value>) {
    if let Some(a) = agent.as_object_mut() {
        for (k, v) in old {
            a.insert(k.clone(), v.clone());
        }
    }
}

/// The `{agent, project}` objects of a list frame: a page's `entries`, or the frame itself (an upsert).
fn entries_mut(payload: &mut Value) -> Vec<&mut Value> {
    if payload.get("entries").is_some_and(Value::is_array) {
        payload["entries"]
            .as_array_mut()
            .map(|a| a.iter_mut().collect())
            .unwrap_or_default()
    } else if payload.get("agent").is_some_and(Value::is_object) {
        vec![payload]
    } else {
        Vec::new()
    }
}

/// Sends a native list frame carrying agent snapshots (`fetch_agents_response`, an `agent_update` upsert with its
/// `subscriptionId`) on `conn`: each conversation in `legacy` is kept as the socket's base and carries the old state
/// where it is known; any other loses its entry.
pub(crate) fn send_list(conn: &Conn, kind: &str, mut payload: Value, legacy: &dyn Fn(&Uuid) -> bool) -> bool {
    let mut m = conn.legacy_agents.lock();
    for e in entries_mut(&mut payload) {
        let Some(id) = id_of(&e["agent"]) else { continue };
        if !legacy(&id) {
            m.remove(&id);
            continue;
        }
        let slot = m.entry(id).or_default();
        slot.base =
            Some(json!({"agent": e["agent"].clone(), "project": e.get("project").cloned().unwrap_or(Value::Null)}));
        if let Some(old) = &slot.old {
            over(&mut e["agent"], old);
        }
    }
    conn.reply(kind, &payload)
}

/// An old snapshot of a legacy conversation the LU received: kept as its old state; when that changed and the
/// socket lists conversations and has been sent the conversation, one `agent_update` upsert with it.
pub(crate) fn old_snapshot(conn: &Conn, agent: &Value) {
    let (Some(id), Some(mut state)) = (id_of(agent), state_of(agent)) else {
        return;
    };
    let mut m = conn.legacy_agents.lock();
    let slot = m.entry(id).or_default();
    let changed = slot.old.as_ref().is_none_or(|o| differs(o, &state));
    if !changed {
        return;
    }
    if let Some(prev) = &slot.old
        && prev.get("requiresAttention") == Some(&Value::Bool(true))
        && state.get("requiresAttention") == Some(&Value::Bool(true))
    {
        // the same attention, from when it began
        state.insert(
            "attentionTimestamp".into(),
            prev.get("attentionTimestamp").cloned().unwrap_or(Value::Null),
        );
    }
    slot.old = Some(state);
    let (Some(base), Some(sub)) = (&slot.base, conn.list_sub(ListKind::Agents)) else {
        return;
    };
    let mut a = base["agent"].clone();
    if let Some(old) = &slot.old {
        over(&mut a, old);
    }
    conn.reply(
        "agent_update",
        &json!({"subscriptionId": sub, "kind": "upsert", "agent": a, "project": base["project"]}),
    );
}

/// The old child's answer to the tab (through the LU): an `agent` snapshot in it becomes the socket's base with the
/// old state over it, when the socket has been sent the conversation natively.
pub(crate) fn send_answer(conn: &Conn, kind: &str, mut payload: Value) -> bool {
    let m = conn.legacy_agents.lock();
    if let Some(id) = payload.get("agent").and_then(id_of)
        && let Some(slot) = m.get(&id)
        && let Some(base) = &slot.base
    {
        let mut a = base["agent"].clone();
        match &slot.old {
            Some(old) => over(&mut a, old),
            None => {
                if let Some(s) = state_of(&payload["agent"]) {
                    over(&mut a, &s);
                }
            }
        }
        payload["agent"] = a;
    }
    conn.reply(kind, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: Uuid, running: bool, asks: &[&str], updated: &str) -> Value {
        let asks: Vec<Value> = asks.iter().map(|a| json!({"id": a, "kind": "question"})).collect();
        let attention = !asks.is_empty();
        json!({"id": id, "title": "t", "updatedAt": updated, "status": if running { "running" } else { "idle" },
               "activeTurn": if running { json!({"turnId": "old-turn", "startedAt": updated}) } else { Value::Null },
               "pendingPermissions": asks, "requiresAttention": attention,
               "attentionReason": if attention { json!("permission") } else { Value::Null },
               "attentionTimestamp": if attention { json!(updated) } else { Value::Null }})
    }

    fn frames(rx: &mut tokio::sync::mpsc::Receiver<String>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(f) = rx.try_recv() {
            out.push(serde_json::from_str::<Value>(&f).unwrap()["message"].clone());
        }
        out
    }

    /// magpie's snapshot carries the old state; a change of it is one upsert without a list position; the attention
    /// keeps its start; a native frame after it carries it too; a conversation no longer legacy is magpie's.
    #[test]
    fn the_old_state_over_magpie_s_snapshot() {
        let (conn, (mut rx, _)) = Conn::detached("staff@example.com");
        conn.subscribe_list(ListKind::Agents, Some("agents".into()));
        let c = Uuid::new_v4();
        let native = snap(c, false, &[], "2026-11-04T09:00:00.000Z");
        let page = json!({"entries": [{"agent": native, "project": {"projectKey": "p"}}]});
        assert!(send_list(&conn, "fetch_agents_response", page.clone(), &|_| true));
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["entries"][0]["agent"]["activeTurn"], Value::Null);
        // the old turn asks: one upsert of magpie's snapshot with the old state, and no list position
        let mut old = snap(c, true, &["ask-1"], "2026-11-04 09:00:05");
        old["title"] = json!("old title");
        old_snapshot(&conn, &old);
        let f = frames(&mut rx);
        assert_eq!(f.len(), 1);
        let p = &f[0]["payload"];
        assert_eq!(f[0]["type"], "agent_update");
        assert_eq!(p["subscriptionId"], "agents");
        assert_eq!(p["agent"]["activeTurn"]["turnId"], "old-turn");
        assert_eq!(p["agent"]["pendingPermissions"][0]["id"], "ask-1");
        assert_eq!(p["agent"]["title"], "t", "magpie's title");
        assert_eq!(
            p["agent"]["updatedAt"], "2026-11-04T09:00:00.000Z",
            "magpie's updatedAt"
        );
        assert!(p.get("seq").is_none() && p.get("generation").is_none());
        // the same state again (a new attention time only): nothing
        old["attentionTimestamp"] = json!("2026-11-04 09:00:09");
        old_snapshot(&conn, &old);
        assert_eq!(frames(&mut rx), Vec::<Value>::new());
        // a native upsert (the running mirror) carries the old state, the attention from when it began
        let up = json!({"subscriptionId": "agents", "kind": "upsert", "agent": snap(c, false, &[], "2026-11-04T09:00:06.000Z"),
                        "project": {"projectKey": "p"}, "generation": "g", "seq": 7});
        assert!(send_list(&conn, "agent_update", up.clone(), &|_| true));
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["agent"]["pendingPermissions"][0]["id"], "ask-1");
        assert_eq!(f[0]["payload"]["agent"]["attentionTimestamp"], "2026-11-04 09:00:05");
        assert_eq!(f[0]["payload"]["seq"], 7);
        // an answer through the LU with the old snapshot from before the resolution: the state the LU said last
        let stopped = snap(c, false, &[], "2026-11-04 09:00:07");
        old_snapshot(&conn, &stopped);
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["agent"]["pendingPermissions"], json!([]));
        assert_eq!(f[0]["payload"]["agent"]["updatedAt"], "2026-11-04T09:00:06.000Z");
        assert!(send_answer(
            &conn,
            "cancel_agent_response",
            json!({"agentId": c, "agent": old, "error": null})
        ));
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["agent"]["pendingPermissions"], json!([]));
        assert_eq!(f[0]["payload"]["agent"]["status"], "idle");
        // released: magpie's snapshot as it is, and the old child's frames of it no longer shown
        assert!(send_list(&conn, "agent_update", up, &|_| false));
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["agent"]["pendingPermissions"], json!([]));
        assert!(!conn.legacy_agents.knows(&c));
        old_snapshot(&conn, &snap(c, true, &["ask-2"], "2026-11-04 09:00:10"));
        assert_eq!(frames(&mut rx), Vec::<Value>::new(), "no base: nothing pushed");
    }

    /// An LU that ends drops what it said: magpie's state until its successor says it again.
    #[test]
    fn an_ended_lu_s_state_is_not_kept() {
        let (conn, (mut rx, _)) = Conn::detached("staff@example.com");
        conn.subscribe_list(ListKind::Agents, Some("agents".into()));
        let c = Uuid::new_v4();
        let page = json!({"entries": [{"agent": snap(c, false, &[], "2026-11-04T09:00:00.000Z"), "project": null}]});
        send_list(&conn, "fetch_agents_response", page.clone(), &|_| true);
        old_snapshot(&conn, &snap(c, true, &[], "x"));
        conn.legacy_agents.lu_ended();
        let _ = frames(&mut rx);
        send_list(&conn, "fetch_agents_response", page, &|_| true);
        let f = frames(&mut rx);
        assert_eq!(f[0]["payload"]["entries"][0]["agent"]["status"], "idle");
    }
}
