//! Sent-file snapshots and the files panel (magpie 11): the two-phase snapshot, its validation, idempotency, the
//! `files.sent` event, the panel (one entry per path, newest first), downloads of a send's snapshot (not the file as
//! it changed afterwards) with `Range`, and live workspace files through fake-hermit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names
)]

mod common;

use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use common::files::{conversation, gateway, grant_lease};
use magpie::blobs;
use magpie_api::files::RawFileQuery;
use serde_json::{Value, json};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

fn hex(b: &[u8]) -> String {
    blobs::to_hex(&blobs::sha256(b))
}

async fn post(r: &common::Replica, c: Uuid, lease: Option<Uuid>, path: &str, body: &Value) -> (u16, Value) {
    let resp = gateway(r, reqwest::Method::POST, path, c, ALICE, lease)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn put(r: &common::Replica, c: Uuid, lease: Uuid, bytes: &[u8]) -> u16 {
    gateway(
        r,
        reqwest::Method::PUT,
        &format!("/v1/sandbox/blobs/{}", hex(bytes)),
        c,
        ALICE,
        Some(lease),
    )
    .body(bytes.to_vec())
    .send()
    .await
    .unwrap()
    .status()
    .as_u16()
}

/// Sends `files` (path, bytes, mime) as `tool_use_id`, all three steps; the commit's answer.
async fn send(r: &common::Replica, c: Uuid, lease: Uuid, tool_use_id: &str, files: &[(&str, &[u8], &str)]) -> Value {
    let specs: Vec<Value> = files
        .iter()
        .map(|(p, b, m)| json!({"path": p, "size": b.len(), "sha256": hex(b), "mimeType": m}))
        .collect();
    let (st, missing) = post(
        r,
        c,
        Some(lease),
        "/v1/sandbox/sent-files",
        &json!({"toolUseId": tool_use_id, "caption": "here", "files": specs}),
    )
    .await;
    assert_eq!(st, 200, "{missing}");
    for (_, b, _) in files {
        if missing["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == &json!(hex(b)))
        {
            assert_eq!(put(r, c, lease, b).await, 201);
        }
    }
    let (st, done) = post(
        r,
        c,
        Some(lease),
        &format!("/v1/sandbox/sent-files/{tool_use_id}/commit"),
        &json!({}),
    )
    .await;
    assert_eq!(st, 200, "{done}");
    done
}

async fn raw(r: &common::Replica, c: Uuid, q: &RawFileQuery, range: Option<&str>) -> Result<reqwest::Response, u16> {
    r.pigeon()
        .person(ALICE)
        .files_raw(c, q, range)
        .await
        .map_err(|e| e.code().map_or(0, magpie_api::errors::ErrorCode::status))
}

#[tokio::test]
async fn sent_file_snapshots_the_panel_and_downloads() {
    let db = common::fresh_db("sent").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k").await;
    let lease = grant_lease(r.pool(), c).await;
    let v1: &[u8] = b"quarter,total\nq3,48211\n";
    let chart = common::files::png(40, 30, false);

    // phase 1 answers what is missing; a repeat is the same; the commit makes the event and the rows
    let done = send(
        &r,
        c,
        lease,
        "call_1",
        &[
            ("/workspace/report/q3.csv", v1, "text/csv"),
            ("file:///workspace/out/./chart.png", &chart, "image/png"),
        ],
    )
    .await;
    assert_eq!(done["kept"], 2);
    let seq = done["seq"].as_u64().unwrap();
    let (st, again) = post(&r, c, Some(lease), "/v1/sandbox/sent-files/call_1/commit", &json!({})).await;
    assert_eq!((st, &again), (200, &done), "a repeated commit answers the first");
    let (st, missing) = post(
        &r,
        c,
        Some(lease),
        "/v1/sandbox/sent-files",
        &json!({"toolUseId": "call_1", "files": [{"path": "/workspace/report/q3.csv", "size": 3, "sha256": hex(b"new"), "mimeType": "text/csv"}]}),
    )
    .await;
    assert_eq!(
        (st, missing),
        (200, json!({"missing": []})),
        "a committed send is not staged again"
    );
    let ev = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select kind, data from events where conversation_id = $1 and seq = $2",
            &[&c, &i64::try_from(seq).unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(ev.get::<_, String>(0), "files.sent");
    let data: Value = ev.get(1);
    assert_eq!(data["toolUseId"], "call_1");
    assert_eq!(data["caption"], "here");
    assert_eq!(data["files"][0]["path"], "report/q3.csv");
    assert_eq!(data["files"][1]["path"], "out/chart.png");
    assert_eq!(data["files"][1]["isImage"], true);
    assert_eq!(data["files"][1]["blob"]["$blob"]["blobId"], hex(&chart));
    assert_eq!(data["files"][1]["blob"]["$blob"]["width"], 40);

    // the file changes and is sent again: the panel lists it once, with its latest send
    let v2: &[u8] = b"quarter,total\nq3,50000\n";
    send(&r, c, lease, "call_2", &[("/workspace/report/q3.csv", v2, "text/csv")]).await;
    let panel = r.pigeon().person(ALICE).files_panel(c).await.unwrap();
    let got: Vec<(&str, &str)> = panel
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.tool_use_id.as_str()))
        .collect();
    assert_eq!(got, vec![("report/q3.csv", "call_2"), ("out/chart.png", "call_1")]);
    assert_eq!(panel.files[0].blob_id, hex(v2));
    assert_eq!(panel.files[0].caption.as_deref(), Some("here"));

    // a download of the first send gives the first snapshot, not the changed file
    let q = |sent: &str, path: &str| RawFileQuery {
        path: path.to_owned(),
        sent: Some(sent.to_owned()),
        inline: false,
    };
    let d = raw(&r, c, &q("call_1", "report/q3.csv"), None).await.unwrap();
    assert_eq!(d.headers()["content-type"], "text/csv");
    assert_eq!(
        d.headers()["content-disposition"],
        "attachment; filename*=UTF-8''q3.csv"
    );
    assert_eq!(d.headers()["cache-control"], "private, no-cache");
    assert_eq!(d.headers()["x-content-type-options"], "nosniff");
    assert_eq!(d.bytes().await.unwrap().as_ref(), v1);
    let part = raw(&r, c, &q("call_2", "report/q3.csv"), Some("bytes=14-"))
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 14-22/{}", v2.len()).as_str()
    );
    assert_eq!(part.bytes().await.unwrap().as_ref(), &v2[14..]);
    let inline = raw(
        &r,
        c,
        &RawFileQuery {
            inline: true,
            ..q("call_1", "out/chart.png")
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(inline.headers()["content-disposition"], "inline");
    assert_eq!(
        raw(&r, c, &q("call_2", "out/chart.png"), None).await.unwrap_err(),
        404,
        "not in that send"
    );
    assert_eq!(raw(&r, c, &q("call_1", "../etc/passwd"), None).await.unwrap_err(), 400);
    assert_eq!(raw(&r, c, &q("call_1", "/report/q3.csv"), None).await.unwrap_err(), 400);
    // someone else's conversation does not exist for them
    let theirs = r.pigeon().person("bob@example.com").files_panel(c).await.unwrap_err();
    assert_eq!(theirs.code(), Some(&magpie_api::errors::ErrorCode::NotFound));
}

#[tokio::test]
async fn sent_files_are_validated_and_need_the_lease_and_their_blobs() {
    let db = common::fresh_db("sentval").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k").await;
    let lease = grant_lease(r.pool(), c).await;
    let file =
        |path: &str, size: u64| json!({"path": path, "size": size, "sha256": hex(b"x"), "mimeType": "text/plain"});
    let req = |id: &str, files: Vec<Value>| json!({"toolUseId": id, "files": files});
    let p = "/v1/sandbox/sent-files";
    let code = |v: &Value| v["code"].as_str().unwrap_or("").to_owned();
    let (st, _) = post(&r, c, Some(lease), p, &req("bad id!", vec![file("/workspace/a", 1)])).await;
    assert_eq!(st, 400);
    let (st, _) = post(&r, c, Some(lease), p, &req("c", vec![])).await;
    assert_eq!(st, 400);
    let many: Vec<Value> = (0..101).map(|i| file(&format!("/workspace/f{i}"), 1)).collect();
    let (st, _) = post(&r, c, Some(lease), p, &req("c", many)).await;
    assert_eq!(st, 400);
    for bad in [
        "/etc/passwd",
        "/workspace/../etc/passwd",
        "/workspace/",
        "relative/path",
        "/workspace/a\u{0}b",
    ] {
        let (st, v) = post(&r, c, Some(lease), p, &req("c", vec![file(bad, 1)])).await;
        assert_eq!((st, code(&v).as_str()), (400, "bad_path"), "{bad}");
    }
    let (st, v) = post(
        &r,
        c,
        Some(lease),
        p,
        &req("c", vec![file("/workspace/big", (64 << 20) + 1)]),
    )
    .await;
    assert_eq!((st, code(&v).as_str()), (413, "too_large"));
    let five: Vec<Value> = (0..5).map(|i| file(&format!("/workspace/f{i}"), 60 << 20)).collect();
    let (st, v) = post(&r, c, Some(lease), p, &req("c", five)).await;
    assert_eq!((st, code(&v).as_str()), (413, "too_large"), "over 256 MiB in all");
    // the lease: none, or a stale one
    let (st, v) = post(&r, c, None, p, &req("c", vec![file("/workspace/a", 1)])).await;
    assert_eq!((st, code(&v).as_str()), (409, "lease_lost"));
    let (st, _) = post(&r, c, Some(Uuid::new_v4()), p, &req("c", vec![file("/workspace/a", 1)])).await;
    assert_eq!(st, 409);
    // a commit without phase 1, and one whose blob was never put
    let (st, _) = post(&r, c, Some(lease), "/v1/sandbox/sent-files/none/commit", &json!({})).await;
    assert_eq!(st, 404);
    let (st, v) = post(&r, c, Some(lease), p, &req("c", vec![file("/workspace/a", 1)])).await;
    assert_eq!((st, v), (200, json!({"missing": [hex(b"x")]})));
    let (st, _) = post(&r, c, Some(lease), "/v1/sandbox/sent-files/c/commit", &json!({})).await;
    assert_eq!(st, 400);
    // migrating: refused
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update conversations set migrating = true where id = $1", &[&c])
        .await
        .unwrap();
    let (st, v) = post(&r, c, Some(lease), p, &req("c", vec![file("/workspace/a", 1)])).await;
    assert_eq!((st, code(&v).as_str()), (409, "conversation_migrating"));
}

#[tokio::test]
async fn live_workspace_files_come_from_hermit() {
    let hermit = FakeHermit::unguarded(HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let db = common::fresh_db("livefiles").await;
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let c = conversation(&r, ALICE, "k").await;
    let body = b"%PDF-1.7 the report".to_vec();
    hermit.write_file(c, "report/q3.pdf", &body);
    let q = |path: &str, inline: bool| RawFileQuery {
        path: path.to_owned(),
        sent: None,
        inline,
    };
    let d = raw(&r, c, &q("report/q3.pdf", true), None).await.unwrap();
    assert_eq!(d.status(), 200);
    assert_eq!(d.headers()["content-type"], "application/pdf");
    assert_eq!(d.headers()["content-disposition"], "inline");
    assert!(d.headers().contains_key("etag"));
    assert!(d.headers().contains_key("last-modified"));
    assert_eq!(d.bytes().await.unwrap().as_ref(), body.as_slice());
    let part = raw(&r, c, &q("report/q3.pdf", false), Some("bytes=0-3")).await.unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 0-3/{}", body.len()).as_str()
    );
    assert_eq!(part.bytes().await.unwrap().as_ref(), b"%PDF");
    assert_eq!(
        raw(&r, c, &q("report/missing.pdf", false), None).await.unwrap_err(),
        404
    );
    // hermit unreachable: 503
    drop(running);
    let r2 = common::replica_with(&db.url, &t, "http://127.0.0.1:1", common::idle()).await;
    let e = r2
        .pigeon()
        .person(ALICE)
        .files_raw(c, &q("report/q3.pdf", false), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&magpie_api::errors::ErrorCode::Unavailable));
}
