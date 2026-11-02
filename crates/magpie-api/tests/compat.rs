//! Version skew: an older reader skips what a newer magpie adds instead of failing (platform 3.6, supervisor 21.3).
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

use magpie_api::changes::{Change, ChangeBody, ChangesResponse};
use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::{Event, EventBody};
use magpie_api::rows::{Row, RowItem};
use magpie_api::sandbox::{Command, CommandBody, CommandsResponse};
use serde_json::json;

#[test]
fn an_unknown_event_kind_is_skipped_with_its_data() {
    let e: Event = serde_json::from_value(json!({
        "seq": 9, "kind": "turn.paused", "data": {"turnId": "x", "why": [1, 2]}, "at": "2026-11-02T03:46:12.345Z",
        "turnId": null, "meta": {}, "viewer": null, "addedLater": true}))
    .unwrap();
    assert_eq!(e.body, EventBody::Unknown);
    assert_eq!(e.seq, 9);
}

#[test]
fn an_unknown_row_kind_is_skipped() {
    let r: Row = serde_json::from_value(json!({
        "seqStart": 4, "seqEnd": 4, "seqs": [4], "timestamp": "2026-11-02T03:46:12.345Z",
        "item": {"kind": "chart", "series": []}}))
    .unwrap();
    assert_eq!(r.item, RowItem::Unknown);
}

#[test]
fn an_unknown_change_type_is_skipped_in_a_page() {
    let page: ChangesResponse = serde_json::from_value(json!({"generation": "g", "headSeq": 2, "changes": [
        {"seq": 1, "at": "2026-11-02T03:46:12.345Z", "conversationId": "a103219b-5c1e-4d8f-9a7b-3e2f1c0d9b8a",
         "type": "presence", "who": "x"},
        {"seq": 2, "at": "2026-11-02T03:46:12.345Z", "conversationId": "a103219b-5c1e-4d8f-9a7b-3e2f1c0d9b8a",
         "type": "timeline_replaced", "epoch": "5"}]}))
    .unwrap();
    let bodies: Vec<&ChangeBody> = page.changes.iter().map(|c: &Change| &c.body).collect();
    assert_eq!(bodies[0], &ChangeBody::Unknown);
    assert_eq!(bodies[1], &ChangeBody::TimelineReplaced { epoch: "5".into() });
}

#[test]
fn an_unknown_command_type_is_read_so_it_can_be_acked_unsupported() {
    let page: CommandsResponse = serde_json::from_value(json!({"commands": [
        {"cmdSeq": 50, "type": "snapshot", "payload": {"kind": "full"}},
        {"cmdSeq": 51, "type": "stop_tasks", "payload": {}}], "head": 51}))
    .unwrap();
    let c: &Command = &page.commands[0];
    assert_eq!(c.cmd_seq, 50);
    assert_eq!(c.body, CommandBody::Unknown);
    assert_eq!(page.commands[1].body.type_name(), Some("stop_tasks"));
}

#[test]
fn an_unknown_data_code_round_trips() {
    let e: ErrorInfo = serde_json::from_value(json!({"code": "sandbox.frozen", "message": "m"})).unwrap();
    assert_eq!(e.code, DataCode::Other("sandbox.frozen".into()));
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"code": "sandbox.frozen", "message": "m"})
    );
}
