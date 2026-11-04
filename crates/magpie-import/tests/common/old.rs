//! A fake old cluster's Kubernetes API: the ui pods (some Terminating), `pods/exec` answering `holds.js` with each
//! pod's children, and `pods/proxy` answering the switch module's `/__jarvis/sockets` and `/__jarvis/restart`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

/// The API's bearer.
pub const TOKEN: &str = "t0ken";
/// The switch module's token.
pub const SWITCH: &str = "sw";

#[derive(Default)]
pub struct Old {
    /// pod → terminating
    pub pods: Vec<(String, bool)>,
    /// pod → its children's lines (schema, ok, sessions)
    pub children: HashMap<String, Vec<(String, bool, Vec<String>)>>,
    /// pod → (non-LU sockets, non-LU http); absent: no report (404)
    pub reports: HashMap<String, (u64, u64)>,
    /// pod → person → (non-LU sockets, non-LU http) in the report's `persons`
    pub persons: HashMap<String, HashMap<String, (u64, u64)>>,
    /// pod → its restart route's answer (`none`, `stopped`, `busy`); absent: `none`
    pub restart: HashMap<String, String>,
    /// every restart call: (pod, person)
    pub restarts: Vec<(String, String)>,
}

pub type Fake = Arc<Mutex<Old>>;

fn authorised(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn switch(h: &HeaderMap) -> bool {
    authorised(h) && h.get("x-jarvis-switch-token").and_then(|v| v.to_str().ok()) == Some(SWITCH)
}

async fn list(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !authorised(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let items: Vec<serde_json::Value> = f
        .lock()
        .unwrap()
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

async fn exec(State(f): State<Fake>, Path(pod): Path<String>, h: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !authorised(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let lines: String = f
        .lock()
        .unwrap()
        .children
        .get(&pod)
        .cloned()
        .unwrap_or_default()
        .iter()
        .fold(String::new(), |mut out, (schema, ok, s)| {
            out.push_str(&serde_json::json!({"schema": schema, "port": 3001, "ok": ok, "sessions": s}).to_string());
            out.push('\n');
            out
        });
    ws.protocols(["v4.channel.k8s.io"])
        .on_upgrade(move |mut s| async move {
            let mut out = vec![1u8];
            out.extend_from_slice(lines.as_bytes());
            let _ = s.send(Message::Binary(out.into())).await;
            let mut st = vec![3u8];
            st.extend_from_slice(b"{\"status\":\"Success\"}");
            let _ = s.send(Message::Binary(st.into())).await;
            let _ = s.send(Message::Close(None)).await;
        })
        .into_response()
}

async fn sockets(State(f): State<Fake>, Path(pod): Path<String>, h: HeaderMap) -> Response {
    if !switch(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(pod) = pod.strip_suffix(":8080") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let o = f.lock().unwrap();
    let Some((sockets, http)) = o.reports.get(pod).copied() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let persons: serde_json::Map<String, serde_json::Value> = o
        .persons
        .get(pod)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(p, (s, h))| {
            let socks: Vec<serde_json::Value> = (0..s)
                .map(|i| serde_json::json!({"id": i, "outstanding": 0, "opaque": false}))
                .collect();
            (p, serde_json::json!({"sockets": socks, "lu": 0, "http": h}))
        })
        .collect();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    axum::Json(serde_json::json!({
        "generatedAt": now, "armed": true, "persons": persons,
        "totals": {"sockets": sockets, "opaque": 0, "lu": 2, "luOpaque": 0, "http": http, "passOnInFlight": 0,
                   "hopSockets": 0, "hopOpaque": 0},
        "counters": {}
    }))
    .into_response()
}

async fn restart(
    State(f): State<Fake>,
    Path(pod): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Response {
    if !switch(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(pod) = pod.strip_suffix(":8080") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(person) = q.get("person").cloned() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut o = f.lock().unwrap();
    o.restarts.push((pod.to_owned(), person));
    let result = o.restart.get(pod).cloned().unwrap_or_else(|| "none".to_owned());
    let mut body = serde_json::json!({ "result": result });
    if result == "busy" {
        body["why"] = "running".into();
    }
    axum::Json(body).into_response()
}

/// Serves the fake; its base URL.
pub async fn serve(f: Fake) -> String {
    let app = axum::Router::new()
        .route("/api/v1/namespaces/jarvis/pods", get(list))
        .route("/api/v1/namespaces/jarvis/pods/{pod}/exec", get(exec))
        .route(
            "/api/v1/namespaces/jarvis/pods/{pod}/proxy/__jarvis/sockets",
            get(sockets),
        )
        .route(
            "/api/v1/namespaces/jarvis/pods/{pod}/proxy/__jarvis/restart",
            post(restart),
        )
        .with_state(f);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

/// A fake with `pods` (none Terminating), each with a clean report and no children; and its API.
pub async fn cluster(pods: &[&str]) -> (Fake, magpie_import::kube::Kube) {
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        for p in pods {
            o.pods.push(((*p).to_owned(), false));
            o.reports.insert((*p).to_owned(), (0, 0));
        }
    }
    let kube = magpie_import::kube::Kube::new(&serve(fake.clone()).await, TOKEN, None).unwrap();
    (fake, kube)
}
