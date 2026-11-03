//! The running sets' transport against a fake Kubernetes API (cutover 7.12 item 5's API half): the ui pods are listed
//! with the token, Terminating ones included; `holds.js` runs in each through the WebSocket exec protocol exactly as
//! the script constant; a pod whose exec fails makes the read incomplete (every conversation may be running).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use magpie_import::{holds, kube};

#[derive(Clone)]
struct Fake {
    pods: Vec<(&'static str, bool)>,
    failing: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
}

fn authorised(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer t0ken")
}

async fn list(State(f): State<Fake>, RawQuery(q): RawQuery, h: HeaderMap) -> Response {
    if !authorised(&h) || q.as_deref() != Some("labelSelector=app%3Dui") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let items: Vec<serde_json::Value> = f
        .pods
        .iter()
        .map(|(n, t)| {
            let mut meta = serde_json::json!({"name": n});
            if *t {
                meta["deletionTimestamp"] = "2026-11-03T00:00:00Z".into();
            }
            serde_json::json!({"metadata": meta})
        })
        .collect();
    axum::Json(serde_json::json!({"items": items})).into_response()
}

fn command(q: &str) -> Vec<String> {
    q.split('&')
        .filter_map(|kv| kv.strip_prefix("command="))
        .map(|v| {
            let b = v.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        })
        .collect()
}

async fn exec(
    State(f): State<Fake>,
    Path(pod): Path<String>,
    RawQuery(q): RawQuery,
    h: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let q = q.unwrap_or_default();
    if !authorised(&h) || !q.contains("container=ui") {
        return StatusCode::FORBIDDEN.into_response();
    }
    assert_eq!(command(&q), ["node", "-e", holds::SCRIPT]);
    f.calls.lock().unwrap().push(pod.clone());
    let fail = pod == f.failing;
    ws.protocols(["v4.channel.k8s.io"])
        .on_upgrade(move |mut s| async move {
            let line =
                format!("{{\"schema\":\"cc_{pod}\",\"port\":3001,\"ok\":true,\"sessions\":[\"conv-of-{pod}\"]}}\n");
            let mut out = vec![1u8];
            out.extend_from_slice(line.as_bytes());
            let _ = s.send(Message::Binary(out.into())).await;
            let status = if fail {
                "{\"status\":\"Failure\",\"reason\":\"NonZeroExitCode\"}"
            } else {
                "{\"status\":\"Success\"}"
            };
            let mut st = vec![3u8];
            st.extend_from_slice(status.as_bytes());
            let _ = s.send(Message::Binary(st.into())).await;
            let _ = s.send(Message::Close(None)).await;
        })
        .into_response()
}

async fn serve(f: Fake) -> String {
    let app = axum::Router::new()
        .route("/api/v1/namespaces/jarvis/pods", get(list))
        .route("/api/v1/namespaces/jarvis/pods/{pod}/exec", get(exec))
        .with_state(f);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

#[tokio::test]
async fn every_ui_pod_is_read_and_a_failed_exec_spoils_the_read() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let base = serve(Fake {
        pods: vec![("ui-a", false), ("ui-b", true)],
        failing: "none",
        calls: calls.clone(),
    })
    .await;
    let k = kube::Kube::new(&base, "t0ken", None).unwrap();
    let pods = k.ui_pods().await.unwrap();
    assert_eq!(pods.len(), 2);
    assert!(pods[1].terminating);
    let set = kube::running_set(&k).await;
    assert!(set.complete);
    assert_eq!(set.pods, 2);
    assert!(set.may_run("cc_ui-a", "conv-of-ui-a"));
    assert!(set.may_run("cc_ui-b", "conv-of-ui-b"), "a Terminating pod is read too");
    assert!(!set.may_run("cc_ui-a", "other"));
    let called: BTreeSet<String> = calls.lock().unwrap().iter().cloned().collect();
    assert_eq!(called, ["ui-a".to_owned(), "ui-b".to_owned()].into_iter().collect());

    let base = serve(Fake {
        pods: vec![("ui-a", false), ("ui-c", false)],
        failing: "ui-c",
        calls: Arc::new(Mutex::new(Vec::new())),
    })
    .await;
    let set = kube::running_set(&kube::Kube::new(&base, "t0ken", None).unwrap()).await;
    assert!(!set.complete);
    assert!(set.may_run("cc_ui-a", "other"), "an incomplete read holds everything");

    let wrong = kube::Kube::new(&base, "nope", None).unwrap();
    assert!(wrong.ui_pods().await.is_err());
    assert!(!kube::running_set(&wrong).await.complete);
}
