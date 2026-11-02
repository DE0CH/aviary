//! A pigeon for `testing/paseo-contract` (spec 18.1): the real pigeon (its router behind its own listener) in front of
//! the in-memory magpie model (`common/model.rs`), fake-hermit and a local Access team, plus a control endpoint the
//! Node harness uses to make magpie's side happen (rows, turns, titles, the sandbox chip, asks, history rewrites).
//!
//! `cargo test -p pigeon --test contract_server -- --ignored --exact --nocapture serve` prints one line,
//! `PIGEON_CONTRACT {"url": …, "control": …, "token": …}`, and serves until its stdin closes. The Node harness drives
//! a socket with the apps' own client package against `url` (sending `token` as the Access assertion) and posts
//! `{"op": …}` to `control`:
//!
//! | op | fields | does | answers |
//! |---|---|---|---|
//! | `conversation` | `title`? | a conversation of the staff member | `{id}` |
//! | `row` | `id`, `turn`?, `item` (a magpie `RowItem`) | appends a row | `{seq}` |
//! | `update` | `id`, `seqStart`, `item` | replaces a row's item (it takes the next seq) | `{seq}` |
//! | `turn_start` | `id` | starts a turn (`origin` person) | `{turn}` |
//! | `turn_end` | `id`, `phase`? (`completed`) | ends the running turn | `{}` |
//! | `title` | `id`, `title` | renames (the list upsert) | `{}` |
//! | `sandbox` | `id`, `phase` | the sandbox chip | `{}` |
//! | `ask` | `id`, `ask` (a magpie `Ask`) | opens an ask | `{}` |
//! | `rewrite` | `id`, `drop`? | drops the last `drop` rows (a retract) and rebuilds the rows in a new epoch | `{epoch}` |
//! | `state` | `id` | the rows magpie holds | `{epoch, maxSeq, rows}` |
#![allow(clippy::unwrap_used, clippy::too_many_lines)] // tests

mod common;

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use common::model::{self, Model};
use common::{Harness, Opts, STAFF};
use magpie_api::conversations::SandboxPhase;
use magpie_api::rows::RowItem;
use magpie_api::turns::{TurnOrigin, TurnPhase};
use serde_json::{Value, json};
use uuid::Uuid;

fn uuid(v: &Value, k: &str) -> Uuid {
    v[k].as_str().and_then(|s| s.parse().ok()).unwrap()
}

async fn control(State(m): State<Model>, Json(v): Json<Value>) -> Json<Value> {
    let op = v["op"].as_str().unwrap_or("");
    let out = match op {
        "conversation" => {
            let id = m.add(STAFF, v["title"].as_str());
            json!({"id": id})
        }
        "row" => {
            let item: RowItem = serde_json::from_value(v["item"].clone()).unwrap();
            let turn = v["turn"].as_str().and_then(|s| s.parse().ok());
            json!({"seq": m.row(uuid(&v, "id"), turn, item)})
        }
        "update" => {
            let item: RowItem = serde_json::from_value(v["item"].clone()).unwrap();
            let seq = m.update(uuid(&v, "id"), v["seqStart"].as_u64().unwrap(), |i| *i = item);
            json!({"seq": seq})
        }
        "turn_start" => json!({"turn": m.start_turn(uuid(&v, "id"), TurnOrigin::Person)}),
        "turn_end" => {
            let phase = match v["phase"].as_str().unwrap_or("completed") {
                "failed" => TurnPhase::Failed,
                "cancelled" => TurnPhase::Canceled,
                _ => TurnPhase::Completed,
            };
            m.end_turn(uuid(&v, "id"), phase, None);
            json!({})
        }
        "title" => {
            let title = v["title"].as_str().unwrap().to_owned();
            m.edit(uuid(&v, "id"), |c| c.title = Some(title));
            json!({})
        }
        "sandbox" => {
            let phase = if v["phase"] == "preparing" {
                SandboxPhase::Preparing
            } else {
                SandboxPhase::Ready
            };
            m.sandbox(uuid(&v, "id"), phase);
            json!({})
        }
        "ask" => {
            m.open_ask(uuid(&v, "id"), serde_json::from_value(v["ask"].clone()).unwrap());
            json!({})
        }
        "rewrite" => {
            let id = uuid(&v, "id");
            let drop = usize::try_from(v["drop"].as_u64().unwrap_or(0)).unwrap();
            m.with_conv(id, |c| {
                let keep = c.rows.len().saturating_sub(drop);
                c.rows.truncate(keep);
            });
            json!({"epoch": m.rewrite(id)})
        }
        "state" => {
            let id = uuid(&v, "id");
            let st = m.st();
            let c = &st.convs[&id];
            let rows: Vec<Value> = c
                .rows
                .iter()
                .map(|r| json!({"seqStart": r.seq_start, "seqEnd": r.seq_end, "item": r.item}))
                .collect();
            json!({"epoch": c.epoch, "maxSeq": c.next_seq - 1, "rows": rows})
        }
        _ => json!({"error": format!("unknown op {op}")}),
    };
    Json(out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a server for testing/paseo-contract: runs until its stdin closes"]
async fn serve() {
    let m = Model::default();
    let served = model::serve(&m).await;
    let h = Harness::start_opts(Opts {
        tuning: pigeon::Tuning {
            poll_backoff_min: Duration::from_millis(50),
            poll_backoff_max: Duration::from_millis(200),
            ..common::tuning()
        },
        magpie: Some(served.url()),
        ..Opts::default()
    })
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ctl = format!("http://{}/ctl", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/ctl", axum::routing::post(control))
        .with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    println!(
        "PIGEON_CONTRACT {}",
        json!({"url": h.url, "control": ctl, "token": h.token(STAFF)})
    );
    // until the harness closes our stdin (or exits)
    tokio::task::spawn_blocking(|| {
        let mut sink = String::new();
        while std::io::stdin().read_line(&mut sink).is_ok_and(|n| n > 0) {
            sink.clear();
        }
    })
    .await
    .unwrap();
    drop(h);
}
