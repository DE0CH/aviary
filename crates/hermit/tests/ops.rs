//! hermit 24.2 "Ops" (ported from jarvis3 `ops.test.ts`) and the admin routes (16): the admin re-check, the
//! monitor token's scope, every snapshot section from live-shaped rows, the verdict, the page's halves, the two
//! monitor contracts (`tests/fixtures/monitor-contract.json`, every run), imported rows keeping their `at`, and every
//! admin call written to `admin_events`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::Caller;
use aviary_fakes::{FakeMagpie, Reply};
use common::{ADMIN, Harness, STAFF, eventually};
use hermit::fabric::{FabricOps, Fut, ScalerPlan};
use hermit::ops::{Cluster, ServiceObjects, Upstreams};
use serde_json::{Value, json};
use uuid::Uuid;

const FORMER: &str = "former@example.com";

/// The Kubernetes API as `hermit-read` sees it: pigeon fine, magpie rolling, hermit fine, egress with one pod in a
/// crash loop, no tools Deployment.
struct FakeCluster;

fn pod(name: &str, app: &str, ok: bool, restarts: i64) -> Value {
    json!({
        "metadata": {"name": name, "labels": {"app": app}, "creationTimestamp": "2026-11-02T00:00:00Z"},
        "spec": {"nodeName": "pool-worker1"},
        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": if ok { "True" } else { "False" }}],
                   "containerStatuses": [{"restartCount": restarts,
                       "state": if ok { json!({"running": {}}) } else { json!({"waiting": {"reason": "CrashLoopBackOff"}}) }}]},
    })
}

fn deployment(name: &str, generation: i64, observed: i64, desired: i64, total: i64, ready: i64, updated: i64) -> Value {
    json!({"metadata": {"name": name, "generation": generation}, "spec": {"replicas": desired},
           "status": {"observedGeneration": observed, "replicas": total, "readyReplicas": ready, "updatedReplicas": updated}})
}

impl Cluster for FakeCluster {
    fn service<'a>(&'a self, apps: &'a [String]) -> Fut<'a, Result<ServiceObjects, String>> {
        Box::pin(async move {
            assert_eq!(apps, ["pigeon", "magpie", "hermit", "egress", "tools"]);
            Ok(Some((
                vec![
                    deployment("pigeon", 3, 3, 2, 2, 2, 2),
                    deployment("magpie", 5, 5, 2, 3, 2, 1),
                    deployment("hermit", 1, 1, 2, 2, 2, 2),
                    deployment("egress", 1, 1, 2, 2, 1, 2),
                    deployment("cloudflared", 1, 1, 2, 2, 2, 2),
                ],
                vec![
                    pod("pigeon-a-1", "pigeon", true, 0),
                    pod("pigeon-a-2", "pigeon", true, 0),
                    pod("egress-b-1", "egress", true, 0),
                    pod("egress-b-2", "egress", false, 4),
                ],
            )))
        })
    }
}

/// egress's in-cluster ops route: the summary it is given, and the queries and bearers it saw.
struct Egress {
    url: String,
    summary: Arc<Mutex<Value>>,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

async fn egress() -> Egress {
    let mut fixture: Value = serde_json::from_str(include_str!("../../gateway-api/fixtures/ops-summary.json")).unwrap();
    // fields gateway-api's types do not name reach the snapshot too (18.3: "as egress returns it")
    fixture["egress"]["recent"][0]["laterField"] = json!("kept");
    fixture["budget"]["status"]["remainingUsd"] = json!(288.0);
    let summary: Arc<Mutex<Value>> = Arc::new(Mutex::new(fixture));
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let (s2, seen2) = (summary.clone(), seen.clone());
    let router = axum::Router::new().route(
        gateway_api::ops::OPS_SUMMARY,
        axum::routing::get(move |req: axum::extract::Request| {
            let (s, seen) = (s2.clone(), seen2.clone());
            async move {
                let q = req.uri().query().unwrap_or("").to_owned();
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                seen.lock().unwrap().push((q, auth));
                let v = s.lock().unwrap().clone();
                axum::Json(v)
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(l, router).await;
    });
    Egress { url, summary, seen }
}

struct World {
    h: Harness,
    magpie: aviary_fakes::magpie::Running,
    egress: Egress,
    sb1: Uuid,
    conv1: Uuid,
    imported_at: String,
}

async fn world(test: &str) -> World {
    let h = Harness::new(test).await;
    let magpie = FakeMagpie::new().serve().await.unwrap();
    let egress = egress().await;
    *h.upstreams.lock().unwrap() = Upstreams::new(
        &magpie.url(),
        magpie.fake.token(Caller::Hermit).unwrap(),
        &egress.url,
        "egress-pair-token".into(),
        Arc::new(FakeCluster),
    );
    *h.fabric.ops.lock().unwrap() = Ok(FabricOps {
        plan: Some(ScalerPlan {
            need: 1,
            demand: 1,
            waiting: 0,
            planned: 0,
            available: 23,
            pending: 0,
            per_host: 12,
        }),
        last_pass_at: Some(hermit_api::Timestamp::now()),
        egress_targets: Some(2),
        slots: [("aws-1".to_owned(), 12), ("aws-2".to_owned(), 12)]
            .into_iter()
            .collect(),
    });
    let conv1 = Uuid::new_v4();
    let view_conv = Uuid::new_v4();
    magpie.fake.script().always(
        "ops_people",
        Reply::ok(json!({"tz": "Asia/Hong_Kong", "people": [
            {"owner": ADMIN, "total": 1, "today": 1, "last7d": 1, "live": 0, "deleted": 0, "purged": 0,
             "lastActiveAt": "2026-11-02T03:00:00.000Z"},
            {"owner": STAFF, "total": 2, "today": 0, "last7d": 2, "live": 1, "deleted": 1, "purged": 0,
             "lastActiveAt": "2026-11-02T02:00:00.000Z"},
            {"owner": FORMER, "total": 1, "today": 0, "last7d": 0, "live": 0, "deleted": 0, "purged": 0,
             "lastActiveAt": "2026-10-01T02:00:00.000Z"}
        ]})),
    );
    magpie.fake.script().always(
        "ops_view_as",
        Reply::ok(json!({"viewAs": [
            {"id": 3, "at": "2026-11-02T03:00:02.000Z", "conversationId": view_conv, "event": "send", "admin": ADMIN,
             "user": STAFF, "session": "v1", "chars": 5},
            {"id": 2, "at": "2026-11-02T03:00:01.000Z", "conversationId": view_conv, "event": "create", "admin": ADMIN,
             "user": STAFF, "session": "v1", "chars": null},
            {"id": 1, "at": "2026-11-02T03:00:00.000Z", "conversationId": null, "event": "start", "admin": ADMIN,
             "user": STAFF, "session": "v1", "chars": null}
        ]})),
    );
    hermit::db::migrate(&h.pool()).await.unwrap();
    let (sb1, sb2, sb3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (conv2, conv3) = (Uuid::new_v4(), Uuid::new_v4());
    h.sql(
        "insert into hosts (name, kind, url, wg_ip, status, cpus, mem_total, instance_id, last_seen_at, info, retire_by) values
           ('hermit-gw', 'gateway', 'https://198.19.0.1:7443', '198.19.0.1', 'active', null, null, null, now(),
            '{\"gvisorRelease\": \"g-1\"}', null),
           ('aws-1', 'aws', 'https://198.19.0.2:7443', '198.19.0.2', 'active', 4, 16642998272, 'i-1', now(),
            '{\"memAvailable\": 8000000000, \"loadAvg\": [0.5, 0.4, 0.3]}', null),
           ('aws-2', 'aws', 'https://198.19.0.3:7443', '198.19.0.3', 'retiring', 4, 16642998272, 'i-2',
            now() - interval '5 minutes', null, 'operator')",
    )
    .await;
    h.sql(
        "insert into hosts (name, kind, url, wg_ip, status, gone_at, gone_reason, created_at) values
           ('aws-0', 'aws', 'u', '198.19.0.9', 'gone', now() - interval '1 hour', 'empty', now() - interval '3 hours')",
    )
    .await;
    let sbx = |id: Uuid, conv: Uuid, person: &str, status: &str, host: &str, reason: &str, ago_min: i64| {
        format!(
            "insert into sandboxes (id, conversation_id, person, status, name, cpu, memory_bytes, start_deadline,
               expires_at, hard_expires_at, host, release_reason, released_at, created_at, last_heartbeat_at, image)
             values ('{id}', '{conv}', '{person}', '{status}', 'sbx-{}', 1, 1073741824, now(), now() + interval '1 hour',
               now() + interval '9 hours', '{host}', nullif('{reason}', ''),
               case when '{status}' = 'gone' then now() - interval '{ago_min} minutes' end,
               now() - interval '1 hour', now(), 'sha256:{}')",
            &id.simple().to_string()[..16],
            "1".repeat(64),
        )
    };
    h.sql(&sbx(sb1, conv1, ADMIN, "ready", "aws-1", "", 0)).await;
    h.sql(&sbx(sb2, conv2, STAFF, "gone", "aws-1", "deleted", 10)).await;
    h.sql(&sbx(sb3, conv3, STAFF, "gone", "aws-2", "idle", 20)).await;
    h.sql(&format!(
        "insert into host_events (kind, host, sandbox_id, data) values
           ('launch', 'aws-1', null, '{{\"reason\": \"scaler\"}}'),
           ('migration', 'aws-1', '{sb1}', '{{\"pauseMs\": 120}}'),
           ('host_lost', 'aws-0', null, '{{\"error\": \"no heartbeat\"}}')"
    ))
    .await;
    h.sql(&format!(
        "insert into sandbox_starts (ok, ms, sandbox_id, conversation_id, person, host, reason) values
           (true, 4200, '{sb1}', '{conv1}', '{ADMIN}', 'aws-1', null),
           (false, 900, null, '{conv3}', '{STAFF}', null, 'pool_full')"
    ))
    .await;
    // an imported start of the old fabric keeps its original time (22.3)
    h.sql(&format!(
        "insert into sandbox_starts (at, ok, ms, conversation_id, person, host, source_id)
         values ('2026-11-02T01:02:03.456Z'::timestamptz + (now() - '2026-11-02T01:02:03.456Z'::timestamptz)
                 - interval '2 hours', true, 3000, '{conv2}', '{STAFF}', 'old-1', 77)"
    ))
    .await;
    let imported_at: String = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select to_char(at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') from sandbox_starts where source_id = 77",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    h.sql(&format!(
        "insert into demand_hints (person, at) values ('{STAFF}', now())"
    ))
    .await;
    World {
        h,
        magpie,
        egress,
        sb1,
        conv1,
        imported_at,
    }
}

async fn snapshot(h: &Harness, url: &str, tz: &str) -> Value {
    let a = h
        .client(Caller::Admin)
        .call(
            url,
            "GET",
            &format!("/ops/snapshot?tz={tz}"),
            None,
            &[("aviary-admin", ADMIN.into())],
        )
        .await;
    assert_eq!(a.status, 200, "{:?}", a.body);
    a.body
}

#[tokio::test]
async fn the_snapshot_now_and_history_from_every_source() {
    let w = world("ops_snapshot").await;
    let h = &w.h;
    let r = h.replica("hermit-a").await;
    hermit::app::report_replica(r.core()).await.unwrap();
    let d = snapshot(h, &r.url, "UTC").await;
    assert_eq!(d["errors"], json!({}), "{:#}", d["errors"]);
    assert_eq!(d["tz"], "UTC");
    assert_eq!(d["fabric"], "gvisor-aws");
    assert_eq!(d["product"], "Example AI");
    // egress was asked for the zone and the window, with hermit's pair token
    let seen = w.egress.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(q, a)| q == "tz=UTC&recentMinutes=15" && a == "Bearer egress-pair-token"),
        "{seen:?}"
    );

    // live sandboxes
    let sl = d["sandboxes"]["list"].as_array().unwrap();
    assert_eq!(sl.len(), 1);
    assert_eq!(sl[0]["owner"], ADMIN);
    assert_eq!(sl[0]["host"], "aws-1");
    assert_eq!(sl[0]["migrations"], 1);
    assert_eq!(sl[0]["reporting"], true);
    assert_eq!(sl[0]["id"], json!(w.sb1));
    // hosts: the gateway without slots, AWS with slots and sandbox counts, the retiring one flagged, gone ones only
    // in history
    let host = |n: &str| {
        d["hosts"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == n)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(host("hermit-gw")["gateway"], true);
    assert_eq!(host("hermit-gw")["kind"], "gateway");
    assert_eq!(host("hermit-gw")["slots"], Value::Null);
    assert_eq!(host("aws-1")["sandboxes"], 1);
    assert_eq!(host("aws-1")["slots"], 12);
    assert_eq!(host("aws-1")["reachable"], true);
    assert_eq!(host("aws-1")["memAvailable"], 8_000_000_000_i64);
    assert_eq!(host("aws-1")["load1"], 0.5);
    assert_eq!(host("aws-2")["retiring"], true);
    assert_eq!(host("aws-2")["retireBy"], "operator");
    assert_eq!(host("aws-2")["reachable"], false);
    assert_eq!(host("aws-0"), Value::Null);
    assert_eq!(d["hosts"]["maxHosts"], 3);
    assert_eq!(d["gatewayMissing"], false);
    assert_eq!(d["hostsGone"]["count24h"], 1);
    assert_eq!(d["hostsGone"]["list"][0]["name"], "aws-0");
    assert_eq!(d["hostsGone"]["list"][0]["goneReason"], "empty");
    assert_eq!(d["hostsGone"]["list"][0]["livedS"], 7200);
    // scaler: the fabric's plan and the hints counted
    assert_eq!(d["scaler"]["plan"]["demand"], 1);
    assert_eq!(d["scaler"]["plan"]["perHost"], 12);
    assert_eq!(d["scaler"]["hints"][0]["who"], STAFF);
    assert_eq!(d["scaler"]["hints"][0]["counted"], true);
    // events, newest first, with the sandbox's person
    let ev = d["events"].as_array().unwrap();
    assert_eq!(ev[0]["kind"], "host_lost");
    let mig = ev.iter().find(|e| e["kind"] == "migration").unwrap();
    assert_eq!(mig["owner"], ADMIN);
    assert_eq!(mig["conversationId"], json!(w.conv1));
    // service: per Deployment in ops.deployments order, with pods; node names lose the prefix
    let role = |n: &str| {
        d["service"]["deployments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(d["service"]["available"], true);
    let names: Vec<&str> = d["service"]["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["pigeon", "magpie", "hermit", "egress", "tools"]);
    assert_eq!(role("pigeon")["state"], "ok");
    assert_eq!(role("pigeon")["pods"].as_array().unwrap().len(), 2);
    assert_eq!(role("pigeon")["pods"][0]["node"], "worker1");
    assert_eq!(role("magpie")["state"], "warming");
    assert_eq!(role("egress")["state"], "problem");
    let bad = role("egress")["pods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["ready"] == false)
        .cloned()
        .unwrap();
    assert_eq!(
        (bad["phase"].clone(), bad["restarts"].clone()),
        (json!("CrashLoopBackOff"), json!(4))
    );
    assert_eq!(role("tools")["missing"], true);
    // starts and releases, newest first, with timings; owners are emails
    assert_eq!(d["starts"]["summary"]["n"], 3);
    assert_eq!(d["starts"]["summary"]["failed"], 1);
    let sl = d["starts"]["list"].as_array().unwrap();
    let ok = sl
        .iter()
        .find(|x| x["what"] == "start" && x["ok"] == true && x["ms"] == 4200)
        .unwrap();
    assert_eq!(ok["owner"], ADMIN);
    let failed = sl.iter().find(|x| x["what"] == "start" && x["ok"] == false).unwrap();
    assert_eq!(
        (failed["reason"].clone(), failed["owner"].clone()),
        (json!("pool_full"), json!(STAFF))
    );
    assert_eq!(sl.iter().filter(|x| x["what"] == "release").count(), 2);
    let release = sl
        .iter()
        .find(|x| x["what"] == "release" && x["reason"] == "deleted")
        .unwrap();
    assert_eq!(release["ms"], 3_000_000, "lived 50 minutes");
    for p in sl.windows(2) {
        assert!(p[0]["at"].as_str() >= p[1]["at"].as_str(), "newest first");
    }
    assert!(
        sl.iter().any(|x| x["at"] == w.imported_at.as_str()),
        "the imported start keeps its time {}",
        w.imported_at
    );
    let releases = d["starts"]["releases"].as_array().unwrap();
    assert_eq!(releases.len(), 2);
    // errors of the last 15 minutes: the failed start and the host loss; egress's counts passed in
    assert_eq!(d["recent"]["failedStarts"], 1);
    assert_eq!(d["recent"]["hostErrors"], 1);
    assert_eq!(d["recent"]["modelErrors"], 0);
    assert_eq!(d["recent"]["blocked"], 1);
    let whats: Vec<&str> = d["recent"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["what"].as_str().unwrap())
        .collect();
    assert!(whats.contains(&"sandbox start failed"), "{whats:?}");
    assert!(whats.contains(&"host_lost"), "{whats:?}");
    // budget and egress: egress's numbers, unchanged
    let fixture = w.egress.summary.lock().unwrap().clone();
    assert_eq!(d["budget"], fixture["budget"]);
    assert_eq!(d["egress"], fixture["egress"]);
    assert_eq!(d["egress"]["recent"][0]["laterField"], "kept");
    assert_eq!(d["budget"]["status"]["remainingUsd"], 288.0);
    let quiet: Vec<bool> = d["egress"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["quiet"].as_bool().unwrap())
        .collect();
    assert_eq!(quiet, [false, true], "the monitor skips the quiet rows");
    // people: every explicit catalogue user, then the rest; live is hermit's sandbox count; former users flagged
    let p = |e: &str| {
        d["people"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["owner"] == e)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (p(ADMIN)["total"].clone(), p(ADMIN)["live"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(
        (p(STAFF)["total"].clone(), p(STAFF)["deleted"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(p(STAFF)["live"], 0, "magpie's own live is not used");
    assert_eq!(p(FORMER)["former"], true);
    assert_eq!(p(ADMIN).get("former"), None);
    // the view-as log
    let va: Vec<(String, Value)> = d["viewAs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["event"].as_str().unwrap().to_owned(), x["chars"].clone()))
        .collect();
    assert_eq!(
        va,
        vec![
            ("send".into(), json!(5)),
            ("create".into(), Value::Null),
            ("start".into(), Value::Null)
        ]
    );
    // the operator's switches, the catalogue, this replica's workspace store
    assert_eq!(d["controls"]["sandboxesEnabled"], true);
    assert_eq!(d["catalogue"]["generation"].as_str().unwrap().len(), 64);
    assert_eq!(d["replicas"][0]["pod"], "hermit-a");
    assert_eq!(d["replicas"][0]["storeError"], Value::Null);
    // the verdict: problems first
    assert_eq!(d["health"]["state"], "problem");
    let texts: Vec<String> = d["health"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| format!("{} {}", x["level"].as_str().unwrap(), x["text"].as_str().unwrap()))
        .collect();
    let has = |t: &str| texts.iter().any(|x| x.starts_with(t));
    assert!(has("problem egress: 1 of 2 pods ready"), "{texts:#?}");
    assert!(has("problem tools: no Deployment"), "{texts:#?}");
    assert!(has("warming magpie: rolling out"), "{texts:#?}");
    assert!(has("warming Host aws-2 retiring"), "{texts:#?}");
    assert!(has("problem 1 sandbox start failed in the last 15 min"), "{texts:#?}");
    assert!(has("problem 1 host or move error in the last 15 min"), "{texts:#?}");
    assert_eq!(d["health"]["findings"][0]["level"], "problem");

    // the monitor's two contracts
    check_contracts(&d);

    // a failed source is a null section with its message; the rest still answers (4 s cache: another zone)
    *w.egress.summary.lock().unwrap() = json!({"not": "a summary"});
    let d = snapshot(h, &r.url, "Europe/London").await;
    assert_eq!(d["tz"], "Europe/London");
    assert_eq!(d["budget"], Value::Null);
    assert!(d["errors"]["egress"].as_str().unwrap().starts_with("egress: bad body"));
    assert!(d["errors"]["budget"].is_string());
    assert_eq!(d["health"]["findings"][0]["about"], "page");
    assert!(d["sandboxes"]["list"].is_array());
    // the fabric's part is a section like the others: its failure is named, never a quiet default
    *h.fabric.ops.lock().unwrap() = Err("the pool timed out".into());
    let d = snapshot(h, &r.url, "Asia/Tokyo").await;
    assert_eq!(d["errors"]["fabricOps"], "the pool timed out");
    assert_eq!(
        (d["fabricOps"].clone(), d["scaler"]["plan"].clone()),
        (Value::Null, Value::Null)
    );
    assert!(d["scaler"]["hints"].is_array());
    drop(w.magpie);
}

/// Walks `path` (dot-separated, `[]` = every element) and checks each value's type; at least one value must be
/// found, so the rows really cover the path.
fn check_path(d: &Value, path: &str, ty: &str, contract: &str) {
    fn walk<'a>(v: &'a Value, parts: &[&str], out: &mut Vec<(String, &'a Value)>, at: String, contract: &str) {
        let Some((first, rest)) = parts.split_first() else {
            out.push((at, v));
            return;
        };
        let (key, each) = first.strip_suffix("[]").map_or((*first, false), |k| (k, true));
        let next = &v[key];
        let here = format!("{at}.{key}");
        if each {
            let arr = next
                .as_array()
                .unwrap_or_else(|| panic!("{contract}: {here} is not an array: {next}"));
            for (i, e) in arr.iter().enumerate() {
                walk(e, rest, out, format!("{here}[{i}]"), contract);
            }
        } else {
            walk(next, rest, out, here, contract);
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    let mut found = Vec::new();
    walk(d, &parts, &mut found, String::new(), contract);
    assert!(
        !found.is_empty(),
        "{contract}: no value at {path}: the fixture rows must cover it"
    );
    let (base, nullable) = ty.strip_suffix('?').map_or((ty, false), |b| (b, true));
    for (at, v) in found {
        let ok = match base {
            "string" => v.is_string(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "object" => v.is_object(),
            "array" => v.is_array(),
            other => panic!("unknown type {other}"),
        } || (nullable && v.is_null());
        assert!(ok, "{contract}: {at} should be {ty}, is {v}");
    }
}

/// Every value under a key named `at`, anywhere.
fn every_at<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "at" {
                    out.push(x);
                }
                every_at(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| every_at(x, out)),
        _ => {}
    }
}

fn check_contracts(d: &Value) {
    let golden: Value = serde_json::from_str(include_str!("fixtures/monitor-contract.json")).unwrap();
    for contract in ["ai_slim", "ai_activity"] {
        for (path, ty) in golden[contract].as_object().unwrap() {
            check_path(d, path, ty.as_str().unwrap(), contract);
        }
    }
    // ai_activity's value rules: `what` is start or release, `owner` an email, every `at` in the one format
    for x in d["starts"]["list"].as_array().unwrap() {
        assert!(x["what"] == "start" || x["what"] == "release", "{x}");
        assert!(x["owner"].as_str().unwrap().contains('@'), "owner is an email: {x}");
    }
    let re = |s: &str| {
        let b = s.as_bytes();
        s.len() == 24
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'.',
                23 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    };
    let mut ats = Vec::new();
    every_at(d, &mut ats);
    assert!(ats.len() > 10);
    for a in ats {
        assert!(a.as_str().is_some_and(re), "`at` {a} is not YYYY-MM-DDTHH:MM:SS.mmmZ");
    }
    // `kind` of the gateway is "gateway" (was "hub"); ai_slim only reads `gateway: true`
    assert!(
        d["hosts"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["gateway"] == true && h["kind"] == "gateway")
    );
}

#[tokio::test]
async fn only_admins_get_the_page_the_monitor_only_the_snapshot() {
    let h = Harness::new("ops_access").await;
    let r = h.replica("hermit-a").await;
    let pigeon = h.client(Caller::Pigeon);
    // pigeon: the person must be an admin in the current catalogue (case and spaces normalised)
    let a = pigeon
        .call(&r.url, "GET", "/ops/", None, &[("aviary-person", STAFF.into())])
        .await;
    assert_eq!((a.status, a.body["code"].clone()), (403, json!("not_admin")));
    let a = pigeon.call(&r.url, "GET", "/ops/snapshot", None, &[]).await;
    assert_eq!(a.status, 403, "no person");
    let a = pigeon
        .call(
            &r.url,
            "GET",
            "/ops/snapshot",
            None,
            &[("aviary-person", "mallory@example.net".into())],
        )
        .await;
    assert_eq!(a.status, 403);
    let res = pigeon
        .http
        .get(format!("{}/ops/", r.url))
        .bearer_auth(&pigeon.token)
        .header("aviary-protocol", "hermit-core=1")
        .header("aviary-person", format!("  {}  ", ADMIN.to_uppercase()))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let hd = res.headers().clone();
    assert!(hd["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(
        hd["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert!(
        hd["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert_eq!(hd["referrer-policy"], "no-referrer");
    assert_eq!(hd["cache-control"], "no-store");
    assert_eq!(hd["x-content-type-options"], "nosniff");
    let html = res.text().await.unwrap();
    assert!(html.contains("<title>Example AI Ops</title>"));
    assert!(html.contains("<h1>Example AI Ops</h1>"));
    assert!(html.contains("\"Asia/Hong_Kong\""), "the default zone");
    assert!(html.contains("id=\"now\"") && html.contains("id=\"history\""));
    assert!(html.contains("History (log)"));
    assert!(html.contains("action=\"/api/jarvis/view-as\" target=\"_blank\""));
    assert!(html.contains(">View as<"));
    assert!(!html.contains("{{"), "every placeholder filled");
    // HEAD is GET without the body
    let res = pigeon
        .http
        .head(format!("{}/ops/", r.url))
        .bearer_auth(&pigeon.token)
        .header("aviary-protocol", "hermit-core=1")
        .header("aviary-person", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    // the monitor: the snapshot only
    let monitor = h.client(Caller::Monitor);
    let a = monitor
        .call(&r.url, "GET", "/ops/snapshot?tz=Asia/Hong_Kong", None, &[])
        .await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["tz"], "Asia/Hong_Kong");
    let a = monitor.call(&r.url, "GET", "/ops/", None, &[]).await;
    assert_eq!((a.status, a.body["code"].clone()), (403, json!("wrong_caller")));
    // the admin token reads both
    let admin = h.client(Caller::Admin);
    assert_eq!(admin.call(&r.url, "GET", "/ops/", None, &[]).await.status, 200);
    // nothing else under /ops/, nothing but GET and HEAD, no other caller
    assert_eq!(admin.call(&r.url, "GET", "/ops/other", None, &[]).await.status, 404);
    assert_eq!(admin.call(&r.url, "POST", "/ops/snapshot", None, &[]).await.status, 405);
    let magpie = h.client(Caller::Magpie);
    assert_eq!(magpie.call(&r.url, "GET", "/ops/snapshot", None, &[]).await.status, 403);
    // a zone that is not one: the deployment's own
    let a = monitor
        .call(&r.url, "GET", "/ops/snapshot?tz=x%27%3B%20drop", None, &[])
        .await;
    assert_eq!(a.body["tz"], "Asia/Hong_Kong");
    // cached 4 s per zone: the same `at`
    let b = monitor
        .call(&r.url, "GET", "/ops/snapshot?tz=Asia/Hong_Kong", None, &[])
        .await;
    assert_eq!(a.body["at"], b.body["at"]);
    // magpie and egress unreachable here: their sections are null with a message, the rest answers
    assert_eq!(a.body["people"], Value::Null);
    assert!(a.body["errors"]["people"].is_string());
    assert!(a.body["errors"]["viewAs"].is_string());
    assert_eq!(a.body["service"]["available"], false, "not in a cluster");
    assert!(a.body["sandboxes"]["list"].is_array());
}

async fn events(h: &Harness) -> Vec<(String, String, Option<String>, Value)> {
    h.pool()
        .get()
        .await
        .unwrap()
        .query("select actor, action, target, data from admin_events order by id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect()
}

#[tokio::test]
async fn every_admin_call_is_audited_and_does_what_it_says() {
    let w = world("ops_admin").await;
    let h = &w.h;
    let r = h.replica("hermit-a").await;
    let admin = h.client(Caller::Admin);
    let as_admin = [("aviary-admin", format!(" {} ", ADMIN.to_uppercase()))];
    // Aviary-Admin is required; other callers are refused
    let a = admin.call(&r.url, "GET", "/v1/admin/hosts", None, &[]).await;
    assert_eq!((a.status, a.body["code"].clone()), (400, json!("bad_request")));
    let a = h
        .client(Caller::Magpie)
        .call(&r.url, "GET", "/v1/admin/hosts", None, &as_admin)
        .await;
    assert_eq!(a.status, 403);
    assert!(events(h).await.is_empty(), "a refused call writes nothing");

    // hosts: list, cap, retire, reactivate
    let a = admin.call(&r.url, "GET", "/v1/admin/hosts", None, &as_admin).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["hosts"].as_array().unwrap().len(), 3);
    let a = admin
        .call(
            &r.url,
            "PATCH",
            "/v1/admin/hosts/aws-1",
            Some(json!({"maxSandboxes": 0, "note": "maintenance"})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (a.status, a.body["maxSandboxes"].clone()),
        (200, json!(0)),
        "{:?}",
        a.body
    );
    let a = admin
        .call(
            &r.url,
            "PATCH",
            "/v1/admin/hosts/aws-1",
            Some(json!({"maxSandboxes": null, "retire": true})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (
            a.body["maxSandboxes"].clone(),
            a.body["status"].clone(),
            a.body["retireBy"].clone()
        ),
        (Value::Null, json!("retiring"), json!("operator"))
    );
    let a = admin
        .call(
            &r.url,
            "PATCH",
            "/v1/admin/hosts/aws-1",
            Some(json!({"reactivate": true})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (a.body["status"].clone(), a.body["retireBy"].clone()),
        (json!("active"), Value::Null)
    );
    let a = admin
        .call(
            &r.url,
            "PATCH",
            "/v1/admin/hosts/aws-1",
            Some(json!({"retire": true, "reactivate": true})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 400);
    let a = admin
        .call(
            &r.url,
            "PATCH",
            "/v1/admin/hosts/aws-0",
            Some(json!({"retire": true})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 404, "a gone host");

    // the off switch: PUT …/sandbox answers sandbox_busy disabled; the snapshot shows it
    let a = admin
        .call(
            &r.url,
            "PUT",
            "/v1/admin/sandboxes-enabled",
            Some(json!({"enabled": false, "reason": "gateway work"})),
            &as_admin,
        )
        .await;
    assert_eq!((a.status, a.body["sandboxesEnabled"].clone()), (200, json!(false)));
    assert_eq!(a.body["updatedBy"], ADMIN);
    let magpie = h.client(Caller::Magpie);
    let a = magpie
        .call(
            &r.url,
            "PUT",
            &format!("/v1/conversations/{}/sandbox", Uuid::new_v4()),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!(
        (a.status, a.body["code"].clone(), a.body["reason"].clone()),
        (503, json!("sandbox_busy"), json!("disabled"))
    );
    let a = admin
        .call(
            &r.url,
            "PUT",
            "/v1/admin/sandboxes-enabled",
            Some(json!({"enabled": true})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (a.body["sandboxesEnabled"].clone(), a.body["disabledReason"].clone()),
        (json!(true), Value::Null)
    );

    // fresh-host: 1..60 minutes
    let a = admin
        .call(
            &r.url,
            "POST",
            "/v1/admin/fresh-host",
            Some(json!({"ttlMinutes": 0})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 400);
    let a = admin
        .call(
            &r.url,
            "POST",
            "/v1/admin/fresh-host",
            Some(json!({"ttlMinutes": 10})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 200);
    assert!(a.body["freshHostUntil"].as_str().unwrap() > hermit_api::Timestamp::now().to_string().as_str());
    // the off switch during a fresh-host test leaves its baseline (placement's "became active after") alone
    let set_at = || async {
        h.pool()
            .get()
            .await
            .unwrap()
            .query_one("select fresh_host_set_at from controls where id = 1", &[])
            .await
            .unwrap()
            .get::<_, Option<time::OffsetDateTime>>(0)
            .unwrap()
    };
    let before = set_at().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let a = admin
        .call(
            &r.url,
            "PUT",
            "/v1/admin/sandboxes-enabled",
            Some(json!({"enabled": true})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 200);
    assert_eq!(set_at().await, before);

    // the trace tool
    let a = admin
        .call(
            &r.url,
            "GET",
            &format!("/v1/admin/sandboxes?conversation={}", w.conv1),
            None,
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 200);
    let s = &a.body["sandboxes"][0];
    assert_eq!(s["id"], json!(w.sb1));
    assert_eq!(s["starts"][0]["ms"], 4200);
    assert_eq!(s["events"][0]["kind"], "migration");
    let a = admin
        .call(
            &r.url,
            "GET",
            &format!("/v1/admin/sandboxes?person={STAFF}"),
            None,
            &as_admin,
        )
        .await;
    assert_eq!(a.body["sandboxes"].as_array().unwrap().len(), 2);

    // a real sandbox: exec through its supervisor (audit: argv and exit code, never stdin or output), then the
    // operator's release and its console tail
    let cid = Uuid::new_v4();
    let a = magpie
        .call(
            &r.url,
            "PUT",
            &format!("/v1/conversations/{cid}/sandbox"),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!(a.status, 202, "{:?}", a.body);
    assert!(
        eventually(Duration::from_secs(20), || async {
            magpie
                .call(&r.url, "GET", &format!("/v1/conversations/{cid}/sandbox"), None, &[])
                .await
                .body["state"]
                == "ready"
        })
        .await
    );
    let row = hermit::db::latest_of(&r.core().pool, cid).await.unwrap().unwrap();
    let sup = h.fabric.supervisor(row.ip.unwrap()).unwrap();
    sup.script().always(
        "exec",
        Reply::ok(json!({"exitCode": 0, "signal": null, "stdout": "aGk=", "stderr": "", "truncated": false})),
    );
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/sandboxes/{}/exec", row.id),
            Some(json!({"argv": ["id", "-u"], "stdin": "c2VjcmV0", "env": {"K": "v"}})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (a.status, a.body["stdout"].clone()),
        (200, json!("aGk=")),
        "{:?}",
        a.body
    );
    assert_eq!(
        sup.script().calls_to("exec")[0].body.as_ref().unwrap()["argv"],
        json!(["id", "-u"])
    );
    let a = admin
        .call(
            &r.url,
            "GET",
            &format!("/v1/admin/sandboxes/{}/log", row.id),
            None,
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 404, "no tail before the release");
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/sandboxes/{}/release", row.id),
            Some(json!({"note": "stuck"})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 200);
    assert!(
        eventually(Duration::from_secs(20), || async {
            hermit::db::load(&r.core().pool, row.id)
                .await
                .unwrap()
                .is_some_and(|r| !r.is_live())
        })
        .await
    );
    let gone = hermit::db::load(&r.core().pool, row.id).await.unwrap().unwrap();
    assert_eq!(
        (gone.release_reason.as_deref(), gone.release_note.as_deref()),
        (Some("operator"), Some("stuck"))
    );
    let rel = h.fabric.release_bodies(row.id);
    assert_eq!(rel.last().unwrap()["force"], true);
    let a = admin
        .call(
            &r.url,
            "GET",
            &format!("/v1/admin/sandboxes/{}/log", row.id),
            None,
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["text"], format!("console of {}\n", row.name));
    assert_eq!(a.body["reason"], "release");
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/sandboxes/{}/release", row.id),
            Some(json!({})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 404, "no live sandbox");

    // recycle: every live sandbox on another image (sb1, inserted with an old image). A start's image is fixed when
    // its row is inserted (the inserting replica's configured one): a start this configuration inserted is left
    // alone, and one an old replica inserted during a rollout is recycled, whichever replica drives it
    let (starting, old_start) = (Uuid::new_v4(), Uuid::new_v4());
    let old_image = format!("sha256:{}", "1".repeat(64));
    for (id, name, image) in [
        (starting, "sbx-starting", h.config.image_digest.clone()),
        (old_start, "sbx-old-start", old_image.clone()),
    ] {
        h.sql(&format!(
            "insert into sandboxes (id, conversation_id, person, status, name, cpu, memory_bytes, start_deadline,
               expires_at, hard_expires_at, created_at, image)
             values ('{id}', '{}', '{STAFF}', 'starting', '{name}', 1, 1073741824, now() + interval '1 minute',
               now() + interval '1 hour', now() + interval '9 hours', now(), '{image}')",
            Uuid::new_v4()
        ))
        .await;
    }
    // sb1 sits on the current image for this call, so its own recycle below is the only one it gets (a release this
    // call started could otherwise finish before that call and leave it nothing to count)
    h.sql(&format!(
        "update sandboxes set image = '{}' where id = '{}'",
        h.config.image_digest, w.sb1
    ))
    .await;
    let a = admin
        .call(
            &r.url,
            "POST",
            "/v1/admin/recycle",
            Some(json!({"exceptImage": h.config.image_digest, "onlyIdle": true})),
            &as_admin,
        )
        .await;
    assert_eq!((a.status, a.body["released"].clone()), (200, json!(1)), "{:?}", a.body);
    let pending = |id: Uuid| {
        let h = &h;
        async move {
            let row = h
                .pool()
                .get()
                .await
                .unwrap()
                .query_one(
                    "select release_pending, release_reason from sandboxes where id = $1",
                    &[&id],
                )
                .await
                .unwrap();
            (row.get::<_, Option<Value>>(0), row.get::<_, Option<String>>(1))
        }
    };
    assert_eq!(
        pending(starting).await,
        (None, None),
        "a start on the current image is not recycled"
    );
    let (p, reason) = pending(old_start).await;
    assert!(
        p.is_some_and(|p| p["reason"] == "recycle") || reason.as_deref() == Some("recycle"),
        "a start an old replica inserted is recycled: {reason:?}"
    );
    h.sql(&format!(
        "delete from sandboxes where id in ('{starting}', '{old_start}')"
    ))
    .await;
    h.sql(&format!(
        "update sandboxes set image = '{old_image}' where id = '{}'",
        w.sb1
    ))
    .await;
    let a = admin
        .call(
            &r.url,
            "POST",
            "/v1/admin/recycle",
            Some(json!({"exceptImage": "nope", "onlyIdle": true})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 400);
    let image = format!("sha256:{}", "0".repeat(64));
    let a = admin
        .call(
            &r.url,
            "POST",
            "/v1/admin/recycle",
            Some(json!({"exceptImage": image, "onlyIdle": true})),
            &as_admin,
        )
        .await;
    assert_eq!((a.status, a.body["released"].clone()), (200, json!(1)), "{:?}", a.body);
    // the recycle starts each release at once (run_requested), so sb1 is either still pending or already released
    let row = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select release_pending, release_reason from sandboxes where id = $1",
            &[&w.sb1],
        )
        .await
        .unwrap();
    let (pending, reason): (Option<Value>, Option<String>) = (row.get(0), row.get(1));
    if let Some(p) = pending {
        assert_eq!(
            (p["reason"].clone(), p["force"].clone()),
            (json!("recycle"), json!(false))
        );
    } else {
        assert_eq!(reason.as_deref(), Some("recycle"), "released by the recycle");
        assert!(
            h.fabric.release_bodies(w.sb1).iter().all(|b| b["force"] != true),
            "a soft recycle: {:?}",
            h.fabric.release_bodies(w.sb1)
        );
    }

    // purge: refused while allowed; for a former person magpie purges and its count is the answer
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/people/{STAFF}/purge"),
            Some(json!({"confirm": STAFF})),
            &as_admin,
        )
        .await;
    assert_eq!((a.status, a.body["code"].clone()), (409, json!("still_allowed")));
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/people/{FORMER}/purge"),
            Some(json!({"confirm": "other@example.com"})),
            &as_admin,
        )
        .await;
    assert_eq!(a.status, 400);
    w.magpie
        .fake
        .script()
        .always("person_purge", Reply::json(202, json!({"conversations": 3})));
    let a = admin
        .call(
            &r.url,
            "POST",
            &format!("/v1/admin/people/{FORMER}/purge"),
            Some(json!({"confirm": FORMER.to_uppercase()})),
            &as_admin,
        )
        .await;
    assert_eq!(
        (a.status, a.body["conversations"].clone()),
        (202, json!(3)),
        "{:?}",
        a.body
    );
    let calls = w.magpie.fake.script().calls_to("person_purge");
    assert_eq!(calls[0].body.as_ref().unwrap()["reason"], "offboarded");

    // the audit log: every call above, by the operator, with what it changed and nothing of an exec's input/output
    let a = admin
        .call(&r.url, "GET", "/v1/admin/events?limit=500", None, &as_admin)
        .await;
    assert_eq!(a.status, 200);
    let ev = events(h).await;
    let actions: Vec<&str> = ev.iter().map(|e| e.1.as_str()).collect();
    for want in [
        "hosts.read",
        "host.patch",
        "sandboxes.enabled",
        "fresh_host",
        "sandboxes.read",
        "sandbox.exec",
        "sandbox.log",
        "sandbox.release",
        "recycle",
        "person.purge",
        "events.read",
    ] {
        assert!(actions.contains(&want), "{want} not audited: {actions:?}");
    }
    assert!(ev.iter().all(|e| e.0 == ADMIN), "the operator's email, normalised");
    let exec = ev.iter().find(|e| e.1 == "sandbox.exec").unwrap();
    assert_eq!(exec.3["argv"], json!(["id", "-u"]));
    assert_eq!(exec.3["exitCode"], 0);
    assert_eq!(exec.3["outcome"], "done", "the intent row, completed");
    let purge = ev.iter().find(|e| e.1 == "person.purge").unwrap();
    assert_eq!(
        (purge.3["outcome"].clone(), purge.3["conversations"].clone()),
        (json!("done"), json!(3))
    );
    let raw = exec.3.to_string();
    assert!(
        !raw.contains("c2VjcmV0") && !raw.contains("aGk="),
        "no stdin or output: {raw}"
    );
    // the refused patch (400) and the gone host (404) wrote nothing; the successful ones did
    assert_eq!(ev.iter().filter(|e| e.1 == "host.patch").count(), 3);
    let listed = a.body["events"].as_array().unwrap();
    assert_eq!(listed[0]["action"], "events.read", "newest first");
    assert_eq!(listed.len(), ev.len());
}

#[tokio::test]
async fn the_runbook_script_speaks_the_admin_routes() {
    let h = Harness::new("ops_script").await;
    let r = h.replica("hermit-a").await;
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/hermit-admin.sh");
    let run = |args: &[&str]| {
        let mut c = tokio::process::Command::new("bash");
        c.arg(script)
            .args(args)
            .env("HERMIT_ADMIN_TOKEN", &h.tokens[&Caller::Admin])
            .env("HERMIT_ADMIN", ADMIN)
            .env("HERMIT_URL", &r.url);
        c
    };
    let out = run(&["disable", "gateway", "work"]).output().await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (v["sandboxesEnabled"].clone(), v["disabledReason"].clone()),
        (json!(false), json!("gateway work"))
    );
    // any text is valid JSON (jq builds the bodies): a newline, a tab, quotes, non-ASCII
    let reason = "gateway\twork \"now\"\n網絡 é";
    let out = run(&["disable", reason]).output().await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["disabledReason"], reason);
    let out = run(&["enable"]).output().await.unwrap();
    assert!(out.status.success());
    // a token that could bend curl's config line is refused before anything is sent
    let out = run(&["hosts"])
        .env("HERMIT_ADMIN_TOKEN", "abc\" -H \"x: y")
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run(&["sandboxes", &format!("person={STAFF}")]).output().await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["sandboxes"].is_array());
    // a non-ASCII value is percent-encoded as its UTF-8 bytes (the server reads the address back intact)
    let out = run(&["sandboxes", "person=zoë@example.com"]).output().await.unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let out = run(&["purge", STAFF]).output().await.unwrap();
    assert!(!out.status.success(), "409 still_allowed fails the script");
    assert!(String::from_utf8_lossy(&out.stdout).contains("still_allowed"));
    let out = run(&["events", "10"]).output().await.unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let actions: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        [
            "events.read",
            "sandboxes.read",
            "sandboxes.read",
            "sandboxes.enabled",
            "sandboxes.enabled",
            "sandboxes.enabled"
        ]
    );
    assert_eq!(v["events"][1]["data"]["person"], "zoë@example.com");
    // the token never reaches curl's command line: it is read from stdin
    let text = std::fs::read_to_string(script).unwrap();
    assert!(text.contains("-K -"));
}

/// The gateway node runbook (hermit 14.7, `fabric/host/gateway-maintenance.md`) driven against a replica with a fake
/// node-work command: new sandboxes off, the node work only once no sandbox is busy, sandboxes back on only once the
/// gateway answers with egress targets; a failed node work is reported; a gateway that never returns leaves them off.
#[tokio::test]
async fn the_gateway_maintenance_runbook_holds_the_node_work_and_the_restart() {
    let w = world("ops_gateway_maintenance").await;
    let h = &w.h;
    let r = h.replica("hermit-a").await;
    let dir = std::env::temp_dir().join(format!("hermit-gw-maint-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let (started, release) = (dir.join("started"), dir.join("release"));
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/hermit-admin.sh");
    let run = |work: &str, extra: &[&str]| {
        let mut c = tokio::process::Command::new("bash");
        c.arg(script)
            .args([
                "gateway-maintenance",
                "--node-work",
                work,
                "--deadline",
                "1",
                "--back",
                "1",
                "--settle",
                "2",
            ])
            .args(extra)
            .env("HERMIT_ADMIN_TOKEN", &h.tokens[&Caller::Admin])
            .env("HERMIT_ADMIN", ADMIN)
            .env("HERMIT_URL", &r.url)
            .env("HERMIT_POLL", "1")
            .kill_on_drop(true);
        c
    };
    let controls = || async {
        let row = h
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select sandboxes_enabled, disabled_reason from controls where id = 1",
                &[],
            )
            .await
            .unwrap();
        (row.get::<_, bool>(0), row.get::<_, Option<String>>(1))
    };
    let gateway = |up: bool| async move {
        let ago = if up { 0 } else { 600 };
        h.sql(&format!(
            "update hosts set last_seen_at = now() - interval '{ago} seconds' where name = 'hermit-gw'"
        ))
        .await;
        if let Ok(ops) = h.fabric.ops.lock().unwrap().as_mut() {
            ops.egress_targets = up.then_some(2);
        }
    };
    h.sql(&format!("update sandboxes set busy = true where id = '{}'", w.sb1))
        .await;

    // a fake node work: says it started, then waits to be released (the node reboot)
    let work = format!(
        "touch '{}'; while [ ! -e '{}' ]; do sleep 0.2; done",
        started.display(),
        release.display()
    );
    let mut child = run(&work, &[]).spawn().unwrap();
    assert!(
        eventually(Duration::from_secs(10), || async {
            controls().await == (false, Some("gateway maintenance".into()))
        })
        .await,
        "step 1: new sandboxes off"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!started.exists(), "step 2: no node work while a sandbox is busy");
    h.sql(&format!("update sandboxes set busy = false where id = '{}'", w.sb1))
        .await;
    let s = started.clone();
    assert!(
        eventually(Duration::from_secs(15), move || {
            let s = s.clone();
            async move { s.exists() }
        })
        .await,
        "step 3: the node work runs once no sandbox is busy"
    );
    // the node is down: the gateway stops answering, egress targets are gone; the node work ends
    gateway(false).await;
    std::fs::write(&release, b"").unwrap();
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(child.try_wait().unwrap().is_none(), "step 4: waits for the gateway");
    assert_eq!(controls().await.0, false, "still off while the gateway is away");
    gateway(true).await;
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(controls().await, (true, None), "step 5: new sandboxes on");
    let err = String::from_utf8_lossy(&out.stderr);
    for step in ["1/5", "2/5 no busy sandbox", "3/5 node work done", "4/5", "5/5"] {
        assert!(err.contains(step), "{step} missing in: {err}");
    }

    // a failed node work is reported (its exit code), and sandboxes still come back with the gateway
    let out = run("exit 3", &[]).output().await.unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("node work FAILED (exit 3)"));
    assert_eq!(controls().await, (true, None));

    // a gateway that does not come back: sandboxes stay off, exit 1
    gateway(false).await;
    let out = run("true", &["--back", "0"]).output().await.unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("stay OFF"));
    assert_eq!(controls().await, (false, Some("gateway maintenance".into())));

    // no node work given: refused before anything is sent
    let out = tokio::process::Command::new("bash")
        .args([script, "gateway-maintenance"])
        .env("HERMIT_ADMIN_TOKEN", &h.tokens[&Caller::Admin])
        .env("HERMIT_ADMIN", ADMIN)
        .env("HERMIT_URL", &r.url)
        .output()
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}
