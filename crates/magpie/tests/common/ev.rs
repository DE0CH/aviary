//! Events for the timeline tests: builders for every kind the projection reads, a conversation to append them to,
//! and appends in one transaction under the conversation lock (as the writers do).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value
)]

use magpie::events::NewEvent;
use magpie_api::conversations::CreateRequest;
use magpie_api::rows::TimelineQuery;
use magpie_api::turns::SendRequest;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

pub const ALICE: &str = "alice@example.com";

pub fn ev(kind: &str, data: Value) -> NewEvent {
    NewEvent {
        kind: kind.to_owned(),
        data,
        meta: None,
        turn_id: None,
        text: None,
        is_prompt: false,
        viewer: None,
        at: OffsetDateTime::now_utc(),
    }
}

pub fn with_meta(mut e: NewEvent, meta: Value) -> NewEvent {
    e.meta = Some(meta);
    e
}

pub fn in_turn(mut e: NewEvent, turn: Uuid) -> NewEvent {
    e.turn_id = Some(turn);
    e
}

pub fn blob(id: &str, kind: &str) -> Value {
    json!({"$blob": {"blobId": id, "bytes": 10, "kind": kind, "mime": "image/png", "width": 4, "height": 3}})
}

pub fn user_sent(cmid: &str, text: &str) -> NewEvent {
    let mut e = ev(
        "user.sent",
        json!({"clientMessageId": cmid, "text": text, "files": [], "images": []}),
    );
    e.text = Some(text.to_owned());
    e.is_prompt = true;
    e
}

pub fn turn_started(turn: Uuid) -> NewEvent {
    in_turn(ev("turn.started", json!({"turnId": turn, "origin": "person"})), turn)
}

pub fn turn_ended(turn: Uuid, outcome: &str) -> NewEvent {
    let mut data = json!({"turnId": turn, "outcome": outcome});
    if outcome == "failed" {
        data["error"] = json!({"code": "run.oom", "message": "killed by the memory guard"});
    }
    in_turn(ev("turn.ended", data), turn)
}

/// An assistant line of message `mid` with `blocks`.
pub fn assistant(mid: &str, blocks: Value) -> NewEvent {
    let text: Vec<String> = blocks
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect();
    let mut e = ev(
        "cc.assistant",
        json!({"type": "assistant", "uuid": Uuid::new_v4(), "message": {"id": mid, "role": "assistant",
               "content": blocks}}),
    );
    if !text.is_empty() {
        e.text = Some(text.join("\n"));
    }
    e
}

pub fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

pub fn thinking(t: &str) -> Value {
    json!({"type": "thinking", "thinking": t, "signature": "s"})
}

pub fn tool_use(id: &str, name: &str) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": {"command": "ls"}})
}

/// A user line with tool results `(id, text, is_error)`.
pub fn results(rs: &[(&str, &str, bool)]) -> NewEvent {
    let content: Vec<Value> = rs
        .iter()
        .map(|(id, t, err)| json!({"type": "tool_result", "tool_use_id": id, "content": t, "is_error": err}))
        .collect();
    let ids: Vec<Value> = rs
        .iter()
        .map(|(id, _, _)| json!({"toolUseId": id, "toolUseSeq": null}))
        .collect();
    with_meta(
        ev(
            "cc.user",
            json!({"type": "user", "uuid": Uuid::new_v4(), "message": {"role": "user", "content": content}}),
        ),
        json!({"results": ids}),
    )
}

pub fn notification(tool_use_id: &str, status: &str, summary: &str) -> NewEvent {
    with_meta(
        ev(
            "cc.user",
            json!({"type": "user", "uuid": Uuid::new_v4(), "message": {"role": "user",
                   "content": format!("<task-notification>\n<status>{status}</status>\n</task-notification>")}}),
        ),
        json!({"taskNotification": {"toolUseId": tool_use_id, "taskId": "task1", "status": status,
               "summary": summary}}),
    )
}

pub fn prompt(uuid: &str, t: &str) -> NewEvent {
    let mut e = with_meta(
        ev(
            "cc.user",
            json!({"type": "user", "uuid": uuid, "message": {"role": "user", "content": t}}),
        ),
        json!({"prompt": true}),
    );
    e.text = Some(t.to_owned());
    e.is_prompt = true;
    e
}

pub fn files_sent(tool_use_id: &str, path: &str) -> NewEvent {
    ev(
        "files.sent",
        json!({"toolUseId": tool_use_id, "toolUseSeq": null, "caption": null, "files": [
            {"path": path, "name": path.rsplit('/').next().unwrap(), "size": 12, "mimeType": "text/plain",
             "isImage": false, "blob": {"$blob": {"blobId": "ab", "bytes": 12, "kind": "file"}}}]}),
    )
}

pub fn task_status(tool_use_id: &str, status: &str) -> NewEvent {
    ev(
        "task.status",
        json!({"taskId": "task1", "toolUseId": tool_use_id, "event": "updated", "status": status,
               "summary": "half done"}),
    )
}

pub fn retract(seqs: &[i64]) -> NewEvent {
    ev("retract", json!({"seqs": seqs}))
}

/// A new, empty conversation of `person` through M3.
pub async fn conversation(client: &magpie_api::client::Client, person: &str) -> Uuid {
    let key = Uuid::new_v4().to_string();
    client
        .person(person)
        .create_conversation(&CreateRequest {
            model: None,
            thinking: None,
            idempotency_key: key.clone(),
            message: SendRequest {
                client_message_id: format!("{key}:m"),
                text: String::new(),
                images: Vec::new(),
                files: Vec::new(),
            },
        })
        .await
        .unwrap()
        .get()
        .conversation
        .id
}

/// Appends `events` in one transaction, as a writer does; their seqs.
pub async fn append(pool: &aviary_pg::Pool, person: &str, conversation: Uuid, events: Vec<NewEvent>) -> Vec<i64> {
    let person = person.to_owned();
    let (person, events) = (&person, &events); // borrowed: the transaction may run more than once
    aviary_pg::tx!(pool, |t| -> magpie::error::Result<Vec<i64>> {
        magpie::conversations::lock(t, person, conversation).await?;
        let mut seqs = Vec::new();
        for e in events.iter().cloned() {
            seqs.push(magpie::events::append(t, conversation, e).await?);
        }
        Ok(seqs)
    })
    .await
    .unwrap()
}

pub async fn append1(pool: &aviary_pg::Pool, person: &str, conversation: Uuid, e: NewEvent) -> i64 {
    append(pool, person, conversation, vec![e]).await[0]
}

pub fn tail(limit: u32) -> TimelineQuery {
    TimelineQuery {
        tail: true,
        limit: Some(limit),
        ..TimelineQuery::default()
    }
}

pub fn before(s: u64, limit: u32) -> TimelineQuery {
    TimelineQuery {
        before: Some(s),
        limit: Some(limit),
        ..TimelineQuery::default()
    }
}

pub fn after(s: u64, limit: u32) -> TimelineQuery {
    TimelineQuery {
        after: Some(s),
        limit: Some(limit),
        ..TimelineQuery::default()
    }
}

pub fn after_wait(s: u64, wait_ms: u64) -> TimelineQuery {
    TimelineQuery {
        after: Some(s),
        wait_ms: Some(wait_ms),
        ..TimelineQuery::default()
    }
}
